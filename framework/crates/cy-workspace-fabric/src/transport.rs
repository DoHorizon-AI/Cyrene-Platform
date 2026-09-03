//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 transport.rs                                                    │
//! │  Module: cy_workspace_fabric::transport                             │
//! │  Role: Outbound mTLS relay clients for frontends and Workspaces.    │
//! │                                                                     │
//! │  模块职责：Frontend 与 Workspace 的出站 mTLS Relay 客户端。             │
//! └─────────────────────────────────────────────────────────────────────┘

use std::sync::Arc;
use std::time::Duration;

use cy_execution_fabric::ConnectivityRoute;
use cy_proto::workspace_v1::relay_frame;
use cy_proto::workspace_v1::workspace_relay_service_client::WorkspaceRelayServiceClient;
use cy_proto::workspace_v1::{
    DiscoverWorkspacesRequest, RelayForwardedResponse, RelayFrame, RelayHello, UserIdentityRef,
    WorkspaceApiRequest, WorkspaceApiResponse, WorkspaceConnectionDescriptor,
};
use thiserror::Error;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint, Identity};
use tonic::{Request, Streaming};

use crate::WorkspaceApi;

const RELAY_RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

/// TLS material and resolved route for one outbound relay connection.
#[derive(Clone)]
pub struct RelayClientConfig {
    pub route: ConnectivityRoute,
    pub ca_certificate_pem: Vec<u8>,
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

/// One authenticated bidirectional relay session.
pub struct RelaySession {
    relay_session_id: String,
    outbound: mpsc::Sender<RelayFrame>,
    inbound: Streaming<RelayFrame>,
}

impl RelaySession {
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
        loop {
            let frame = self.next().await?;
            let Some(relay_frame::Body::ForwardedRequest(forwarded)) = frame.body else {
                continue;
            };
            let Some(request) = forwarded.request else {
                continue;
            };
            let response = api.handle(request).await;
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

    async fn send(&self, frame: RelayFrame) -> Result<(), RelayTransportError> {
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
pub async fn connect_relay_session(
    config: &RelayClientConfig,
    hello: RelayHello,
) -> Result<RelaySession, RelayTransportError> {
    let channel = connect_channel(config).await?;
    let mut client = WorkspaceRelayServiceClient::new(channel);
    let (outbound, receiver) = mpsc::channel(128);
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
    Ok(RelaySession {
        relay_session_id: ready.relay_session_id,
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
    let session = connect_relay_session(config, hello).await?;
    let relay_session_id = session.relay_session_id().to_string();
    session.serve_workspace(api).await?;
    Ok(relay_session_id)
}

async fn connect_channel(config: &RelayClientConfig) -> Result<Channel, RelayTransportError> {
    if config.route.control_endpoint.is_empty()
        || config.route.server_name.is_empty()
        || !config.route.outbound_only
    {
        return Err(RelayTransportError::Endpoint(
            "relay route must be outbound and include endpoint/server name".to_string(),
        ));
    }
    let endpoint = Endpoint::from_shared(config.route.control_endpoint.clone())
        .map_err(|error| RelayTransportError::Endpoint(error.to_string()))?;
    let tls = ClientTlsConfig::new()
        .domain_name(config.route.server_name.clone())
        .ca_certificate(Certificate::from_pem(config.ca_certificate_pem.clone()))
        .identity(Identity::from_pem(
            config.client_certificate_pem.clone(),
            config.client_key_pem.clone(),
        ));
    endpoint
        .tls_config(tls)
        .map_err(|error| RelayTransportError::Transport(error.to_string()))?
        .connect()
        .await
        .map_err(|error| RelayTransportError::Transport(error.to_string()))
}
