// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: agents/node/cy-node-agent/src/bridge.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! Typed bridge from the external Node Agent to the local Kernel UDS service.
//!
//! This module contains no scheduling policy and accepts no shell, argv, or
//! environment payloads. It forwards only the versioned `KernelCommand` oneof
//! over the protected local KernelService socket.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use cy_proto::{
    core_v1::{
        kernel_authority_command, kernel_authority_command_result,
        kernel_authority_service_client::KernelAuthorityServiceClient, kernel_command,
        kernel_command_result, kernel_service_client::KernelServiceClient, ControlPlaneToNode,
        GetKernelCapabilitiesRequest, KernelAuthorityCommand, KernelAuthorityCommandResult,
        KernelCommand, KernelCommandResult, NodeRef,
    },
    google::rpc::Status as RpcStatus,
    semantic_v1::Rejection,
};
use prost::Message;
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
        let mut client = KernelServiceClient::new(self.connect_channel().await?);
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
        let channel = self.connect_channel().await?;
        let mut client = KernelServiceClient::new(channel.clone());
        let mut authority = KernelAuthorityServiceClient::new(channel);
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
            kernel_command::Request::Authority(command) => {
                execute_authority_command(&mut authority, command)
                    .await
                    .map(kernel_command_result::Outcome::Authority)
            }
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
    async fn connect_channel(&self) -> Result<tonic::transport::Channel, LocalKernelError> {
        use tokio::net::UnixStream;
        use tower::service_fn;

        let socket_path = self.socket_path.clone();
        let channel = Endpoint::from_static("http://[::]:50051")
            .connect_with_connector(service_fn(move |_| {
                UnixStream::connect(socket_path.clone())
            }))
            .await
            .map_err(|error| LocalKernelError::Transport(error.to_string()))?;
        Ok(channel)
    }

    #[cfg(not(unix))]
    async fn connect_channel(&self) -> Result<tonic::transport::Channel, LocalKernelError> {
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
    let reason_code = status
        .metadata()
        .get("x-cyrene-reason-code")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    RpcStatus {
        code: status.code() as i32,
        message: status.message().to_string(),
        details: reason_code
            .map(|reason_code| prost_types::Any {
                type_url: "type.googleapis.com/cyrene.semantic.v1.Rejection".to_string(),
                value: Rejection {
                    reason_code,
                    message: status.message().to_string(),
                }
                .encode_to_vec(),
            })
            .into_iter()
            .collect(),
    }
}

async fn execute_authority_command(
    client: &mut KernelAuthorityServiceClient<tonic::transport::Channel>,
    command: KernelAuthorityCommand,
) -> Result<KernelAuthorityCommandResult, Status> {
    let request = command.request.ok_or_else(|| {
        Status::invalid_argument("KernelAuthorityCommand request oneof is required")
    })?;
    let outcome = match request {
        kernel_authority_command::Request::Negotiate(request) => client
            .negotiate(Request::new(request))
            .await
            .map(|response| {
                kernel_authority_command_result::Outcome::NegotiatedContract(response.into_inner())
            }),
        kernel_authority_command::Request::AcquireLease(request) => client
            .acquire_lease(Request::new(request))
            .await
            .map(|response| kernel_authority_command_result::Outcome::Lease(response.into_inner())),
        kernel_authority_command::Request::RenewLease(request) => client
            .renew_lease(Request::new(request))
            .await
            .map(|response| kernel_authority_command_result::Outcome::Lease(response.into_inner())),
        kernel_authority_command::Request::ReleaseLease(request) => client
            .release_lease(Request::new(request))
            .await
            .map(|response| kernel_authority_command_result::Outcome::Lease(response.into_inner())),
        kernel_authority_command::Request::StartWorker(request) => client
            .start_worker(Request::new(request))
            .await
            .map(|response| {
                kernel_authority_command_result::Outcome::Operation(response.into_inner())
            }),
        kernel_authority_command::Request::HeartbeatWorker(request) => client
            .heartbeat_worker(Request::new(request))
            .await
            .map(|response| {
                kernel_authority_command_result::Outcome::Worker(response.into_inner())
            }),
        kernel_authority_command::Request::StopWorker(request) => client
            .stop_worker(Request::new(request))
            .await
            .map(|response| {
                kernel_authority_command_result::Outcome::Operation(response.into_inner())
            }),
        kernel_authority_command::Request::CreateOperation(request) => client
            .create_operation(Request::new(request))
            .await
            .map(|response| {
                kernel_authority_command_result::Outcome::Operation(response.into_inner())
            }),
        kernel_authority_command::Request::ReportOperation(request) => client
            .report_operation(Request::new(request))
            .await
            .map(|response| {
                kernel_authority_command_result::Outcome::Operation(response.into_inner())
            }),
        kernel_authority_command::Request::CancelOperation(request) => client
            .cancel_operation(Request::new(request))
            .await
            .map(|response| {
                kernel_authority_command_result::Outcome::Operation(response.into_inner())
            }),
        kernel_authority_command::Request::PublishEndpoint(request) => client
            .publish_endpoint(Request::new(request))
            .await
            .map(|response| {
                kernel_authority_command_result::Outcome::Endpoint(response.into_inner())
            }),
        kernel_authority_command::Request::AuthorizeEndpoint(request) => client
            .authorize_endpoint(Request::new(request))
            .await
            .map(|response| {
                kernel_authority_command_result::Outcome::EndpointGrant(response.into_inner())
            }),
        kernel_authority_command::Request::RevokeEndpoint(request) => client
            .revoke_endpoint(Request::new(request))
            .await
            .map(|_| kernel_authority_command_result::Outcome::Revoked(true)),
        kernel_authority_command::Request::SubscribeEvents(request) => {
            let mut stream = client
                .subscribe_events(Request::new(request))
                .await?
                .into_inner();
            let mut events = Vec::new();
            while let Ok(Some(Ok(event))) = tokio::time::timeout(
                std::time::Duration::from_millis(20),
                tokio_stream::StreamExt::next(&mut stream),
            )
            .await
            {
                events.push(event);
            }
            Ok(kernel_authority_command_result::Outcome::EventPage(
                cy_proto::semantic_v1::EventPage {
                    source: None,
                    status: cy_proto::semantic_v1::ReplayStatus::Current as i32,
                    events,
                    oldest_available_sequence: 0,
                    latest_available_sequence: 0,
                    next_sequence: 0,
                },
            ))
        }
    }?;
    Ok(KernelAuthorityCommandResult {
        outcome: Some(outcome),
    })
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
                    selected_contract: Some(cy_proto::semantic_v1::ContractRevision {
                        contract_id: "cyrene.kernel.semantic".to_string(),
                        major: 1,
                        minor: 0,
                    }),
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

    #[test]
    fn authority_rejection_reason_is_preserved_as_a_semantic_detail() {
        let mut status = Status::failed_precondition("lease fence is stale");
        status
            .metadata_mut()
            .insert("x-cyrene-reason-code", "FENCE_MISMATCH".parse().unwrap());
        let status = rpc_status(status);
        let rejection = Rejection::decode(status.details[0].value.as_slice()).unwrap();
        assert_eq!(rejection.reason_code, "FENCE_MISMATCH");
    }
}
