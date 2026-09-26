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

use crate::{RelayAuthenticator, RelaySessionClaims, SessionPrincipal, WorkspaceDirectory};

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
    connections: Mutex<Connections>,
    next_session: AtomicU64,
}

/// Application relay that routes frames without owning Workspace state.
#[derive(Clone)]
pub struct WorkspaceRelay {
    state: Arc<RelayState>,
}

impl WorkspaceRelay {
    pub fn new(
        directory: Arc<dyn WorkspaceDirectory>,
        authenticator: Arc<dyn RelayAuthenticator>,
    ) -> Self {
        Self {
            state: Arc::new(RelayState {
                directory,
                authenticator,
                connections: Mutex::new(Connections::default()),
                next_session: AtomicU64::new(1),
            }),
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
        let mut inbound = request.into_inner();
        let first = tokio::time::timeout(Duration::from_secs(5), inbound.message())
            .await
            .map_err(|_| Status::deadline_exceeded("relay hello timed out"))??
            .ok_or_else(|| Status::invalid_argument("relay hello is required"))?;
        let Some(relay_frame::Body::Hello(hello)) = first.body else {
            return Err(Status::invalid_argument("first relay frame must be hello"));
        };
        let claims = self
            .state
            .authenticator
            .authenticate(&hello, now_unix_ms())
            .map_err(|error| Status::unauthenticated(error.to_string()))?;
        let role = RelayParticipantRole::try_from(hello.role)
            .map_err(|_| Status::invalid_argument("unknown relay participant role"))?;
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
                        match relay.state.directory.discover(
                            &user,
                            &claims.organization_id,
                            now_unix_ms(),
                        ) {
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
                                let _ = send_error(&sender, &frame.frame_id, 9, &error.to_string())
                                    .await;
                            }
                        }
                    }
                    Some(relay_frame::Body::WorkspaceRequest(request)) => {
                        relay
                            .forward_workspace_request(
                                &sender,
                                &relay_session_id,
                                &user,
                                &claims,
                                request,
                            )
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
        user: &cy_proto::workspace_v1::UserIdentityRef,
        claims: &RelaySessionClaims,
        request: cy_proto::workspace_v1::WorkspaceApiRequest,
    ) {
        if (!claims.workspace_id.is_empty() && request.workspace_id != claims.workspace_id)
            || !self
                .state
                .directory
                .is_member(user, &claims.organization_id, &request.workspace_id)
        {
            let _ = send_workspace_error(
                frontend_sender,
                request.request_id,
                7,
                "WORKSPACE_MEMBERSHIP_DENIED",
            )
            .await;
            return;
        }
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
        if send_frame(
            &workspace_sender,
            RelayFrame {
                frame_id: request.request_id.clone(),
                body: Some(relay_frame::Body::ForwardedRequest(RelayForwardedRequest {
                    frontend_session_id: frontend_session_id.to_string(),
                    request: Some(request),
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
        let SessionPrincipal::WorkspaceDevice { workspace_id, .. } = &claims.principal else {
            return Err(Status::permission_denied(
                "Workspace connector requires device identity",
            ));
        };
        if workspace_id != &hello.workspace_id {
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
    use crate::{DevelopmentSessionVerifier, InMemoryWorkspaceDirectory};

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
}
