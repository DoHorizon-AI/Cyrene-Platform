//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 relay.rs                                                        │
//! │  Module: cy_workspace_fabric::relay                                 │
//! │  Role: Stateless application-level Workspace request relay.         │
//! │                                                                     │
//! │  模块职责：无 Workspace 状态权威的应用层请求中继。                       │
//! └─────────────────────────────────────────────────────────────────────┘

use std::collections::BTreeMap;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use cy_proto::google::rpc::Status as RpcStatus;
use cy_proto::workspace_v1::relay_frame;
use cy_proto::workspace_v1::workspace_api_response;
use cy_proto::workspace_v1::workspace_relay_service_server::WorkspaceRelayService;
use cy_proto::workspace_v1::{
    DiscoverWorkspacesResponse, RelayForwardedRequest, RelayFrame, RelayHello,
    RelayParticipantRole, RelayReady, WorkspaceApiResponse,
};
use tokio::sync::mpsc;
use tokio_stream::{wrappers::ReceiverStream, Stream};
use tonic::{Request, Response, Status, Streaming};

use crate::{
    AcaForwardedCertificateAdapter, RegistryWorkspaceDeviceVerifier, RelayAuthenticator,
    RelaySessionClaims, SessionPrincipal, VerifiedClientCertificate, WorkspaceCallerContext,
    WorkspaceDeviceAuthenticationError, WorkspaceDeviceRegistry, WorkspaceDirectory,
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
    aca_forwarded_certificate_adapter: Option<AcaForwardedCertificateAdapter>,
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
        Self::build(directory, authenticator, None, None)
    }

    /// Construct a Relay whose Workspace connectors authenticate with the
    /// Tonic TLS peer certificate and the supplied device registry.
    ///
    /// `new` deliberately leaves connector authentication disabled. A Relay
    /// without a trusted device registry therefore cannot accept connectors.
    pub fn with_workspace_device_registry(
        directory: Arc<dyn WorkspaceDirectory>,
        authenticator: Arc<dyn RelayAuthenticator>,
        registry: Arc<dyn WorkspaceDeviceRegistry>,
    ) -> Self {
        Self::build(
            directory,
            authenticator,
            Some(Arc::new(RegistryWorkspaceDeviceVerifier::new(registry))),
            None,
        )
    }

    /// Construct a Relay that validates ACA-overwritten XFCC metadata for Workspace connectors.
    ///
    /// This mode must only be selected behind ACA HTTP/2 ingress configured to require client
    /// certificates and overwrite XFCC. It never falls back to a Tonic peer certificate when
    /// the forwarded certificate is missing or invalid.
    pub fn with_aca_forwarded_certificate_adapter(
        directory: Arc<dyn WorkspaceDirectory>,
        authenticator: Arc<dyn RelayAuthenticator>,
        registry: Arc<dyn WorkspaceDeviceRegistry>,
        adapter: AcaForwardedCertificateAdapter,
    ) -> Self {
        Self::build(
            directory,
            authenticator,
            Some(Arc::new(RegistryWorkspaceDeviceVerifier::new(registry))),
            Some(adapter),
        )
    }

    fn build(
        directory: Arc<dyn WorkspaceDirectory>,
        authenticator: Arc<dyn RelayAuthenticator>,
        workspace_device_verifier: Option<Arc<RegistryWorkspaceDeviceVerifier>>,
        aca_forwarded_certificate_adapter: Option<AcaForwardedCertificateAdapter>,
    ) -> Self {
        Self {
            state: Arc::new(RelayState {
                directory,
                authenticator,
                workspace_device_verifier,
                aca_forwarded_certificate_adapter,
                connections: Mutex::new(Connections::default()),
                next_session: AtomicU64::new(1),
            }),
        }
    }

    /// Route user sessions and device certificates through separate authorities.
    fn authenticate_participant(
        &self,
        role: RelayParticipantRole,
        hello: &RelayHello,
        peer_certificate: Result<VerifiedClientCertificate, WorkspaceDeviceAuthenticationError>,
        aca_request: Option<&Request<()>>,
        now_unix_ms: u64,
    ) -> Result<RelaySessionClaims, Box<Status>> {
        match role {
            RelayParticipantRole::Frontend => self
                .state
                .authenticator
                .authenticate(hello, now_unix_ms)
                .map_err(|error| Box::new(Status::unauthenticated(error.to_string()))),
            RelayParticipantRole::WorkspaceConnector => {
                let result = if let Some(adapter) = &self.state.aca_forwarded_certificate_adapter {
                    let verifier =
                        self.state
                            .workspace_device_verifier
                            .as_ref()
                            .ok_or_else(|| {
                                Box::new(Status::unauthenticated(
                                    "WORKSPACE_DEVICE_CERTIFICATE_AUTHENTICATION_NOT_CONFIGURED",
                                ))
                            })?;
                    let request = aca_request.ok_or_else(|| {
                        Box::new(Status::unauthenticated(
                            "WORKSPACE_DEVICE_TLS_CERTIFICATE_REQUIRED",
                        ))
                    })?;
                    adapter.authenticate_request(request, hello, verifier, now_unix_ms)
                } else {
                    let certificate = peer_certificate
                        .map_err(|error| Box::new(Status::unauthenticated(error.to_string())))?;
                    let verifier =
                        self.state
                            .workspace_device_verifier
                            .as_ref()
                            .ok_or_else(|| {
                                Box::new(Status::unauthenticated(
                                    "WORKSPACE_DEVICE_CERTIFICATE_AUTHENTICATION_NOT_CONFIGURED",
                                ))
                            })?;
                    verifier.authenticate(hello, &certificate, now_unix_ms)
                };
                result.map_err(|error| Box::new(Status::unauthenticated(error.to_string())))
            }
            RelayParticipantRole::Unspecified => Err(Box::new(Status::invalid_argument(
                "relay participant role is required",
            ))),
        }
    }

    fn next_session(&self, prefix: &str) -> String {
        let sequence = self.state.next_session.fetch_add(1, Ordering::Relaxed);
        format!("{prefix}-{sequence}")
    }

    fn register_pending(
        &self,
        workspace_id: &str,
        frontend_session_id: &str,
        request_id: &str,
    ) -> Result<Option<RelaySender>, (i32, &'static str)> {
        let mut connections = self
            .state
            .connections
            .lock()
            .map_err(|_| (13, "RELAY_CONNECTION_STATE_POISONED"))?;
        connections
            .pending
            .retain(|_, pending| pending.expires_at > Instant::now());
        let Some(workspace) = connections.workspaces.get(workspace_id).cloned() else {
            return Ok(None);
        };
        if request_id.is_empty() || request_id.len() > MAX_REQUEST_ID_BYTES {
            return Err((3, "WORKSPACE_REQUEST_ID_INVALID"));
        }
        let key = (frontend_session_id.to_string(), request_id.to_string());
        if connections.pending.contains_key(&key) {
            return Err((3, "WORKSPACE_REQUEST_ID_INVALID"));
        }
        if connections.pending.len() >= MAX_PENDING_REQUESTS {
            return Err((8, "RELAY_PENDING_REQUEST_LIMIT"));
        }
        connections.pending.insert(
            key,
            PendingRequest {
                workspace_session_id: workspace.relay_session_id,
                expires_at: Instant::now() + PENDING_REQUEST_TTL,
            },
        );
        Ok(Some(workspace.sender))
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

    fn clear_pending(&self, frontend_session_id: &str, request_id: &str) {
        if let Ok(mut connections) = self.state.connections.lock() {
            connections
                .pending
                .remove(&(frontend_session_id.to_string(), request_id.to_string()));
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
        let peer_certificate = VerifiedClientCertificate::from_tonic_request(&request);
        let aca_request = self
            .state
            .aca_forwarded_certificate_adapter
            .as_ref()
            .map(|_| {
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
        let claims = self
            .authenticate_participant(
                role,
                &hello,
                peer_certificate,
                aca_request.as_ref(),
                now_unix_ms(),
            )
            .map_err(|status| *status)?;
        let (sender, receiver) = mpsc::channel(RELAY_QUEUE_FRAMES);
        match role {
            RelayParticipantRole::Frontend => {
                self.open_frontend(claims, inbound, sender).await?;
            }
            RelayParticipantRole::WorkspaceConnector => {
                self.open_workspace(hello, claims, inbound, sender).await?;
            }
            RelayParticipantRole::Unspecified => {
                return Err(Status::invalid_argument(
                    "relay participant role is required",
                ));
            }
        }
        Ok(Response::new(Box::pin(ReceiverStream::new(receiver))))
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
                },
            );

        let relay = self.clone();
        let session_deadline = session_deadline(claims.expires_at_unix_ms);
        tokio::spawn(async move {
            while let Some(frame) = next_authorized_frame(&mut inbound, session_deadline).await {
                match frame.body {
                    Some(relay_frame::Body::DiscoverRequest(request)) => {
                        let valid_user = request
                            .user
                            .as_ref()
                            .is_some_and(|actual| crate::auth::same_user(&user, actual));
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
        let SessionPrincipal::User(user) = &claims.principal else {
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
        let directory_roles = match self
            .state
            .directory
            .roles_for_member(user, &claims.organization_id, &request.workspace_id)
            .await
        {
            Ok(Some(roles)) => roles,
            Ok(None) => {
                let _ = send_workspace_error(
                    frontend_sender,
                    request.request_id,
                    7,
                    "WORKSPACE_MEMBERSHIP_DENIED",
                )
                .await;
                return;
            }
            Err(error) => {
                let (code, message) = error.rpc_error();
                let _ =
                    send_workspace_error(frontend_sender, request.request_id, code, message).await;
                return;
            }
        };
        let caller = match WorkspaceCallerContext::user_member(
            user.clone(),
            claims.organization_id.clone(),
            request.workspace_id.clone(),
            directory_roles,
        ) {
            Ok(caller) => caller,
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
        let workspace_sender = match self.register_pending(
            &request.workspace_id,
            frontend_session_id,
            &request.request_id,
        ) {
            Ok(sender) => sender,
            Err((code, message)) => {
                let _ =
                    send_workspace_error(frontend_sender, request.request_id, code, message).await;
                return;
            }
        };
        let Some(workspace_sender) = workspace_sender else {
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
        let (caller_principal, caller_organization_id, caller_workspace_id, caller_roles) =
            caller.to_relay_fields();
        if send_frame(
            &workspace_sender,
            RelayFrame {
                frame_id: request.request_id.clone(),
                body: Some(relay_frame::Body::ForwardedRequest(RelayForwardedRequest {
                    frontend_session_id: frontend_session_id.to_string(),
                    request: Some(request),
                    caller_principal,
                    caller_organization_id,
                    caller_workspace_id,
                    caller_roles,
                })),
            },
        )
        .await
        .is_err()
        {
            self.clear_pending(frontend_session_id, &request_id);
            let _ = send_workspace_error(
                frontend_sender,
                request_id,
                14,
                "WORKSPACE_CONNECTOR_OFFLINE",
            )
            .await;
        }
    }

    async fn open_workspace(
        &self,
        hello: RelayHello,
        claims: RelaySessionClaims,
        mut inbound: Streaming<RelayFrame>,
        sender: RelaySender,
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
        if workspace_id != &hello.workspace_id
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
        self.state
            .connections
            .lock()
            .map_err(|_| Status::internal("relay connection state poisoned"))?
            .workspaces
            .insert(
                workspace_id.clone(),
                RegisteredConnection {
                    relay_session_id: relay_session_id.clone(),
                    sender: sender.clone(),
                },
            );

        let relay = self.clone();
        let registered_workspace = workspace_id.clone();
        let session_deadline = session_deadline(claims.expires_at_unix_ms);
        tokio::spawn(async move {
            while let Some(frame) = next_authorized_frame(&mut inbound, session_deadline).await {
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

    fn relay_with_connections() -> WorkspaceRelay {
        let directory = Arc::new(InMemoryWorkspaceDirectory::new(Vec::new(), Vec::new()).unwrap());
        let relay = WorkspaceRelay::new(directory, Arc::new(DevelopmentSessionVerifier::default()));
        let (frontend_sender, _) = mpsc::channel(RELAY_QUEUE_FRAMES);
        let (workspace_sender, _) = mpsc::channel(RELAY_QUEUE_FRAMES);
        let mut connections = relay.state.connections.lock().unwrap();
        connections.frontends.insert(
            "frontend-1".into(),
            RegisteredConnection {
                relay_session_id: "frontend-1".into(),
                sender: frontend_sender,
            },
        );
        connections.workspaces.insert(
            "workspace-1".into(),
            RegisteredConnection {
                relay_session_id: "workspace-session-1".into(),
                sender: workspace_sender,
            },
        );
        drop(connections);
        relay
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
                now_unix_ms(),
            )
            .unwrap_err();

        assert_eq!(error.code(), tonic::Code::Unauthenticated);
        assert_eq!(error.message(), "WORKSPACE_DEVICE_TLS_CERTIFICATE_REQUIRED");
    }

    #[test]
    fn response_requires_the_registered_workspace_session_and_request() {
        let relay = relay_with_connections();
        assert!(relay
            .register_pending("workspace-1", "frontend-1", "request-1")
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

    #[test]
    fn duplicate_request_ids_do_not_overwrite_pending_routes() {
        let relay = relay_with_connections();
        relay
            .register_pending("workspace-1", "frontend-1", "request-1")
            .unwrap();
        assert!(matches!(
            relay.register_pending("workspace-1", "frontend-1", "request-1"),
            Err((3, "WORKSPACE_REQUEST_ID_INVALID"))
        ));
        assert!(relay
            .take_response_sender("workspace-session-1", "frontend-1", "request-1")
            .unwrap()
            .is_some());
    }

    #[test]
    fn request_ids_are_bounded_before_entering_the_pending_table() {
        let relay = relay_with_connections();
        assert!(matches!(
            relay.register_pending("workspace-1", "frontend-1", &"x".repeat(129)),
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
        relay.state.connections.lock().unwrap().workspaces.insert(
            "workspace-1".to_string(),
            RegisteredConnection {
                relay_session_id: "workspace-session-1".to_string(),
                sender: workspace_sender,
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
