//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 relay.rs                                                        │
//! │  Module: cy_workspace_fabric::relay                                 │
//! │  Role: Stateless application-level Workspace request relay.         │
//! │                                                                     │
//! │  模块职责：无 Workspace 状态权威的应用层请求中继。                       │
//! └─────────────────────────────────────────────────────────────────────┘

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use cy_proto::google::rpc::Status as RpcStatus;
use cy_proto::workspace_v1::relay_frame;
use cy_proto::workspace_v1::workspace_api_response;
use cy_proto::workspace_v1::workspace_relay_service_server::WorkspaceRelayService;
use cy_proto::workspace_v1::{
    DiscoverWorkspacesResponse, RelayFrame, RelayHello, RelayParticipantRole, RelayReady,
    WorkspaceApiResponse,
};
use futures_util::StreamExt as FuturesStreamExt;
use tokio::sync::{mpsc, watch};
use tokio_stream::{wrappers::ReceiverStream, Stream};
use tonic::{Request, Response, Status, Streaming};

use crate::relay_peer_certificate_validation::{
    AuthenticatedRelayWorkspaceDevice, RelayPeerCertificateError,
    RelayPeerCertificateRevocationChecker, RelayPeerCertificateValidator,
    TonicPeerCertificateChain,
};
use crate::{
    AcaForwardedBffWorkloadCertificateAdapter, AcaForwardedCertificateAdapter,
    RegistryWorkspaceDeviceVerifier, RelayAuthenticator, RelaySessionClaims, SessionPrincipal,
    TonicBffWorkloadCertificateAdapter, VerifiedClientCertificate, WorkspaceCallerContext,
    WorkspaceDeviceAuthenticationError, WorkspaceDeviceCertificateIdentity,
    WorkspaceDeviceDispatchFence, WorkspaceDeviceRegistry, WorkspaceDirectory,
};

type RelayStream = Pin<Box<dyn Stream<Item = Result<RelayFrame, Status>> + Send + 'static>>;
type RelaySender = mpsc::Sender<Result<RelayFrame, Status>>;
// Bound each participant's queued frames so a slow reader blocks its upstream peer.
// 每条连接限制排队帧数，让慢速消费端向上游施加背压。
const RELAY_QUEUE_FRAMES: usize = 8;
const MAX_PENDING_REQUESTS: usize = 4096;
const MAX_REQUEST_ID_BYTES: usize = 128;
const PENDING_REQUEST_TTL: Duration = Duration::from_secs(35);

#[derive(Clone)]
struct RegisteredConnection {
    relay_session_id: String,
    sender: RelaySender,
    authenticated_device: Option<AuthenticatedRelayWorkspaceDevice>,
    peer_certificate_chain: Option<TonicPeerCertificateChain>,
    session_cancel: Option<watch::Sender<bool>>,
    dispatch_gate: Arc<tokio::sync::Mutex<()>>,
}

struct AuthenticatedWorkspacePeer {
    device: AuthenticatedRelayWorkspaceDevice,
    certificate_chain: TonicPeerCertificateChain,
}

struct WorkspaceDispatch {
    permit: mpsc::OwnedPermit<Result<RelayFrame, Status>>,
    session_cancel: watch::Sender<bool>,
    relay_session_id: String,
    _dispatch_guard: tokio::sync::OwnedMutexGuard<()>,
    _registry_dispatch_fence: Option<Box<dyn WorkspaceDeviceDispatchFence>>,
}

impl WorkspaceDispatch {
    /// Enqueue synchronously after revalidation so no await can widen the authorization window.
    fn send(self, frame: RelayFrame) -> Result<(), ()> {
        if *self.session_cancel.borrow() {
            return Err(());
        }
        self.permit.send(Ok(frame));
        Ok(())
    }
}

struct PendingRequest {
    workspace_session_id: String,
    expires_at: Instant,
}

#[derive(Default)]
struct Connections {
    frontends: BTreeMap<String, RegisteredConnection>,
    workspaces: BTreeMap<String, RegisteredConnection>,
    pending: BTreeMap<(String, String), PendingRequest>,
}

struct RelayState {
    directory: Arc<dyn WorkspaceDirectory>,
    authenticator: Arc<dyn RelayAuthenticator>,
    workspace_device_verifier: Option<Arc<RegistryWorkspaceDeviceVerifier>>,
    workspace_device_registry: Option<Arc<dyn WorkspaceDeviceRegistry>>,
    workspace_peer_certificate_validator: Option<Arc<RelayPeerCertificateValidator>>,
    workspace_peer_revocation_checker: Option<Arc<dyn RelayPeerCertificateRevocationChecker>>,
    aca_forwarded_certificate_adapter: Option<AcaForwardedCertificateAdapter>,
    aca_forwarded_bff_workload_certificate_adapter:
        Option<AcaForwardedBffWorkloadCertificateAdapter>,
    tonic_bff_workload_certificate_adapter: Option<TonicBffWorkloadCertificateAdapter>,
    connections: Mutex<Connections>,
    next_session: AtomicU64,
}

/// Application relay that routes frames without owning Workspace state.
#[derive(Clone)]
pub struct WorkspaceRelay {
    state: Arc<RelayState>,
}

impl WorkspaceRelay {
    /// Build a Relay that authenticates Frontend sessions and denies Workspace connectors.
    pub fn new(
        directory: Arc<dyn WorkspaceDirectory>,
        authenticator: Arc<dyn RelayAuthenticator>,
    ) -> Self {
        Self::build(directory, authenticator, None, None, None, None, None)
    }

    /// Compatibility constructor that keeps Workspace connectors disabled.
    ///
    /// A basic fingerprint lookup cannot prove the private certificate profile,
    /// current Directory generation, or fresh signed revocation status.
    pub fn with_workspace_device_registry(
        directory: Arc<dyn WorkspaceDirectory>,
        authenticator: Arc<dyn RelayAuthenticator>,
        _registry: Arc<dyn WorkspaceDeviceRegistry>,
    ) -> Self {
        Self::build(directory, authenticator, None, None, None, None, None)
    }

    /// Compatibility constructor that keeps Workspace connectors disabled.
    ///
    /// XFCC is not an authenticated peer fact by itself. A trusted ingress
    /// adapter, certificate validator, revocation checker, and current Registry
    /// binding lookup must all be composed before connector authentication is enabled.
    pub fn with_aca_forwarded_certificate_adapter(
        directory: Arc<dyn WorkspaceDirectory>,
        authenticator: Arc<dyn RelayAuthenticator>,
        _registry: Arc<dyn WorkspaceDeviceRegistry>,
        adapter: AcaForwardedCertificateAdapter,
    ) -> Self {
        Self::build(
            directory,
            authenticator,
            None,
            Some(adapter),
            None,
            None,
            None,
        )
    }

    /// Construct an ACA Relay that applies separate Frontend workload and device trust.
    ///
    /// The Frontend branch validates the dedicated BFF service CA and exact certificate pin
    /// from this RPC's ACA-overwritten XFCC before validating the signed user handoff. The
    /// WorkspaceConnector authentication remains disabled by this compatibility constructor.
    ///
    /// 为 ACA Relay 配置 Frontend workload 信任；Connector 认证保持关闭。
    pub fn with_aca_forwarded_certificate_adapters(
        directory: Arc<dyn WorkspaceDirectory>,
        authenticator: Arc<dyn RelayAuthenticator>,
        _registry: Arc<dyn WorkspaceDeviceRegistry>,
        device_adapter: AcaForwardedCertificateAdapter,
        frontend_workload_adapter: AcaForwardedBffWorkloadCertificateAdapter,
    ) -> Self {
        Self::build(
            directory,
            authenticator,
            None,
            Some(device_adapter),
            Some(frontend_workload_adapter),
            None,
            None,
        )
    }

    /// Construct an ACA Relay with Frontend workload trust while denying Connectors.
    ///
    /// Use this until the durable Workspace device registry and certificate
    /// authorization adapter are composed. It validates BFF XFCC before the
    /// signed Frontend handoff but does not accept a WorkspaceConnector.
    ///
    /// 配置 ACA Frontend workload 信任，同时拒绝 Connector；持久设备注册表接通前使用。
    pub fn with_aca_forwarded_frontend_certificate_adapter(
        directory: Arc<dyn WorkspaceDirectory>,
        authenticator: Arc<dyn RelayAuthenticator>,
        frontend_workload_adapter: AcaForwardedBffWorkloadCertificateAdapter,
    ) -> Self {
        Self::build(
            directory,
            authenticator,
            None,
            None,
            Some(frontend_workload_adapter),
            None,
            None,
        )
    }

    /// Enable the Tonic TLS peer path only when all trusted providers are present.
    ///
    /// The Registry must verify current binding/generation, the validator must
    /// verify the private certificate profile and chain, and the revocation
    /// checker must return fresh signed good-status evidence. Production startup
    /// does not call this until those providers are configured.
    #[allow(dead_code)] // Production host remains disabled until trust providers are available.
    pub(crate) fn with_tonic_workspace_peer_validation(
        directory: Arc<dyn WorkspaceDirectory>,
        authenticator: Arc<dyn RelayAuthenticator>,
        registry: Arc<dyn WorkspaceDeviceRegistry>,
        validator: RelayPeerCertificateValidator,
        revocation_checker: Arc<dyn RelayPeerCertificateRevocationChecker>,
    ) -> Self {
        let verifier = Arc::new(RegistryWorkspaceDeviceVerifier::new(registry.clone()));
        let mut relay = Self::build(
            directory,
            authenticator,
            Some(verifier),
            None,
            None,
            Some(Arc::new(validator)),
            Some(revocation_checker),
        );
        Arc::get_mut(&mut relay.state)
            .expect("new Relay state is uniquely owned")
            .workspace_device_registry = Some(registry);
        relay
    }

    /// Construct a native mTLS Relay with independent BFF and Workspace-device trust.
    ///
    /// Frontend identities are verified from Tonic TLS peer certificates against the dedicated
    /// BFF CA and exact pin set. Connector identities use the device CA, fresh revocation proof,
    /// and current registry binding. ACA/XFCC adapters are not enabled in this mode.
    pub fn with_tonic_bff_frontend_certificate_adapter(
        directory: Arc<dyn WorkspaceDirectory>,
        authenticator: Arc<dyn RelayAuthenticator>,
        registry: Arc<dyn WorkspaceDeviceRegistry>,
        device_validator: RelayPeerCertificateValidator,
        peer_revocation_checker: Arc<dyn RelayPeerCertificateRevocationChecker>,
        bff_adapter: TonicBffWorkloadCertificateAdapter,
    ) -> Self {
        let verifier = Arc::new(RegistryWorkspaceDeviceVerifier::new(registry.clone()));
        let mut relay = Self::build(
            directory,
            authenticator,
            Some(verifier),
            None,
            None,
            Some(Arc::new(device_validator)),
            Some(peer_revocation_checker),
        );
        let state = Arc::get_mut(&mut relay.state).expect("new Relay state is uniquely owned");
        state.workspace_device_registry = Some(registry);
        state.tonic_bff_workload_certificate_adapter = Some(bff_adapter);
        relay
    }

    fn build(
        directory: Arc<dyn WorkspaceDirectory>,
        authenticator: Arc<dyn RelayAuthenticator>,
        workspace_device_verifier: Option<Arc<RegistryWorkspaceDeviceVerifier>>,
        aca_forwarded_certificate_adapter: Option<AcaForwardedCertificateAdapter>,
        aca_forwarded_bff_workload_certificate_adapter: Option<
            AcaForwardedBffWorkloadCertificateAdapter,
        >,
        workspace_peer_certificate_validator: Option<Arc<RelayPeerCertificateValidator>>,
        workspace_peer_revocation_checker: Option<Arc<dyn RelayPeerCertificateRevocationChecker>>,
    ) -> Self {
        Self {
            state: Arc::new(RelayState {
                directory,
                authenticator,
                workspace_device_verifier,
                workspace_device_registry: None,
                workspace_peer_certificate_validator,
                workspace_peer_revocation_checker,
                aca_forwarded_certificate_adapter,
                aca_forwarded_bff_workload_certificate_adapter,
                tonic_bff_workload_certificate_adapter: None,
                connections: Mutex::new(Connections::default()),
                next_session: AtomicU64::new(1),
            }),
        }
    }

    fn native_xfcc_metadata_must_be_rejected(
        &self,
        role: RelayParticipantRole,
        has_xfcc: bool,
    ) -> bool {
        if !has_xfcc {
            return false;
        }
        let native_peer_mode = self.state.tonic_bff_workload_certificate_adapter.is_some()
            || self.state.workspace_peer_certificate_validator.is_some()
            || self.state.workspace_peer_revocation_checker.is_some()
            || self.state.workspace_device_verifier.is_some();
        let aca_frontend_mode = self.state.aca_forwarded_certificate_adapter.is_some()
            || self
                .state
                .aca_forwarded_bff_workload_certificate_adapter
                .is_some();
        native_peer_mode && !(role == RelayParticipantRole::Frontend && aca_frontend_mode)
    }

    /// Route user sessions and device certificates through separate authorities.
    fn authenticate_participant(
        &self,
        role: RelayParticipantRole,
        hello: &RelayHello,
        peer_certificate: Result<VerifiedClientCertificate, WorkspaceDeviceAuthenticationError>,
        frontend_metadata_request: Option<&Request<()>>,
        tonic_peer_chain: Option<&Result<TonicPeerCertificateChain, RelayPeerCertificateError>>,
        now_unix_ms: u64,
    ) -> Result<RelaySessionClaims, Box<Status>> {
        match role {
            RelayParticipantRole::Frontend => {
                let workload_identity = if let Some(adapter) = self
                    .state
                    .aca_forwarded_bff_workload_certificate_adapter
                    .as_ref()
                {
                    let request = frontend_metadata_request.ok_or_else(|| {
                        Box::new(Status::unauthenticated(
                            "BFF_WORKLOAD_TLS_CERTIFICATE_REQUIRED",
                        ))
                    })?;
                    Some(
                        adapter
                            .authenticate_request(request, hello, now_unix_ms)
                            .map_err(|_| {
                                Box::new(Status::unauthenticated(
                                    "BFF_WORKLOAD_CERTIFICATE_INVALID",
                                ))
                            })?,
                    )
                } else if let Some(adapter) =
                    self.state.tonic_bff_workload_certificate_adapter.as_ref()
                {
                    let request = frontend_metadata_request.ok_or_else(|| {
                        Box::new(Status::unauthenticated(
                            "BFF_WORKLOAD_TLS_CERTIFICATE_REQUIRED",
                        ))
                    })?;
                    if request.metadata().contains_key("x-forwarded-client-cert") {
                        return Err(Box::new(Status::unauthenticated(
                            "BFF_WORKLOAD_XFCC_NOT_ALLOWED_IN_NATIVE_MODE",
                        )));
                    }
                    let chain = tonic_peer_chain
                        .ok_or_else(|| {
                            Box::new(Status::unauthenticated(
                                "BFF_WORKLOAD_TLS_CERTIFICATE_REQUIRED",
                            ))
                        })?
                        .as_ref()
                        .map_err(|_| {
                            Box::new(Status::unauthenticated(
                                "BFF_WORKLOAD_TLS_CERTIFICATE_REQUIRED",
                            ))
                        })?;
                    Some(
                        adapter
                            .authenticate_peer_certificate_chain(
                                chain.leaf_der(),
                                chain.intermediate_chain_der(),
                                hello,
                                now_unix_ms,
                            )
                            .map_err(|_| {
                                Box::new(Status::unauthenticated(
                                    "BFF_WORKLOAD_CERTIFICATE_INVALID",
                                ))
                            })?,
                    )
                } else if self.state.aca_forwarded_certificate_adapter.is_some() {
                    return Err(Box::new(Status::unauthenticated(
                        "BFF_WORKLOAD_CERTIFICATE_AUTHENTICATION_NOT_CONFIGURED",
                    )));
                } else {
                    None
                };
                let claims = self
                    .state
                    .authenticator
                    .authenticate(hello, now_unix_ms)
                    .map_err(|error| Box::new(Status::unauthenticated(error.to_string())))?;
                if let (Some(workload_identity), SessionPrincipal::User(user)) =
                    (workload_identity, &claims.principal)
                {
                    tracing::info!(
                        event.name = "platform.relay.frontend_authentication_succeeded",
                        workload_certificate_sha256 = %workload_identity.fingerprint_sha256(),
                        principal_issuer = %user.issuer,
                        principal_subject = %user.subject,
                        organization_id = %claims.organization_id,
                        message = "Relay authenticated the BFF workload and signed Frontend principal",
                    );
                }
                Ok(claims)
            }
            RelayParticipantRole::WorkspaceConnector => {
                let _ = (hello, now_unix_ms, frontend_metadata_request);
                match peer_certificate {
                    Err(error) => Err(Box::new(Status::unauthenticated(error.to_string()))),
                    Ok(_) => Err(Box::new(Status::unauthenticated(
                        "WORKSPACE_DEVICE_PEER_CERTIFICATE_VALIDATION_NOT_CONFIGURED",
                    ))),
                }
            }
            RelayParticipantRole::Unspecified => Err(Box::new(Status::invalid_argument(
                "relay participant role is required",
            ))),
        }
    }

    fn authenticate_tonic_workspace_device(
        &self,
        hello: &RelayHello,
        chain: Result<TonicPeerCertificateChain, RelayPeerCertificateError>,
        now_unix_ms: u64,
    ) -> Result<(RelaySessionClaims, AuthenticatedWorkspacePeer), Box<Status>> {
        let validator = self
            .state
            .workspace_peer_certificate_validator
            .as_ref()
            .ok_or_else(|| {
                Box::new(Status::unauthenticated(
                    "WORKSPACE_DEVICE_PEER_CERTIFICATE_VALIDATION_NOT_CONFIGURED",
                ))
            })?;
        let revocation_checker = self
            .state
            .workspace_peer_revocation_checker
            .as_ref()
            .ok_or_else(|| {
                Box::new(Status::unauthenticated(
                    "WORKSPACE_DEVICE_CERTIFICATE_REVOCATION_NOT_CONFIGURED",
                ))
            })?;
        let registry_verifier = self
            .state
            .workspace_device_verifier
            .as_ref()
            .ok_or_else(|| {
                Box::new(Status::unauthenticated(
                    "WORKSPACE_DEVICE_REGISTRY_CURRENT_BINDING_NOT_CONFIGURED",
                ))
            })?;
        let chain = chain.map_err(|error| Box::new(Status::unauthenticated(error.to_string())))?;
        let certificate = validator
            .validate_tonic_peer_chain(&chain, now_unix_ms, revocation_checker.as_ref())
            .map_err(|error| Box::new(Status::unauthenticated(error.to_string())))?;
        let (claims, authenticated_device) = registry_verifier
            .authenticate_validated_peer(hello, &certificate, now_unix_ms)
            .map_err(|error| Box::new(Status::unauthenticated(error.to_string())))?;
        Ok((
            claims,
            AuthenticatedWorkspacePeer {
                device: authenticated_device,
                certificate_chain: chain,
            },
        ))
    }

    fn next_session(&self, prefix: &str) -> String {
        let sequence = self.state.next_session.fetch_add(1, Ordering::Relaxed);
        format!("{prefix}-{sequence}")
    }

    fn revalidate_workspace_connection(
        &self,
        workspace_id: &str,
        workspace: &RegisteredConnection,
        now_unix_ms: u64,
    ) -> Result<(), WorkspaceDeviceAuthenticationError> {
        #[cfg(test)]
        if workspace.authenticated_device.is_none()
            && workspace.peer_certificate_chain.is_none()
            && workspace.session_cancel.is_some()
            && self.state.workspace_device_verifier.is_none()
            && self.state.workspace_peer_certificate_validator.is_none()
            && self.state.workspace_peer_revocation_checker.is_none()
        {
            // Unit routing fixtures do not represent a transport-authenticated Connector.
            return Ok(());
        }

        let authenticated_device = workspace
            .authenticated_device
            .as_ref()
            .ok_or(WorkspaceDeviceAuthenticationError::RegistryUnavailable)?;
        if authenticated_device.key().workspace_id != workspace_id {
            return Err(WorkspaceDeviceAuthenticationError::IdentityMismatch);
        }
        let chain = workspace
            .peer_certificate_chain
            .as_ref()
            .ok_or(WorkspaceDeviceAuthenticationError::MissingClientCertificate)?;
        let validator = self
            .state
            .workspace_peer_certificate_validator
            .as_ref()
            .ok_or(WorkspaceDeviceAuthenticationError::InvalidClientCertificate)?;
        let revocation_checker = self
            .state
            .workspace_peer_revocation_checker
            .as_ref()
            .ok_or(WorkspaceDeviceAuthenticationError::RevocationStatusUnknown)?;
        let registry_verifier = self
            .state
            .workspace_device_verifier
            .as_ref()
            .ok_or(WorkspaceDeviceAuthenticationError::RegistryUnavailable)?;

        let certificate = validator
            .validate_tonic_peer_chain(chain, now_unix_ms, revocation_checker.as_ref())
            .map_err(|_| WorkspaceDeviceAuthenticationError::RevocationStatusUnknown)?;
        registry_verifier.revalidate_authenticated_peer(
            authenticated_device,
            &certificate,
            now_unix_ms,
        )
    }

    fn invalidate_workspace_connection(
        &self,
        workspace_id: &str,
        workspace: &RegisteredConnection,
    ) -> Result<(), ()> {
        let mut connections = self.state.connections.lock().map_err(|_| ())?;
        let registered = connections
            .workspaces
            .get(workspace_id)
            .is_some_and(|current| current.relay_session_id == workspace.relay_session_id);
        if registered {
            connections.workspaces.remove(workspace_id);
        }
        connections
            .pending
            .retain(|_, pending| pending.workspace_session_id != workspace.relay_session_id);
        drop(connections);

        if let Some(session_cancel) = &workspace.session_cancel {
            session_cancel.send_replace(true);
        }
        Ok(())
    }

    /// Revalidate the retained peer chain and current Registry binding before the final fence.
    ///
    /// Certificate and external revocation checks run before the transaction guard is acquired.
    /// The strict Registry check then holds the exact identity row through the synchronous local
    /// queue admission, without carrying that guard across network I/O or an async wait.
    ///
    /// 每次派发先完成证书及外部撤销检查，再取得 Registry 行锁 guard；guard 只覆盖本地同步队列入队。
    async fn register_pending(
        &self,
        workspace_id: &str,
        frontend_session_id: &str,
        request_id: &str,
    ) -> Result<Option<WorkspaceDispatch>, (i32, &'static str)> {
        if request_id.is_empty() || request_id.len() > MAX_REQUEST_ID_BYTES {
            return Err((3, "WORKSPACE_REQUEST_ID_INVALID"));
        }
        let key = (frontend_session_id.to_string(), request_id.to_string());
        let workspace = {
            let mut connections = self
                .state
                .connections
                .lock()
                .map_err(|_| (13, "RELAY_CONNECTION_STATE_POISONED"))?;
            connections
                .pending
                .retain(|_, pending| pending.expires_at > Instant::now());
            if connections.pending.contains_key(&key) {
                return Err((3, "WORKSPACE_REQUEST_ID_INVALID"));
            }
            if connections.pending.len() >= MAX_PENDING_REQUESTS {
                return Err((8, "RELAY_PENDING_REQUEST_LIMIT"));
            }
            let Some(workspace) = connections.workspaces.get(workspace_id).cloned() else {
                return Ok(None);
            };
            workspace
        };

        let Some(session_cancel) = workspace.session_cancel.as_ref() else {
            let _ = self.invalidate_workspace_connection(workspace_id, &workspace);
            return Err((7, "WORKSPACE_DEVICE_SESSION_AUTHORIZATION_STALE"));
        };
        if *session_cancel.borrow() {
            let _ = self.invalidate_workspace_connection(workspace_id, &workspace);
            return Err((7, "WORKSPACE_DEVICE_SESSION_AUTHORIZATION_STALE"));
        }
        let dispatch_deadline = tokio::time::Instant::now() + PENDING_REQUEST_TTL;
        let dispatch_guard = match tokio::time::timeout_at(
            dispatch_deadline,
            workspace.dispatch_gate.clone().lock_owned(),
        )
        .await
        {
            Ok(guard) => guard,
            Err(_) => return Err((14, "WORKSPACE_CONNECTOR_BACKPRESSURE")),
        };
        if *session_cancel.borrow() {
            let _ = self.invalidate_workspace_connection(workspace_id, &workspace);
            return Err((7, "WORKSPACE_DEVICE_SESSION_AUTHORIZATION_STALE"));
        }

        // Reserve queue capacity before checking trust, so a slow Connector cannot
        // leave a previously valid authorization waiting in an async send queue.
        let permit = match tokio::time::timeout_at(
            dispatch_deadline,
            workspace.sender.clone().reserve_owned(),
        )
        .await
        {
            Ok(Ok(permit)) => permit,
            Ok(Err(_)) => {
                let _ = self.invalidate_workspace_connection(workspace_id, &workspace);
                return Ok(None);
            }
            Err(_) => return Err((14, "WORKSPACE_CONNECTOR_BACKPRESSURE")),
        };

        // Directory/registry and revocation adapters are synchronous ports. Keep their I/O off
        // the async executor, with no connection-map lock held across this blocking check.
        let relay = self.clone();
        let connection = workspace.clone();
        let workspace_id_for_check = workspace_id.to_string();
        let validation = tokio::task::spawn_blocking(move || {
            relay.revalidate_workspace_connection(
                &workspace_id_for_check,
                &connection,
                now_unix_ms(),
            )?;

            #[cfg(test)]
            if connection.authenticated_device.is_none()
                && connection.peer_certificate_chain.is_none()
                && connection.session_cancel.is_some()
                && relay.state.workspace_device_verifier.is_none()
                && relay.state.workspace_peer_certificate_validator.is_none()
                && relay.state.workspace_peer_revocation_checker.is_none()
            {
                // Test-only route fixtures have no authenticated device or durable Registry.
                return Ok(None);
            }

            let authenticated_device = connection
                .authenticated_device
                .as_ref()
                .ok_or(WorkspaceDeviceAuthenticationError::RegistryUnavailable)?;
            let registry = relay
                .state
                .workspace_device_registry
                .as_ref()
                .ok_or(WorkspaceDeviceAuthenticationError::RegistryUnavailable)?;
            let expected = registry_identity_for_authenticated_device(authenticated_device);
            registry
                .acquire_relay_dispatch_fence(&expected)
                .map(Some)
                .map_err(|_| WorkspaceDeviceAuthenticationError::RegistryUnavailable)
        })
        .await;
        let registry_dispatch_fence = match validation {
            Ok(Ok(fence)) => fence,
            _ => {
                let _ = self.invalidate_workspace_connection(workspace_id, &workspace);
                tracing::warn!(
                    event.name = "platform.relay.workspace_session_invalidated",
                    error.code = "PLATFORM.RELAY.WORKSPACE_SESSION_AUTHORIZATION_STALE",
                    session_id = %workspace.relay_session_id,
                    workspace_id = %workspace_id,
                    message = "Relay removed a Connector session after current authorization revalidation or dispatch fencing failed",
                );
                return Err((7, "WORKSPACE_DEVICE_SESSION_AUTHORIZATION_STALE"));
            }
        };

        let session_cancel = workspace
            .session_cancel
            .as_ref()
            .ok_or((7, "WORKSPACE_DEVICE_SESSION_AUTHORIZATION_STALE"))?
            .clone();
        let mut connections = self
            .state
            .connections
            .lock()
            .map_err(|_| (13, "RELAY_CONNECTION_STATE_POISONED"))?;
        connections
            .pending
            .retain(|_, pending| pending.expires_at > Instant::now());
        let is_current_session = connections
            .workspaces
            .get(workspace_id)
            .is_some_and(|current| current.relay_session_id == workspace.relay_session_id);
        if !is_current_session || *session_cancel.borrow() {
            return Ok(None);
        }
        if connections.pending.contains_key(&key) {
            return Err((3, "WORKSPACE_REQUEST_ID_INVALID"));
        }
        if connections.pending.len() >= MAX_PENDING_REQUESTS {
            return Err((8, "RELAY_PENDING_REQUEST_LIMIT"));
        }
        connections.pending.insert(
            key,
            PendingRequest {
                workspace_session_id: workspace.relay_session_id.clone(),
                expires_at: Instant::now() + PENDING_REQUEST_TTL,
            },
        );
        Ok(Some(WorkspaceDispatch {
            permit,
            session_cancel,
            relay_session_id: workspace.relay_session_id,
            _dispatch_guard: dispatch_guard,
            _registry_dispatch_fence: registry_dispatch_fence,
        }))
    }

    /// Fence the synchronous enqueue against session replacement and pending-route cleanup.
    ///
    /// The map lock is held only across local ID checks and the bounded-channel permit send; no
    /// network or database work runs under this lock.
    fn send_workspace_dispatch(
        &self,
        workspace_id: &str,
        frontend_session_id: &str,
        request_id: &str,
        dispatch: WorkspaceDispatch,
        frame: RelayFrame,
    ) -> Result<(), (i32, &'static str)> {
        let connections = self
            .state
            .connections
            .lock()
            .map_err(|_| (13, "RELAY_CONNECTION_STATE_POISONED"))?;
        let is_current_session = connections
            .workspaces
            .get(workspace_id)
            .is_some_and(|current| current.relay_session_id == dispatch.relay_session_id);
        let has_pending_route = connections
            .pending
            .get(&(frontend_session_id.to_string(), request_id.to_string()))
            .is_some_and(|pending| pending.workspace_session_id == dispatch.relay_session_id);
        if !is_current_session || !has_pending_route {
            return Err((7, "WORKSPACE_DEVICE_SESSION_AUTHORIZATION_STALE"));
        }
        dispatch
            .send(frame)
            .map_err(|_| (7, "WORKSPACE_DEVICE_SESSION_AUTHORIZATION_STALE"))
    }

    fn take_response_sender(
        &self,
        workspace_session_id: &str,
        frontend_session_id: &str,
        request_id: &str,
    ) -> Result<Option<RelaySender>, ()> {
        let mut connections = self.state.connections.lock().map_err(|_| ())?;
        let key = (frontend_session_id.to_string(), request_id.to_string());
        let valid = connections.pending.get(&key).is_some_and(|pending| {
            pending.workspace_session_id == workspace_session_id
                && pending.expires_at > Instant::now()
        });
        if !valid {
            return Ok(None);
        }
        connections.pending.remove(&key);
        Ok(connections
            .frontends
            .get(frontend_session_id)
            .map(|connection| connection.sender.clone()))
    }

    fn clear_pending(
        &self,
        frontend_session_id: &str,
        request_id: &str,
        workspace_session_id: &str,
    ) {
        if let Ok(mut connections) = self.state.connections.lock() {
            let key = (frontend_session_id.to_string(), request_id.to_string());
            let belongs_to_session = connections
                .pending
                .get(&key)
                .is_some_and(|pending| pending.workspace_session_id == workspace_session_id);
            if belongs_to_session {
                connections.pending.remove(&key);
            }
        }
    }
}

#[tonic::async_trait]
impl WorkspaceRelayService for WorkspaceRelay {
    type ConnectStream = RelayStream;

    async fn connect(
        &self,
        request: Request<Streaming<RelayFrame>>,
    ) -> Result<Response<Self::ConnectStream>, Status> {
        let tonic_peer_chain = TonicPeerCertificateChain::from_request(&request);
        let has_xfcc = request.metadata().contains_key("x-forwarded-client-cert");
        let has_aca_forwarded_certificate_adapter =
            self.state.aca_forwarded_certificate_adapter.is_some()
                || self
                    .state
                    .aca_forwarded_bff_workload_certificate_adapter
                    .is_some();
        let needs_frontend_metadata = has_aca_forwarded_certificate_adapter
            || self.state.tonic_bff_workload_certificate_adapter.is_some();
        let frontend_metadata_request = needs_frontend_metadata.then(|| {
            let mut metadata_request = Request::new(());
            *metadata_request.metadata_mut() = request.metadata().clone();
            metadata_request
        });
        let mut inbound = request.into_inner();
        let first = tokio::time::timeout(Duration::from_secs(5), inbound.message())
            .await
            .map_err(|_| Status::deadline_exceeded("relay hello timed out"))??
            .ok_or_else(|| Status::invalid_argument("relay hello is required"))?;
        let Some(relay_frame::Body::Hello(hello)) = first.body else {
            return Err(Status::invalid_argument("first relay frame must be hello"));
        };
        let role = RelayParticipantRole::try_from(hello.role)
            .map_err(|_| Status::invalid_argument("unknown relay participant role"))?;
        if self.native_xfcc_metadata_must_be_rejected(role, has_xfcc) {
            return Err(Status::unauthenticated(
                "RELAY_XFCC_NOT_ALLOWED_IN_NATIVE_MODE",
            ));
        }
        let now = now_unix_ms();
        let (claims, authenticated_workspace_peer) = match role {
            RelayParticipantRole::Frontend => (
                self.authenticate_participant(
                    role,
                    &hello,
                    Err(WorkspaceDeviceAuthenticationError::MissingClientCertificate),
                    frontend_metadata_request.as_ref(),
                    Some(&tonic_peer_chain),
                    now,
                )
                .map_err(|status| *status)?,
                None,
            ),
            RelayParticipantRole::WorkspaceConnector => {
                let (claims, authenticated_workspace_peer) = self
                    .authenticate_tonic_workspace_device(&hello, tonic_peer_chain, now)
                    .map_err(|status| *status)?;
                (claims, Some(authenticated_workspace_peer))
            }
            RelayParticipantRole::Unspecified => {
                return Err(Status::invalid_argument(
                    "relay participant role is required",
                ));
            }
        };
        let (sender, receiver) = mpsc::channel(RELAY_QUEUE_FRAMES);
        let mut workspace_session_cancel_receiver = None;
        match role {
            RelayParticipantRole::Frontend => {
                self.open_frontend(claims, inbound, sender).await?;
            }
            RelayParticipantRole::WorkspaceConnector => {
                let authenticated_workspace_peer =
                    authenticated_workspace_peer.ok_or_else(|| {
                        Status::unauthenticated(
                            "WORKSPACE_DEVICE_REGISTRY_CURRENT_BINDING_NOT_CONFIGURED",
                        )
                    })?;
                let (session_cancel, session_cancel_receiver) = watch::channel(false);
                self.open_workspace(
                    hello,
                    claims,
                    authenticated_workspace_peer,
                    inbound,
                    sender,
                    session_cancel,
                )
                .await?;
                workspace_session_cancel_receiver = Some(session_cancel_receiver);
            }
            RelayParticipantRole::Unspecified => {
                return Err(Status::invalid_argument(
                    "relay participant role is required",
                ));
            }
        }
        let stream: RelayStream =
            if let Some(mut session_cancel) = workspace_session_cancel_receiver {
                Box::pin(ReceiverStream::new(receiver).take_until(async move {
                    if !*session_cancel.borrow() {
                        let _ = session_cancel.changed().await;
                    }
                }))
            } else {
                Box::pin(ReceiverStream::new(receiver))
            };
        Ok(Response::new(stream))
    }
}

impl WorkspaceRelay {
    async fn open_frontend(
        &self,
        claims: RelaySessionClaims,
        mut inbound: Streaming<RelayFrame>,
        sender: RelaySender,
    ) -> Result<(), Status> {
        let SessionPrincipal::User(user) = claims.principal.clone() else {
            return Err(Status::permission_denied("frontend requires user identity"));
        };
        let relay_session_id = self.next_session("frontend");
        send_ready(&sender, &relay_session_id, claims.expires_at_unix_ms).await?;
        tracing::info!(
            event.name = "platform.relay.connected",
            session_id = %relay_session_id,
            role = "frontend",
            organization_id = %claims.organization_id,
            message = "Relay accepted frontend connection",
        );
        self.state
            .connections
            .lock()
            .map_err(|_| Status::internal("relay connection state poisoned"))?
            .frontends
            .insert(
                relay_session_id.clone(),
                RegisteredConnection {
                    relay_session_id: relay_session_id.clone(),
                    sender: sender.clone(),
                    authenticated_device: None,
                    peer_certificate_chain: None,
                    session_cancel: None,
                    dispatch_gate: Arc::new(tokio::sync::Mutex::new(())),
                },
            );

        let relay = self.clone();
        let session_deadline = session_deadline(claims.expires_at_unix_ms);
        tokio::spawn(async move {
            while let Some(frame) = next_authorized_frame(&mut inbound, session_deadline).await {
                match frame.body {
                    Some(relay_frame::Body::DiscoverRequest(request)) => {
                        let valid_user = request.user.as_ref().is_some_and(|actual| {
                            actual.issuer == user.issuer && actual.subject == user.subject
                        });
                        if !valid_user || request.organization_id != claims.organization_id {
                            let _ = send_error(
                                &sender,
                                &frame.frame_id,
                                7,
                                "WORKSPACE_DIRECTORY_SCOPE_DENIED",
                            )
                            .await;
                            continue;
                        }
                        match relay
                            .state
                            .directory
                            .discover(&user, &claims.organization_id, now_unix_ms())
                            .await
                        {
                            Ok(workspaces) => {
                                let _ = send_frame(
                                    &sender,
                                    RelayFrame {
                                        frame_id: frame.frame_id,
                                        body: Some(relay_frame::Body::DiscoverResponse(
                                            DiscoverWorkspacesResponse { workspaces },
                                        )),
                                    },
                                )
                                .await;
                            }
                            Err(error) => {
                                let (code, message) = error.rpc_error();
                                let _ = send_error(&sender, &frame.frame_id, code, message).await;
                            }
                        }
                    }
                    Some(relay_frame::Body::WorkspaceRequest(request)) => {
                        relay
                            .forward_workspace_request(&sender, &relay_session_id, &claims, request)
                            .await;
                    }
                    _ => {
                        tracing::warn!(
                            event.name = "platform.relay.frame_error",
                            error.code = "PLATFORM.RELAY.FRAME_ERROR",
                            session_id = %relay_session_id,
                            frame_id = %frame.frame_id,
                            message = "Frontend sent invalid or unexpected relay frame",
                        );
                        let _ =
                            send_error(&sender, &frame.frame_id, 3, "FRONTEND_RELAY_FRAME_INVALID")
                                .await;
                    }
                }
            }
            if let Ok(mut connections) = relay.state.connections.lock() {
                connections.frontends.remove(&relay_session_id);
                connections
                    .pending
                    .retain(|(frontend_id, _), _| frontend_id != &relay_session_id);
            }
            tracing::info!(
                event.name = "platform.relay.disconnected",
                session_id = %relay_session_id,
                role = "frontend",
                message = "Relay frontend connection terminated",
            );
        });
        Ok(())
    }

    async fn forward_workspace_request(
        &self,
        frontend_sender: &RelaySender,
        frontend_session_id: &str,
        claims: &RelaySessionClaims,
        request: cy_proto::workspace_v1::WorkspaceApiRequest,
    ) {
        let SessionPrincipal::User(_) = &claims.principal else {
            let _ = send_workspace_error(
                frontend_sender,
                request.request_id,
                7,
                "WORKSPACE_MEMBERSHIP_DENIED",
            )
            .await;
            return;
        };
        if !claims.workspace_id.is_empty() && request.workspace_id != claims.workspace_id {
            let _ = send_workspace_error(
                frontend_sender,
                request.request_id,
                7,
                "WORKSPACE_MEMBERSHIP_DENIED",
            )
            .await;
            return;
        }
        let caller = match WorkspaceCallerContext::from_relay_session_member(
            claims,
            request.workspace_id.clone(),
            self.state.directory.as_ref(),
        )
        .await
        {
            Ok(caller) => caller,
            Err(crate::WorkspaceCallerContextError::DirectoryUnavailable) => {
                let _ = send_workspace_error(
                    frontend_sender,
                    request.request_id,
                    14,
                    "WORKSPACE_DIRECTORY_UNAVAILABLE",
                )
                .await;
                return;
            }
            Err(_) => {
                let _ = send_workspace_error(
                    frontend_sender,
                    request.request_id,
                    7,
                    "WORKSPACE_MEMBERSHIP_DENIED",
                )
                .await;
                return;
            }
        };
        let workspace_dispatch = match self
            .register_pending(
                &request.workspace_id,
                frontend_session_id,
                &request.request_id,
            )
            .await
        {
            Ok(sender) => sender,
            Err((code, message)) => {
                let _ =
                    send_workspace_error(frontend_sender, request.request_id, code, message).await;
                return;
            }
        };
        let Some(workspace_dispatch) = workspace_dispatch else {
            tracing::warn!(
                event.name = "platform.relay.workspace_offline",
                error.code = "PLATFORM.RELAY.STREAM_DISCONNECTED",
                frontend_session_id = %frontend_session_id,
                workspace_id = %request.workspace_id,
                request_id = %request.request_id,
                message = "Workspace connector is offline for requested workspace",
            );
            let _ = send_workspace_error(
                frontend_sender,
                request.request_id,
                14,
                "WORKSPACE_CONNECTOR_OFFLINE",
            )
            .await;
            return;
        };
        let request_id = request.request_id.clone();
        let workspace_session_id = workspace_dispatch.relay_session_id.clone();
        let workspace_id = request.workspace_id.clone();
        let forwarded = match caller.relay_forwarded_request(frontend_session_id, request) {
            Ok(forwarded) => forwarded,
            Err(_) => {
                self.clear_pending(frontend_session_id, &request_id, &workspace_session_id);
                let _ = send_workspace_error(
                    frontend_sender,
                    request_id,
                    7,
                    "WORKSPACE_MEMBERSHIP_DENIED",
                )
                .await;
                return;
            }
        };
        let frame = RelayFrame {
            frame_id: request_id.clone(),
            body: Some(relay_frame::Body::ForwardedRequest(forwarded)),
        };
        if let Err((code, message)) = self.send_workspace_dispatch(
            &workspace_id,
            frontend_session_id,
            &request_id,
            workspace_dispatch,
            frame,
        ) {
            self.clear_pending(frontend_session_id, &request_id, &workspace_session_id);
            let _ = send_workspace_error(frontend_sender, request_id, code, message).await;
        }
    }

    async fn open_workspace(
        &self,
        hello: RelayHello,
        claims: RelaySessionClaims,
        authenticated_peer: AuthenticatedWorkspacePeer,
        mut inbound: Streaming<RelayFrame>,
        sender: RelaySender,
        session_cancel: watch::Sender<bool>,
    ) -> Result<(), Status> {
        let SessionPrincipal::WorkspaceDevice {
            workspace_id,
            device_id,
        } = &claims.principal
        else {
            return Err(Status::permission_denied(
                "Workspace connector requires device identity",
            ));
        };
        let authenticated_device = authenticated_peer.device;
        let peer_certificate_chain = authenticated_peer.certificate_chain;
        let authenticated_key = authenticated_device.key();
        if authenticated_device.authorization_generation() == 0
            || authenticated_device
                .registration_binding_id()
                .iter()
                .all(|byte| *byte == 0)
            || authenticated_device
                .certificate_sha256()
                .iter()
                .all(|byte| *byte == 0)
            || authenticated_device.serial_number().is_empty()
            || authenticated_device.not_after_unix_ms() != claims.expires_at_unix_ms
            || authenticated_key.organization_id != claims.organization_id
            || authenticated_key.workspace_id != *workspace_id
            || authenticated_key.device_id != *device_id
            || authenticated_key.organization_id != hello.organization_id
            || workspace_id != &hello.workspace_id
            || hello.device.as_ref().is_none_or(|device| {
                device.workspace_id != *workspace_id || device.device_id != *device_id
            })
        {
            return Err(Status::permission_denied("Workspace identity mismatch"));
        }
        let relay_session_id = self.next_session("workspace");
        send_ready(&sender, &relay_session_id, claims.expires_at_unix_ms).await?;
        tracing::info!(
            event.name = "platform.relay.connected",
            session_id = %relay_session_id,
            role = "workspace",
            workspace_id = %workspace_id,
            message = "Relay accepted workspace connector connection",
        );
        {
            let mut connections = self
                .state
                .connections
                .lock()
                .map_err(|_| Status::internal("relay connection state poisoned"))?;
            let displaced = connections.workspaces.insert(
                workspace_id.clone(),
                RegisteredConnection {
                    relay_session_id: relay_session_id.clone(),
                    sender: sender.clone(),
                    authenticated_device: Some(authenticated_device),
                    peer_certificate_chain: Some(peer_certificate_chain),
                    session_cancel: Some(session_cancel.clone()),
                    dispatch_gate: Arc::new(tokio::sync::Mutex::new(())),
                },
            );
            if let Some(previous) = displaced.as_ref() {
                connections
                    .pending
                    .retain(|_, pending| pending.workspace_session_id != previous.relay_session_id);
                if let Some(previous_cancel) = &previous.session_cancel {
                    previous_cancel.send_replace(true);
                }
            }
        }

        let relay = self.clone();
        let registered_workspace = workspace_id.clone();
        let session_deadline = session_deadline(claims.expires_at_unix_ms);
        let mut session_cancel_receiver = session_cancel.subscribe();
        tokio::spawn(async move {
            while let Some(frame) = tokio::select! {
                _ = session_cancel_receiver.changed() => None,
                frame = next_authorized_frame(&mut inbound, session_deadline) => frame,
            } {
                let Some(relay_frame::Body::ForwardedResponse(forwarded)) = frame.body else {
                    tracing::warn!(
                        event.name = "platform.relay.frame_error",
                        error.code = "PLATFORM.RELAY.FRAME_ERROR",
                        session_id = %relay_session_id,
                        workspace_id = %registered_workspace,
                        frame_id = %frame.frame_id,
                        message = "Workspace connector sent invalid or unexpected relay frame",
                    );
                    let _ =
                        send_error(&sender, &frame.frame_id, 3, "WORKSPACE_RELAY_FRAME_INVALID")
                            .await;
                    continue;
                };
                let Some(response) = forwarded.response else {
                    continue;
                };
                if frame.frame_id != response.request_id {
                    tracing::warn!(
                        event.name = "platform.relay.response_rejected",
                        error.code = "PLATFORM.RELAY.RESPONSE_UNMATCHED",
                        session_id = %relay_session_id,
                        message = "Workspace response frame and request identities differ",
                    );
                    continue;
                }
                let frontend_sender = match relay.take_response_sender(
                    &relay_session_id,
                    &forwarded.frontend_session_id,
                    &response.request_id,
                ) {
                    Ok(sender) => sender,
                    Err(_) => {
                        let _ = send_error(
                            &sender,
                            &frame.frame_id,
                            13,
                            "RELAY_CONNECTION_STATE_POISONED",
                        )
                        .await;
                        continue;
                    }
                };
                if let Some(frontend_sender) = frontend_sender {
                    let _ = send_frame(
                        &frontend_sender,
                        RelayFrame {
                            frame_id: frame.frame_id,
                            body: Some(relay_frame::Body::WorkspaceResponse(response)),
                        },
                    )
                    .await;
                } else {
                    tracing::warn!(
                        event.name = "platform.relay.response_rejected",
                        error.code = "PLATFORM.RELAY.RESPONSE_UNMATCHED",
                        session_id = %relay_session_id,
                        frame_id = %frame.frame_id,
                        message = "Workspace response has no pending request",
                    );
                }
            }
            if let Ok(mut connections) = relay.state.connections.lock() {
                let remove = connections
                    .workspaces
                    .get(&registered_workspace)
                    .is_some_and(|connection| connection.relay_session_id == relay_session_id);
                if remove {
                    connections.workspaces.remove(&registered_workspace);
                }
                connections
                    .pending
                    .retain(|_, pending| pending.workspace_session_id != relay_session_id);
            }
            session_cancel.send_replace(true);
            tracing::info!(
                event.name = "platform.relay.disconnected",
                session_id = %relay_session_id,
                role = "workspace",
                workspace_id = %registered_workspace,
                message = "Relay workspace connection terminated",
            );
        });
        Ok(())
    }
}

async fn send_ready(
    sender: &RelaySender,
    relay_session_id: &str,
    expires_at_unix_ms: u64,
) -> Result<(), Status> {
    send_frame(
        sender,
        RelayFrame {
            frame_id: format!("{relay_session_id}-ready"),
            body: Some(relay_frame::Body::Ready(RelayReady {
                relay_session_id: relay_session_id.to_string(),
                expires_at: Some(timestamp_from_ms(expires_at_unix_ms)),
            })),
        },
    )
    .await
}

async fn send_workspace_error(
    sender: &RelaySender,
    request_id: String,
    code: i32,
    message: &str,
) -> Result<(), Status> {
    send_frame(
        sender,
        RelayFrame {
            frame_id: request_id.clone(),
            body: Some(relay_frame::Body::WorkspaceResponse(WorkspaceApiResponse {
                request_id,
                outcome: Some(workspace_api_response::Outcome::Error(RpcStatus {
                    code,
                    message: message.to_string(),
                    details: Vec::new(),
                })),
            })),
        },
    )
    .await
}

async fn send_error(
    sender: &RelaySender,
    frame_id: &str,
    code: i32,
    message: &str,
) -> Result<(), Status> {
    send_frame(
        sender,
        RelayFrame {
            frame_id: frame_id.to_string(),
            body: Some(relay_frame::Body::Error(RpcStatus {
                code,
                message: message.to_string(),
                details: Vec::new(),
            })),
        },
    )
    .await
}

async fn send_frame(sender: &RelaySender, frame: RelayFrame) -> Result<(), Status> {
    sender
        .send(Ok(frame))
        .await
        .map_err(|_| Status::unavailable("relay participant disconnected"))
}

async fn next_authorized_frame(
    inbound: &mut Streaming<RelayFrame>,
    session_deadline: Instant,
) -> Option<RelayFrame> {
    let remaining = session_deadline.checked_duration_since(Instant::now())?;
    tokio::time::timeout(remaining, inbound.message())
        .await
        .ok()?
        .ok()?
}

fn session_deadline(expires_at_unix_ms: u64) -> Instant {
    // Use a monotonic deadline after authentication; cap one stream at 24 hours.
    // 认证后改用单调时钟，并限制单条连接最长存活 24 小时。
    let remaining_ms = expires_at_unix_ms.saturating_sub(now_unix_ms());
    Instant::now() + Duration::from_millis(remaining_ms.min(24 * 60 * 60 * 1000))
}

fn now_unix_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

fn registry_identity_for_authenticated_device(
    device: &AuthenticatedRelayWorkspaceDevice,
) -> WorkspaceDeviceCertificateIdentity {
    let mut certificate_fingerprint_sha256 = String::with_capacity(64);
    for byte in device.certificate_sha256() {
        write!(certificate_fingerprint_sha256, "{byte:02x}")
            .expect("writing a digest into String cannot fail");
    }
    WorkspaceDeviceCertificateIdentity {
        key: device.key().clone(),
        certificate_fingerprint_sha256,
        registration_binding_id: *device.registration_binding_id(),
        authorization_generation: device.authorization_generation(),
        csr_sha256: *device.csr_sha256(),
        spki_sha256: *device.spki_sha256(),
        serial_number: device.serial_number().to_vec(),
        not_after_unix_ms: device.not_after_unix_ms(),
    }
}

fn timestamp_from_ms(value: u64) -> prost_types::Timestamp {
    prost_types::Timestamp {
        seconds: i64::try_from(value / 1000).unwrap_or(i64::MAX),
        nanos: i32::try_from((value % 1000) * 1_000_000).unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use cy_proto::workspace_v1::{
        relay_forwarded_request, UserIdentityRef, WorkspaceConnectionDescriptor,
    };
    use prost::Message;
    use rcgen::{
        BasicConstraints, CertificateParams, CertifiedIssuer, IsCa, KeyPair, KeyUsagePurpose,
    };

    use crate::{
        DevelopmentSessionVerifier, FileWorkspaceDirectory, InMemoryWorkspaceDirectory,
        RelaySessionClaims, SessionPrincipal, WorkspaceDirectory, WorkspaceDirectoryError,
        WorkspaceMembership,
    };

    use super::*;

    fn relay_with_connections() -> (WorkspaceRelay, mpsc::Receiver<Result<RelayFrame, Status>>) {
        let directory = Arc::new(InMemoryWorkspaceDirectory::new(Vec::new(), Vec::new()).unwrap());
        let relay = WorkspaceRelay::new(directory, Arc::new(DevelopmentSessionVerifier::default()));
        let (frontend_sender, _) = mpsc::channel(RELAY_QUEUE_FRAMES);
        let (workspace_sender, workspace_receiver) = mpsc::channel(RELAY_QUEUE_FRAMES);
        let mut connections = relay.state.connections.lock().unwrap();
        connections.frontends.insert(
            "frontend-1".into(),
            RegisteredConnection {
                relay_session_id: "frontend-1".into(),
                sender: frontend_sender,
                authenticated_device: None,
                peer_certificate_chain: None,
                session_cancel: None,
                dispatch_gate: Arc::new(tokio::sync::Mutex::new(())),
            },
        );
        let (workspace_cancel, _) = watch::channel(false);
        connections.workspaces.insert(
            "workspace-1".into(),
            RegisteredConnection {
                relay_session_id: "workspace-session-1".into(),
                sender: workspace_sender,
                authenticated_device: None,
                peer_certificate_chain: None,
                session_cancel: Some(workspace_cancel),
                dispatch_gate: Arc::new(tokio::sync::Mutex::new(())),
            },
        );
        drop(connections);
        (relay, workspace_receiver)
    }

    #[test]
    fn relay_rejects_workspace_connector_without_tls_peer_certificate() {
        let directory = Arc::new(InMemoryWorkspaceDirectory::new(Vec::new(), Vec::new()).unwrap());
        let relay = WorkspaceRelay::new(directory, Arc::new(DevelopmentSessionVerifier::default()));
        let hello = RelayHello {
            role: RelayParticipantRole::WorkspaceConnector as i32,
            session_credential: "spoofed-session".into(),
            user: None,
            organization_id: "organization-1".into(),
            workspace_id: "workspace-1".into(),
            device: Some(cy_proto::workspace_v1::DeviceEnrollmentRef {
                device_id: "device-1".into(),
                workspace_id: "workspace-1".into(),
                enrollment_state: "approved".into(),
            }),
        };

        let error = relay
            .authenticate_participant(
                RelayParticipantRole::WorkspaceConnector,
                &hello,
                Err(WorkspaceDeviceAuthenticationError::MissingClientCertificate),
                None,
                None,
                now_unix_ms(),
            )
            .unwrap_err();

        assert_eq!(error.code(), tonic::Code::Unauthenticated);
        assert_eq!(error.message(), "WORKSPACE_DEVICE_TLS_CERTIFICATE_REQUIRED");
    }

    #[test]
    fn explicit_aca_mode_never_falls_back_to_a_tonic_peer_certificate() {
        let mut root_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        root_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        root_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let root = CertifiedIssuer::self_signed(root_params, KeyPair::generate().unwrap()).unwrap();
        let adapter = AcaForwardedCertificateAdapter::new(root.pem().as_bytes()).unwrap();

        let tonic_peer = rcgen::generate_simple_self_signed(vec!["connector.test".into()]).unwrap();
        let tonic_peer =
            VerifiedClientCertificate::from_validated_der(tonic_peer.cert.der().as_ref()).unwrap();
        let temporary_directory = tempfile::tempdir().unwrap();
        let directory = Arc::new(
            FileWorkspaceDirectory::open(temporary_directory.path().join("relay-state")).unwrap(),
        );
        let relay = WorkspaceRelay::with_aca_forwarded_certificate_adapter(
            directory.clone(),
            Arc::new(DevelopmentSessionVerifier::default()),
            directory,
            adapter,
        );
        let hello = RelayHello {
            role: RelayParticipantRole::WorkspaceConnector as i32,
            session_credential: String::new(),
            user: None,
            organization_id: "organization-1".into(),
            workspace_id: "workspace-1".into(),
            device: Some(cy_proto::workspace_v1::DeviceEnrollmentRef {
                device_id: "device-1".into(),
                workspace_id: "workspace-1".into(),
                enrollment_state: "approved".into(),
            }),
        };
        let request = Request::new(());

        let error = relay
            .authenticate_participant(
                RelayParticipantRole::WorkspaceConnector,
                &hello,
                Ok(tonic_peer),
                Some(&request),
                None,
                now_unix_ms(),
            )
            .unwrap_err();

        assert_eq!(error.code(), tonic::Code::Unauthenticated);
        assert_eq!(
            error.message(),
            "WORKSPACE_DEVICE_PEER_CERTIFICATE_VALIDATION_NOT_CONFIGURED"
        );
    }

    #[test]
    fn aca_frontend_requires_the_bff_certificate_adapter_before_handoff_verification() {
        let mut root_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        root_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        root_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let root = CertifiedIssuer::self_signed(root_params, KeyPair::generate().unwrap()).unwrap();
        let device_adapter = AcaForwardedCertificateAdapter::new(root.pem().as_bytes()).unwrap();
        let temporary_directory = tempfile::tempdir().unwrap();
        let directory = Arc::new(
            FileWorkspaceDirectory::open(temporary_directory.path().join("relay-state")).unwrap(),
        );
        let relay = WorkspaceRelay::with_aca_forwarded_certificate_adapter(
            directory.clone(),
            Arc::new(DevelopmentSessionVerifier::default()),
            directory,
            device_adapter,
        );
        let hello = RelayHello {
            role: RelayParticipantRole::Frontend as i32,
            session_credential: "invalid-handoff".into(),
            user: None,
            organization_id: String::new(),
            workspace_id: String::new(),
            device: None,
        };

        let error = relay
            .authenticate_participant(
                RelayParticipantRole::Frontend,
                &hello,
                Err(WorkspaceDeviceAuthenticationError::MissingClientCertificate),
                None,
                None,
                now_unix_ms(),
            )
            .unwrap_err();

        assert_eq!(error.code(), tonic::Code::Unauthenticated);
        assert_eq!(
            error.message(),
            "BFF_WORKLOAD_CERTIFICATE_AUTHENTICATION_NOT_CONFIGURED"
        );
    }

    #[test]
    fn aca_frontend_checks_same_request_xfcc_before_signed_handoff() {
        let mut root_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        root_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        root_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let root = CertifiedIssuer::self_signed(root_params, KeyPair::generate().unwrap()).unwrap();
        let device_adapter = AcaForwardedCertificateAdapter::new(root.pem().as_bytes()).unwrap();
        let frontend_adapter = AcaForwardedBffWorkloadCertificateAdapter::new(
            root.pem().as_bytes(),
            [crate::BffWorkloadCertificatePin::new(
                "11".repeat(32),
                "CN=Cyrene Web BFF Workload",
                false,
            )],
        )
        .unwrap();
        let temporary_directory = tempfile::tempdir().unwrap();
        let directory = Arc::new(
            FileWorkspaceDirectory::open(temporary_directory.path().join("relay-state")).unwrap(),
        );
        let relay = WorkspaceRelay::with_aca_forwarded_certificate_adapters(
            directory.clone(),
            Arc::new(DevelopmentSessionVerifier::default()),
            directory,
            device_adapter,
            frontend_adapter,
        );
        let hello = RelayHello {
            role: RelayParticipantRole::Frontend as i32,
            session_credential: "invalid-handoff".into(),
            user: None,
            organization_id: String::new(),
            workspace_id: String::new(),
            device: None,
        };

        let error = relay
            .authenticate_participant(
                RelayParticipantRole::Frontend,
                &hello,
                Err(WorkspaceDeviceAuthenticationError::MissingClientCertificate),
                Some(&Request::new(())),
                None,
                now_unix_ms(),
            )
            .unwrap_err();

        assert_eq!(error.code(), tonic::Code::Unauthenticated);
        assert_eq!(error.message(), "BFF_WORKLOAD_CERTIFICATE_INVALID");
    }

    #[test]
    fn native_frontend_rejects_xfcc_metadata_even_with_tonic_peer_path() {
        let mut root_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        root_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        root_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let root = CertifiedIssuer::self_signed(root_params, KeyPair::generate().unwrap()).unwrap();
        let frontend_adapter = TonicBffWorkloadCertificateAdapter::new(
            root.pem().as_bytes(),
            vec![crate::BffWorkloadCertificatePin::new(
                "11".repeat(32),
                "CN=Cyrene Web BFF Workload",
                false,
            )],
        )
        .unwrap();
        let directory = Arc::new(InMemoryWorkspaceDirectory::new(Vec::new(), Vec::new()).unwrap());
        let mut relay =
            WorkspaceRelay::new(directory, Arc::new(DevelopmentSessionVerifier::default()));
        Arc::get_mut(&mut relay.state)
            .expect("unique test Relay")
            .tonic_bff_workload_certificate_adapter = Some(frontend_adapter);

        let mut request = Request::new(());
        request
            .metadata_mut()
            .insert("x-forwarded-client-cert", "spoofed".parse().unwrap());
        let hello = RelayHello {
            role: RelayParticipantRole::Frontend as i32,
            session_credential: "invalid-handoff".into(),
            user: None,
            organization_id: String::new(),
            workspace_id: String::new(),
            device: None,
        };

        let error = relay
            .authenticate_participant(
                RelayParticipantRole::Frontend,
                &hello,
                Err(WorkspaceDeviceAuthenticationError::MissingClientCertificate),
                Some(&request),
                None,
                now_unix_ms(),
            )
            .unwrap_err();

        assert_eq!(error.code(), tonic::Code::Unauthenticated);
        assert_eq!(
            error.message(),
            "BFF_WORKLOAD_XFCC_NOT_ALLOWED_IN_NATIVE_MODE"
        );
    }

    #[test]
    fn native_connector_rejects_spoofed_xfcc_metadata() {
        let mut root_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        root_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        root_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let root = CertifiedIssuer::self_signed(root_params, KeyPair::generate().unwrap()).unwrap();
        let validator = RelayPeerCertificateValidator::from_pem_bundle(root.pem().as_bytes())
            .expect("native device CA bundle");
        let directory = Arc::new(InMemoryWorkspaceDirectory::new(Vec::new(), Vec::new()).unwrap());
        let mut relay =
            WorkspaceRelay::new(directory, Arc::new(DevelopmentSessionVerifier::default()));
        Arc::get_mut(&mut relay.state)
            .expect("unique test Relay")
            .workspace_peer_certificate_validator = Some(Arc::new(validator));
        let mut request = Request::new(());
        request
            .metadata_mut()
            .insert("x-forwarded-client-cert", "spoofed".parse().unwrap());

        assert!(relay.native_xfcc_metadata_must_be_rejected(
            RelayParticipantRole::WorkspaceConnector,
            request.metadata().contains_key("x-forwarded-client-cert"),
        ));
    }

    #[tokio::test]
    async fn response_requires_the_registered_workspace_session_and_request() {
        let (relay, _workspace_receiver) = relay_with_connections();
        assert!(relay
            .register_pending("workspace-1", "frontend-1", "request-1")
            .await
            .unwrap()
            .is_some());
        assert!(relay
            .take_response_sender("workspace-session-2", "frontend-1", "request-1")
            .unwrap()
            .is_none());
        assert!(relay
            .take_response_sender("workspace-session-1", "frontend-1", "request-2")
            .unwrap()
            .is_none());
        assert!(relay
            .take_response_sender("workspace-session-1", "frontend-1", "request-1")
            .unwrap()
            .is_some());
        assert!(relay
            .take_response_sender("workspace-session-1", "frontend-1", "request-1")
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn duplicate_request_ids_do_not_overwrite_pending_routes() {
        let (relay, _workspace_receiver) = relay_with_connections();
        relay
            .register_pending("workspace-1", "frontend-1", "request-1")
            .await
            .unwrap();
        assert!(matches!(
            relay
                .register_pending("workspace-1", "frontend-1", "request-1")
                .await,
            Err((3, "WORKSPACE_REQUEST_ID_INVALID"))
        ));
        assert!(relay
            .take_response_sender("workspace-session-1", "frontend-1", "request-1")
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn request_ids_are_bounded_before_entering_the_pending_table() {
        let (relay, _workspace_receiver) = relay_with_connections();
        assert!(matches!(
            relay
                .register_pending("workspace-1", "frontend-1", &"x".repeat(129))
                .await,
            Err((3, "WORKSPACE_REQUEST_ID_INVALID"))
        ));
        assert!(relay.state.connections.lock().unwrap().pending.is_empty());
    }

    #[tokio::test]
    async fn forwarded_caller_comes_from_verified_session_not_request_wire() {
        let verified_user = UserIdentityRef {
            issuer: "https://identity.test".to_string(),
            subject: "verified-user".to_string(),
        };
        let directory = Arc::new(
            InMemoryWorkspaceDirectory::new(
                vec![WorkspaceMembership {
                    user: verified_user.clone(),
                    organization_id: "organization-1".to_string(),
                    workspace_id: "workspace-1".to_string(),
                    roles: BTreeSet::from(["workspace.operator".to_string()]),
                }],
                Vec::new(),
            )
            .unwrap(),
        );
        let relay = WorkspaceRelay::new(directory, Arc::new(DevelopmentSessionVerifier::default()));
        let (frontend_sender, _) = mpsc::channel(RELAY_QUEUE_FRAMES);
        let (workspace_sender, mut workspace_receiver) = mpsc::channel(RELAY_QUEUE_FRAMES);
        let (workspace_cancel, _) = watch::channel(false);
        relay.state.connections.lock().unwrap().workspaces.insert(
            "workspace-1".to_string(),
            RegisteredConnection {
                relay_session_id: "workspace-session-1".to_string(),
                sender: workspace_sender,
                authenticated_device: None,
                peer_certificate_chain: None,
                session_cancel: Some(workspace_cancel),
                dispatch_gate: Arc::new(tokio::sync::Mutex::new(())),
            },
        );

        let mut encoded_request = cy_proto::workspace_v1::WorkspaceApiRequest {
            request_id: "request-1".to_string(),
            workspace_id: "workspace-1".to_string(),
            traceparent: String::new(),
            request: None,
        }
        .encode_to_vec();
        // Tag 4 is not part of WorkspaceApiRequest. A caller-like browser field is ignored.
        encoded_request.extend_from_slice(&[0x22, 11]);
        encoded_request.extend_from_slice(b"forged-user");
        let request =
            cy_proto::workspace_v1::WorkspaceApiRequest::decode(encoded_request.as_slice())
                .expect("unknown browser-supplied field should be ignored");

        relay
            .forward_workspace_request(
                &frontend_sender,
                "frontend-1",
                &RelaySessionClaims {
                    principal: SessionPrincipal::User(verified_user.clone()),
                    organization_id: "organization-1".to_string(),
                    workspace_id: String::new(),
                    expires_at_unix_ms: now_unix_ms().saturating_add(60_000),
                },
                request,
            )
            .await;

        let frame = workspace_receiver
            .recv()
            .await
            .expect("relay should forward the request")
            .expect("forwarded frame should be valid");
        let Some(relay_frame::Body::ForwardedRequest(forwarded)) = frame.body else {
            panic!("expected a Relay-only forwarded request");
        };
        let Some(relay_forwarded_request::CallerPrincipal::User(user)) = forwarded.caller_principal
        else {
            panic!("expected the verified user principal");
        };
        assert_eq!(user, verified_user);
        assert!(forwarded
            .caller_roles
            .contains(&"workspace.member".to_string()));
        assert!(forwarded
            .caller_roles
            .contains(&"workspace.operator".to_string()));
    }

    struct UnavailableDirectory;

    #[tonic::async_trait]
    impl WorkspaceDirectory for UnavailableDirectory {
        async fn discover(
            &self,
            _user: &UserIdentityRef,
            _organization_id: &str,
            _now_unix_ms: u64,
        ) -> Result<Vec<WorkspaceConnectionDescriptor>, WorkspaceDirectoryError> {
            Err(WorkspaceDirectoryError::Storage(
                "postgres://private-diagnostics".into(),
            ))
        }

        async fn is_member(
            &self,
            _user: &UserIdentityRef,
            _organization_id: &str,
            _workspace_id: &str,
        ) -> Result<bool, WorkspaceDirectoryError> {
            Err(WorkspaceDirectoryError::Storage(
                "postgres://private-diagnostics".into(),
            ))
        }

        async fn roles_for_member(
            &self,
            _user: &UserIdentityRef,
            _organization_id: &str,
            _workspace_id: &str,
        ) -> Result<Option<BTreeSet<String>>, WorkspaceDirectoryError> {
            Err(WorkspaceDirectoryError::Storage(
                "postgres://private-diagnostics".into(),
            ))
        }
    }

    #[tokio::test]
    async fn directory_outage_fails_closed_without_leaking_backend_details() {
        let relay = WorkspaceRelay::new(
            Arc::new(UnavailableDirectory),
            Arc::new(DevelopmentSessionVerifier::default()),
        );
        let (frontend_sender, mut frontend_receiver) = mpsc::channel(RELAY_QUEUE_FRAMES);
        let request = cy_proto::workspace_v1::WorkspaceApiRequest {
            request_id: "request-1".into(),
            workspace_id: "workspace-1".into(),
            traceparent: String::new(),
            request: None,
        };

        relay
            .forward_workspace_request(
                &frontend_sender,
                "frontend-1",
                &RelaySessionClaims {
                    principal: SessionPrincipal::User(UserIdentityRef {
                        issuer: "https://identity.test".into(),
                        subject: "user-1".into(),
                    }),
                    organization_id: "organization-1".into(),
                    workspace_id: String::new(),
                    expires_at_unix_ms: now_unix_ms().saturating_add(60_000),
                },
                request,
            )
            .await;

        let frame = frontend_receiver
            .recv()
            .await
            .expect("Relay should return a closed failure")
            .expect("failure frame should be valid");
        let Some(relay_frame::Body::WorkspaceResponse(response)) = frame.body else {
            panic!("expected a Workspace error response");
        };
        let Some(workspace_api_response::Outcome::Error(status)) = response.outcome else {
            panic!("expected a Workspace error status");
        };
        assert_eq!(status.code, 14);
        assert_eq!(status.message, "WORKSPACE_DIRECTORY_UNAVAILABLE");
        assert!(!status.message.contains("private-diagnostics"));
    }
}
