//! Typed bridge from the external Node Agent to the local Kernel UDS service.
//!
//! This module contains no scheduling policy and accepts no shell, argv, or
//! environment payloads. It forwards only the versioned `KernelCommand` oneof
//! over the protected local KernelService socket.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use cy_proto::{
    core_v1::{
        kernel_command, kernel_command_result, kernel_service_client::KernelServiceClient,
        ControlPlaneToNode, GetKernelCapabilitiesRequest, KernelCommand, KernelCommandResult,
        NodeRef,
    },
    google::rpc::Status as RpcStatus,
};
use thiserror::Error;
#[cfg(unix)]
use tonic::transport::Endpoint;
use tonic::{Code, Request, Status};

use crate::{NodeControlSession, NodeControlSessionError};

#[derive(Debug, Error)]
pub enum LocalKernelError {
    #[error("Kernel UDS path must be absolute")]
    RelativeSocketPath,
    #[error("local Kernel UDS is unavailable on this host")]
    UnsupportedHost,
    #[error("local Kernel transport failed: {0}")]
    Transport(String),
    #[error("local Kernel did not return its NodeRef")]
    MissingNodeRef,
}

/// The narrow external Agent port. Tests can replace the UDS implementation,
/// while the production Agent keeps command mapping outside the Kernel crate.
#[async_trait]
pub trait KernelCommandExecutor: Send + Sync {
    async fn execute(&self, command: KernelCommand) -> KernelCommandResult;
}

/// A strict UDS implementation of the Agent's typed Kernel-command port.
#[derive(Debug, Clone)]
pub struct UdsKernelCommandExecutor {
    socket_path: PathBuf,
}

impl UdsKernelCommandExecutor {
    pub fn new(socket_path: impl Into<PathBuf>) -> Result<Self, LocalKernelError> {
        let socket_path = socket_path.into();
        if !socket_path.is_absolute() {
            return Err(LocalKernelError::RelativeSocketPath);
        }
        Ok(Self { socket_path })
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// The local Kernel is authoritative for the active node epoch. The Agent
    /// refreshes it before every outbound control-stream attempt, so a Kernel
    /// restart cannot leave the Agent presenting an old epoch to the control
    /// plane.
    pub async fn discover_node(&self) -> Result<NodeRef, LocalKernelError> {
        let mut client = self.connect_client().await?;
        let capabilities = client
            .get_kernel_capabilities(Request::new(GetKernelCapabilitiesRequest {
                context: None,
                node: None,
            }))
            .await
            .map_err(status_error)?
            .into_inner();
        capabilities.node.ok_or(LocalKernelError::MissingNodeRef)
    }

    async fn execute_inner(
        &self,
        command: KernelCommand,
    ) -> Result<KernelCommandResult, LocalKernelError> {
        let command_id = command.command_id;
        let request = command.request.ok_or_else(|| {
            LocalKernelError::Transport("KernelCommand request oneof is required".to_string())
        })?;
        let mut client = self.connect_client().await?;
        let outcome = match request {
            kernel_command::Request::GetCapabilities(request) => client
                .get_kernel_capabilities(Request::new(request))
                .await
                .map(|response| {
                    kernel_command_result::Outcome::Capabilities(response.into_inner())
                }),
            kernel_command::Request::ReserveResources(request) => client
                .reserve_resources(Request::new(request))
                .await
                .map(|response| {
                    kernel_command_result::Outcome::ResourceLease(response.into_inner())
                }),
            kernel_command::Request::ReleaseResources(request) => client
                .release_resources(Request::new(request))
                .await
                .map(|response| {
                    kernel_command_result::Outcome::ResourceLease(response.into_inner())
                }),
            kernel_command::Request::LaunchPlugin(request) => client
                .launch_plugin(Request::new(request))
                .await
                .map(|response| kernel_command_result::Outcome::Operation(response.into_inner())),
            kernel_command::Request::TerminatePlugin(request) => client
                .terminate_plugin(Request::new(request))
                .await
                .map(|response| kernel_command_result::Outcome::Operation(response.into_inner())),
            kernel_command::Request::CancelOperation(request) => client
                .cancel_operation(Request::new(request))
                .await
                .map(|response| kernel_command_result::Outcome::Operation(response.into_inner())),
        };
        Ok(KernelCommandResult {
            command_id,
            outcome: Some(match outcome {
                Ok(outcome) => outcome,
                Err(status) => kernel_command_result::Outcome::Error(rpc_status(status)),
            }),
        })
    }

    #[cfg(unix)]
    async fn connect_client(
        &self,
    ) -> Result<KernelServiceClient<tonic::transport::Channel>, LocalKernelError> {
        use tokio::net::UnixStream;
        use tower::service_fn;

        let socket_path = self.socket_path.clone();
        let channel = Endpoint::from_static("http://[::]:50051")
            .connect_with_connector(service_fn(move |_| {
                UnixStream::connect(socket_path.clone())
            }))
            .await
            .map_err(|error| LocalKernelError::Transport(error.to_string()))?;
        Ok(KernelServiceClient::new(channel))
    }

    #[cfg(not(unix))]
    async fn connect_client(
        &self,
    ) -> Result<KernelServiceClient<tonic::transport::Channel>, LocalKernelError> {
        Err(LocalKernelError::UnsupportedHost)
    }
}

#[async_trait]
impl KernelCommandExecutor for UdsKernelCommandExecutor {
    async fn execute(&self, command: KernelCommand) -> KernelCommandResult {
        let command_id = command.command_id.clone();
        match self.execute_inner(command).await {
            Ok(result) => result,
            Err(error) => rejected_result(command_id, Code::Unavailable, error.to_string()),
        }
    }
}

/// Binds command admission and result framing to the currently fenced session.
#[derive(Debug, Clone)]
pub struct NodeCommandBridge<E> {
    executor: E,
}

impl<E> NodeCommandBridge<E> {
    pub fn new(executor: E) -> Self {
        Self { executor }
    }
}

impl<E: KernelCommandExecutor> NodeCommandBridge<E> {
    pub async fn forward(
        &self,
        session: &mut NodeControlSession,
        frame: ControlPlaneToNode,
    ) -> Result<cy_proto::core_v1::NodeToControlPlane, NodeControlSessionError> {
        let command = session.accept_command(frame)?;
        let result = self.executor.execute(command).await;
        session.command_result(result)
    }
}

fn status_error(status: Status) -> LocalKernelError {
    LocalKernelError::Transport(format!("{}: {}", status.code(), status.message()))
}

fn rejected_result(command_id: String, code: Code, message: String) -> KernelCommandResult {
    KernelCommandResult {
        command_id,
        outcome: Some(kernel_command_result::Outcome::Error(RpcStatus {
            code: code as i32,
            message,
            details: Vec::new(),
        })),
    }
}

fn rpc_status(status: Status) -> RpcStatus {
    RpcStatus {
        code: status.code() as i32,
        message: status.message().to_string(),
        details: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cy_proto::core_v1::{
        control_plane_to_node, node_to_control_plane, ControlPlaneToNode, NodeWelcome,
    };

    #[derive(Debug, Clone)]
    struct FakeKernel;

    #[async_trait]
    impl KernelCommandExecutor for FakeKernel {
        async fn execute(&self, command: KernelCommand) -> KernelCommandResult {
            KernelCommandResult {
                command_id: command.command_id,
                outcome: Some(kernel_command_result::Outcome::Error(RpcStatus {
                    code: Code::Unimplemented as i32,
                    message: "fake Kernel".to_string(),
                    details: Vec::new(),
                })),
            }
        }
    }

    fn established_session() -> NodeControlSession {
        let mut session = NodeControlSession::new("node-1", 7, "0.1.0", 1, 1, "resume-1");
        session.hello();
        session
            .accept_welcome(ControlPlaneToNode {
                frame_id: "welcome".to_string(),
                sequence_number: 1,
                session_id: "session-1".to_string(),
                body: Some(control_plane_to_node::Body::Welcome(NodeWelcome {
                    session_id: "session-1".to_string(),
                    selected_protocol_version: 1,
                    desired_generation: 3,
                    heartbeat_interval: None,
                    server_time: None,
                    resume_token: "resume-2".to_string(),
                })),
            })
            .unwrap();
        session
    }

    #[test]
    fn bridge_forwards_only_a_fenced_typed_command() {
        let bridge = NodeCommandBridge::new(FakeKernel);
        let mut session = established_session();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let outbound = runtime
            .block_on(bridge.forward(
                &mut session,
                ControlPlaneToNode {
                    frame_id: "command-1".to_string(),
                    sequence_number: 2,
                    session_id: "session-1".to_string(),
                    body: Some(control_plane_to_node::Body::Command(KernelCommand {
                        command_id: "command-1".to_string(),
                        request: None,
                    })),
                },
            ))
            .unwrap();
        assert_eq!(outbound.session_id, "session-1");
        assert!(matches!(
            outbound.body,
            Some(node_to_control_plane::Body::CommandResult(result))
                if result.command_id == "command-1"
        ));
    }

    #[test]
    fn relative_kernel_socket_is_rejected() {
        assert!(matches!(
            UdsKernelCommandExecutor::new("kernel.sock"),
            Err(LocalKernelError::RelativeSocketPath)
        ));
    }
}
