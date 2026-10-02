//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 transport.rs                                                    │
//! │  Module: cy_workspace_control_plane::connector_session             │
//! │  Role: Authenticated Workspace Connector Relay serving session.   │
//! │                                                                     │
//! │  模块职责：Frontend 与 Workspace 的出站 mTLS Relay 客户端。             │
//! └─────────────────────────────────────────────────────────────────────┘

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cy_mtls_channel_client::{connect_mtls_channel, MtlsChannelConnectError};
use cy_proto::core_v1::ConnectivityMode;
use cy_proto::workspace_v1::relay_frame;
use cy_proto::workspace_v1::workspace_direct_service_client::WorkspaceDirectServiceClient;
use cy_proto::workspace_v1::workspace_relay_service_client::WorkspaceRelayServiceClient;
use cy_proto::workspace_v1::{
    DiscoverWorkspacesRequest, RelayForwardedResponse, RelayFrame, RelayHello,
    RelayParticipantRole, UserIdentityRef, WorkspaceApiRequest, WorkspaceApiResponse,
    WorkspaceConnectionCandidate, WorkspaceConnectionDescriptor, WorkspaceDirectRequest,
};
use thiserror::Error;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::Channel;
use tonic::{Request, Streaming};

use crate::{
    dispatch_authenticated_workspace_request, unauthenticated_workspace_response,
    validate_descriptor, WorkspaceApi, WorkspaceCallerContext,
    WORKSPACE_API_GRPC_MESSAGE_MAX_BYTES,
};

const RELAY_RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
const DIRECT_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
// Match the relay's bounded queue so slow consumers backpressure both directions.
// 与 Relay 有界队列一致，使双向慢速消费都能向上游施加背压。
const RELAY_QUEUE_FRAMES: usize = 8;

/// TLS material and public endpoint facts for one outbound relay connection.
///
/// This configuration deliberately contains primitives rather than a route
/// type owned by the execution-fabric implementation crate. A consumer can
/// construct it from environment, configuration, or another resolver without
/// linking Platform implementation code.
#[derive(Clone)]
pub struct RelayClientConfig {
    pub control_endpoint: String,
    /// Expected TLS server name, such as the Relay service identity.
    pub server_name: String,
    /// Roots used to authenticate the remote TLS server, independently of the client certificate.
    pub ca_certificate_pem: Vec<u8>,
    /// Client identity presented to the endpoint; a Workspace device cert does not identify Relay.
    pub client_certificate_pem: Vec<u8>,
    pub client_key_pem: Vec<u8>,
}

/// Relay client failure without credential material in diagnostics.
#[derive(Debug, Error)]
pub enum RelayTransportError {
    #[error("RELAY_ENDPOINT_INVALID: {0}")]
    Endpoint(String),
    #[error("RELAY_TRANSPORT_FAILED: {0}")]
    Transport(String),
    #[error("RELAY_STREAM_CLOSED")]
    Closed,
    #[error("RELAY_RESPONSE_TIMEOUT")]
    ResponseTimeout,
    #[error("RELAY_PROTOCOL_INVALID: {0}")]
    Protocol(String),
}

/// A selected Workspace path. Direct calls never pass through the relay stream.
pub enum WorkspaceConnection {
    Direct {
        client: WorkspaceDirectServiceClient<Channel>,
        hello: RelayHello,
        workspace_id: String,
    },
    Relay {
        session: RelayServingSession,
        workspace_id: String,
    },
}

impl WorkspaceConnection {
    pub fn mode(&self) -> ConnectivityMode {
        match self {
            Self::Direct { .. } => ConnectivityMode::LanDirect,
            Self::Relay { .. } => ConnectivityMode::Relay,
        }
    }

    pub async fn execute(
        &mut self,
        request: WorkspaceApiRequest,
    ) -> Result<WorkspaceApiResponse, RelayTransportError> {
        let workspace_id = match self {
            Self::Direct { workspace_id, .. } | Self::Relay { workspace_id, .. } => workspace_id,
        };
        if request.workspace_id != *workspace_id {
            return Err(RelayTransportError::Protocol(
                "request Workspace does not match the discovered descriptor".to_string(),
            ));
        }
        match self {
            Self::Direct { client, hello, .. } => client
                .execute(WorkspaceDirectRequest {
                    frontend: Some(hello.clone()),
                    request: Some(request),
                })
                .await
                .map(|response| response.into_inner())
                .map_err(|error| RelayTransportError::Transport(error.to_string())),
            Self::Relay { session, .. } => session.execute(request).await,
        }
    }
}

/// Select the first reachable supported descriptor candidate by priority.
///
/// A failed direct connection may fall back to the already authenticated Relay
/// session. Once connected, an authorization or API error is returned to the
/// caller and never retried through another transport.
pub async fn connect_discovered_workspace(
    descriptor: &WorkspaceConnectionDescriptor,
    hello: RelayHello,
    relay_config: &RelayClientConfig,
    relay_session: RelayServingSession,
) -> Result<WorkspaceConnection, RelayTransportError> {
    let now_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX);
    validate_descriptor(descriptor, now_unix_ms)
        .map_err(|error| RelayTransportError::Protocol(error.to_string()))?;
    if descriptor.organization_id != hello.organization_id || hello.user.is_none() {
        return Err(RelayTransportError::Protocol(
            "descriptor and frontend identity do not match".to_string(),
        ));
    }
    let mut candidates: Vec<&WorkspaceConnectionCandidate> = descriptor.candidates.iter().collect();
    candidates.sort_by_key(|candidate| candidate.priority);
    let mut direct_error = None;
    for candidate in candidates {
        match ConnectivityMode::try_from(candidate.mode) {
            Ok(ConnectivityMode::LanDirect) => {
                match connect_candidate(candidate, relay_config).await {
                    Ok(channel) => {
                        tracing::info!(
                            event.name = "platform.workspace.route_selected",
                            workspace_id = %descriptor.workspace_id,
                            mode = "LAN_DIRECT",
                            message = "Workspace direct connection established",
                        );
                        return Ok(WorkspaceConnection::Direct {
                            client: WorkspaceDirectServiceClient::new(channel)
                                .max_decoding_message_size(WORKSPACE_API_GRPC_MESSAGE_MAX_BYTES)
                                .max_encoding_message_size(WORKSPACE_API_GRPC_MESSAGE_MAX_BYTES),
                            hello,
                            workspace_id: descriptor.workspace_id.clone(),
                        });
                    }
                    Err(error) => {
                        tracing::warn!(
                            event.name = "platform.workspace.direct_unreachable",
                            workspace_id = %descriptor.workspace_id,
                            reason = %error,
                            message = "Workspace direct connection unavailable",
                        );
                        direct_error = Some(error);
                    }
                }
            }
            Ok(ConnectivityMode::Relay)
                if candidate.connection_uri == relay_config.control_endpoint
                    && candidate.server_name == relay_config.server_name =>
            {
                tracing::info!(
                    event.name = "platform.workspace.route_selected",
                    workspace_id = %descriptor.workspace_id,
                    mode = "RELAY",
                    message = "Workspace relay connection selected",
                );
                return Ok(WorkspaceConnection::Relay {
                    session: relay_session,
                    workspace_id: descriptor.workspace_id.clone(),
                });
            }
            _ => {}
        }
    }
    Err(direct_error.unwrap_or_else(|| {
        RelayTransportError::Protocol("no usable Workspace transport candidate".to_string())
    }))
}

async fn connect_candidate(
    candidate: &WorkspaceConnectionCandidate,
    credentials: &RelayClientConfig,
) -> Result<Channel, RelayTransportError> {
    if !candidate.connection_uri.starts_with("https://") {
        return Err(RelayTransportError::Endpoint(
            "direct candidate requires HTTPS".to_string(),
        ));
    }
    connect_tls_channel(
        &candidate.connection_uri,
        &candidate.server_name,
        credentials,
        Some(DIRECT_CONNECT_TIMEOUT),
    )
    .await
}

/// One authenticated bidirectional relay session.
pub struct RelayServingSession {
    relay_session_id: String,
    participant_role: RelayParticipantRole,
    outbound: mpsc::Sender<RelayFrame>,
    inbound: Streaming<RelayFrame>,
}

impl RelayServingSession {
    pub fn relay_session_id(&self) -> &str {
        &self.relay_session_id
    }

    /// Discover Workspaces by user identity and membership, never by address.
    pub async fn discover(
        &mut self,
        request_id: impl Into<String>,
        user: UserIdentityRef,
        organization_id: impl Into<String>,
    ) -> Result<Vec<WorkspaceConnectionDescriptor>, RelayTransportError> {
        let request_id = request_id.into();
        self.send(RelayFrame {
            frame_id: request_id.clone(),
            body: Some(relay_frame::Body::DiscoverRequest(
                DiscoverWorkspacesRequest {
                    user: Some(user),
                    organization_id: organization_id.into(),
                },
            )),
        })
        .await?;
        let frame = self.next_matching(&request_id).await?;
        match frame.body {
            Some(relay_frame::Body::DiscoverResponse(response)) => Ok(response.workspaces),
            Some(relay_frame::Body::Error(error)) => {
                Err(RelayTransportError::Protocol(error.message))
            }
            _ => Err(RelayTransportError::Protocol(
                "discovery received an unexpected relay frame".to_string(),
            )),
        }
    }

    /// Send one stable Workspace API request through the relay.
    pub async fn execute(
        &mut self,
        request: WorkspaceApiRequest,
    ) -> Result<WorkspaceApiResponse, RelayTransportError> {
        let request_id = request.request_id.clone();
        self.send(RelayFrame {
            frame_id: request_id.clone(),
            body: Some(relay_frame::Body::WorkspaceRequest(request)),
        })
        .await?;
        let frame = self.next_matching(&request_id).await?;
        match frame.body {
            Some(relay_frame::Body::WorkspaceResponse(response)) => Ok(response),
            Some(relay_frame::Body::Error(error)) => {
                Err(RelayTransportError::Protocol(error.message))
            }
            _ => Err(RelayTransportError::Protocol(
                "Workspace request received an unexpected relay frame".to_string(),
            )),
        }
    }

    /// Serve relayed requests from the authoritative Workspace API.
    pub async fn serve_workspace(
        mut self,
        api: Arc<dyn WorkspaceApi>,
    ) -> Result<(), RelayTransportError> {
        if self.participant_role != RelayParticipantRole::WorkspaceConnector {
            return Err(RelayTransportError::Protocol(
                "only an authenticated Workspace connector stream may serve requests".to_string(),
            ));
        }
        loop {
            let frame = self.next().await?;
            let Some(relay_frame::Body::ForwardedRequest(forwarded)) = frame.body else {
                continue;
            };
            let caller = WorkspaceCallerContext::from_relay_forwarded(&forwarded);
            let Some(request) = forwarded.request else {
                continue;
            };
            let response = match caller {
                Ok(caller) => {
                    dispatch_authenticated_workspace_request(api.as_ref(), request, caller).await
                }
                Err(_) => unauthenticated_workspace_response(request.request_id),
            };
            self.send(RelayFrame {
                frame_id: frame.frame_id,
                body: Some(relay_frame::Body::ForwardedResponse(
                    RelayForwardedResponse {
                        frontend_session_id: forwarded.frontend_session_id,
                        response: Some(response),
                    },
                )),
            })
            .await?;
        }
    }

    async fn send(&mut self, frame: RelayFrame) -> Result<(), RelayTransportError> {
        self.outbound
            .send(frame)
            .await
            .map_err(|_| RelayTransportError::Closed)
    }

    async fn next(&mut self) -> Result<RelayFrame, RelayTransportError> {
        self.inbound
            .message()
            .await
            .map_err(|error| RelayTransportError::Transport(error.to_string()))?
            .ok_or(RelayTransportError::Closed)
    }

    async fn next_matching(&mut self, request_id: &str) -> Result<RelayFrame, RelayTransportError> {
        tokio::time::timeout(RELAY_RESPONSE_TIMEOUT, async {
            loop {
                let frame = self.next().await?;
                if frame.frame_id == request_id {
                    return Ok(frame);
                }
            }
        })
        .await
        .map_err(|_| RelayTransportError::ResponseTimeout)?
    }
}

/// Establish one outbound authenticated relay session.
pub async fn connect_relay_serving_session(
    config: &RelayClientConfig,
    hello: RelayHello,
) -> Result<RelayServingSession, RelayTransportError> {
    let participant_role = RelayParticipantRole::try_from(hello.role).map_err(|_| {
        RelayTransportError::Protocol("relay participant role is invalid".to_string())
    })?;
    let channel = connect_channel(config).await?;
    let mut client = WorkspaceRelayServiceClient::new(channel)
        .max_decoding_message_size(WORKSPACE_API_GRPC_MESSAGE_MAX_BYTES)
        .max_encoding_message_size(WORKSPACE_API_GRPC_MESSAGE_MAX_BYTES);
    let (outbound, receiver) = mpsc::channel(RELAY_QUEUE_FRAMES);
    outbound
        .send(RelayFrame {
            frame_id: "relay-hello".to_string(),
            body: Some(relay_frame::Body::Hello(hello)),
        })
        .await
        .map_err(|_| RelayTransportError::Closed)?;
    let mut inbound = client
        .connect(Request::new(ReceiverStream::new(receiver)))
        .await
        .map_err(|error| RelayTransportError::Transport(error.to_string()))?
        .into_inner();
    let ready = tokio::time::timeout(RELAY_RESPONSE_TIMEOUT, inbound.message())
        .await
        .map_err(|_| RelayTransportError::ResponseTimeout)?
        .map_err(|error| RelayTransportError::Transport(error.to_string()))?
        .ok_or(RelayTransportError::Closed)?;
    let Some(relay_frame::Body::Ready(ready)) = ready.body else {
        return Err(RelayTransportError::Protocol(
            "relay did not acknowledge the authenticated session".to_string(),
        ));
    };
    Ok(RelayServingSession {
        relay_session_id: ready.relay_session_id,
        participant_role,
        outbound,
        inbound,
    })
}

/// Connect once and serve until the relay stream breaks.
pub async fn run_workspace_connector_session(
    config: &RelayClientConfig,
    hello: RelayHello,
    api: Arc<dyn WorkspaceApi>,
) -> Result<String, RelayTransportError> {
    let session = connect_relay_serving_session(config, hello).await?;
    let relay_session_id = session.relay_session_id().to_string();
    session.serve_workspace(api).await?;
    Ok(relay_session_id)
}

async fn connect_channel(config: &RelayClientConfig) -> Result<Channel, RelayTransportError> {
    if !config.control_endpoint.starts_with("https://") || config.server_name.is_empty() {
        return Err(RelayTransportError::Endpoint(
            "relay endpoint requires HTTPS and a server name".to_string(),
        ));
    }
    connect_tls_channel(&config.control_endpoint, &config.server_name, config, None).await
}

async fn connect_tls_channel(
    address: &str,
    server_name: &str,
    credentials: &RelayClientConfig,
    connect_timeout: Option<Duration>,
) -> Result<Channel, RelayTransportError> {
    connect_mtls_channel(
        address,
        server_name,
        &credentials.ca_certificate_pem,
        &credentials.client_certificate_pem,
        &credentials.client_key_pem,
        connect_timeout,
    )
    .await
    .map_err(|error| match error {
        MtlsChannelConnectError::Endpoint(message) => RelayTransportError::Endpoint(message),
        MtlsChannelConnectError::Transport(message) => RelayTransportError::Transport(message),
    })
}
