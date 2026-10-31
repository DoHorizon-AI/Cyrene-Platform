//! Platform-owned v2 Authority RPC implementation.
//!
//! This module independently authenticates browser bearers, resolves current Directory roles,
//! authorizes against the active pinned policy, and persists approvals through the unique outbox.

use std::{collections::BTreeSet, sync::Arc, time::Duration};

use async_trait::async_trait;
use cy_proto::{
    cyrene::workspace::authority::v2 as wire,
    cyrene::workspace::authority::v2::workspace_authority_service_server::WorkspaceAuthorityService,
    workspace_v1::UserIdentityRef,
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
        is_automation_transport_connector_operation, is_automation_viewer_connector_operation,
        is_office_task_writer_connector_operation, is_supported_connector_operation, now_unix_ms,
        AuthorityDelegation, AuthorityDelegationError, AuthorityDelegationStore,
        AuthorityOutboxError, AuthorityOutboxStore, DelegatedOperationScope, EnqueueOutcome,
        ExecutionDevicePeer, InvocationWaitScope, NewAuthorityInvocation,
        PersistedInvocationResult, CONNECTOR_OPERATION_GRANT_SCHEMA_VERSION,
        CONNECTOR_OPERATION_GRANT_TTL_MS, NAVIGATOR_AUTOMATION_VIEWER_ROLE,
        NAVIGATOR_CONNECTOR_OPERATOR_ROLE, NAVIGATOR_OFFICE_TASK_WRITER_ROLE,
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

    fn sign_workload_feature_token(
        &self,
        token: &mut wire::ManagedWorkloadFeatureToken,
    ) -> AuthorityHelperResult<()> {
        token.signing_key_id = self.key_id.to_string();
        let mut unsigned = token.clone();
        unsigned.authority_signature.clear();
        let digest = Sha256::digest(unsigned.encode_to_vec());
        let mut message = workload_feature_signature_message(&self.key_id, &digest)?;
        token.authority_signature = self.key.sign(&message).to_bytes().to_vec();
        message.zeroize();
        Ok(())
    }

    fn sign_connector_operation_grant(
        &self,
        grant: &mut wire::ConnectorOperationGrantV1,
    ) -> AuthorityHelperResult<()> {
        grant.signing_key_id = self.key_id.to_string();
        grant.grant_ref.clear();
        grant.authority_signature.clear();
        let digest = crate::authority::connector_grant::grant_digest(grant);
        grant.grant_ref = format!(
            "sha256:{}",
            crate::authority::connector_grant::encode_hex(&digest)
        );
        let mut message =
            crate::authority::connector_grant::signature_message(&self.key_id, &digest).map_err(
                |_| Status::failed_precondition("connector grant signing is unavailable"),
            )?;
        grant.authority_signature = self.key.sign(&message).to_bytes().to_vec();
        message.zeroize();
        Ok(())
    }

    fn verify_workload_feature_token(
        &self,
        token: &wire::ManagedWorkloadFeatureToken,
    ) -> AuthorityHelperResult<()> {
        if token.signing_key_id != self.key_id.as_ref()
            || token.authority_signature.len() != 64
            || token.client_certificate_sha256.len() != 32
        {
            return Err(Status::permission_denied("workload feature token is invalid").into());
        }
        let mut unsigned = token.clone();
        unsigned.authority_signature.clear();
        let digest = Sha256::digest(unsigned.encode_to_vec());
        let message = workload_feature_signature_message(&self.key_id, &digest)?;
        let signature = Signature::from_slice(&token.authority_signature)
            .map_err(|_| Status::permission_denied("workload feature token is invalid"))?;
        self.key
            .verifying_key()
            .verify(&message, &signature)
            .map_err(|_| Status::permission_denied("workload feature token is invalid"))?;
        Ok(())
    }
}

/// All durable and trusted dependencies for the Authority process.
pub struct AuthorityServiceDependencies {
    /// Root-configured Organization identity used by the first-adoption admission receipt.
    pub organization_id: String,
    /// Root-configured Workspace identity used by the first-adoption admission receipt.
    pub workspace_id: String,
    /// Exact Microsoft Entra v2 issuer whose immutable AAD object IDs are Directory subjects.
    pub connector_identity_issuer: String,
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
    /// Durable, revocable user-to-workload grants.
    pub delegations: Arc<dyn AuthorityDelegationStore>,
    /// Authenticated managed workload identity and its fixed grant audience.
    pub exchange_workload: AuthorityManagedWorkload,
    /// Certificate-derived Product workload identities with narrow owner and target scopes.
    pub product_workload_profiles: Vec<AuthorityProductWorkloadProfile>,
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

/// Server-configured workload identity. Certificate bytes are matched before this is returned.
#[derive(Clone, Debug)]
pub struct AuthorityManagedWorkload {
    /// Stable workload ID derived from trusted host configuration.
    pub workload_id: String,
    /// Audience required in every grant used by this workload.
    pub audience: String,
    /// SHA-256 fingerprints of currently managed mTLS leaf certificates.
    pub certificate_fingerprints_sha256: Vec<[u8; 32]>,
}

/// Product-specific workload identity derived only from an authenticated mTLS certificate.
#[derive(Clone, Debug)]
pub struct AuthorityProductWorkloadProfile {
    /// Stable operator-configured workload name.
    pub workload_id: String,
    /// Product owner this workload may validate.
    pub owner_id: String,
    /// Active Connector target component this workload represents.
    pub target_component: String,
    /// Expected audience for grants used by this workload.
    pub audience: String,
    /// Optional exact organization restriction.
    pub organization_id: Option<String>,
    /// Optional exact workspace restriction.
    pub workspace_id: Option<String>,
    /// Allowlisted client certificate SHA-256 fingerprints.
    pub certificate_fingerprints_sha256: Vec<[u8; 32]>,
}

#[derive(Clone, Debug)]
struct InvocationAuthorizationContext {
    source: wire::InvocationAuthorizationSource,
    session_id: String,
    session_generation: u64,
    delegation_id: String,
    delegation_generation: u64,
    delegation_audience: String,
    expires_at_unix_ms: u64,
}

#[derive(Clone, Debug)]
struct InvocationPrincipalContext {
    organization_id: String,
    workspace_id: String,
    issuer: String,
    subject: String,
}

#[derive(Clone, Copy)]
struct DelegationFence<'a> {
    organization_id: &'a str,
    workspace_id: &'a str,
    principal_issuer: &'a str,
    principal_subject: &'a str,
    audience: &'a str,
    managed_workload_id: &'a str,
    generation: Option<u64>,
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

    fn exchange_workload_peer<T>(
        &self,
        request: &Request<T>,
    ) -> AuthorityHelperResult<&AuthorityManagedWorkload> {
        let certificates = request
            .peer_certs()
            .ok_or_else(|| Status::unauthenticated("managed workload certificate is required"))?;
        let leaf = certificates
            .first()
            .ok_or_else(|| Status::unauthenticated("managed workload certificate is required"))?;
        let fingerprint: [u8; 32] = Sha256::digest(leaf.as_ref()).into();
        if !self
            .dependencies
            .exchange_workload
            .certificate_fingerprints_sha256
            .contains(&fingerprint)
        {
            return Err(Status::unauthenticated("managed workload is not configured").into());
        }
        Ok(&self.dependencies.exchange_workload)
    }

    async fn authorize_connector_operation_scope(
        &self,
        input: &wire::AuthorizeConnectorOperationRequest,
        workload: &AuthorityManagedWorkload,
    ) -> Result<wire::ConnectorOperationGrantV1, Status> {
        issue_connector_operation_grant(
            self.dependencies.directory.as_ref(),
            &self.dependencies.signer,
            &self.dependencies.organization_id,
            &self.dependencies.workspace_id,
            &self.dependencies.connector_identity_issuer,
            workload,
            input,
        )
        .await
    }

    fn product_workload_peer<T>(
        &self,
        request: &Request<T>,
    ) -> AuthorityHelperResult<&AuthorityProductWorkloadProfile> {
        let certificate = request
            .peer_certs()
            .and_then(|certificates| {
                certificates
                    .first()
                    .map(|certificate| Sha256::digest(certificate.as_ref()).into())
            })
            .ok_or_else(|| Status::unauthenticated("Product workload certificate is required"))?;
        let mut matches = self
            .dependencies
            .product_workload_profiles
            .iter()
            .filter(|profile| {
                profile
                    .certificate_fingerprints_sha256
                    .contains(&certificate)
            });
        let profile = matches
            .next()
            .ok_or_else(|| Status::unauthenticated("Product workload is not configured"))?;
        if matches.next().is_some()
            || profile.workload_id.trim().is_empty()
            || profile.owner_id.trim().is_empty()
            || profile.target_component.trim().is_empty()
            || profile.audience.trim().is_empty()
        {
            return Err(Status::failed_precondition("Product workload profile is invalid").into());
        }
        Ok(profile)
    }

    async fn revalidate_delegated_record(
        &self,
        record: &crate::authority::AuthorityInvocationRecord,
    ) -> Result<(), Status> {
        self.revalidate_delegated_record_for_workload(
            record,
            &self.dependencies.exchange_workload.workload_id,
        )
        .await
    }

    async fn revalidate_delegated_record_for_workload(
        &self,
        record: &crate::authority::AuthorityInvocationRecord,
        expected_workload_id: &str,
    ) -> Result<(), Status> {
        let envelope = record
            .approved_invocation
            .canonical_envelope
            .as_ref()
            .ok_or_else(|| Status::data_loss("approved invocation is incomplete"))?;
        if wire::InvocationAuthorizationSource::try_from(envelope.authorization_source)
            .map_err(|_| Status::data_loss("authorization source is invalid"))?
            != wire::InvocationAuthorizationSource::Delegation
        {
            return Ok(());
        }
        let delegation_id = Uuid::parse_str(&envelope.delegation_id)
            .map_err(|_| Status::data_loss("delegation reference is invalid"))?;
        let delegation = self
            .dependencies
            .delegations
            .get(
                &envelope.organization_id,
                &envelope.workspace_id,
                delegation_id,
            )
            .await
            .map_err(map_delegation_error)?;
        validate_delegation_fence(
            &delegation,
            DelegationFence {
                organization_id: &envelope.organization_id,
                workspace_id: &envelope.workspace_id,
                principal_issuer: &envelope.principal_issuer,
                principal_subject: &envelope.principal_subject,
                audience: &envelope.delegation_audience,
                managed_workload_id: expected_workload_id,
                generation: Some(envelope.delegation_generation),
            },
            now_unix_ms(),
        )?;
        if delegation.audience != self.dependencies.exchange_workload.audience
            && expected_workload_id == self.dependencies.exchange_workload.workload_id
        {
            return Err(Status::permission_denied("delegation is no longer active"));
        }
        let roles = self
            .dependencies
            .directory
            .roles_for_member(
                &cy_proto::workspace_v1::UserIdentityRef {
                    issuer: delegation.principal_issuer.clone(),
                    subject: delegation.principal_subject.clone(),
                },
                &delegation.organization_id,
                &delegation.workspace_id,
            )
            .await
            .map_err(|_| Status::unavailable("workspace directory is unavailable"))?
            .ok_or_else(|| Status::permission_denied("delegation owner is no longer a member"))?;
        let invocation = envelope
            .invocation
            .as_ref()
            .ok_or_else(|| Status::data_loss("approved invocation is incomplete"))?;
        let snapshot = self
            .dependencies
            .snapshots
            .active_snapshot()
            .map_err(|_| Status::unavailable("active Authority snapshot is unavailable"))?;
        if snapshot.generation != envelope.contract_activation_generation {
            return Err(Status::permission_denied(
                "delegation contract is no longer active",
            ));
        }
        let authorized = snapshot
            .policy
            .authorize(
                &snapshot.bundle,
                invocation.clone(),
                &VerifiedProductPrincipal::directory_user(roles),
                TrustedWorkspaceScope::new(&delegation.organization_id, &delegation.workspace_id)
                    .map_err(|_| Status::permission_denied("delegation scope is invalid"))?,
            )
            .map_err(map_policy_error)?;
        let resource_id = authorized.resource_id().unwrap_or_default();
        if !delegation.operations.iter().any(|scope| {
            scope.owner_id == authorized.owner_id()
                && scope.operation_id == authorized.operation_id()
                && (scope.resource_ids.is_empty() && resource_id.is_empty()
                    || scope.resource_ids.iter().any(|id| id == resource_id))
        }) {
            return Err(Status::permission_denied(
                "delegation no longer authorizes this operation",
            ));
        }
        Ok(())
    }

    async fn revalidate_product_record(
        &self,
        record: &crate::authority::AuthorityInvocationRecord,
        workload_id: &str,
    ) -> Result<(), Status> {
        let envelope = record
            .approved_invocation
            .canonical_envelope
            .as_ref()
            .ok_or_else(|| Status::data_loss("approved invocation is incomplete"))?;
        let invocation = envelope
            .invocation
            .as_ref()
            .ok_or_else(|| Status::data_loss("approved invocation is incomplete"))?;
        let snapshot = self
            .dependencies
            .snapshots
            .active_snapshot()
            .map_err(|_| Status::unavailable("active Authority snapshot is unavailable"))?;
        if snapshot.generation != envelope.contract_activation_generation {
            return Err(Status::permission_denied(
                "Product contract is no longer active",
            ));
        }
        let roles = self
            .dependencies
            .directory
            .roles_for_member(
                &cy_proto::workspace_v1::UserIdentityRef {
                    issuer: envelope.principal_issuer.clone(),
                    subject: envelope.principal_subject.clone(),
                },
                &envelope.organization_id,
                &envelope.workspace_id,
            )
            .await
            .map_err(|_| Status::unavailable("workspace directory is unavailable"))?
            .ok_or_else(|| Status::permission_denied("Product principal is no longer a member"))?;
        snapshot
            .policy
            .authorize(
                &snapshot.bundle,
                invocation.clone(),
                &VerifiedProductPrincipal::directory_user(roles),
                TrustedWorkspaceScope::new(&envelope.organization_id, &envelope.workspace_id)
                    .map_err(|_| Status::permission_denied("approved scope is invalid"))?,
            )
            .map_err(map_policy_error)?;
        self.revalidate_delegated_record_for_workload(record, workload_id)
            .await
    }

    async fn validate_product_execution_credential<'a, T>(
        &'a self,
        request: &Request<T>,
        credential: &wire::ExecutionCredential,
    ) -> Result<
        (
            &'a AuthorityProductWorkloadProfile,
            crate::authority::AuthorityInvocationRecord,
        ),
        Status,
    > {
        let profile = self.product_workload_peer(request)?;
        self.dependencies.signer.verify(credential)?;
        validate_credential_time(credential)?;
        validate_product_workload_scope(credential, profile)?;
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
            .validate_product_execution_authorization(credential)
            .await
            .map_err(map_outbox_error)?;
        if record.approved_invocation.credential.as_ref() != Some(credential) {
            return Err(Status::permission_denied(
                "execution credential is not the active approved credential",
            ));
        }
        verify_approved(&record.approved_invocation)?;
        self.revalidate_product_record(&record, &profile.workload_id)
            .await?;
        Ok((profile, record))
    }

    async fn revalidate_delegation_read_scopes(
        &self,
        delegation: &AuthorityDelegation,
        roles: &BTreeSet<String>,
    ) -> Result<(), Status> {
        let snapshot = self
            .dependencies
            .snapshots
            .active_snapshot()
            .map_err(|_| Status::unavailable("active Authority snapshot is unavailable"))?;
        let principal = VerifiedProductPrincipal::directory_user(roles.iter().cloned());
        for scope in &delegation.operations {
            let operation = snapshot
                .policy
                .authorize_delegation(
                    &snapshot.bundle,
                    &scope.owner_id,
                    &scope.operation_id,
                    &principal,
                )
                .map_err(map_policy_error)?;
            for resource_id in &scope.resource_ids {
                operation
                    .validate_resource_id(Some(resource_id))
                    .map_err(|_| {
                        Status::permission_denied("delegation scope is no longer valid")
                    })?;
            }
            if operation.resource_id().required && scope.resource_ids.is_empty() {
                return Err(Status::permission_denied(
                    "delegation scope is no longer valid",
                ));
            }
            if operation.resource_id().path_parameter.is_none() && !scope.resource_ids.is_empty() {
                return Err(Status::permission_denied(
                    "delegation scope is no longer valid",
                ));
            }
        }
        Ok(())
    }

    async fn validate_product_result(
        &self,
        credential: &wire::ExecutionCredential,
        approved_invocation: &cy_proto::cyrene::workspace::product::v2::ProductApiInvocationV2,
        outcome: wire::ExecutionOutcomeStatus,
        response: Option<&cy_proto::cyrene::workspace::product::v2::ProductApiResponseV2>,
    ) -> Result<(), Status> {
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
        if outcome != wire::ExecutionOutcomeStatus::Success {
            if response.is_some() {
                return Err(Status::invalid_argument(
                    "failed execution must not include a successful Product response",
                ));
            }
            return Ok(());
        }
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

impl AuthorityRpcService {
    async fn enqueue_authorized_product(
        &self,
        invocation: cy_proto::cyrene::workspace::product::v2::ProductApiInvocationV2,
        authorized: cy_workspace_product_contracts::AuthorizedProductInvocation,
        principal: InvocationPrincipalContext,
        authorization: InvocationAuthorizationContext,
    ) -> Result<(String, i32), Status> {
        let snapshot = self
            .dependencies
            .snapshots
            .active_snapshot()
            .map_err(|_| Status::unavailable("active Authority snapshot is unavailable"))?;
        let _admission = self
            .deployment_admission
            .acquire_dispatch(
                &principal.organization_id,
                &principal.workspace_id,
                &self.deployment_identity.authority_instance_id,
            )
            .await
            .map_err(map_deployment_admission_error)?;
        let target = self
            .dependencies
            .targets
            .resolve(
                &principal.organization_id,
                &principal.workspace_id,
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
            principal.organization_id, principal.workspace_id, resource_id
        );
        let invocation_digest = Sha256::digest(invocation.encode_to_vec()).to_vec();
        let invocation_id = Uuid::new_v4().to_string();
        let issued_ms = now_unix_ms();
        let expires_ms = authorization
            .expires_at_unix_ms
            .min(issued_ms.saturating_add(self.dependencies.signer.ttl.as_millis() as u64));
        if expires_ms <= issued_ms {
            return Err(Status::unauthenticated(
                "authorization cannot authorize a new execution",
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
            organization_id: principal.organization_id.clone(),
            workspace_id: principal.workspace_id.clone(),
            principal_issuer: principal.issuer.clone(),
            principal_subject: principal.subject.clone(),
            execution_device_id: target.execution_device_id.clone(),
            execution_device_generation: target.execution_device_generation,
            execution_authorization_id: target.execution_authorization_id.as_bytes().to_vec(),
            execution_device_certificate_sha256: target.certificate_fingerprint_sha256.to_vec(),
            session_id: authorization.session_id.clone(),
            session_generation: authorization.session_generation,
            contract_activation_generation: snapshot.generation,
            issued_at: Some(issued_at),
            expires_at: Some(expires_at),
            authorization_source: authorization.source as i32,
            delegation_id: authorization.delegation_id.clone(),
            delegation_generation: authorization.delegation_generation,
            delegation_audience: authorization.delegation_audience.clone(),
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
            session_id: authorization.session_id,
            session_generation: authorization.session_generation,
            contract_activation_generation: snapshot.generation,
            idempotency_key,
            idempotency_semantics,
            signing_key_id: String::new(),
            issued_at: Some(issued_at),
            expires_at: Some(expires_at),
            authority_signature: Vec::new(),
            authorization_source: authorization.source as i32,
            delegation_id: authorization.delegation_id,
            delegation_generation: authorization.delegation_generation,
            delegation_audience: authorization.delegation_audience,
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
        Ok((
            record.approved_invocation.invocation_id,
            record.state as i32,
        ))
    }
}

#[tonic::async_trait]
impl WorkspaceAuthorityService for AuthorityRpcService {
    async fn negotiate_version(
        &self,
        request: Request<wire::NegotiateVersionRequest>,
    ) -> Result<Response<wire::NegotiateVersionResponse>, Status> {
        let input = request.get_ref();
        if input.minimum_version > input.maximum_version {
            return Err(Status::invalid_argument("version range is invalid"));
        }
        if input.minimum_version > 2 || input.maximum_version < 2 {
            return Err(Status::failed_precondition(
                "no mutually supported Authority version",
            ));
        }
        let mut selected_features = Vec::new();
        let mut managed_workload_feature_token = Vec::new();
        if input
            .requested_features
            .iter()
            .any(|feature| feature == "delegated_invocations_v1")
        {
            let workload = self.exchange_workload_peer(&request)?;
            let fingerprint = request
                .peer_certs()
                .and_then(|certificates| {
                    certificates
                        .first()
                        .map(|certificate| Sha256::digest(certificate.as_ref()).to_vec())
                })
                .ok_or_else(|| {
                    Status::unauthenticated("managed workload certificate is required")
                })?;
            let now = now_unix_ms();
            let mut token = wire::ManagedWorkloadFeatureToken {
                audience: workload.audience.clone(),
                client_certificate_sha256: fingerprint,
                signing_key_id: String::new(),
                issued_at: Some(timestamp(now)),
                expires_at: Some(timestamp(now.saturating_add(300_000))),
                authority_signature: Vec::new(),
            };
            self.dependencies
                .signer
                .sign_workload_feature_token(&mut token)?;
            managed_workload_feature_token = token.encode_to_vec();
            selected_features.push("delegated_invocations_v1".to_owned());
        }
        Ok(Response::new(wire::NegotiateVersionResponse {
            selected_version: 2,
            supported_versions: vec![2],
            selected_features,
            supported_features: vec!["delegated_invocations_v1".to_owned()],
            managed_workload_feature_token,
        }))
    }

    async fn create_delegation(
        &self,
        request: Request<wire::CreateDelegationRequest>,
    ) -> Result<Response<wire::CreateDelegationResponse>, Status> {
        let workspace_id = request.get_ref().workspace_id.clone();
        let (principal, roles, _) = self.authenticate(&request, &workspace_id).await?;
        let request = request.into_inner();
        if self
            .dependencies
            .exchange_workload
            .workload_id
            .trim()
            .is_empty()
            || self
                .dependencies
                .exchange_workload
                .certificate_fingerprints_sha256
                .is_empty()
        {
            return Err(Status::failed_precondition(
                "managed workload delegation is not configured",
            ));
        }
        if request.audience != self.dependencies.exchange_workload.audience {
            return Err(Status::invalid_argument(
                "delegation audience is not supported",
            ));
        }
        if request.operations.is_empty() || request.operations.len() > 64 {
            return Err(Status::invalid_argument(
                "delegation operations are invalid",
            ));
        }
        let now = now_unix_ms();
        let expires_at_unix_ms = match request.expires_at {
            Some(value) => timestamp_to_unix_ms(Some(&value))?,
            None => now.saturating_add(7 * 24 * 60 * 60 * 1_000),
        };
        if expires_at_unix_ms <= now
            || expires_at_unix_ms > now.saturating_add(7 * 24 * 60 * 60 * 1_000)
        {
            return Err(Status::invalid_argument(
                "delegation expiry must be within seven days",
            ));
        }
        let snapshot = self
            .dependencies
            .snapshots
            .active_snapshot()
            .map_err(|_| Status::unavailable("active Authority snapshot is unavailable"))?;
        let directory_principal = VerifiedProductPrincipal::directory_user(roles);
        let mut operations = Vec::with_capacity(request.operations.len());
        let mut seen = BTreeSet::new();
        for requested in request.operations {
            let key = (requested.owner_id.clone(), requested.operation_id.clone());
            if requested.owner_id.is_empty()
                || requested.operation_id.is_empty()
                || !seen.insert(key)
                || requested.resource_ids.len() > 256
            {
                return Err(Status::invalid_argument(
                    "delegation operation scope is invalid",
                ));
            }
            let operation = snapshot
                .policy
                .authorize_delegation(
                    &snapshot.bundle,
                    &requested.owner_id,
                    &requested.operation_id,
                    &directory_principal,
                )
                .map_err(map_policy_error)?;
            let mut resource_ids = requested.resource_ids;
            resource_ids.sort();
            if resource_ids.windows(2).any(|pair| pair[0] == pair[1])
                || (operation.resource_id().required && resource_ids.is_empty())
            {
                return Err(Status::invalid_argument(
                    "delegation resource scope is invalid",
                ));
            }
            for resource_id in &resource_ids {
                operation
                    .validate_resource_id(Some(resource_id))
                    .map_err(|_| {
                        Status::invalid_argument("delegation resource scope is invalid")
                    })?;
            }
            if operation.resource_id().path_parameter.is_none() && !resource_ids.is_empty() {
                return Err(Status::invalid_argument(
                    "delegation resource scope is invalid",
                ));
            }
            operations.push(DelegatedOperationScope {
                owner_id: requested.owner_id,
                operation_id: requested.operation_id,
                resource_ids,
            });
        }
        let delegation = AuthorityDelegation {
            delegation_id: Uuid::new_v4(),
            generation: 1,
            organization_id: principal.organization_id().to_owned(),
            workspace_id,
            principal_issuer: principal.identity().issuer.clone(),
            principal_subject: principal.identity().subject.clone(),
            audience: request.audience,
            managed_workload_id: self.dependencies.exchange_workload.workload_id.clone(),
            operations,
            expires_at_unix_ms,
            revoked_at_unix_ms: None,
        };
        let stored = self
            .dependencies
            .delegations
            .create(delegation)
            .await
            .map_err(map_delegation_error)?;
        Ok(Response::new(wire::CreateDelegationResponse {
            delegation_id: stored.delegation_id.to_string(),
            generation: stored.generation,
            expires_at: Some(timestamp(stored.expires_at_unix_ms)),
        }))
    }

    async fn revoke_delegation(
        &self,
        request: Request<wire::RevokeDelegationRequest>,
    ) -> Result<Response<wire::RevokeDelegationResponse>, Status> {
        let workspace_id = request.get_ref().workspace_id.clone();
        let (principal, _, _) = self.authenticate(&request, &workspace_id).await?;
        let request = request.into_inner();
        let delegation_id = Uuid::parse_str(&request.delegation_id)
            .map_err(|_| Status::invalid_argument("delegation id is invalid"))?;
        let current = self
            .dependencies
            .delegations
            .get(principal.organization_id(), &workspace_id, delegation_id)
            .await
            .map_err(map_delegation_error)?;
        if current.principal_issuer != principal.identity().issuer
            || current.principal_subject != principal.identity().subject
        {
            return Err(Status::permission_denied("delegation owner is required"));
        }
        let revoked = self
            .dependencies
            .delegations
            .revoke(
                principal.organization_id(),
                &workspace_id,
                delegation_id,
                now_unix_ms(),
            )
            .await
            .map_err(map_delegation_error)?;
        Ok(Response::new(wire::RevokeDelegationResponse {
            revoked: revoked.revoked_at_unix_ms.is_some(),
            generation: revoked.generation,
        }))
    }

    async fn get_delegation(
        &self,
        request: Request<wire::GetDelegationRequest>,
    ) -> Result<Response<wire::GetDelegationResponse>, Status> {
        let workspace_id = request.get_ref().workspace_id.clone();
        let delegation = if let Some(credential) = request.get_ref().execution_credential.as_ref() {
            if request.metadata().contains_key("authorization") {
                return Err(Status::invalid_argument(
                    "bearer and Product execution proof cannot be combined",
                ));
            }
            if workspace_id.trim().is_empty() || workspace_id.len() > 256 {
                return Err(Status::invalid_argument("workspace id is invalid"));
            }
            let requested_delegation_id = Uuid::parse_str(&request.get_ref().delegation_id)
                .map_err(|_| Status::invalid_argument("delegation id is invalid"))?;
            let (profile, record) = self
                .validate_product_execution_credential(&request, credential)
                .await?;
            let approved = &record.approved_invocation;
            let envelope = approved
                .canonical_envelope
                .as_ref()
                .ok_or_else(|| Status::data_loss("approved invocation is incomplete"))?;
            let approved_credential = approved
                .credential
                .as_ref()
                .ok_or_else(|| Status::data_loss("approved invocation is incomplete"))?;
            let authorization_source =
                wire::InvocationAuthorizationSource::try_from(envelope.authorization_source)
                    .map_err(|_| Status::data_loss("approved authorization source is invalid"))?;
            if workspace_id != envelope.workspace_id {
                return Err(Status::not_found("delegation was not found"));
            }
            if authorization_source == wire::InvocationAuthorizationSource::Delegation {
                let approved_delegation_id = Uuid::parse_str(&envelope.delegation_id)
                    .map_err(|_| Status::data_loss("approved delegation reference is invalid"))?;
                if requested_delegation_id != approved_delegation_id {
                    return Err(Status::not_found("delegation was not found"));
                }
            }
            let delegation = self
                .dependencies
                .delegations
                .get(
                    &envelope.organization_id,
                    &envelope.workspace_id,
                    requested_delegation_id,
                )
                .await
                .map_err(map_delegation_error)?;
            if delegation.organization_id != envelope.organization_id
                || delegation.workspace_id != envelope.workspace_id
                || delegation.principal_issuer != envelope.principal_issuer
                || delegation.principal_subject != envelope.principal_subject
            {
                return Err(Status::not_found("delegation was not found"));
            }
            if authorization_source == wire::InvocationAuthorizationSource::Delegation {
                validate_delegation_fence(
                    &delegation,
                    DelegationFence {
                        organization_id: &envelope.organization_id,
                        workspace_id: &envelope.workspace_id,
                        principal_issuer: &envelope.principal_issuer,
                        principal_subject: &envelope.principal_subject,
                        audience: &envelope.delegation_audience,
                        managed_workload_id: &profile.workload_id,
                        generation: Some(approved_credential.delegation_generation),
                    },
                    now_unix_ms(),
                )?;
            }
            let roles = self
                .dependencies
                .directory
                .roles_for_member(
                    &cy_proto::workspace_v1::UserIdentityRef {
                        issuer: envelope.principal_issuer.clone(),
                        subject: envelope.principal_subject.clone(),
                    },
                    &envelope.organization_id,
                    &envelope.workspace_id,
                )
                .await
                .map_err(|_| Status::unavailable("workspace directory is unavailable"))?
                .ok_or_else(|| {
                    Status::permission_denied("delegation owner is no longer a member")
                })?;
            self.revalidate_delegation_read_scopes(&delegation, &roles)
                .await?;
            delegation
        } else {
            let (principal, roles, _) = self.authenticate(&request, &workspace_id).await?;
            let delegation_id = Uuid::parse_str(&request.get_ref().delegation_id)
                .map_err(|_| Status::invalid_argument("delegation id is invalid"))?;
            let delegation = self
                .dependencies
                .delegations
                .get(principal.organization_id(), &workspace_id, delegation_id)
                .await
                .map_err(map_delegation_error)?;
            if delegation.organization_id != principal.organization_id()
                || delegation.workspace_id != workspace_id
                || delegation.principal_issuer != principal.identity().issuer
                || delegation.principal_subject != principal.identity().subject
            {
                return Err(Status::not_found("delegation was not found"));
            }
            self.revalidate_delegation_read_scopes(&delegation, &roles)
                .await?;
            delegation
        };

        Ok(Response::new(project_delegation(delegation)))
    }

    async fn approve_and_enqueue_delegated_invocation(
        &self,
        request: Request<wire::ApproveAndEnqueueDelegatedInvocationRequest>,
    ) -> Result<Response<wire::ApproveAndEnqueueDelegatedInvocationResponse>, Status> {
        let workload = self.exchange_workload_peer(&request)?;
        let fingerprint = request
            .peer_certs()
            .and_then(|certificates| {
                certificates
                    .first()
                    .map(|certificate| Sha256::digest(certificate.as_ref()).to_vec())
            })
            .ok_or_else(|| Status::unauthenticated("managed workload certificate is required"))?;
        let request = request.into_inner();
        let token = wire::ManagedWorkloadFeatureToken::decode(
            request.managed_workload_feature_token.as_slice(),
        )
        .map_err(|_| Status::permission_denied("workload feature negotiation is required"))?;
        self.dependencies
            .signer
            .verify_workload_feature_token(&token)?;
        let issued_at = timestamp_to_unix_ms(token.issued_at.as_ref())?;
        let token_expires_at = timestamp_to_unix_ms(token.expires_at.as_ref())?;
        let now = now_unix_ms();
        if token.audience != workload.audience
            || token.client_certificate_sha256 != fingerprint
            || issued_at > now.saturating_add(30_000)
            || token_expires_at <= now
            || token_expires_at <= issued_at
            || token_expires_at.saturating_sub(issued_at) > 300_000
        {
            return Err(Status::permission_denied(
                "workload feature negotiation is expired or invalid",
            ));
        }
        let delegation_id = Uuid::parse_str(&request.delegation_id)
            .map_err(|_| Status::invalid_argument("delegation id is invalid"))?;
        let delegation = self
            .dependencies
            .delegations
            .get(
                &self.dependencies.organization_id,
                &request.workspace_id,
                delegation_id,
            )
            .await
            .map_err(map_delegation_error)?;
        validate_delegation_fence(
            &delegation,
            DelegationFence {
                organization_id: &self.dependencies.organization_id,
                workspace_id: &request.workspace_id,
                principal_issuer: &delegation.principal_issuer,
                principal_subject: &delegation.principal_subject,
                audience: &workload.audience,
                managed_workload_id: &workload.workload_id,
                generation: None,
            },
            now,
        )?;
        let roles = self
            .dependencies
            .directory
            .roles_for_member(
                &cy_proto::workspace_v1::UserIdentityRef {
                    issuer: delegation.principal_issuer.clone(),
                    subject: delegation.principal_subject.clone(),
                },
                &delegation.organization_id,
                &delegation.workspace_id,
            )
            .await
            .map_err(|_| Status::unavailable("workspace directory is unavailable"))?
            .ok_or_else(|| Status::permission_denied("delegation owner is no longer a member"))?;
        let snapshot = self
            .dependencies
            .snapshots
            .active_snapshot()
            .map_err(|_| Status::unavailable("active Authority snapshot is unavailable"))?;
        let invocation = request
            .invocation
            .ok_or_else(|| Status::invalid_argument("invocation is required"))?;
        let authorized = snapshot
            .policy
            .authorize(
                &snapshot.bundle,
                invocation.clone(),
                &VerifiedProductPrincipal::directory_user(roles),
                TrustedWorkspaceScope::new(&delegation.organization_id, &delegation.workspace_id)
                    .map_err(|_| Status::permission_denied("delegation scope is invalid"))?,
            )
            .map_err(map_policy_error)?;
        let resource_id = authorized.resource_id().unwrap_or_default();
        if !delegation.operations.iter().any(|scope| {
            scope.owner_id == authorized.owner_id()
                && scope.operation_id == authorized.operation_id()
                && (scope.resource_ids.is_empty() && resource_id.is_empty()
                    || scope.resource_ids.iter().any(|id| id == resource_id))
        }) {
            return Err(Status::permission_denied(
                "delegation does not authorize this operation",
            ));
        }
        let (invocation_id, state) = self
            .enqueue_authorized_product(
                invocation,
                authorized,
                InvocationPrincipalContext {
                    organization_id: delegation.organization_id.clone(),
                    workspace_id: delegation.workspace_id.clone(),
                    issuer: delegation.principal_issuer.clone(),
                    subject: delegation.principal_subject.clone(),
                },
                InvocationAuthorizationContext {
                    source: wire::InvocationAuthorizationSource::Delegation,
                    session_id: String::new(),
                    session_generation: 0,
                    delegation_id: delegation.delegation_id.to_string(),
                    delegation_generation: delegation.generation,
                    delegation_audience: delegation.audience,
                    expires_at_unix_ms: delegation.expires_at_unix_ms,
                },
            )
            .await?;
        Ok(Response::new(
            wire::ApproveAndEnqueueDelegatedInvocationResponse {
                invocation_id,
                state,
            },
        ))
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

    async fn authorize_connector_operation(
        &self,
        request: Request<wire::AuthorizeConnectorOperationRequest>,
    ) -> Result<Response<wire::AuthorizeConnectorOperationResponse>, Status> {
        let workload = self.exchange_workload_peer(&request)?;
        if workload.workload_id.trim().is_empty() || workload.audience.trim().is_empty() {
            return Err(Status::failed_precondition(
                "connector authorization caller is not configured",
            ));
        }
        let grant = self
            .authorize_connector_operation_scope(request.get_ref(), workload)
            .await?;
        Ok(Response::new(wire::AuthorizeConnectorOperationResponse {
            grant: Some(grant),
        }))
    }

    async fn get_catalog_snapshot(
        &self,
        request: Request<wire::CatalogSnapshotRequest>,
    ) -> Result<Response<wire::CatalogSnapshotResponse>, Status> {
        let workspace_id = request.get_ref().workspace_id.clone();
        let (principal, roles, _) = self.authenticate(&request, &workspace_id).await?;
        let snapshot = self
            .dependencies
            .snapshots
            .active_snapshot()
            .map_err(|_| Status::unavailable("active Authority snapshot is unavailable"))?;
        let directory_principal = VerifiedProductPrincipal::directory_user(roles);
        let mut owners = Vec::new();
        for owner_id in snapshot.identity.source_commits.keys() {
            let granted_operation_ids = snapshot
                .bundle
                .operations()
                .filter(|operation| {
                    operation.owner_id() == owner_id
                        && snapshot
                            .policy
                            .authorize_delegation(
                                &snapshot.bundle,
                                owner_id,
                                operation.operation_id(),
                                &directory_principal,
                            )
                            .is_ok()
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
        self.enqueue_authorized_product(
            invocation,
            authorized,
            InvocationPrincipalContext {
                organization_id: principal.organization_id().to_owned(),
                workspace_id: workspace_id.clone(),
                issuer: principal.identity().issuer.clone(),
                subject: principal.identity().subject.clone(),
            },
            InvocationAuthorizationContext {
                // Zero-valued source keeps legacy interactive envelope bytes unchanged.
                source: wire::InvocationAuthorizationSource::Unspecified,
                session_id: session.session_id.to_string(),
                session_generation: session.session_generation,
                delegation_id: String::new(),
                delegation_generation: 0,
                delegation_audience: String::new(),
                expires_at_unix_ms: session.expires_at_unix_ms,
            },
        )
        .await
        .map(|(invocation_id, state)| {
            Response::new(wire::ApproveAndEnqueueInvocationResponse {
                invocation_id,
                state,
            })
        })
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
            self.revalidate_delegated_record(&record).await?;
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
        self.revalidate_delegated_record(&record).await?;
        Ok(Response::new(
            wire::ValidateExecutionAuthorizationResponse {
                approved_invocation: Some(record.approved_invocation),
            },
        ))
    }

    async fn validate_product_execution_authorization(
        &self,
        request: Request<wire::ValidateProductExecutionAuthorizationRequest>,
    ) -> Result<Response<wire::ValidateProductExecutionAuthorizationResponse>, Status> {
        let credential = request
            .get_ref()
            .credential
            .as_ref()
            .ok_or_else(|| Status::invalid_argument("credential is required"))?;
        let (_, record) = self
            .validate_product_execution_credential(&request, credential)
            .await?;
        Ok(Response::new(
            wire::ValidateProductExecutionAuthorizationResponse {
                approved_invocation: Some(record.approved_invocation),
            },
        ))
    }

    async fn get_product_invocation_status(
        &self,
        request: Request<wire::GetProductInvocationStatusRequest>,
    ) -> Result<Response<wire::GetProductInvocationStatusResponse>, Status> {
        let profile = self.product_workload_peer(&request)?;
        let invocation_id = request.get_ref().invocation_id.as_str();
        if invocation_id.trim().is_empty() || invocation_id.len() > 128 {
            return Err(Status::invalid_argument("invocation id is invalid"));
        }
        let record = self
            .dependencies
            .outbox
            .get_product_invocation_status(invocation_id)
            .await
            .map_err(map_outbox_error)?;
        let envelope = record
            .approved_invocation
            .canonical_envelope
            .as_ref()
            .ok_or_else(|| Status::data_loss("persisted invocation is incomplete"))?;
        let invocation = envelope
            .invocation
            .as_ref()
            .ok_or_else(|| Status::data_loss("persisted invocation is incomplete"))?;
        let target = envelope
            .target
            .as_ref()
            .ok_or_else(|| Status::data_loss("persisted invocation is incomplete"))?;
        if profile.owner_id != invocation.owner_id
            || profile.target_component != target.target_component
            || profile
                .organization_id
                .as_deref()
                .is_some_and(|organization_id| organization_id != envelope.organization_id)
            || profile
                .workspace_id
                .as_deref()
                .is_some_and(|workspace_id| workspace_id != envelope.workspace_id)
        {
            return Err(Status::not_found("invocation was not found"));
        }
        if wire::InvocationAuthorizationSource::try_from(envelope.authorization_source)
            .map_err(|_| Status::data_loss("persisted authorization source is invalid"))?
            == wire::InvocationAuthorizationSource::Delegation
        {
            let delegation_id = Uuid::parse_str(&envelope.delegation_id)
                .map_err(|_| Status::data_loss("persisted delegation reference is invalid"))?;
            let delegation = self
                .dependencies
                .delegations
                .get(
                    &envelope.organization_id,
                    &envelope.workspace_id,
                    delegation_id,
                )
                .await
                .map_err(map_delegation_error)?;
            if !delegation_status_visible_to_workload(
                &delegation,
                envelope,
                invocation,
                target,
                profile,
            ) {
                return Err(Status::not_found("invocation was not found"));
            }
        }
        verify_approved(&record.approved_invocation)?;
        let result = record.result;
        Ok(Response::new(wire::GetProductInvocationStatusResponse {
            invocation_id: record.approved_invocation.invocation_id,
            state: record.state as i32,
            product_response: result
                .as_ref()
                .and_then(|result| result.product_response.clone()),
            error_code: result
                .as_ref()
                .map(|result| result.error_code.clone())
                .unwrap_or_default(),
            error_message: result
                .as_ref()
                .map(|result| result.error_message.clone())
                .unwrap_or_default(),
            result_digest_sha256: result
                .as_ref()
                .map(|result| result.result_digest_sha256.clone())
                .unwrap_or_default(),
            result_receipt: result
                .as_ref()
                .map(|result| result.result_receipt.clone())
                .unwrap_or_default(),
        }))
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
        let record = self
            .dependencies
            .outbox
            .validate_execution_credential(&peer, &credential)
            .await
            .map_err(map_outbox_error)?;
        self.revalidate_delegated_record(&record).await?;
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
        self.revalidate_delegated_record(&durable).await?;
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

fn workload_feature_signature_message(
    key_id: &str,
    digest: &[u8],
) -> AuthorityHelperResult<Vec<u8>> {
    if digest.len() != 32 {
        return Err(Status::permission_denied("workload feature token is invalid").into());
    }
    let len = u32::try_from(key_id.len())
        .map_err(|_| Status::failed_precondition("signing key id is invalid"))?;
    let mut message = Vec::with_capacity(80 + key_id.len());
    message.extend_from_slice(b"cyrene.workspace.authority.v2/workload-feature-token\0");
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

fn validate_product_workload_scope(
    credential: &wire::ExecutionCredential,
    profile: &AuthorityProductWorkloadProfile,
) -> AuthorityHelperResult<()> {
    if credential.operation_owner_id != profile.owner_id
        || credential.target_component != profile.target_component
        || profile
            .organization_id
            .as_deref()
            .is_some_and(|organization_id| organization_id != credential.organization_id)
        || profile
            .workspace_id
            .as_deref()
            .is_some_and(|workspace_id| workspace_id != credential.workspace_id)
    {
        return Err(Status::permission_denied(
            "Product workload profile does not authorize this target",
        )
        .into());
    }
    if wire::InvocationAuthorizationSource::try_from(credential.authorization_source)
        .map_err(|_| Status::permission_denied("authorization source is invalid"))?
        == wire::InvocationAuthorizationSource::Delegation
        && credential.delegation_audience != profile.audience
    {
        return Err(Status::permission_denied(
            "delegation audience does not match Product workload",
        )
        .into());
    }
    Ok(())
}

fn validate_delegation_fence(
    delegation: &AuthorityDelegation,
    fence: DelegationFence<'_>,
    now_unix_ms: u64,
) -> AuthorityHelperResult<()> {
    if delegation.revoked_at_unix_ms.is_some()
        || delegation.expires_at_unix_ms <= now_unix_ms
        || delegation.generation == 0
        || fence
            .generation
            .is_some_and(|generation| generation != delegation.generation)
        || delegation.organization_id != fence.organization_id
        || delegation.workspace_id != fence.workspace_id
        || delegation.principal_issuer != fence.principal_issuer
        || delegation.principal_subject != fence.principal_subject
        || delegation.audience != fence.audience
        || delegation.managed_workload_id != fence.managed_workload_id
    {
        return Err(Status::permission_denied("delegation is no longer active").into());
    }
    Ok(())
}

fn delegation_status_visible_to_workload(
    delegation: &AuthorityDelegation,
    envelope: &wire::CanonicalInvocationEnvelope,
    invocation: &cy_proto::cyrene::workspace::product::v2::ProductApiInvocationV2,
    target: &wire::ExecutionTarget,
    profile: &AuthorityProductWorkloadProfile,
) -> bool {
    profile.owner_id == invocation.owner_id
        && profile.target_component == target.target_component
        && profile
            .organization_id
            .as_deref()
            .is_none_or(|organization_id| organization_id == envelope.organization_id)
        && profile
            .workspace_id
            .as_deref()
            .is_none_or(|workspace_id| workspace_id == envelope.workspace_id)
        && delegation.organization_id == envelope.organization_id
        && delegation.workspace_id == envelope.workspace_id
        && delegation.principal_issuer == envelope.principal_issuer
        && delegation.principal_subject == envelope.principal_subject
        && delegation.audience == profile.audience
        && delegation.audience == envelope.delegation_audience
        && delegation.managed_workload_id == profile.workload_id
        && delegation.operations.iter().any(|scope| {
            scope.owner_id == invocation.owner_id
                && scope.operation_id == invocation.operation_id
                && (scope.resource_ids.is_empty() && target.resource_id.is_empty()
                    || scope
                        .resource_ids
                        .iter()
                        .any(|id| id == &target.resource_id))
        })
}

fn timestamp_to_unix_ms(value: Option<&prost_types::Timestamp>) -> AuthorityHelperResult<u64> {
    let value = value.ok_or_else(|| Status::invalid_argument("delegation expiry is invalid"))?;
    if value.seconds < 0 || !(0..1_000_000_000).contains(&value.nanos) {
        return Err(Status::invalid_argument("delegation expiry is invalid").into());
    }
    u64::try_from(value.seconds)
        .ok()
        .and_then(|seconds| seconds.checked_mul(1_000))
        .and_then(|millis| millis.checked_add(u64::try_from(value.nanos).ok()? / 1_000_000))
        .ok_or_else(|| Status::invalid_argument("delegation expiry is invalid").into())
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

#[allow(clippy::result_large_err)]
fn validate_connector_operation_request(
    request: &wire::AuthorizeConnectorOperationRequest,
    expected_workspace_id: &str,
    expected_identity_issuer: &str,
) -> Result<(), Status> {
    if request.workspace_id != expected_workspace_id {
        return Err(Status::permission_denied(
            "connector grant workspace is not configured",
        ));
    }
    if request.principal_issuer != expected_identity_issuer {
        return Err(Status::permission_denied(
            "connector identity issuer is not configured",
        ));
    }
    let parsed_subject = Uuid::parse_str(&request.principal_subject)
        .map_err(|_| Status::invalid_argument("connector identity subject is invalid"))?;
    if parsed_subject.to_string() != request.principal_subject {
        return Err(Status::invalid_argument(
            "connector identity subject is not canonical",
        ));
    }
    if request.arguments_digest_sha256.len() != 32
        || !is_supported_connector_operation(&request.provider_id, &request.operation_id)
        || !valid_connector_scope_value(&request.connection_ref, 256)
        || !valid_connector_scope_value(&request.provider_id, 64)
        || !valid_connector_scope_value(&request.profile_ref, 256)
        || !valid_connector_scope_value(&request.operation_id, 256)
        || !valid_connector_scope_value(&request.resource_ref, 2048)
        || !valid_connector_scope_value(&request.request_id, 256)
    {
        return Err(Status::invalid_argument(
            "connector operation scope is invalid",
        ));
    }
    Ok(())
}

/// Reload the authoritative Directory role and mint one exact-scope grant.
///
/// The network RPC invokes this only after validating the registered connector authorization
/// caller's mTLS peer. The
/// organization, workspace, issuer and workload audience arguments must come from that Authority
/// host's protected configuration; request identity and operation values are checked against them.
pub async fn issue_connector_operation_grant(
    directory: &dyn WorkspaceDirectory,
    signer: &AuthorityCredentialSigner,
    organization_id: &str,
    workspace_id: &str,
    expected_identity_issuer: &str,
    workload: &AuthorityManagedWorkload,
    input: &wire::AuthorizeConnectorOperationRequest,
) -> Result<wire::ConnectorOperationGrantV1, Status> {
    validate_connector_operation_request(input, workspace_id, expected_identity_issuer)?;
    if workload.workload_id.trim().is_empty() || workload.audience.trim().is_empty() {
        return Err(Status::failed_precondition(
            "connector authorization caller is not configured",
        ));
    }
    let identity = UserIdentityRef {
        issuer: input.principal_issuer.clone(),
        subject: input.principal_subject.clone(),
    };
    require_directory_connector_operation_role(
        directory,
        &identity,
        organization_id,
        &input.workspace_id,
        &input.provider_id,
        &input.operation_id,
    )
    .await?;

    let issued_at_unix_ms = now_unix_ms();
    let expires_at_unix_ms = issued_at_unix_ms.saturating_add(CONNECTOR_OPERATION_GRANT_TTL_MS);
    let mut grant = wire::ConnectorOperationGrantV1 {
        schema_version: CONNECTOR_OPERATION_GRANT_SCHEMA_VERSION.to_owned(),
        grant_ref: String::new(),
        audience: workload.audience.clone(),
        organization_id: organization_id.to_owned(),
        workspace_id: input.workspace_id.clone(),
        principal_issuer: input.principal_issuer.clone(),
        principal_subject: input.principal_subject.clone(),
        connection_ref: input.connection_ref.clone(),
        provider_id: input.provider_id.clone(),
        profile_ref: input.profile_ref.clone(),
        operation_id: input.operation_id.clone(),
        resource_ref: input.resource_ref.clone(),
        arguments_digest_sha256: input.arguments_digest_sha256.clone(),
        request_id: input.request_id.clone(),
        issued_at: Some(timestamp(issued_at_unix_ms)),
        expires_at: Some(timestamp(expires_at_unix_ms)),
        signing_key_id: String::new(),
        authority_signature: Vec::new(),
    };
    signer.sign_connector_operation_grant(&mut grant)?;
    Ok(grant)
}

#[allow(clippy::result_large_err)]
pub(super) async fn require_directory_connector_operation_role(
    directory: &dyn WorkspaceDirectory,
    identity: &UserIdentityRef,
    organization_id: &str,
    workspace_id: &str,
    provider_id: &str,
    operation_id: &str,
) -> Result<(), Status> {
    let roles = directory
        .roles_for_member(identity, organization_id, workspace_id)
        .await
        .map_err(|_| Status::unavailable("workspace directory is unavailable"))?
        .ok_or_else(|| Status::permission_denied("workspace membership is required"))?;
    if roles.contains(NAVIGATOR_CONNECTOR_OPERATOR_ROLE)
        || (is_automation_viewer_connector_operation(provider_id, operation_id)
            && roles.contains(NAVIGATOR_AUTOMATION_VIEWER_ROLE))
        || (is_office_task_writer_connector_operation(provider_id, operation_id)
            && roles.contains(NAVIGATOR_OFFICE_TASK_WRITER_ROLE))
        || (is_automation_transport_connector_operation(provider_id, operation_id)
            && (roles.contains(NAVIGATOR_AUTOMATION_VIEWER_ROLE)
                || roles.contains(NAVIGATOR_OFFICE_TASK_WRITER_ROLE)))
    {
        return Ok(());
    }
    Err(Status::permission_denied(
        "connector operation role is required",
    ))
}

fn valid_connector_scope_value(value: &str, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && value.trim() == value
        && !value.chars().any(char::is_control)
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
        || envelope.authorization_source != credential.authorization_source
        || envelope.delegation_id != credential.delegation_id
        || envelope.delegation_generation != credential.delegation_generation
        || envelope.delegation_audience != credential.delegation_audience
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
    match wire::InvocationAuthorizationSource::try_from(envelope.authorization_source)
        .map_err(|_| Status::data_loss("approved authorization source is invalid"))?
    {
        wire::InvocationAuthorizationSource::Unspecified
        | wire::InvocationAuthorizationSource::InteractiveSession => {
            if Uuid::parse_str(&envelope.session_id).is_err()
                || envelope.session_generation == 0
                || !envelope.delegation_id.is_empty()
                || envelope.delegation_generation != 0
                || !envelope.delegation_audience.is_empty()
            {
                return Err(Status::data_loss("approved authorization source is invalid").into());
            }
        }
        wire::InvocationAuthorizationSource::Delegation => {
            if !envelope.session_id.is_empty()
                || envelope.session_generation != 0
                || Uuid::parse_str(&envelope.delegation_id).is_err()
                || envelope.delegation_generation == 0
                || envelope.delegation_audience.trim().is_empty()
            {
                return Err(Status::data_loss("approved authorization source is invalid").into());
            }
        }
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

fn project_delegation(delegation: AuthorityDelegation) -> wire::GetDelegationResponse {
    wire::GetDelegationResponse {
        delegation_id: delegation.delegation_id.to_string(),
        organization_id: delegation.organization_id,
        workspace_id: delegation.workspace_id,
        principal_issuer: delegation.principal_issuer,
        principal_subject: delegation.principal_subject,
        audience: delegation.audience,
        operations: delegation
            .operations
            .into_iter()
            .map(|scope| wire::DelegatedOperationScope {
                owner_id: scope.owner_id,
                operation_id: scope.operation_id,
                resource_ids: scope.resource_ids,
            })
            .collect(),
        expires_at: Some(timestamp(delegation.expires_at_unix_ms)),
        generation: delegation.generation,
        revoked: delegation.revoked_at_unix_ms.is_some(),
        revoked_at: delegation.revoked_at_unix_ms.map(timestamp),
        managed_workload_id: delegation.managed_workload_id,
    }
}

fn map_delegation_error(error: AuthorityDelegationError) -> Status {
    match error {
        AuthorityDelegationError::NotFound => Status::not_found("delegation was not found"),
        AuthorityDelegationError::Inactive => Status::permission_denied("delegation is inactive"),
        AuthorityDelegationError::Unavailable => {
            Status::unavailable("delegation storage is unavailable")
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

#[cfg(test)]
mod workload_feature_token_tests {
    use super::*;

    #[test]
    fn default_interactive_authorization_fields_preserve_legacy_protobuf_bytes() {
        let envelope = wire::CanonicalInvocationEnvelope {
            invocation_id: "id".into(),
            ..Default::default()
        };
        assert_eq!(envelope.encode_to_vec(), vec![0x0a, 0x02, b'i', b'd']);

        let credential = wire::ExecutionCredential {
            invocation_id: "id".into(),
            ..Default::default()
        };
        assert_eq!(credential.encode_to_vec(), vec![0x0a, 0x02, b'i', b'd']);
    }

    #[test]
    fn product_workload_scope_rejects_wrong_owner_target_audience_and_workspace() {
        let profile = AuthorityProductWorkloadProfile {
            workload_id: "exchange".into(),
            owner_id: "exchange".into(),
            target_component: "cy-workspace-exchange".into(),
            audience: "exchange".into(),
            organization_id: Some("org-1".into()),
            workspace_id: Some("workspace-1".into()),
            certificate_fingerprints_sha256: vec![[4; 32]],
        };
        let mut credential = wire::ExecutionCredential {
            operation_owner_id: "exchange".into(),
            target_component: "cy-workspace-exchange".into(),
            organization_id: "org-1".into(),
            workspace_id: "workspace-1".into(),
            authorization_source: wire::InvocationAuthorizationSource::Delegation as i32,
            delegation_audience: "exchange".into(),
            ..Default::default()
        };
        assert!(validate_product_workload_scope(&credential, &profile).is_ok());

        credential.operation_owner_id = "github".into();
        assert!(validate_product_workload_scope(&credential, &profile).is_err());
        credential.operation_owner_id = "exchange".into();
        credential.target_component = "cy-workspace-github".into();
        assert!(validate_product_workload_scope(&credential, &profile).is_err());
        credential.target_component = "cy-workspace-exchange".into();
        credential.delegation_audience = "other".into();
        assert!(validate_product_workload_scope(&credential, &profile).is_err());
        credential.delegation_audience = "exchange".into();
        credential.workspace_id = "workspace-2".into();
        assert!(validate_product_workload_scope(&credential, &profile).is_err());
    }

    #[test]
    fn delegation_fence_binds_workload_lifetime_revocation_and_full_issuer_subject() {
        let mut delegation = AuthorityDelegation {
            delegation_id: Uuid::new_v4(),
            generation: 4,
            organization_id: "org-1".into(),
            workspace_id: "workspace-1".into(),
            principal_issuer: "https://issuer.example/tenant-a".into(),
            principal_subject: "same-subject".into(),
            audience: "exchange".into(),
            managed_workload_id: "exchange-prod-v1".into(),
            operations: vec![],
            expires_at_unix_ms: 2_000,
            revoked_at_unix_ms: None,
        };
        let fence = DelegationFence {
            organization_id: "org-1",
            workspace_id: "workspace-1",
            principal_issuer: "https://issuer.example/tenant-a",
            principal_subject: "same-subject",
            audience: "exchange",
            managed_workload_id: "exchange-prod-v1",
            generation: Some(4),
        };
        assert!(validate_delegation_fence(&delegation, fence, 1_000).is_ok());
        assert!(validate_delegation_fence(
            &delegation,
            DelegationFence {
                managed_workload_id: "exchange-staging-v1",
                ..fence
            },
            1_000
        )
        .is_err());
        assert!(validate_delegation_fence(
            &delegation,
            DelegationFence {
                principal_issuer: "https://issuer.example/tenant-b",
                ..fence
            },
            1_000
        )
        .is_err());
        assert!(validate_delegation_fence(
            &delegation,
            DelegationFence {
                generation: Some(5),
                ..fence
            },
            1_000
        )
        .is_err());
        assert!(validate_delegation_fence(&delegation, fence, 2_000).is_err());
        delegation.revoked_at_unix_ms = Some(1_500);
        assert!(validate_delegation_fence(&delegation, fence, 1_600).is_err());
    }

    #[test]
    fn status_read_allows_same_workload_after_revoke_but_denies_same_audience_peer() {
        let delegation = AuthorityDelegation {
            delegation_id: Uuid::new_v4(),
            generation: 5,
            organization_id: "org-1".into(),
            workspace_id: "workspace-1".into(),
            principal_issuer: "https://issuer.example".into(),
            principal_subject: "user-1".into(),
            audience: "exchange".into(),
            managed_workload_id: "exchange-a".into(),
            operations: vec![DelegatedOperationScope {
                owner_id: "exchange".into(),
                operation_id: "integrationExecuteIssueComment".into(),
                resource_ids: vec!["binding-1".into()],
            }],
            expires_at_unix_ms: 2_000,
            revoked_at_unix_ms: Some(1_500),
        };
        let envelope = wire::CanonicalInvocationEnvelope {
            organization_id: "org-1".into(),
            workspace_id: "workspace-1".into(),
            principal_issuer: "https://issuer.example".into(),
            principal_subject: "user-1".into(),
            delegation_audience: "exchange".into(),
            delegation_generation: 4,
            ..Default::default()
        };
        let invocation = cy_proto::cyrene::workspace::product::v2::ProductApiInvocationV2 {
            owner_id: "exchange".into(),
            operation_id: "integrationExecuteIssueComment".into(),
            ..Default::default()
        };
        let target = wire::ExecutionTarget {
            target_component: "cy-workspace-exchange".into(),
            resource_id: "binding-1".into(),
            ..Default::default()
        };
        let profile = |workload_id: &str| AuthorityProductWorkloadProfile {
            workload_id: workload_id.into(),
            owner_id: "exchange".into(),
            target_component: "cy-workspace-exchange".into(),
            audience: "exchange".into(),
            organization_id: Some("org-1".into()),
            workspace_id: Some("workspace-1".into()),
            certificate_fingerprints_sha256: vec![[8; 32]],
        };

        assert!(delegation_status_visible_to_workload(
            &delegation,
            &envelope,
            &invocation,
            &target,
            &profile("exchange-a"),
        ));
        assert!(!delegation_status_visible_to_workload(
            &delegation,
            &envelope,
            &invocation,
            &target,
            &profile("exchange-b"),
        ));
    }

    #[test]
    fn workload_feature_token_signature_binds_its_audience_and_certificate() {
        let signer = AuthorityCredentialSigner::new(
            "test-key",
            SigningKey::from_bytes(&[7; 32]),
            Duration::from_secs(180),
        )
        .expect("signer configuration is valid");
        let mut token = wire::ManagedWorkloadFeatureToken {
            audience: "exchange".into(),
            client_certificate_sha256: vec![9; 32],
            signing_key_id: String::new(),
            issued_at: Some(timestamp(1_000)),
            expires_at: Some(timestamp(301_000)),
            authority_signature: Vec::new(),
        };
        signer
            .sign_workload_feature_token(&mut token)
            .expect("token signs");
        signer
            .verify_workload_feature_token(&token)
            .expect("issued token verifies");

        let mut changed_audience = token.clone();
        changed_audience.audience = "other".into();
        assert!(signer
            .verify_workload_feature_token(&changed_audience)
            .is_err());
        let mut changed_certificate = token;
        changed_certificate.client_certificate_sha256[0] ^= 1;
        assert!(signer
            .verify_workload_feature_token(&changed_certificate)
            .is_err());
    }
}

#[cfg(test)]
mod get_delegation_identity_tests {
    //! Real mTLS RPC tests for Product-proof and legacy bearer delegation reads.
    use std::{
        collections::{BTreeMap, BTreeSet},
        fs,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
        time::Duration,
    };

    use async_trait::async_trait;
    use cy_proto::{
        cyrene::workspace::authority::v2::workspace_authority_service_client::WorkspaceAuthorityServiceClient,
        cyrene::workspace::authority::v2::workspace_authority_service_server::WorkspaceAuthorityServiceServer,
        workspace_v1::UserIdentityRef,
    };
    use cy_workspace_product_contracts::{
        ProductBundlePins, ProductContractBundle, TrustedProductPolicy,
    };
    use serde_json::json;
    use sha2::{Digest, Sha256};
    use tempfile::TempDir;
    use tokio::net::TcpListener;
    use tokio_stream::wrappers::TcpListenerStream;
    use tonic::{
        transport::{Certificate, ClientTlsConfig, Endpoint, Identity, Server, ServerTlsConfig},
        Code, Request,
    };
    use uuid::Uuid;

    use super::*;
    use crate::{
        authority::{
            snapshot::{ContractSnapshot, SnapshotIdentity, VerifiedContractSnapshot},
            AuthorityInvocationRecord, AuthorityOutboxError, AuthorityOutboxStore,
            AuthorityTargetBinding, AuthorityWebSession, EnqueueOutcome, ExecutionDevicePeer,
            InvocationWaitScope, MemoryAuthorityDelegationStore, NewAuthorityInvocation,
            PersistedInvocationResult, SubmitResultOutcome,
        },
        device_registry::{
            ApprovedWorkspaceDeviceCertificate, CurrentRelayPeerRevocationEvidence,
            RelayPeerRevocationCheckError, WorkspaceDeviceCertificateIdentity,
            WorkspaceDeviceDispatchFence, WorkspaceDeviceKey,
            WorkspaceDevicePeerCertificateStatusChecker, WorkspaceDeviceRecord,
            WorkspaceDeviceRegistry,
        },
        directory::{InMemoryWorkspaceDirectory, WorkspaceDirectoryError, WorkspaceMembership},
        web_identity::{
            test_principal, VerifiedWebPrincipal, WebIdentityError, WebPrincipalVerifier,
        },
    };

    const ORGANIZATION_ID: &str = "org-1";
    const WORKSPACE_ID: &str = "workspace-1";
    const PRINCIPAL_ISSUER: &str = "https://issuer.example/tenant-a";
    const PRINCIPAL_SUBJECT: &str = "actor-1";
    const PRODUCT_WORKLOAD_ID: &str = "example-product-workload-v1";
    const OWNER_ID: &str = "example";
    const OPERATION_ID: &str = "listWorkspaceItems";
    const TARGET_COMPONENT: &str = "cy-workspace-example";
    // These self-signed keys are test-only and have no production trust relationship.
    const CLIENT_CERTIFICATE_DER: &[u8] =
        include_bytes!("../../testdata/product-identity/client.der");
    const CLIENT_CERTIFICATE_PEM: &[u8] =
        include_bytes!("../../testdata/product-identity/client.pem");
    const CLIENT_PRIVATE_KEY_PEM: &[u8] =
        include_bytes!("../../testdata/product-identity/client-key.pem");
    const AUTHORITY_CA_PEM: &[u8] = include_bytes!("../../testdata/product-identity/ca.pem");
    const AUTHORITY_CERTIFICATE_PEM: &[u8] =
        include_bytes!("../../testdata/product-identity/server.pem");
    const AUTHORITY_PRIVATE_KEY_PEM: &[u8] =
        include_bytes!("../../testdata/product-identity/server-key.pem");

    struct TestOutbox {
        record: AuthorityInvocationRecord,
        active_claim: bool,
        validations: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl AuthorityOutboxStore for TestOutbox {
        async fn enqueue_authorized(
            &self,
            _invocation: NewAuthorityInvocation,
        ) -> Result<EnqueueOutcome, AuthorityOutboxError> {
            Err(AuthorityOutboxError::Unavailable)
        }

        async fn claim_for_execution_device(
            &self,
            _peer: &ExecutionDevicePeer,
            _max_batch_size: usize,
        ) -> Result<Vec<AuthorityInvocationRecord>, AuthorityOutboxError> {
            Err(AuthorityOutboxError::Unavailable)
        }

        async fn validate_execution_credential(
            &self,
            _peer: &ExecutionDevicePeer,
            _credential: &wire::ExecutionCredential,
        ) -> Result<AuthorityInvocationRecord, AuthorityOutboxError> {
            Err(AuthorityOutboxError::Unavailable)
        }

        async fn validate_product_execution_authorization(
            &self,
            credential: &wire::ExecutionCredential,
        ) -> Result<AuthorityInvocationRecord, AuthorityOutboxError> {
            self.validations.fetch_add(1, Ordering::Relaxed);
            if !self.active_claim {
                return Err(AuthorityOutboxError::NotFound);
            }
            if self.record.approved_invocation.credential.as_ref() != Some(credential) {
                return Err(AuthorityOutboxError::AuthorizationRevoked);
            }
            Ok(self.record.clone())
        }

        async fn get_product_invocation_status(
            &self,
            _invocation_id: &str,
        ) -> Result<AuthorityInvocationRecord, AuthorityOutboxError> {
            Err(AuthorityOutboxError::Unavailable)
        }

        async fn acknowledge_delivery(
            &self,
            _peer: &ExecutionDevicePeer,
            _invocation_id: &str,
            _credential: &wire::ExecutionCredential,
            _delivery_receipt: &[u8],
        ) -> Result<bool, AuthorityOutboxError> {
            Err(AuthorityOutboxError::Unavailable)
        }

        async fn submit_result(
            &self,
            _peer: &ExecutionDevicePeer,
            _result: PersistedInvocationResult,
        ) -> Result<SubmitResultOutcome, AuthorityOutboxError> {
            Err(AuthorityOutboxError::Unavailable)
        }

        async fn wait_for_result(
            &self,
            _invocation_id: &str,
            _scope: &InvocationWaitScope,
            _timeout: Duration,
        ) -> Result<Option<AuthorityInvocationRecord>, AuthorityOutboxError> {
            Err(AuthorityOutboxError::Unavailable)
        }
    }

    struct TestVerifier {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl WebPrincipalVerifier for TestVerifier {
        async fn verify_access_token(
            &self,
            access_token: &str,
        ) -> Result<VerifiedWebPrincipal, WebIdentityError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            let subject = match access_token {
                "self" => PRINCIPAL_SUBJECT,
                "other" => "actor-2",
                _ => return Err(WebIdentityError::InvalidToken),
            };
            Ok(test_principal(
                UserIdentityRef {
                    issuer: PRINCIPAL_ISSUER.to_owned(),
                    subject: subject.to_owned(),
                },
                ORGANIZATION_ID,
                now_unix_ms() as i64 + 60_000,
            ))
        }
    }

    struct TestSessions {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl AuthorityWebSessionStore for TestSessions {
        async fn resolve_verified_bearer(
            &self,
            principal: &VerifiedWebPrincipal,
            workspace_id: &str,
            _bearer_token: &str,
            now: u64,
        ) -> Result<AuthorityWebSession, tonic::Status> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(AuthorityWebSession {
                session_id: Uuid::from_bytes([12; 16]),
                organization_id: principal.organization_id().to_owned(),
                workspace_id: workspace_id.to_owned(),
                principal_issuer: principal.identity().issuer.clone(),
                principal_subject: principal.identity().subject.clone(),
                session_generation: 1,
                issued_at_unix_ms: now,
                expires_at_unix_ms: principal.expires_at_unix_ms() as u64,
            })
        }
    }

    struct TestTargets;

    #[async_trait]
    impl AuthorityExecutionTargetStore for TestTargets {
        async fn resolve(
            &self,
            _organization_id: &str,
            _workspace_id: &str,
            _owner_id: &str,
            _operation_id: &str,
        ) -> Result<Option<AuthorityTargetBinding>, tonic::Status> {
            Ok(None)
        }
    }

    struct UnusedDeviceRegistry;

    impl WorkspaceDeviceRegistry for UnusedDeviceRegistry {
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
            _fingerprint_sha256: &str,
        ) -> Result<Option<WorkspaceDeviceCertificateIdentity>, WorkspaceDirectoryError> {
            Err(WorkspaceDirectoryError::Storage(
                "unused test method".into(),
            ))
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

    struct UnusedDeviceCertificateStatus;

    impl WorkspaceDevicePeerCertificateStatusChecker for UnusedDeviceCertificateStatus {
        fn require_current_good_status_for_peer_chain(
            &self,
            _leaf_certificate_der: &[u8],
            _intermediate_chain_der: &[Vec<u8>],
            _checked_at_unix_ms: u64,
        ) -> Result<CurrentRelayPeerRevocationEvidence, RelayPeerRevocationCheckError> {
            Err(RelayPeerRevocationCheckError::Unknown)
        }
    }

    struct FixtureOptions {
        active_claim: bool,
        grant_subject: &'static str,
        expired_credential: bool,
        wrong_profile: bool,
        authorization_source: wire::InvocationAuthorizationSource,
    }

    impl Default for FixtureOptions {
        fn default() -> Self {
            Self {
                active_claim: true,
                grant_subject: PRINCIPAL_SUBJECT,
                expired_credential: false,
                wrong_profile: false,
                authorization_source: wire::InvocationAuthorizationSource::Delegation,
            }
        }
    }

    struct ServiceFixture {
        service: AuthorityRpcService,
        delegation: AuthorityDelegation,
        delegation_id: Uuid,
        credential: wire::ExecutionCredential,
        validations: Arc<AtomicUsize>,
        verifier_calls: Arc<AtomicUsize>,
        session_calls: Arc<AtomicUsize>,
        _temporary: TempDir,
    }

    impl ServiceFixture {
        async fn new(options: FixtureOptions) -> Self {
            let temporary = TempDir::new().expect("temporary fixture directory is available");
            let snapshots = Arc::new(test_snapshot(&temporary));
            let signer = AuthorityCredentialSigner::new(
                "product-identity-test-key",
                SigningKey::from_bytes(&[7; 32]),
                Duration::from_secs(180),
            )
            .expect("test signer configuration is valid");
            let delegation_id = Uuid::from_bytes([42; 16]);
            let approved = approved_invocation(
                &signer,
                delegation_id,
                options.expired_credential,
                options.authorization_source,
            );
            let credential = approved
                .credential
                .as_ref()
                .expect("test invocation has its signed credential")
                .clone();
            let record = AuthorityInvocationRecord {
                approved_invocation: approved,
                state: wire::InvocationState::Claimed,
                result: None,
            };
            let validations = Arc::new(AtomicUsize::new(0));
            let outbox = Arc::new(TestOutbox {
                record,
                active_claim: options.active_claim,
                validations: validations.clone(),
            });
            let delegation = AuthorityDelegation {
                delegation_id,
                generation: 7,
                organization_id: ORGANIZATION_ID.to_owned(),
                workspace_id: WORKSPACE_ID.to_owned(),
                principal_issuer: PRINCIPAL_ISSUER.to_owned(),
                principal_subject: options.grant_subject.to_owned(),
                audience: "example".to_owned(),
                managed_workload_id: PRODUCT_WORKLOAD_ID.to_owned(),
                operations: vec![DelegatedOperationScope {
                    owner_id: OWNER_ID.to_owned(),
                    operation_id: OPERATION_ID.to_owned(),
                    resource_ids: Vec::new(),
                }],
                expires_at_unix_ms: now_unix_ms().saturating_add(3_600_000),
                revoked_at_unix_ms: None,
            };
            let delegations = Arc::new(MemoryAuthorityDelegationStore::default());
            delegations
                .create(delegation.clone())
                .await
                .expect("test delegation is stored");

            let directory = Arc::new(
                InMemoryWorkspaceDirectory::new(
                    [PRINCIPAL_SUBJECT, "actor-2"]
                        .into_iter()
                        .map(|subject| WorkspaceMembership {
                            user: UserIdentityRef {
                                issuer: PRINCIPAL_ISSUER.to_owned(),
                                subject: subject.to_owned(),
                            },
                            organization_id: ORGANIZATION_ID.to_owned(),
                            workspace_id: WORKSPACE_ID.to_owned(),
                            roles: BTreeSet::from(["workspace.member".to_owned()]),
                        })
                        .collect(),
                    Vec::new(),
                )
                .expect("test Directory is valid"),
            );
            let certificate_fingerprint: [u8; 32] = Sha256::digest(CLIENT_CERTIFICATE_DER).into();
            let profile_fingerprint = if options.wrong_profile {
                [0xff; 32]
            } else {
                certificate_fingerprint
            };
            let verifier_calls = Arc::new(AtomicUsize::new(0));
            let session_calls = Arc::new(AtomicUsize::new(0));
            let service = AuthorityRpcService::new(AuthorityServiceDependencies {
                organization_id: ORGANIZATION_ID.to_owned(),
                workspace_id: WORKSPACE_ID.to_owned(),
                connector_identity_issuer: format!("{PRINCIPAL_ISSUER}/v2.0"),
                authority_instance_id: "authority-test".to_owned(),
                snapshots,
                web_verifier: Arc::new(TestVerifier {
                    calls: verifier_calls.clone(),
                }),
                directory,
                sessions: Arc::new(TestSessions {
                    calls: session_calls.clone(),
                }),
                delegations,
                exchange_workload: AuthorityManagedWorkload {
                    workload_id: PRODUCT_WORKLOAD_ID.to_owned(),
                    audience: "example".to_owned(),
                    certificate_fingerprints_sha256: Vec::new(),
                },
                product_workload_profiles: vec![AuthorityProductWorkloadProfile {
                    workload_id: PRODUCT_WORKLOAD_ID.to_owned(),
                    owner_id: OWNER_ID.to_owned(),
                    target_component: TARGET_COMPONENT.to_owned(),
                    audience: "example".to_owned(),
                    organization_id: Some(ORGANIZATION_ID.to_owned()),
                    workspace_id: Some(WORKSPACE_ID.to_owned()),
                    certificate_fingerprints_sha256: vec![profile_fingerprint],
                }],
                targets: Arc::new(TestTargets),
                device_registry: Arc::new(UnusedDeviceRegistry),
                device_certificate_status: Arc::new(UnusedDeviceCertificateStatus),
                outbox,
                signer,
            });

            Self {
                service,
                delegation,
                delegation_id,
                credential,
                validations,
                verifier_calls,
                session_calls,
                _temporary: temporary,
            }
        }

        fn proof_request(&self) -> wire::GetDelegationRequest {
            wire::GetDelegationRequest {
                workspace_id: WORKSPACE_ID.to_owned(),
                delegation_id: self.delegation_id.to_string(),
                execution_credential: Some(self.credential.clone()),
            }
        }
    }

    fn test_snapshot(temporary: &TempDir) -> crate::authority::ContractSnapshotManager {
        let bundle_root = temporary.path().join("bundle");
        fs::create_dir_all(&bundle_root).expect("create Product bundle root");
        let source_commit = "a".repeat(40);
        let catalog_path = "Cyrene-Example/contracts/product/v2/catalog.json";
        let openapi_path = "Cyrene-Example/contracts/product/v2/openapi.json";
        let catalog = json!({
            "schemaVersion": "cyrene.product.operation-catalog.v2",
            "catalogVersion": "2.0.0",
            "ownerId": OWNER_ID,
            "operations": [{
                "operationId": OPERATION_ID,
                "routeId": OPERATION_ID,
                "openapiPath": "contracts/product/v2/openapi.json",
                "kind": "READ",
                "requestSchemaPointer": null,
                "responseSchemaPointers": [],
                "resourceId": {
                    "required": false,
                    "pathParameter": null,
                    "minLength": 1,
                    "maxLength": 128,
                    "pattern": null
                },
                "idempotency": {
                    "required": false,
                    "header": null,
                    "minLength": 8,
                    "maxLength": 128
                },
                "scope": {
                    "organizationPathParameter": null,
                    "workspacePathParameter": null,
                    "requestBindings": [],
                    "responseBindings": []
                }
            }]
        });
        let openapi = json!({
            "openapi": "3.1.0",
            "info": {"title": "Example Product test", "version": "2.0.0"},
            "paths": {
                "/v2/workspaces": {
                    "get": {
                        "operationId": OPERATION_ID,
                        "responses": {"200": {"description": "OK"}}
                    }
                }
            }
        });
        let policy = json!({
            "schemaVersion": "cyrene.workspace.product.authorization-policy.v2",
            "policyVersion": "2.0.0",
            "grants": [{
                "ownerId": OWNER_ID,
                "operationId": OPERATION_ID,
                "principalKinds": ["DIRECTORY_USER"],
                "requiredRoles": ["workspace.member"],
                "requiresExactWorkspace": true,
                "requiredRequestBindings": [],
                "requiredResponseBindings": []
            }]
        });
        let catalog_bytes = serde_json::to_vec(&catalog).expect("serialize test catalog");
        let openapi_bytes = serde_json::to_vec(&openapi).expect("serialize test OpenAPI");
        let policy_bytes = serde_json::to_vec(&policy).expect("serialize test policy");
        let files = BTreeMap::from([
            (catalog_path.to_owned(), catalog_bytes.clone()),
            (openapi_path.to_owned(), openapi_bytes),
            (
                "workspace-product-policy-v2.json".to_owned(),
                policy_bytes.clone(),
            ),
        ]);
        for (relative_path, bytes) in &files {
            let path = bundle_root.join(relative_path);
            fs::create_dir_all(path.parent().expect("bundle file has a parent"))
                .expect("create bundle file parent");
            fs::write(path, bytes).expect("write test bundle file");
        }
        let catalog_digest = sha256_hex(&catalog_bytes);
        let policy_digest = sha256_hex(&policy_bytes);
        let manifest = json!({
            "formatVersion": 2,
            "wireApiVersion": "cyrene.workspace.product.v2",
            "owners": [{
                "ownerId": OWNER_ID,
                "repository": "Cyrene-Example",
                "sourceSha": source_commit.clone(),
                "catalogPath": catalog_path,
                "catalogSha256": catalog_digest
            }],
            "files": files.iter().map(|(path, bytes)| json!({
                "path": path,
                "sha256": sha256_hex(bytes)
            })).collect::<Vec<_>>()
        });
        let manifest_bytes = serde_json::to_vec(&manifest).expect("serialize test manifest");
        let manifest_digest = sha256_hex(&manifest_bytes);
        fs::write(
            bundle_root.join("product-contract-bundle.json"),
            &manifest_bytes,
        )
        .expect("write test manifest");
        let pins = ProductBundlePins::new(
            "cyrene.workspace.product.v2",
            manifest_digest.clone(),
            BTreeMap::from([(OWNER_ID.to_owned(), source_commit.clone())]),
            "cyrene.workspace.product.authorization-policy.v2",
            policy_digest.clone(),
        );
        let bundle = ProductContractBundle::load(&bundle_root, &pins)
            .expect("load pinned test Product bundle");
        let policy =
            TrustedProductPolicy::load(bundle_root.join("workspace-product-policy-v2.json"), &pins)
                .expect("load test Platform policy");
        policy
            .validate_bundle(&bundle)
            .expect("test policy matches the Product bundle");
        let content_digest = format!("sha256:{}", sha256_hex(b"test snapshot artifact"));
        let snapshot = ContractSnapshot {
            generation: 1,
            activation_epoch: 1,
            bundle: Arc::new(bundle),
            policy: Arc::new(policy),
            pins,
            identity: SnapshotIdentity {
                wire_api_version: "cyrene.workspace.product.v2".to_owned(),
                bundle_manifest_sha256: manifest_digest,
                policy_sha256: policy_digest,
                content_digest: content_digest.clone(),
                policy_source_commit: source_commit.clone(),
                source_commits: BTreeMap::from([(OWNER_ID.to_owned(), source_commit)]),
                owner_catalog_digests: BTreeMap::from([(OWNER_ID.to_owned(), catalog_digest)]),
            },
            artifact_id: content_digest,
            activated_at_unix_ms: now_unix_ms(),
        };
        crate::authority::ContractSnapshotManager::open(
            temporary.path().join("state"),
            VerifiedContractSnapshot::from_verified(snapshot),
        )
        .expect("open test active snapshot")
    }

    fn approved_invocation(
        signer: &AuthorityCredentialSigner,
        delegation_id: Uuid,
        expired: bool,
        authorization_source: wire::InvocationAuthorizationSource,
    ) -> wire::ApprovedInvocation {
        let invocation_id = Uuid::from_bytes([99; 16]).to_string();
        let invocation = cy_proto::cyrene::workspace::product::v2::ProductApiInvocationV2 {
            owner_id: OWNER_ID.to_owned(),
            operation_id: OPERATION_ID.to_owned(),
            ..Default::default()
        };
        let target = wire::ExecutionTarget {
            target_component: TARGET_COMPONENT.to_owned(),
            http_method: "GET".to_owned(),
            endpoint: "https://example.test/v2/workspaces".to_owned(),
            resource_id: String::new(),
            scope: "org-1/workspace-1".to_owned(),
            idempotency_key: String::new(),
            idempotency_semantics: wire::IdempotencySemantics::NotSupported as i32,
            execution_device_id: "connector-1".to_owned(),
            execution_device_generation: 4,
            execution_authorization_id: vec![3; 16],
            execution_device_certificate_sha256: vec![4; 32],
        };
        let now = now_unix_ms();
        let issued_at = if expired {
            now.saturating_sub(120_000)
        } else {
            now.saturating_sub(1_000)
        };
        let expires_at = if expired {
            now.saturating_sub(1)
        } else {
            now.saturating_add(120_000)
        };
        let is_delegated = authorization_source == wire::InvocationAuthorizationSource::Delegation;
        let session_id = if is_delegated {
            String::new()
        } else {
            Uuid::from_bytes([13; 16]).to_string()
        };
        let session_generation = if is_delegated { 0 } else { 1 };
        let approved_delegation_id = if is_delegated {
            delegation_id.to_string()
        } else {
            String::new()
        };
        let delegation_generation = if is_delegated { 7 } else { 0 };
        let delegation_audience = if is_delegated { "example" } else { "" };
        let envelope = wire::CanonicalInvocationEnvelope {
            invocation_id: invocation_id.clone(),
            invocation: Some(invocation.clone()),
            target: Some(target.clone()),
            organization_id: ORGANIZATION_ID.to_owned(),
            workspace_id: WORKSPACE_ID.to_owned(),
            principal_issuer: PRINCIPAL_ISSUER.to_owned(),
            principal_subject: PRINCIPAL_SUBJECT.to_owned(),
            execution_device_id: target.execution_device_id.clone(),
            execution_device_generation: target.execution_device_generation,
            execution_authorization_id: target.execution_authorization_id.clone(),
            execution_device_certificate_sha256: target.execution_device_certificate_sha256.clone(),
            session_id: session_id.clone(),
            session_generation,
            contract_activation_generation: 1,
            issued_at: Some(timestamp(issued_at)),
            expires_at: Some(timestamp(expires_at)),
            authorization_source: authorization_source as i32,
            delegation_id: approved_delegation_id.clone(),
            delegation_generation,
            delegation_audience: delegation_audience.to_owned(),
        };
        let request_digest = Sha256::digest(envelope.encode_to_vec()).to_vec();
        let invocation_digest = Sha256::digest(invocation.encode_to_vec()).to_vec();
        let mut credential = wire::ExecutionCredential {
            invocation_id: invocation_id.clone(),
            invocation_digest_sha256: invocation_digest,
            request_digest_sha256: request_digest,
            organization_id: ORGANIZATION_ID.to_owned(),
            workspace_id: WORKSPACE_ID.to_owned(),
            principal_issuer: PRINCIPAL_ISSUER.to_owned(),
            principal_subject: PRINCIPAL_SUBJECT.to_owned(),
            operation_owner_id: OWNER_ID.to_owned(),
            operation_id: OPERATION_ID.to_owned(),
            scope: target.scope.clone(),
            resource_id: target.resource_id.clone(),
            target_component: target.target_component.clone(),
            http_method: target.http_method.clone(),
            endpoint: target.endpoint.clone(),
            execution_device_id: target.execution_device_id.clone(),
            execution_device_generation: target.execution_device_generation,
            execution_authorization_id: target.execution_authorization_id.clone(),
            execution_device_certificate_sha256: target.execution_device_certificate_sha256.clone(),
            session_id,
            session_generation,
            contract_activation_generation: 1,
            idempotency_key: target.idempotency_key.clone(),
            idempotency_semantics: target.idempotency_semantics,
            signing_key_id: String::new(),
            issued_at: Some(timestamp(issued_at)),
            expires_at: Some(timestamp(expires_at)),
            authority_signature: Vec::new(),
            authorization_source: authorization_source as i32,
            delegation_id: approved_delegation_id,
            delegation_generation,
            delegation_audience: delegation_audience.to_owned(),
        };
        signer
            .sign(&mut credential)
            .expect("test execution credential signs");
        wire::ApprovedInvocation {
            invocation_id,
            canonical_envelope: Some(envelope),
            credential: Some(credential),
        }
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    async fn call_over_product_mtls(
        service: AuthorityRpcService,
        request: wire::GetDelegationRequest,
    ) -> Result<wire::GetDelegationResponse, tonic::Status> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local test Authority listener");
        let address = listener.local_addr().expect("test listener has address");
        let tls = ServerTlsConfig::new()
            .identity(Identity::from_pem(
                AUTHORITY_CERTIFICATE_PEM,
                AUTHORITY_PRIVATE_KEY_PEM,
            ))
            .client_ca_root(Certificate::from_pem(AUTHORITY_CA_PEM));
        let mut server = Server::builder()
            .tls_config(tls)
            .expect("test Authority TLS configuration is valid");
        let server_task = tokio::spawn(async move {
            server
                .add_service(WorkspaceAuthorityServiceServer::new(service))
                .serve_with_incoming(TcpListenerStream::new(listener))
                .await
        });

        let client_tls = ClientTlsConfig::new()
            .ca_certificate(Certificate::from_pem(AUTHORITY_CA_PEM))
            .domain_name("authority.test")
            .identity(Identity::from_pem(
                CLIENT_CERTIFICATE_PEM,
                CLIENT_PRIVATE_KEY_PEM,
            ));
        let endpoint = Endpoint::from_shared(format!("https://{address}"))
            .expect("test Authority endpoint is valid")
            .tls_config(client_tls)
            .expect("test client TLS configuration is valid");
        let result = match endpoint.connect().await {
            Ok(channel) => WorkspaceAuthorityServiceClient::new(channel)
                .get_delegation(Request::new(request))
                .await
                .map(|response| response.into_inner()),
            Err(error) => Err(tonic::Status::unavailable(format!(
                "test mTLS client connection failed: {error}"
            ))),
        };
        server_task.abort();
        result
    }

    #[tokio::test]
    async fn approved_product_mtls_proof_returns_the_same_self_metadata_as_bearer() {
        let fixture = ServiceFixture::new(FixtureOptions::default()).await;
        let proof_response =
            call_over_product_mtls(fixture.service.clone(), fixture.proof_request())
                .await
                .expect("current approved Product proof reads its own delegation");
        assert_eq!(fixture.verifier_calls.load(Ordering::Relaxed), 0);
        assert_eq!(fixture.session_calls.load(Ordering::Relaxed), 0);
        assert_eq!(fixture.validations.load(Ordering::Relaxed), 1);
        assert_eq!(proof_response.principal_issuer, PRINCIPAL_ISSUER);
        assert_eq!(proof_response.principal_subject, PRINCIPAL_SUBJECT);
        assert_eq!(proof_response.workspace_id, WORKSPACE_ID);

        let mut bearer_request = Request::new(wire::GetDelegationRequest {
            workspace_id: WORKSPACE_ID.to_owned(),
            delegation_id: fixture.delegation_id.to_string(),
            execution_credential: None,
        });
        bearer_request.metadata_mut().insert(
            "authorization",
            "Bearer self"
                .parse()
                .expect("static bearer metadata is valid"),
        );
        let bearer_response =
            WorkspaceAuthorityService::get_delegation(&fixture.service, bearer_request)
                .await
                .expect("legacy self bearer read remains supported")
                .into_inner();
        assert_eq!(proof_response, bearer_response);
        assert_eq!(fixture.verifier_calls.load(Ordering::Relaxed), 1);
        assert_eq!(fixture.session_calls.load(Ordering::Relaxed), 1);
        assert_eq!(fixture.validations.load(Ordering::Relaxed), 1);
        assert_eq!(
            project_delegation(fixture.delegation.clone()),
            proof_response
        );
    }

    #[tokio::test]
    async fn interactive_session_and_legacy_interactive_proofs_read_only_self_grants() {
        for authorization_source in [
            wire::InvocationAuthorizationSource::InteractiveSession,
            wire::InvocationAuthorizationSource::Unspecified,
        ] {
            let fixture = ServiceFixture::new(FixtureOptions {
                authorization_source,
                ..FixtureOptions::default()
            })
            .await;
            let response = call_over_product_mtls(fixture.service.clone(), fixture.proof_request())
                .await
                .expect("validated interactive Product proof reads its own grant");

            assert_eq!(response.principal_issuer, PRINCIPAL_ISSUER);
            assert_eq!(response.principal_subject, PRINCIPAL_SUBJECT);
            assert_eq!(response.workspace_id, WORKSPACE_ID);
            assert_eq!(fixture.validations.load(Ordering::Relaxed), 1);
            assert_eq!(fixture.verifier_calls.load(Ordering::Relaxed), 0);
            assert_eq!(fixture.session_calls.load(Ordering::Relaxed), 0);
        }
    }

    #[tokio::test]
    async fn interactive_session_proof_cannot_read_another_actors_grant() {
        let fixture = ServiceFixture::new(FixtureOptions {
            grant_subject: "actor-2",
            authorization_source: wire::InvocationAuthorizationSource::InteractiveSession,
            ..FixtureOptions::default()
        })
        .await;
        let status = call_over_product_mtls(fixture.service.clone(), fixture.proof_request())
            .await
            .expect_err("interactive proof cannot read another actor's grant");
        assert_eq!(status.code(), Code::NotFound);
        assert_eq!(fixture.validations.load(Ordering::Relaxed), 1);
        assert_eq!(fixture.verifier_calls.load(Ordering::Relaxed), 0);
        assert_eq!(fixture.session_calls.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn product_proof_rejects_an_unconfigured_mtls_profile() {
        let fixture = ServiceFixture::new(FixtureOptions {
            wrong_profile: true,
            ..FixtureOptions::default()
        })
        .await;
        let status = call_over_product_mtls(fixture.service.clone(), fixture.proof_request())
            .await
            .expect_err("unconfigured Product certificate is denied");
        assert_eq!(status.code(), Code::Unauthenticated);
        assert_eq!(fixture.validations.load(Ordering::Relaxed), 0);
        assert_eq!(fixture.verifier_calls.load(Ordering::Relaxed), 0);
        assert_eq!(fixture.session_calls.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn product_proof_cannot_read_a_delegation_owned_by_another_actor() {
        let fixture = ServiceFixture::new(FixtureOptions {
            grant_subject: "actor-2",
            ..FixtureOptions::default()
        })
        .await;
        let status = call_over_product_mtls(fixture.service.clone(), fixture.proof_request())
            .await
            .expect_err("an approved invocation cannot read another actor's grant");
        assert_eq!(status.code(), Code::PermissionDenied);
        assert_eq!(fixture.validations.load(Ordering::Relaxed), 1);
        assert_eq!(fixture.verifier_calls.load(Ordering::Relaxed), 0);
        assert_eq!(fixture.session_calls.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn product_proof_rejects_expiry_before_reading_the_outbox_claim() {
        let fixture = ServiceFixture::new(FixtureOptions {
            expired_credential: true,
            ..FixtureOptions::default()
        })
        .await;
        let status = call_over_product_mtls(fixture.service.clone(), fixture.proof_request())
            .await
            .expect_err("expired approved invocation proof is denied");
        assert_eq!(status.code(), Code::PermissionDenied);
        assert_eq!(fixture.validations.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn product_proof_rejects_tampering_and_never_combines_bearer_auth() {
        let fixture = ServiceFixture::new(FixtureOptions::default()).await;
        let mut tampered = fixture.proof_request();
        tampered
            .execution_credential
            .as_mut()
            .expect("proof is present")
            .request_digest_sha256[0] ^= 1;
        let status = call_over_product_mtls(fixture.service.clone(), tampered)
            .await
            .expect_err("tampered signed proof is denied");
        assert_eq!(status.code(), Code::PermissionDenied);
        assert_eq!(fixture.validations.load(Ordering::Relaxed), 0);

        let mut mixed_request = Request::new(fixture.proof_request());
        mixed_request.metadata_mut().insert(
            "authorization",
            "Bearer self"
                .parse()
                .expect("static bearer metadata is valid"),
        );
        let status = WorkspaceAuthorityService::get_delegation(&fixture.service, mixed_request)
            .await
            .expect_err("proof and bearer authentication are mutually exclusive");
        assert_eq!(status.code(), Code::InvalidArgument);
        assert_eq!(fixture.validations.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn product_proof_requires_the_outbox_active_claim() {
        let fixture = ServiceFixture::new(FixtureOptions {
            active_claim: false,
            ..FixtureOptions::default()
        })
        .await;
        let status = call_over_product_mtls(fixture.service.clone(), fixture.proof_request())
            .await
            .expect_err("invocation without an active claim is denied");
        assert_eq!(status.code(), Code::NotFound);
        assert_eq!(fixture.validations.load(Ordering::Relaxed), 1);
        assert_eq!(fixture.verifier_calls.load(Ordering::Relaxed), 0);
        assert_eq!(fixture.session_calls.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn legacy_bearer_read_remains_self_only() {
        let fixture = ServiceFixture::new(FixtureOptions::default()).await;
        let mut request = Request::new(wire::GetDelegationRequest {
            workspace_id: WORKSPACE_ID.to_owned(),
            delegation_id: fixture.delegation_id.to_string(),
            execution_credential: None,
        });
        request.metadata_mut().insert(
            "authorization",
            "Bearer other".parse().expect("valid metadata"),
        );
        let status = WorkspaceAuthorityService::get_delegation(&fixture.service, request)
            .await
            .expect_err("another authenticated actor cannot read the grant");
        assert_eq!(status.code(), Code::NotFound);
        assert_eq!(fixture.verifier_calls.load(Ordering::Relaxed), 1);
        assert_eq!(fixture.session_calls.load(Ordering::Relaxed), 1);
        assert_eq!(fixture.validations.load(Ordering::Relaxed), 0);
    }
}

#[cfg(test)]
mod connector_operation_request_tests {
    use tonic::Code;

    use crate::authority::{
        OFFICE_CALENDAR_OPERATOR_ONLY_CONNECTOR_OPERATION_IDS,
        OFFICE_MAIL_OPERATOR_ONLY_CONNECTOR_OPERATION_IDS,
        OFFICE_TASK_WRITER_CONNECTOR_OPERATION_IDS,
    };

    use super::*;

    const ISSUER: &str =
        "https://login.microsoftonline.com/11111111-1111-4111-8111-111111111111/v2.0";
    const SUBJECT: &str = "11111111-1111-4111-8111-111111111111";

    fn request() -> wire::AuthorizeConnectorOperationRequest {
        wire::AuthorizeConnectorOperationRequest {
            workspace_id: "workspace-1".to_owned(),
            principal_issuer: ISSUER.to_owned(),
            principal_subject: SUBJECT.to_owned(),
            connection_ref: "github-connection".to_owned(),
            provider_id: "github".to_owned(),
            profile_ref: "github-profile".to_owned(),
            operation_id: "github.repository.list".to_owned(),
            resource_ref: "repositories".to_owned(),
            arguments_digest_sha256: vec![0x5a; 32],
            request_id: "teams-activity-1".to_owned(),
        }
    }

    #[test]
    fn connector_request_requires_exact_deployment_workspace_and_verified_aad_identity() {
        assert!(validate_connector_operation_request(&request(), "workspace-1", ISSUER).is_ok());

        let mut altered = request();
        altered.workspace_id = "workspace-2".to_owned();
        assert_eq!(
            validate_connector_operation_request(&altered, "workspace-1", ISSUER)
                .expect_err("a caller cannot choose another Authority workspace")
                .code(),
            Code::PermissionDenied
        );

        let mut altered = request();
        altered.principal_issuer = "https://attacker.example/tenant/v2.0".to_owned();
        assert_eq!(
            validate_connector_operation_request(&altered, "workspace-1", ISSUER)
                .expect_err("issuer must match the configured tenant")
                .code(),
            Code::PermissionDenied
        );

        let mut altered = request();
        altered.principal_subject = "AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA".to_owned();
        assert_eq!(
            validate_connector_operation_request(&altered, "workspace-1", ISSUER)
                .expect_err("AAD object id must use canonical lowercase UUID encoding")
                .code(),
            Code::InvalidArgument
        );
    }

    #[test]
    fn connector_request_rejects_unlisted_or_mismatched_provider_operations() {
        let mut altered = request();
        altered.operation_id = "shell.execute".to_owned();
        assert_eq!(
            validate_connector_operation_request(&altered, "workspace-1", ISSUER)
                .expect_err("arbitrary shell operations are outside the catalog")
                .code(),
            Code::InvalidArgument
        );

        let mut altered = request();
        altered.provider_id = "azure".to_owned();
        assert_eq!(
            validate_connector_operation_request(&altered, "workspace-1", ISSUER)
                .expect_err("provider must match the catalog operation")
                .code(),
            Code::InvalidArgument
        );

        let mut altered = request();
        altered.arguments_digest_sha256.pop();
        assert_eq!(
            validate_connector_operation_request(&altered, "workspace-1", ISSUER)
                .expect_err("argument digest must be a raw SHA-256 value")
                .code(),
            Code::InvalidArgument
        );
    }

    #[test]
    fn connector_request_accepts_new_catalog_operations_and_rejects_cross_provider_pairs() {
        let mut github_rerun = request();
        github_rerun.operation_id = "github.actions.rerun_failed".into();
        assert!(validate_connector_operation_request(&github_rerun, "workspace-1", ISSUER).is_ok());

        let mut computer_command = request();
        computer_command.connection_ref = "computer-connection".into();
        computer_command.provider_id = "computer".into();
        computer_command.profile_ref = "computer-workspace".into();
        computer_command.operation_id = "computer.command.execute".into();
        computer_command.resource_ref = "workspace:workspace-1".into();
        assert!(
            validate_connector_operation_request(&computer_command, "workspace-1", ISSUER).is_ok()
        );

        let mut wrong_provider = computer_command.clone();
        wrong_provider.provider_id = "github".into();
        assert_eq!(
            validate_connector_operation_request(&wrong_provider, "workspace-1", ISSUER)
                .expect_err("computer operation must not be granted under github provider")
                .code(),
            Code::InvalidArgument
        );

        let mut unknown_operation = computer_command;
        unknown_operation.operation_id = "computer.shell.execute".into();
        assert_eq!(
            validate_connector_operation_request(&unknown_operation, "workspace-1", ISSUER)
                .expect_err("unlisted computer operations must remain denied")
                .code(),
            Code::InvalidArgument
        );
    }

    #[test]
    fn connector_request_accepts_office_mail_catalog_and_rejects_unlisted_mail_actions() {
        for operation_id in OFFICE_TASK_WRITER_CONNECTOR_OPERATION_IDS
            .into_iter()
            .chain(OFFICE_CALENDAR_OPERATOR_ONLY_CONNECTOR_OPERATION_IDS)
            .chain(OFFICE_MAIL_OPERATOR_ONLY_CONNECTOR_OPERATION_IDS)
        {
            let mut office_request = request();
            office_request.connection_ref = "graph-connection".to_owned();
            office_request.provider_id = "microsoft_graph".to_owned();
            office_request.profile_ref = "graph-profile".to_owned();
            office_request.operation_id = operation_id.to_owned();
            office_request.resource_ref = format!("sha256:{}", "a".repeat(64));
            assert!(
                validate_connector_operation_request(&office_request, "workspace-1", ISSUER)
                    .is_ok(),
                "supported Office operation {operation_id} must pass the RPC request gate"
            );
        }

        for operation_id in [
            "microsoft_graph.mail.message.body.get",
            "microsoft_graph.mail.message.permanent_delete",
            "microsoft_graph.mail.credentials.export",
        ] {
            let mut mail_request = request();
            mail_request.provider_id = "microsoft_graph".to_owned();
            mail_request.operation_id = operation_id.to_owned();
            assert_eq!(
                validate_connector_operation_request(&mail_request, "workspace-1", ISSUER)
                    .expect_err("unlisted Graph Mail actions must remain outside the catalog")
                    .code(),
                Code::InvalidArgument,
                "Graph Mail operation {operation_id} must remain unsupported"
            );
        }
    }
}
