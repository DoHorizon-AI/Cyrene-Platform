//! Platform-owned v2 Authority RPC implementation.
//!
//! This module independently authenticates browser bearers, resolves current Directory roles,
//! authorizes against the active pinned policy, and persists approvals through the unique outbox.

use std::{collections::BTreeSet, sync::Arc, time::Duration};

use async_trait::async_trait;
use cy_proto::{
    cyrene::workspace::authority::v2 as wire,
    cyrene::workspace::authority::v2::workspace_authority_service_server::WorkspaceAuthorityService,
};
use cy_workspace_product_contracts::{
    ProductInvocationResponse, TrustedWorkspaceScope, VerifiedProductPrincipal,
};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use prost::Message;
use sha2::{Digest, Sha256};
use tonic::{Request, Response, Status};
use uuid::Uuid;
use zeroize::Zeroize;
use zeroize::Zeroizing;

use crate::deployment_admission::{
    DeploymentAdmission, DeploymentAdmissionError, DeploymentIdentity,
};
use crate::{
    authority::{
        now_unix_ms, AuthorityOutboxError, AuthorityOutboxStore, EnqueueOutcome,
        ExecutionDevicePeer, InvocationWaitScope, NewAuthorityInvocation,
        PersistedInvocationResult,
    },
    device_registry::{
        CurrentRelayPeerRevocationEvidence, RelayPeerRevocationCheckError,
        WorkspaceDevicePeerCertificateStatusChecker, WorkspaceDeviceRegistry,
    },
    directory::{validate_descriptor, WorkspaceDirectory},
    web_identity::{VerifiedWebPrincipal, WebPrincipalVerifier},
};

/// Keep helper errors small while preserving tonic's sanitized status at the RPC boundary.
#[derive(Debug)]
struct AuthorityServiceError(Box<Status>);

impl From<Status> for AuthorityServiceError {
    fn from(status: Status) -> Self {
        Self(Box::new(status))
    }
}

impl From<AuthorityServiceError> for Status {
    fn from(error: AuthorityServiceError) -> Self {
        *error.0
    }
}

type AuthorityHelperResult<T> = Result<T, AuthorityServiceError>;

const MAX_CONNECTOR_CRL_EVIDENCE_AGE_MS: u64 = 5 * 60 * 1_000;

/// Durable identity for one Authority-verified bearer session.
#[derive(Debug, Clone)]
pub struct AuthorityWebSession {
    /// Persistent session identifier.
    pub session_id: Uuid,
    /// Directory organization.
    pub organization_id: String,
    /// Exact membership workspace.
    pub workspace_id: String,
    /// Verified OIDC issuer.
    pub principal_issuer: String,
    /// Verified OIDC subject.
    pub principal_subject: String,
    /// Durable session generation.
    pub session_generation: u64,
    /// Stable issuance time from the persistent session record.
    pub issued_at_unix_ms: u64,
    /// Token expiry from verified claims.
    pub expires_at_unix_ms: u64,
}

/// Persistent store for sessions created from verified bearer tokens.
#[async_trait]
pub trait AuthorityWebSessionStore: Send + Sync {
    /// Resolve or create the exact verified bearer session.
    async fn resolve_verified_bearer(
        &self,
        principal: &VerifiedWebPrincipal,
        workspace_id: &str,
        bearer_token: &str,
        now_unix_ms: u64,
    ) -> Result<AuthorityWebSession, Status>;
}

/// Operator-provisioned operation binding to one active Connector device.
#[derive(Debug, Clone)]
pub struct AuthorityTargetBinding {
    /// Component allowed to execute the operation.
    pub target_component: String,
    /// Exact execution device resolved by the trusted configuration.
    pub execution_device_id: String,
    /// Current Directory device generation.
    pub execution_device_generation: u64,
    /// Device authorization record UUID.
    pub execution_authorization_id: Uuid,
    /// SHA-256 fingerprint of the target certificate.
    pub certificate_fingerprint_sha256: [u8; 32],
    /// SHA-256 of the signed target binding manifest.
    pub target_binding_manifest_sha256: [u8; 32],
    /// SHA-256 of the active bundle manifest.
    pub bundle_manifest_sha256: [u8; 32],
    /// Active contract generation bound to the target row.
    pub contract_activation_generation: u64,
    /// Pinned owner source commit.
    pub owner_source_commit: String,
    /// Trusted HTTPS base URL; never supplied by the caller.
    pub endpoint_base: String,
}

/// Read-only operation target resolver.
#[async_trait]
pub trait AuthorityExecutionTargetStore: Send + Sync {
    /// Resolve one exact operation target.
    async fn resolve(
        &self,
        organization_id: &str,
        workspace_id: &str,
        owner_id: &str,
        operation_id: &str,
    ) -> Result<Option<AuthorityTargetBinding>, Status>;
}

/// Ed25519 key used for short-lived execution credentials.
#[derive(Clone)]
pub struct AuthorityCredentialSigner {
    key_id: Arc<str>,
    key: Arc<SigningKey>,
    ttl: Duration,
}

impl AuthorityCredentialSigner {
    /// Create a signer from key bytes loaded by the protected host configuration.
    /// This public host-composition API retains tonic's standard status error type.
    #[allow(clippy::result_large_err)]
    pub fn new(
        key_id: impl Into<Arc<str>>,
        key: SigningKey,
        ttl: Duration,
    ) -> Result<Self, Status> {
        let key_id = key_id.into();
        if key_id.is_empty()
            || key_id.len() > 128
            || ttl.is_zero()
            || ttl > Duration::from_secs(300)
        {
            return Err(Status::failed_precondition(
                "authority signing configuration is invalid",
            ));
        }
        Ok(Self {
            key_id,
            key: Arc::new(key),
            ttl,
        })
    }

    /// Public key used by Connector credential verification.
    pub fn verifying_key(&self) -> VerifyingKey {
        self.key.verifying_key()
    }

    fn sign(&self, credential: &mut wire::ExecutionCredential) -> AuthorityHelperResult<()> {
        let mut message = signature_message(&self.key_id, &credential.request_digest_sha256)?;
        credential.signing_key_id = self.key_id.to_string();
        credential.authority_signature = self.key.sign(&message).to_bytes().to_vec();
        message.zeroize();
        Ok(())
    }

    fn verify(&self, credential: &wire::ExecutionCredential) -> AuthorityHelperResult<()> {
        if credential.signing_key_id != self.key_id.as_ref()
            || credential.request_digest_sha256.len() != 32
        {
            return Err(Status::permission_denied("execution credential is invalid").into());
        }
        let message = signature_message(&self.key_id, &credential.request_digest_sha256)?;
        let signature = Signature::from_slice(&credential.authority_signature)
            .map_err(|_| Status::permission_denied("execution credential is invalid"))?;
        self.key
            .verifying_key()
            .verify(&message, &signature)
            .map_err(|_| Status::permission_denied("execution credential is invalid"))?;
        Ok(())
    }
}

/// All durable and trusted dependencies for the Authority process.
pub struct AuthorityServiceDependencies {
    /// Root-configured Organization identity used by the first-adoption admission receipt.
    pub organization_id: String,
    /// Root-configured Workspace identity used by the first-adoption admission receipt.
    pub workspace_id: String,
    /// Root-configured Authority instance identity used by the first-adoption receipt.
    pub authority_instance_id: String,
    /// Persistent active catalog/policy manager.
    pub snapshots: Arc<crate::authority::ContractSnapshotManager>,
    /// Independently configured Entra verifier.
    pub web_verifier: Arc<dyn WebPrincipalVerifier>,
    /// Authoritative membership and role source.
    pub directory: Arc<dyn WorkspaceDirectory>,
    /// Persistent web sessions.
    pub sessions: Arc<dyn AuthorityWebSessionStore>,
    /// Operator-managed execution targets.
    pub targets: Arc<dyn AuthorityExecutionTargetStore>,
    /// Certificate registry for real Connector mTLS peers.
    pub device_registry: Arc<dyn WorkspaceDeviceRegistry>,
    /// Fresh signed-CRL status checker for Tonic-authenticated Connector peer certificates.
    pub device_certificate_status: Arc<dyn WorkspaceDevicePeerCertificateStatusChecker>,
    /// Unique durable invocation outbox.
    pub outbox: Arc<dyn AuthorityOutboxStore>,
    /// Protected-config signer.
    pub signer: AuthorityCredentialSigner,
}

/// Independent Authority v2 service.
#[derive(Clone)]
pub struct AuthorityRpcService {
    dependencies: Arc<AuthorityServiceDependencies>,
    deployment_admission: DeploymentAdmission,
    deployment_identity: DeploymentIdentity,
}

impl AuthorityRpcService {
    /// Construct a service with production dependencies; no in-memory fallback exists.
    pub fn new(dependencies: AuthorityServiceDependencies) -> Self {
        let deployment_identity = DeploymentIdentity {
            organization_id: dependencies.organization_id.clone(),
            workspace_id: dependencies.workspace_id.clone(),
            authority_instance_id: dependencies.authority_instance_id.clone(),
        };
        let deployment_admission =
            DeploymentAdmission::from_environment(deployment_identity.clone())
                .unwrap_or_else(|_| DeploymentAdmission::invalid_configuration());
        Self {
            dependencies: Arc::new(dependencies),
            deployment_admission,
            deployment_identity,
        }
    }

    async fn authenticate<T>(
        &self,
        request: &Request<T>,
        workspace_id: &str,
    ) -> Result<(VerifiedWebPrincipal, BTreeSet<String>, AuthorityWebSession), Status> {
        let token = Zeroizing::new(bearer_from_metadata(request)?);
        let principal = self
            .dependencies
            .web_verifier
            .verify_access_token(&token)
            .await
            .map_err(map_identity_error)?;
        if workspace_id.trim().is_empty() || workspace_id.len() > 256 {
            return Err(Status::invalid_argument("workspace id is invalid"));
        }
        let roles = self
            .dependencies
            .directory
            .roles_for_member(
                principal.identity(),
                principal.organization_id(),
                workspace_id,
            )
            .await
            .map_err(|_| Status::unavailable("workspace directory is unavailable"))?
            .ok_or_else(|| Status::permission_denied("workspace membership is required"))?;
        let session = self
            .dependencies
            .sessions
            .resolve_verified_bearer(&principal, workspace_id, &token, now_unix_ms())
            .await?;
        if session.organization_id != principal.organization_id()
            || session.workspace_id != workspace_id
            || session.principal_issuer != principal.identity().issuer
            || session.principal_subject != principal.identity().subject
            || session.expires_at_unix_ms <= now_unix_ms()
        {
            return Err(Status::permission_denied("web session scope is invalid"));
        }
        Ok((principal, roles, session))
    }

    async fn connector_peer<T>(&self, request: &Request<T>) -> Result<ExecutionDevicePeer, Status> {
        let certificates = request
            .peer_certs()
            .ok_or_else(|| Status::unauthenticated("Connector certificate is required"))?;
        let certificate = certificates
            .first()
            .ok_or_else(|| Status::unauthenticated("Connector certificate is required"))?;
        let intermediate_chain_der = certificates
            .iter()
            .skip(1)
            .map(|certificate| certificate.as_ref().to_vec())
            .collect::<Vec<_>>();
        connector_peer_from_certificate_chain(
            self.dependencies.device_certificate_status.as_ref(),
            self.dependencies.device_registry.as_ref(),
            certificate.as_ref(),
            &intermediate_chain_der,
            now_unix_ms(),
        )
        .map_err(ConnectorPeerAdmissionError::into_status)
    }

    async fn validate_product_result(
        &self,
        credential: &wire::ExecutionCredential,
        approved_invocation: &cy_proto::cyrene::workspace::product::v2::ProductApiInvocationV2,
        outcome: wire::ExecutionOutcomeStatus,
        response: Option<&cy_proto::cyrene::workspace::product::v2::ProductApiResponseV2>,
    ) -> Result<(), Status> {
        if outcome != wire::ExecutionOutcomeStatus::Success {
            if response.is_some() {
                return Err(Status::invalid_argument(
                    "failed execution must not include a successful Product response",
                ));
            }
            return Ok(());
        }
        let snapshot = self
            .dependencies
            .snapshots
            .active_snapshot()
            .map_err(|_| Status::unavailable("active Authority snapshot is unavailable"))?;
        if snapshot.generation != credential.contract_activation_generation {
            return Err(Status::aborted(
                "contract activation changed before result validation",
            ));
        }
        snapshot
            .bundle
            .operation(&credential.operation_owner_id, &credential.operation_id)
            .ok_or_else(|| Status::permission_denied("approved operation is no longer active"))?;
        let roles = self
            .dependencies
            .directory
            .roles_for_member(
                &cy_proto::workspace_v1::UserIdentityRef {
                    issuer: credential.principal_issuer.clone(),
                    subject: credential.principal_subject.clone(),
                },
                &credential.organization_id,
                &credential.workspace_id,
            )
            .await
            .map_err(|_| Status::unavailable("workspace directory is unavailable"))?
            .ok_or_else(|| {
                Status::permission_denied("principal is no longer a workspace member")
            })?;
        let product_invocation = snapshot
            .policy
            .authorize(
                &snapshot.bundle,
                approved_invocation.clone(),
                &VerifiedProductPrincipal::directory_user(roles),
                TrustedWorkspaceScope::new(&credential.organization_id, &credential.workspace_id)
                    .map_err(|_| Status::permission_denied("approved scope is invalid"))?,
            )
            .map_err(map_policy_error)?;
        let response = response.ok_or_else(|| {
            Status::invalid_argument("successful execution must include a Product response")
        })?;
        product_invocation
            .validate_response(&ProductInvocationResponse {
                status_code: u16::try_from(response.status_code)
                    .map_err(|_| Status::data_loss("Product response status is invalid"))?,
                content_type: response.content_type.clone(),
                json_body: response.json_body.clone(),
            })
            .map_err(|_| Status::data_loss("Product response violates the pinned schema or scope"))
    }
}

/// Check current signed-CRL evidence before resolving a peer's active Registry identity.
///
/// `leaf_certificate_der` and `intermediate_chain_der` must come only from the server-side Tonic
/// peer-certificate extension. Registry lookup remains a separate required authorization fence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectorPeerAdmissionError {
    CertificateRequired,
    CertificateRevoked,
    RevocationStatusUnavailable,
    RegistryUnavailable,
    CertificateNotActive,
    RegistryIdentityInvalid,
}

impl ConnectorPeerAdmissionError {
    fn into_status(self) -> Status {
        match self {
            Self::CertificateRequired => {
                Status::unauthenticated("Connector certificate is required")
            }
            Self::CertificateRevoked => Status::unauthenticated("Connector certificate is revoked"),
            Self::RevocationStatusUnavailable => {
                Status::unavailable("Connector certificate revocation status is unavailable")
            }
            Self::RegistryUnavailable => Status::unavailable("Connector registry is unavailable"),
            Self::CertificateNotActive => {
                Status::unauthenticated("Connector certificate is not active")
            }
            Self::RegistryIdentityInvalid => {
                Status::unauthenticated("Connector registry identity is invalid")
            }
        }
    }
}

fn connector_peer_from_certificate_chain(
    status_checker: &dyn WorkspaceDevicePeerCertificateStatusChecker,
    device_registry: &dyn WorkspaceDeviceRegistry,
    leaf_certificate_der: &[u8],
    intermediate_chain_der: &[Vec<u8>],
    checked_at_unix_ms: u64,
) -> Result<ExecutionDevicePeer, ConnectorPeerAdmissionError> {
    if leaf_certificate_der.is_empty() {
        return Err(ConnectorPeerAdmissionError::CertificateRequired);
    }

    let fingerprint: [u8; 32] = Sha256::digest(leaf_certificate_der).into();
    let revocation_evidence = status_checker
        .require_current_good_status_for_peer_chain(
            leaf_certificate_der,
            intermediate_chain_der,
            checked_at_unix_ms,
        )
        .map_err(|error| match error {
            RelayPeerRevocationCheckError::Revoked => {
                ConnectorPeerAdmissionError::CertificateRevoked
            }
            RelayPeerRevocationCheckError::Unknown => {
                ConnectorPeerAdmissionError::RevocationStatusUnavailable
            }
        })?;
    if !current_revocation_evidence_matches(&revocation_evidence, &fingerprint, checked_at_unix_ms)
    {
        return Err(ConnectorPeerAdmissionError::RevocationStatusUnavailable);
    }

    let identity = device_registry
        .find_current_device_certificate_identity(&encode_hex(&fingerprint))
        .map_err(|_| ConnectorPeerAdmissionError::RegistryUnavailable)?
        .ok_or(ConnectorPeerAdmissionError::CertificateNotActive)?;
    let digest: [u8; 32] = decode_hex(&identity.certificate_fingerprint_sha256)
        .map_err(|_| ConnectorPeerAdmissionError::RegistryIdentityInvalid)?
        .try_into()
        .map_err(|_| ConnectorPeerAdmissionError::RegistryIdentityInvalid)?;
    if digest != fingerprint {
        return Err(ConnectorPeerAdmissionError::RegistryIdentityInvalid);
    }

    Ok(ExecutionDevicePeer {
        organization_id: identity.key.organization_id,
        workspace_id: identity.key.workspace_id,
        device_id: identity.key.device_id,
        authorization_id: identity.authorization_id,
        authorization_generation: identity.authorization_generation,
        certificate_fingerprint_sha256: digest,
    })
}

fn current_revocation_evidence_matches(
    evidence: &CurrentRelayPeerRevocationEvidence,
    certificate_fingerprint: &[u8; 32],
    checked_at_unix_ms: u64,
) -> bool {
    evidence.certificate_sha256 == *certificate_fingerprint
        && evidence.this_update_unix_ms <= checked_at_unix_ms
        && evidence.next_update_unix_ms > checked_at_unix_ms
        && evidence.next_update_unix_ms > evidence.this_update_unix_ms
        && checked_at_unix_ms.saturating_sub(evidence.this_update_unix_ms)
            <= MAX_CONNECTOR_CRL_EVIDENCE_AGE_MS
}

#[tonic::async_trait]
impl WorkspaceAuthorityService for AuthorityRpcService {
    async fn negotiate_version(
        &self,
        request: Request<wire::NegotiateVersionRequest>,
    ) -> Result<Response<wire::NegotiateVersionResponse>, Status> {
        let request = request.into_inner();
        if request.minimum_version > request.maximum_version {
            return Err(Status::invalid_argument("version range is invalid"));
        }
        if request.minimum_version > 2 || request.maximum_version < 2 {
            return Err(Status::failed_precondition(
                "no mutually supported Authority version",
            ));
        }
        Ok(Response::new(wire::NegotiateVersionResponse {
            selected_version: 2,
            supported_versions: vec![2],
        }))
    }

    async fn verify_identity(
        &self,
        request: Request<wire::VerifyIdentityRequest>,
    ) -> Result<Response<wire::VerifyIdentityResponse>, Status> {
        let workspace_id = request.get_ref().workspace_id.clone();
        if workspace_id.is_empty() {
            let token = Zeroizing::new(bearer_from_metadata(&request)?);
            let principal = self
                .dependencies
                .web_verifier
                .verify_access_token(&token)
                .await
                .map_err(map_identity_error)?;
            let now = now_unix_ms();
            let descriptors = self
                .dependencies
                .directory
                .discover(principal.identity(), principal.organization_id(), now)
                .await
                .map_err(|_| Status::unavailable("workspace directory is unavailable"))?;
            let mut permitted_workspaces = Vec::with_capacity(descriptors.len());
            for descriptor in descriptors {
                validate_descriptor(&descriptor, now).map_err(|_| {
                    Status::data_loss("workspace directory returned an invalid descriptor")
                })?;
                if descriptor.organization_id != principal.organization_id() {
                    return Err(Status::data_loss(
                        "workspace directory returned an invalid organization",
                    ));
                }
                permitted_workspaces.push(descriptor.workspace_id);
            }
            permitted_workspaces.sort();
            if permitted_workspaces
                .windows(2)
                .any(|pair| pair[0] == pair[1])
            {
                return Err(Status::data_loss(
                    "workspace directory returned duplicate workspaces",
                ));
            }
            return Ok(Response::new(wire::VerifyIdentityResponse {
                principal_id: principal.identity().subject.clone(),
                issuer: principal.identity().issuer.clone(),
                subject: principal.identity().subject.clone(),
                organization_id: principal.organization_id().to_owned(),
                workspace_id: String::new(),
                session_id: String::new(),
                session_generation: 0,
                permitted_workspaces,
                directory_roles: Vec::new(),
            }));
        }
        let (principal, roles, session) = self.authenticate(&request, &workspace_id).await?;
        Ok(Response::new(wire::VerifyIdentityResponse {
            principal_id: principal.identity().subject.clone(),
            issuer: principal.identity().issuer.clone(),
            subject: principal.identity().subject.clone(),
            organization_id: principal.organization_id().to_owned(),
            workspace_id: workspace_id.clone(),
            session_id: session.session_id.to_string(),
            session_generation: session.session_generation,
            permitted_workspaces: vec![workspace_id],
            directory_roles: roles.into_iter().collect(),
        }))
    }

    async fn get_catalog_snapshot(
        &self,
        request: Request<wire::CatalogSnapshotRequest>,
    ) -> Result<Response<wire::CatalogSnapshotResponse>, Status> {
        let workspace_id = request.get_ref().workspace_id.clone();
        let (principal, _, _) = self.authenticate(&request, &workspace_id).await?;
        let snapshot = self
            .dependencies
            .snapshots
            .active_snapshot()
            .map_err(|_| Status::unavailable("active Authority snapshot is unavailable"))?;
        let mut owners = Vec::new();
        for owner_id in snapshot.identity.source_commits.keys() {
            let granted_operation_ids = snapshot
                .bundle
                .operations()
                .filter(|operation| {
                    operation.owner_id() == owner_id
                        && snapshot
                            .policy
                            .has_grant(owner_id, operation.operation_id())
                })
                .map(|operation| operation.operation_id().to_owned())
                .collect();
            owners.push(wire::OwnerSnapshot {
                owner_id: owner_id.clone(),
                component_id: format!("cy-workspace-{owner_id}"),
                source_commit: snapshot.identity.source_commits[owner_id].clone(),
                catalog_digest_sha256: snapshot
                    .identity
                    .owner_catalog_digests
                    .get(owner_id)
                    .cloned()
                    .ok_or_else(|| {
                        Status::data_loss(
                            "owner catalog digest is missing from the verified snapshot",
                        )
                    })?,
                granted_operation_ids,
            });
        }
        Ok(Response::new(wire::CatalogSnapshotResponse {
            organization_id: principal.organization_id().to_owned(),
            workspace_id,
            wire_api_version: snapshot.identity.wire_api_version.clone(),
            contract_api_version: cy_workspace_product_contracts::CONTRACT_API_VERSION.to_owned(),
            catalog_generation: snapshot.generation,
            contract_activation_generation: snapshot.activation_epoch,
            bundle_manifest_sha256: snapshot.identity.bundle_manifest_sha256.clone(),
            policy_digest_sha256: snapshot.identity.policy_sha256.clone(),
            policy_schema_version: snapshot.pins.policy_schema_version().to_owned(),
            owners,
            artifact_id: snapshot.identity.content_digest.clone(),
        }))
    }

    async fn approve_and_enqueue_invocation(
        &self,
        request: Request<wire::ApproveAndEnqueueInvocationRequest>,
    ) -> Result<Response<wire::ApproveAndEnqueueInvocationResponse>, Status> {
        let workspace_id = request.get_ref().workspace_id.clone();
        let (principal, roles, session) = self.authenticate(&request, &workspace_id).await?;
        let invocation = request
            .into_inner()
            .invocation
            .ok_or_else(|| Status::invalid_argument("invocation is required"))?;
        let snapshot = self
            .dependencies
            .snapshots
            .active_snapshot()
            .map_err(|_| Status::unavailable("active Authority snapshot is unavailable"))?;
        let scope = TrustedWorkspaceScope::new(principal.organization_id(), workspace_id.as_str())
            .map_err(|_| Status::permission_denied("workspace scope is invalid"))?;
        // TrustedProductPolicy::authorize checks the approved operation, each required role,
        // request schema, resource/idempotency constraints and all mandatory request bindings.
        let authorized = snapshot
            .policy
            .authorize(
                &snapshot.bundle,
                invocation.clone(),
                &VerifiedProductPrincipal::directory_user(roles),
                scope,
            )
            .map_err(map_policy_error)?;
        let _admission = self
            .deployment_admission
            .acquire_dispatch(
                principal.organization_id(),
                &workspace_id,
                &self.deployment_identity.authority_instance_id,
            )
            .await
            .map_err(map_deployment_admission_error)?;
        let target = self
            .dependencies
            .targets
            .resolve(
                principal.organization_id(),
                &workspace_id,
                authorized.owner_id(),
                authorized.operation_id(),
            )
            .await?
            .ok_or_else(|| {
                Status::failed_precondition("no approved Connector target is configured")
            })?;
        if encode_hex(&target.bundle_manifest_sha256) != snapshot.identity.bundle_manifest_sha256
            || target.contract_activation_generation != snapshot.generation
            || snapshot.identity.source_commits.get(authorized.owner_id())
                != Some(&target.owner_source_commit)
        {
            return Err(Status::aborted(
                "target binding differs from the active contract snapshot",
            ));
        }
        let operation = snapshot
            .bundle
            .operation(authorized.owner_id(), authorized.operation_id())
            .ok_or_else(|| Status::permission_denied("operation is absent from active catalog"))?;
        let endpoint = resolve_endpoint(
            &target.endpoint_base,
            authorized.route().path_template(),
            authorized.route().path_parameters(),
        )?;
        let resource_id = authorized.resource_id().unwrap_or_default().to_owned();
        let idempotency_key = authorized.idempotency_key().unwrap_or_default().to_owned();
        let idempotency_semantics = if operation.idempotency().required {
            wire::IdempotencySemantics::Required as i32
        } else if operation.idempotency().header.is_some() {
            wire::IdempotencySemantics::Optional as i32
        } else {
            wire::IdempotencySemantics::NotSupported as i32
        };
        let scope = format!(
            "organization={};workspace={};resource={}",
            principal.organization_id(),
            workspace_id,
            resource_id
        );
        let invocation_digest = Sha256::digest(invocation.encode_to_vec()).to_vec();
        let invocation_id = Uuid::new_v4().to_string();
        let issued_ms = now_unix_ms();
        let expires_ms = session
            .expires_at_unix_ms
            .min(issued_ms.saturating_add(self.dependencies.signer.ttl.as_millis() as u64));
        if expires_ms <= issued_ms {
            return Err(Status::unauthenticated(
                "web session cannot authorize a new execution",
            ));
        }
        let issued_at = timestamp(issued_ms);
        let expires_at = timestamp(expires_ms);
        let target_message = wire::ExecutionTarget {
            target_component: target.target_component.clone(),
            http_method: authorized.route().method().to_owned(),
            endpoint: endpoint.clone(),
            resource_id: resource_id.clone(),
            scope: scope.clone(),
            idempotency_key: idempotency_key.clone(),
            idempotency_semantics,
            execution_device_id: target.execution_device_id.clone(),
            execution_device_generation: target.execution_device_generation,
            execution_authorization_id: target.execution_authorization_id.as_bytes().to_vec(),
            execution_device_certificate_sha256: target.certificate_fingerprint_sha256.to_vec(),
        };
        let envelope = wire::CanonicalInvocationEnvelope {
            invocation_id: invocation_id.clone(),
            invocation: Some(invocation),
            target: Some(target_message),
            organization_id: principal.organization_id().to_owned(),
            workspace_id: workspace_id.clone(),
            principal_issuer: principal.identity().issuer.clone(),
            principal_subject: principal.identity().subject.clone(),
            execution_device_id: target.execution_device_id.clone(),
            execution_device_generation: target.execution_device_generation,
            execution_authorization_id: target.execution_authorization_id.as_bytes().to_vec(),
            execution_device_certificate_sha256: target.certificate_fingerprint_sha256.to_vec(),
            session_id: session.session_id.to_string(),
            session_generation: session.session_generation,
            contract_activation_generation: snapshot.generation,
            issued_at: Some(issued_at),
            expires_at: Some(expires_at),
        };
        let canonical_envelope_proto = envelope.encode_to_vec();
        let request_digest_sha256 = Sha256::digest(&canonical_envelope_proto).to_vec();
        let mut credential = wire::ExecutionCredential {
            invocation_id: invocation_id.clone(),
            invocation_digest_sha256: invocation_digest,
            request_digest_sha256,
            organization_id: envelope.organization_id.clone(),
            workspace_id: envelope.workspace_id.clone(),
            principal_issuer: envelope.principal_issuer.clone(),
            principal_subject: envelope.principal_subject.clone(),
            operation_owner_id: authorized.owner_id().to_owned(),
            operation_id: authorized.operation_id().to_owned(),
            scope,
            resource_id,
            target_component: target.target_component.clone(),
            http_method: authorized.route().method().to_owned(),
            endpoint,
            execution_device_id: target.execution_device_id,
            execution_device_generation: target.execution_device_generation,
            execution_authorization_id: target.execution_authorization_id.as_bytes().to_vec(),
            execution_device_certificate_sha256: target.certificate_fingerprint_sha256.to_vec(),
            session_id: session.session_id.to_string(),
            session_generation: session.session_generation,
            contract_activation_generation: snapshot.generation,
            idempotency_key,
            idempotency_semantics,
            signing_key_id: String::new(),
            issued_at: Some(issued_at),
            expires_at: Some(expires_at),
            authority_signature: Vec::new(),
        };
        self.dependencies.signer.sign(&mut credential)?;
        let approved_invocation = wire::ApprovedInvocation {
            invocation_id: invocation_id.clone(),
            canonical_envelope: Some(envelope),
            credential: Some(credential),
        };
        let record = self
            .dependencies
            .outbox
            .enqueue_authorized(NewAuthorityInvocation {
                approved_invocation,
                canonical_envelope_proto,
                target_binding_manifest_sha256: target.target_binding_manifest_sha256,
                bundle_manifest_sha256: target.bundle_manifest_sha256,
                owner_source_commit: target.owner_source_commit,
            })
            .await
            .map_err(map_outbox_error)?;
        let record = match record {
            EnqueueOutcome::Inserted(record) | EnqueueOutcome::Existing(record) => record,
        };
        Ok(Response::new(wire::ApproveAndEnqueueInvocationResponse {
            invocation_id: record.approved_invocation.invocation_id,
            state: record.state as i32,
        }))
    }

    async fn claim_invocations(
        &self,
        request: Request<wire::ClaimInvocationsRequest>,
    ) -> Result<Response<wire::ClaimInvocationsResponse>, Status> {
        let peer = self.connector_peer(&request).await?;
        let _admission = self
            .deployment_admission
            .acquire_dispatch(
                &peer.organization_id,
                &peer.workspace_id,
                &self.deployment_identity.authority_instance_id,
            )
            .await
            .map_err(map_deployment_admission_error)?;
        let max_batch_size = request.get_ref().max_batch_size.clamp(1, 32) as usize;
        let records = self
            .dependencies
            .outbox
            .claim_for_execution_device(&peer, max_batch_size)
            .await
            .map_err(map_outbox_error)?;
        let mut invocations = Vec::with_capacity(records.len());
        for record in records {
            verify_approved(&record.approved_invocation)?;
            let credential = record
                .approved_invocation
                .credential
                .as_ref()
                .ok_or_else(|| Status::data_loss("approved invocation is incomplete"))?;
            self.dependencies.signer.verify(credential)?;
            invocations.push(record.approved_invocation);
        }
        Ok(Response::new(wire::ClaimInvocationsResponse {
            invocations,
        }))
    }

    async fn validate_execution_authorization(
        &self,
        request: Request<wire::ValidateExecutionAuthorizationRequest>,
    ) -> Result<Response<wire::ValidateExecutionAuthorizationResponse>, Status> {
        let peer = self.connector_peer(&request).await?;
        let credential = request
            .into_inner()
            .credential
            .ok_or_else(|| Status::invalid_argument("credential is required"))?;
        self.dependencies.signer.verify(&credential)?;
        validate_credential_time(&credential)?;
        let snapshot = self
            .dependencies
            .snapshots
            .active_snapshot()
            .map_err(|_| Status::unavailable("active Authority snapshot is unavailable"))?;
        if credential.contract_activation_generation != snapshot.generation {
            return Err(Status::permission_denied(
                "execution credential belongs to an inactive contract generation",
            ));
        }
        let record = self
            .dependencies
            .outbox
            .validate_execution_credential(&peer, &credential)
            .await
            .map_err(map_outbox_error)?;
        verify_approved(&record.approved_invocation)?;
        Ok(Response::new(
            wire::ValidateExecutionAuthorizationResponse {
                approved_invocation: Some(record.approved_invocation),
            },
        ))
    }

    async fn acknowledge_delivery(
        &self,
        request: Request<wire::AcknowledgeDeliveryRequest>,
    ) -> Result<Response<wire::AcknowledgeDeliveryResponse>, Status> {
        let peer = self.connector_peer(&request).await?;
        let request = request.into_inner();
        let credential = request
            .credential
            .ok_or_else(|| Status::invalid_argument("credential is required"))?;
        self.dependencies.signer.verify(&credential)?;
        validate_credential_time(&credential)?;
        let acknowledged = self
            .dependencies
            .outbox
            .acknowledge_delivery(
                &peer,
                &request.invocation_id,
                &credential,
                &request.delivery_receipt,
            )
            .await
            .map_err(map_outbox_error)?;
        Ok(Response::new(wire::AcknowledgeDeliveryResponse {
            acknowledged,
            state: wire::InvocationState::Acknowledged as i32,
        }))
    }

    async fn submit_invocation_result(
        &self,
        request: Request<wire::SubmitInvocationResultRequest>,
    ) -> Result<Response<wire::SubmitInvocationResultResponse>, Status> {
        let peer = self.connector_peer(&request).await?;
        let request = request.into_inner();
        let credential = request
            .credential
            .ok_or_else(|| Status::invalid_argument("credential is required"))?;
        if request.invocation_id != credential.invocation_id {
            return Err(Status::permission_denied(
                "result does not match execution credential",
            ));
        }
        self.dependencies.signer.verify(&credential)?;
        validate_credential_time(&credential)?;
        let result = request
            .result
            .ok_or_else(|| Status::invalid_argument("canonical result envelope is required"))?;
        if result.invocation_id != request.invocation_id
            || result.request_digest_sha256 != credential.request_digest_sha256
        {
            return Err(Status::permission_denied(
                "result is not bound to its approved invocation",
            ));
        }
        let result_digest = Sha256::digest(result.encode_to_vec());
        if result_digest.as_slice() != request.result_digest_sha256.as_slice() {
            return Err(Status::invalid_argument(
                "canonical result digest is invalid",
            ));
        }
        let outcome_status = wire::ExecutionOutcomeStatus::try_from(result.outcome_status)
            .map_err(|_| Status::invalid_argument("execution result status is invalid"))?;
        let durable = self
            .dependencies
            .outbox
            .validate_execution_credential(&peer, &credential)
            .await
            .map_err(map_outbox_error)?;
        let approved_product_invocation = durable
            .approved_invocation
            .canonical_envelope
            .as_ref()
            .and_then(|envelope| envelope.invocation.as_ref())
            .ok_or_else(|| Status::data_loss("persisted approved invocation is incomplete"))?;
        self.validate_product_result(
            &credential,
            approved_product_invocation,
            outcome_status,
            result.product_response.as_ref(),
        )
        .await?;
        let result = PersistedInvocationResult {
            invocation_id: request.invocation_id,
            credential,
            outcome_status,
            product_response: result.product_response,
            result_digest_sha256: result_digest.to_vec(),
            error_code: result.error_code,
            error_message: result.error_message.chars().take(512).collect(),
            delivery_receipt: result.delivery_receipt,
            result_receipt: Vec::new(),
        };
        let outcome = self
            .dependencies
            .outbox
            .submit_result(&peer, result)
            .await
            .map_err(map_outbox_error)?;
        Ok(Response::new(wire::SubmitInvocationResultResponse {
            accepted: outcome.accepted,
            state: outcome.state as i32,
            result_receipt: outcome.result_receipt,
        }))
    }

    async fn wait_invocation_result(
        &self,
        request: Request<wire::WaitInvocationResultRequest>,
    ) -> Result<Response<wire::WaitInvocationResultResponse>, Status> {
        let workspace = request.get_ref().workspace_id.clone();
        let (principal, _, session) = self.authenticate(&request, &workspace).await?;
        let request = request.into_inner();
        let scope = InvocationWaitScope {
            organization_id: principal.organization_id().to_owned(),
            workspace_id: session.workspace_id.clone(),
            principal_issuer: principal.identity().issuer.clone(),
            principal_subject: principal.identity().subject.clone(),
            session_id: session.session_id.to_string(),
            session_generation: session.session_generation,
        };
        let timeout = Duration::from_millis(u64::from(request.timeout_ms.min(30_000)));
        let Some(record) = self
            .dependencies
            .outbox
            .wait_for_result(&request.invocation_id, &scope, timeout)
            .await
            .map_err(map_outbox_error)?
        else {
            return Ok(Response::new(wire::WaitInvocationResultResponse {
                state: wire::InvocationState::Pending as i32,
                product_response: None,
                error_code: String::new(),
                error_message: String::new(),
                result_receipt: Vec::new(),
            }));
        };
        let Some(result) = record.result else {
            return Ok(Response::new(wire::WaitInvocationResultResponse {
                state: record.state as i32,
                product_response: None,
                error_code: String::new(),
                error_message: String::new(),
                result_receipt: Vec::new(),
            }));
        };
        if result.outcome_status == wire::ExecutionOutcomeStatus::Success {
            let credential = record
                .approved_invocation
                .credential
                .as_ref()
                .ok_or_else(|| Status::data_loss("persisted approval is incomplete"))?;
            let invocation = record
                .approved_invocation
                .canonical_envelope
                .as_ref()
                .and_then(|envelope| envelope.invocation.as_ref())
                .ok_or_else(|| Status::data_loss("persisted approval is incomplete"))?;
            self.validate_product_result(
                credential,
                invocation,
                result.outcome_status,
                result.product_response.as_ref(),
            )
            .await?;
        }
        Ok(Response::new(wire::WaitInvocationResultResponse {
            state: record.state as i32,
            product_response: result.product_response,
            error_code: result.error_code,
            error_message: result.error_message,
            result_receipt: result.result_receipt,
        }))
    }
}

fn bearer_from_metadata<T>(request: &Request<T>) -> AuthorityHelperResult<String> {
    let value = request
        .metadata()
        .get("authorization")
        .ok_or_else(|| Status::unauthenticated("bearer authorization is required"))?
        .to_str()
        .map_err(|_| Status::unauthenticated("bearer authorization is invalid"))?;
    let token = value
        .strip_prefix("Bearer ")
        .ok_or_else(|| Status::unauthenticated("bearer authorization is invalid"))?;
    if token.is_empty() || token.len() > 16 * 1024 || token.trim() != token {
        return Err(Status::unauthenticated("bearer authorization is invalid").into());
    }
    Ok(token.to_owned())
}

fn signature_message(key_id: &str, digest: &[u8]) -> AuthorityHelperResult<Vec<u8>> {
    if digest.len() != 32 {
        return Err(Status::permission_denied("execution credential is invalid").into());
    }
    let len = u32::try_from(key_id.len())
        .map_err(|_| Status::failed_precondition("signing key id is invalid"))?;
    let mut message = Vec::with_capacity(64 + key_id.len());
    message.extend_from_slice(b"cyrene.workspace.authority.v2/execution-credential\0");
    message.extend_from_slice(&len.to_be_bytes());
    message.extend_from_slice(key_id.as_bytes());
    message.extend_from_slice(digest);
    Ok(message)
}

fn timestamp(unix_ms: u64) -> prost_types::Timestamp {
    prost_types::Timestamp {
        seconds: (unix_ms / 1000) as i64,
        nanos: ((unix_ms % 1000) * 1_000_000) as i32,
    }
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn decode_hex(value: &str) -> AuthorityHelperResult<Vec<u8>> {
    if !value.len().is_multiple_of(2) || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(Status::unauthenticated("Connector registry identity is invalid").into());
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let nibble = |byte: u8| match byte {
                b'0'..=b'9' => Some(byte - b'0'),
                b'a'..=b'f' => Some(byte - b'a' + 10),
                b'A'..=b'F' => Some(byte - b'A' + 10),
                _ => None,
            };
            match (nibble(pair[0]), nibble(pair[1])) {
                (Some(high), Some(low)) => Ok((high << 4) | low),
                _ => Err(Status::unauthenticated("Connector registry identity is invalid").into()),
            }
        })
        .collect()
}

fn resolve_endpoint(
    base: &str,
    path_template: &str,
    parameters: &std::collections::BTreeMap<String, String>,
) -> AuthorityHelperResult<String> {
    let mut path = path_template.to_owned();
    for (name, value) in parameters {
        path = path.replace(&format!("{{{name}}}"), value);
    }
    if path.contains('{') || path.contains('}') || !path.starts_with('/') || path.starts_with("//")
    {
        return Err(
            Status::failed_precondition("catalog route could not be safely resolved").into(),
        );
    }
    let endpoint = format!("{}{}", base.trim_end_matches('/'), path);
    let url = reqwest::Url::parse(&endpoint)
        .map_err(|_| Status::failed_precondition("configured target URL is invalid"))?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(Status::failed_precondition("configured target URL is invalid").into());
    }
    Ok(endpoint)
}

fn validate_credential_time(credential: &wire::ExecutionCredential) -> AuthorityHelperResult<()> {
    let issued = credential
        .issued_at
        .as_ref()
        .ok_or_else(|| Status::permission_denied("execution credential is invalid"))?;
    let expires = credential
        .expires_at
        .as_ref()
        .ok_or_else(|| Status::permission_denied("execution credential is invalid"))?;
    let issued_ms = i128::from(issued.seconds) * 1000 + i128::from(issued.nanos) / 1_000_000;
    let expiry_ms = i128::from(expires.seconds) * 1000 + i128::from(expires.nanos) / 1_000_000;
    let now = i128::from(now_unix_ms());
    if issued_ms <= 0
        || expiry_ms <= issued_ms
        || expiry_ms - issued_ms > 300_000
        || now >= expiry_ms
        || now + 30_000 < issued_ms
    {
        return Err(
            Status::permission_denied("execution credential is expired or not yet valid").into(),
        );
    }
    Ok(())
}

fn verify_approved(approved: &wire::ApprovedInvocation) -> AuthorityHelperResult<()> {
    let envelope = approved
        .canonical_envelope
        .as_ref()
        .ok_or_else(|| Status::data_loss("approved invocation is incomplete"))?;
    let credential = approved
        .credential
        .as_ref()
        .ok_or_else(|| Status::data_loss("approved invocation is incomplete"))?;
    let invocation = envelope
        .invocation
        .as_ref()
        .ok_or_else(|| Status::data_loss("approved invocation is incomplete"))?;
    let digest = Sha256::digest(envelope.encode_to_vec());
    let invocation_digest = Sha256::digest(invocation.encode_to_vec());
    let target = envelope
        .target
        .as_ref()
        .ok_or_else(|| Status::data_loss("approved invocation target is missing"))?;
    if approved.invocation_id != envelope.invocation_id
        || approved.invocation_id != credential.invocation_id
        || digest.as_slice() != credential.request_digest_sha256.as_slice()
        || invocation_digest.as_slice() != credential.invocation_digest_sha256.as_slice()
        || envelope.organization_id != credential.organization_id
        || envelope.workspace_id != credential.workspace_id
        || envelope.principal_issuer != credential.principal_issuer
        || envelope.principal_subject != credential.principal_subject
        || envelope.execution_device_id != credential.execution_device_id
        || envelope.execution_device_generation != credential.execution_device_generation
        || envelope.execution_authorization_id != credential.execution_authorization_id
        || envelope.execution_device_certificate_sha256
            != credential.execution_device_certificate_sha256
        || envelope.session_id != credential.session_id
        || envelope.session_generation != credential.session_generation
        || envelope.contract_activation_generation != credential.contract_activation_generation
        || invocation.owner_id != credential.operation_owner_id
        || invocation.operation_id != credential.operation_id
        || target.target_component != credential.target_component
        || target.http_method != credential.http_method
        || target.endpoint != credential.endpoint
        || target.resource_id != credential.resource_id
        || target.scope != credential.scope
        || target.idempotency_key != credential.idempotency_key
        || target.idempotency_semantics != credential.idempotency_semantics
        || target.execution_device_id != credential.execution_device_id
        || target.execution_device_generation != credential.execution_device_generation
        || target.execution_authorization_id != credential.execution_authorization_id
        || target.execution_device_certificate_sha256
            != credential.execution_device_certificate_sha256
        || envelope.issued_at != credential.issued_at
        || envelope.expires_at != credential.expires_at
    {
        return Err(
            Status::data_loss("approval credential does not match canonical envelope").into(),
        );
    }
    Ok(())
}

fn map_identity_error(error: crate::web_identity::WebIdentityError) -> Status {
    match error {
        crate::web_identity::WebIdentityError::InvalidToken => {
            Status::unauthenticated("web identity is invalid")
        }
        crate::web_identity::WebIdentityError::Forbidden => {
            Status::permission_denied("web identity is not authorized")
        }
        _ => Status::unavailable("web identity verification is unavailable"),
    }
}

fn map_deployment_admission_error(error: DeploymentAdmissionError) -> Status {
    match error {
        DeploymentAdmissionError::Closed => {
            Status::failed_precondition("Control Host Product admission is closed")
        }
        DeploymentAdmissionError::Unavailable => {
            Status::unavailable("Control Host Product admission is unavailable")
        }
    }
}

fn map_policy_error(error: cy_workspace_product_contracts::AuthorizationError) -> Status {
    use cy_workspace_product_contracts::AuthorizationError;
    match error {
        AuthorizationError::InvalidRequest => {
            Status::invalid_argument("Product invocation does not satisfy its contract")
        }
        AuthorizationError::UnknownOperation
        | AuthorizationError::UnapprovedOperation
        | AuthorizationError::PrincipalNotAllowed
        | AuthorizationError::MissingRole
        | AuthorizationError::ScopeMismatch => {
            Status::permission_denied("Product operation is not authorized")
        }
        AuthorizationError::Catalog(_) | AuthorizationError::Policy(_) => {
            Status::unavailable("Product authorization snapshot is invalid")
        }
    }
}

fn map_outbox_error(error: AuthorityOutboxError) -> Status {
    match error {
        AuthorityOutboxError::IdempotencyConflict => {
            Status::already_exists("idempotency key conflicts with a different invocation")
        }
        AuthorityOutboxError::AuthorizationRevoked => {
            Status::permission_denied("invocation authorization is no longer current")
        }
        AuthorityOutboxError::InvalidState => {
            Status::failed_precondition("invocation state transition is invalid")
        }
        AuthorityOutboxError::NotFound => Status::not_found("invocation was not found"),
        AuthorityOutboxError::InvalidRecord => {
            Status::data_loss("durable invocation record is invalid")
        }
        AuthorityOutboxError::Unavailable => {
            Status::unavailable("durable invocation store is unavailable")
        }
    }
}

#[cfg(test)]
mod connector_peer_tests {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    };

    use sha2::{Digest, Sha256};
    use uuid::Uuid;

    use super::connector_peer_from_certificate_chain;
    use crate::{
        authority::ExecutionDevicePeer,
        device_registry::{
            ApprovedWorkspaceDeviceCertificate, CurrentRelayPeerRevocationEvidence,
            RelayPeerRevocationCheckError, WorkspaceDeviceCertificateIdentity,
            WorkspaceDeviceDispatchFence, WorkspaceDeviceKey,
            WorkspaceDevicePeerCertificateStatusChecker, WorkspaceDeviceRecord,
            WorkspaceDeviceRegistry,
        },
        directory::WorkspaceDirectoryError,
    };

    const CHECKED_AT_UNIX_MS: u64 = 500_000;
    const LEAF_CERTIFICATE_DER: &[u8] = b"transport-authenticated leaf certificate";
    type CapturedPeerCheckInput = (Vec<u8>, Vec<Vec<u8>>, u64);

    #[derive(Clone, Copy)]
    enum StatusResult {
        Good,
        Revoked,
        Unknown,
        WrongFingerprint,
        Stale,
    }

    struct TestStatusChecker {
        result: StatusResult,
        calls: AtomicUsize,
        input: Mutex<Option<CapturedPeerCheckInput>>,
    }

    impl TestStatusChecker {
        fn new(result: StatusResult) -> Self {
            Self {
                result,
                calls: AtomicUsize::new(0),
                input: Mutex::new(None),
            }
        }
    }

    impl WorkspaceDevicePeerCertificateStatusChecker for TestStatusChecker {
        fn require_current_good_status_for_peer_chain(
            &self,
            leaf_certificate_der: &[u8],
            intermediate_chain_der: &[Vec<u8>],
            checked_at_unix_ms: u64,
        ) -> Result<CurrentRelayPeerRevocationEvidence, RelayPeerRevocationCheckError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            *self.input.lock().unwrap() = Some((
                leaf_certificate_der.to_vec(),
                intermediate_chain_der.to_vec(),
                checked_at_unix_ms,
            ));
            match self.result {
                StatusResult::Good => Ok(
                    CurrentRelayPeerRevocationEvidence::from_verified_good_status(
                        Sha256::digest(leaf_certificate_der).into(),
                        checked_at_unix_ms - 1_000,
                        checked_at_unix_ms + 10_000,
                    ),
                ),
                StatusResult::Revoked => Err(RelayPeerRevocationCheckError::Revoked),
                StatusResult::Unknown => Err(RelayPeerRevocationCheckError::Unknown),
                StatusResult::WrongFingerprint => Ok(
                    CurrentRelayPeerRevocationEvidence::from_verified_good_status(
                        [0; 32],
                        checked_at_unix_ms - 1_000,
                        checked_at_unix_ms + 10_000,
                    ),
                ),
                StatusResult::Stale => Ok(
                    CurrentRelayPeerRevocationEvidence::from_verified_good_status(
                        Sha256::digest(leaf_certificate_der).into(),
                        checked_at_unix_ms - 300_001,
                        checked_at_unix_ms + 10_000,
                    ),
                ),
            }
        }
    }

    struct TestRegistry {
        identity: WorkspaceDeviceCertificateIdentity,
        lookups: AtomicUsize,
    }

    impl TestRegistry {
        fn new(identity: WorkspaceDeviceCertificateIdentity) -> Self {
            Self {
                identity,
                lookups: AtomicUsize::new(0),
            }
        }
    }

    impl WorkspaceDeviceRegistry for TestRegistry {
        fn import_approved_device_certificate(
            &self,
            _certificate: ApprovedWorkspaceDeviceCertificate,
        ) -> Result<WorkspaceDeviceRecord, WorkspaceDirectoryError> {
            Err(WorkspaceDirectoryError::Storage(
                "unused test method".into(),
            ))
        }

        fn revoke_device(
            &self,
            _key: &WorkspaceDeviceKey,
        ) -> Result<WorkspaceDeviceRecord, WorkspaceDirectoryError> {
            Err(WorkspaceDirectoryError::Storage(
                "unused test method".into(),
            ))
        }

        fn find_device(
            &self,
            _key: &WorkspaceDeviceKey,
        ) -> Result<Option<WorkspaceDeviceRecord>, WorkspaceDirectoryError> {
            Err(WorkspaceDirectoryError::Storage(
                "unused test method".into(),
            ))
        }

        fn find_device_by_certificate_fingerprint(
            &self,
            _fingerprint_sha256: &str,
        ) -> Result<Option<WorkspaceDeviceRecord>, WorkspaceDirectoryError> {
            Err(WorkspaceDirectoryError::Storage(
                "unused test method".into(),
            ))
        }

        fn find_current_device_certificate_identity(
            &self,
            fingerprint_sha256: &str,
        ) -> Result<Option<WorkspaceDeviceCertificateIdentity>, WorkspaceDirectoryError> {
            self.lookups.fetch_add(1, Ordering::Relaxed);
            if fingerprint_sha256 != self.identity.certificate_fingerprint_sha256 {
                return Ok(None);
            }
            Ok(Some(self.identity.clone()))
        }

        fn acquire_relay_dispatch_fence(
            &self,
            _expected: &WorkspaceDeviceCertificateIdentity,
        ) -> Result<Box<dyn WorkspaceDeviceDispatchFence>, WorkspaceDirectoryError> {
            Err(WorkspaceDirectoryError::Storage(
                "unused test method".into(),
            ))
        }
    }

    fn registry_for_leaf(leaf_certificate_der: &[u8]) -> TestRegistry {
        let fingerprint: [u8; 32] = Sha256::digest(leaf_certificate_der).into();
        TestRegistry::new(WorkspaceDeviceCertificateIdentity {
            key: WorkspaceDeviceKey {
                organization_id: "org-test".into(),
                workspace_id: "workspace-test".into(),
                device_id: "device-test".into(),
            },
            certificate_fingerprint_sha256: fingerprint
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
            authorization_id: Uuid::from_bytes([7; 16]),
            registration_binding_id: [8; 16],
            authorization_generation: 9,
            csr_sha256: [10; 32],
            spki_sha256: [11; 32],
            serial_number: vec![1, 2, 3],
            not_after_unix_ms: 900_000,
        })
    }

    #[test]
    fn connector_peer_rejects_revoked_and_unknown_status_before_registry_lookup() {
        for (checker_result, expected_code) in [
            (StatusResult::Revoked, tonic::Code::Unauthenticated),
            (StatusResult::Unknown, tonic::Code::Unavailable),
        ] {
            let checker = TestStatusChecker::new(checker_result);
            let registry = registry_for_leaf(LEAF_CERTIFICATE_DER);
            let result = connector_peer_from_certificate_chain(
                &checker,
                &registry,
                LEAF_CERTIFICATE_DER,
                &[],
                CHECKED_AT_UNIX_MS,
            );

            assert_eq!(result.unwrap_err().into_status().code(), expected_code);
            assert_eq!(checker.calls.load(Ordering::Relaxed), 1);
            assert_eq!(registry.lookups.load(Ordering::Relaxed), 0);
        }
    }

    #[test]
    fn connector_peer_requires_fresh_evidence_for_the_exact_transport_leaf() {
        for checker_result in [StatusResult::WrongFingerprint, StatusResult::Stale] {
            let checker = TestStatusChecker::new(checker_result);
            let registry = registry_for_leaf(LEAF_CERTIFICATE_DER);
            let result = connector_peer_from_certificate_chain(
                &checker,
                &registry,
                LEAF_CERTIFICATE_DER,
                &[],
                CHECKED_AT_UNIX_MS,
            );

            assert_eq!(
                result.unwrap_err().into_status().code(),
                tonic::Code::Unavailable
            );
            assert_eq!(registry.lookups.load(Ordering::Relaxed), 0);
        }
    }

    #[test]
    fn connector_peer_passes_tonic_chain_and_current_time_then_requires_registry_identity() {
        let checker = TestStatusChecker::new(StatusResult::Good);
        let registry = registry_for_leaf(LEAF_CERTIFICATE_DER);
        let intermediates = vec![vec![0x30, 0x01, 0x00]];
        let peer: ExecutionDevicePeer = connector_peer_from_certificate_chain(
            &checker,
            &registry,
            LEAF_CERTIFICATE_DER,
            &intermediates,
            CHECKED_AT_UNIX_MS,
        )
        .unwrap();

        assert_eq!(
            checker.input.lock().unwrap().as_ref().unwrap(),
            &(
                LEAF_CERTIFICATE_DER.to_vec(),
                intermediates,
                CHECKED_AT_UNIX_MS,
            )
        );
        assert_eq!(registry.lookups.load(Ordering::Relaxed), 1);
        assert_eq!(peer.organization_id, "org-test");
        assert_eq!(peer.workspace_id, "workspace-test");
        assert_eq!(peer.device_id, "device-test");
        assert_eq!(peer.authorization_id, Uuid::from_bytes([7; 16]));
        assert_eq!(peer.authorization_generation, 9);
        assert_eq!(
            peer.certificate_fingerprint_sha256,
            Sha256::digest(LEAF_CERTIFICATE_DER).as_slice()
        );
    }

    #[test]
    fn connector_peer_rejects_missing_leaf_without_consulting_authorities() {
        let checker = TestStatusChecker::new(StatusResult::Good);
        let registry = registry_for_leaf(LEAF_CERTIFICATE_DER);
        let result = connector_peer_from_certificate_chain(
            &checker,
            &registry,
            &[],
            &[],
            CHECKED_AT_UNIX_MS,
        );

        assert_eq!(
            result.unwrap_err().into_status().code(),
            tonic::Code::Unauthenticated
        );
        assert_eq!(checker.calls.load(Ordering::Relaxed), 0);
        assert_eq!(registry.lookups.load(Ordering::Relaxed), 0);
    }
}
