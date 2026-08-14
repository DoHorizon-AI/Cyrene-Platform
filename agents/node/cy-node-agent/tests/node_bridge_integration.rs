use std::{collections::HashMap, time::Duration};

use cy_node_agent::{
    NodeCommandBridge, NodeControlSession, NodeControlSessionError, UdsKernelCommandExecutor,
};
use cy_proto::{
    core_v1::{
        control_plane_to_node,
        kernel_authority_command, kernel_authority_command_result,
        kernel_authority_service_server::{KernelAuthorityService, KernelAuthorityServiceServer},
        kernel_command, kernel_command_result,
        kernel_service_server::{KernelService, KernelServiceServer},
        node_to_control_plane, CancelOperationRequest, ControlPlaneToNode,
        GetKernelCapabilitiesRequest, GetOperationRequest, KernelAuthorityCommand,
        KernelCapabilities, KernelCommand, LaunchPluginRequest, NegotiateRequest, NodeRef,
        NodeWelcome, Operation, ReleaseLeaseRequest, ReleaseResourcesRequest,
        ReserveResourcesRequest, ResourceLease, StartWorkerRequest, StopWorkerRequest,
        TerminatePluginRequest, WatchOperationsRequest,
    },
    semantic_v1::{ContractRevision, OperationState, WorkerState},
};
use tempfile::tempdir;
use tokio::net::UnixListener;
use tokio_stream::wrappers::UnixListenerStream;
use tonic::{Request, Response, Status};

// --- Mock Kernel gRPC Server Implementation on local UDS ---

#[derive(Default)]
struct MockKernelService {
    node_id: String,
    node_epoch: u64,
}

#[tonic::async_trait]
impl KernelService for MockKernelService {
    async fn get_kernel_capabilities(
        &self,
        _request: Request<GetKernelCapabilitiesRequest>,
    ) -> Result<Response<KernelCapabilities>, Status> {
        Ok(Response::new(KernelCapabilities {
            node: Some(NodeRef {
                node_id: self.node_id.clone(),
                node_epoch: self.node_epoch,
            }),
            kernel_version: "1.0.0".to_string(),
            inventory_generation: 1,
            observed_at: None,
            capacity: None,
            accelerators: vec![],
            sandbox_backends: vec!["cgroupv2-linux".to_string()],
            enforcement: vec![],
            feature_flags: vec!["cgroup-v2".to_string()],
            resources: vec![],
        }))
    }

    async fn reserve_resources(
        &self,
        request: Request<ReserveResourcesRequest>,
    ) -> Result<Response<ResourceLease>, Status> {
        let req = request.into_inner();
        Ok(Response::new(ResourceLease {
            name: "lease-model-eval".to_string(),
            node: req.node,
            state: cy_proto::core_v1::LeaseState::Active as i32,
            granted: req.requirements,
            accelerators: vec![],
            enforcement: vec![],
            expires_at: None,
            fence_token: 100,
            inventory_generation: 1,
        }))
    }

    async fn acquire_lease(
        &self,
        _request: Request<cy_proto::core_v1::AcquireLeaseRequest>,
    ) -> Result<Response<cy_proto::semantic_v1::Lease>, Status> {
        Err(Status::unimplemented("not needed in this test"))
    }

    async fn release_resources(
        &self,
        _request: Request<ReleaseResourcesRequest>,
    ) -> Result<Response<ResourceLease>, Status> {
        Ok(Response::new(ResourceLease {
            name: "test-lease".to_string(),
            node: None,
            state: cy_proto::core_v1::LeaseState::Released as i32,
            granted: None,
            accelerators: vec![],
            enforcement: vec![],
            expires_at: None,
            fence_token: 100,
            inventory_generation: 1,
        }))
    }

    async fn release_lease(
        &self,
        _request: Request<ReleaseLeaseRequest>,
    ) -> Result<Response<cy_proto::semantic_v1::Lease>, Status> {
        Err(Status::unimplemented("not needed in this test"))
    }

    async fn launch_plugin(
        &self,
        _request: Request<LaunchPluginRequest>,
    ) -> Result<Response<Operation>, Status> {
        Err(Status::unimplemented("not needed in this test"))
    }

    async fn terminate_plugin(
        &self,
        _request: Request<TerminatePluginRequest>,
    ) -> Result<Response<Operation>, Status> {
        Err(Status::unimplemented("not needed in this test"))
    }

    async fn get_operation(
        &self,
        _request: Request<GetOperationRequest>,
    ) -> Result<Response<Operation>, Status> {
        Err(Status::unimplemented("not needed in this test"))
    }

    async fn cancel_operation(
        &self,
        _request: Request<CancelOperationRequest>,
    ) -> Result<Response<Operation>, Status> {
        Err(Status::unimplemented("not needed in this test"))
    }

    type WatchOperationsStream = tonic::codec::Streaming<cy_proto::core_v1::OperationEvent>;
    async fn watch_operations(
        &self,
        _request: Request<WatchOperationsRequest>,
    ) -> Result<Response<Self::WatchOperationsStream>, Status> {
        Err(Status::unimplemented("not needed in this test"))
    }
}

#[derive(Default)]
struct MockKernelAuthorityService;

#[tonic::async_trait]
impl KernelAuthorityService for MockKernelAuthorityService {
    async fn negotiate(
        &self,
        _request: Request<NegotiateRequest>,
    ) -> Result<Response<ContractRevision>, Status> {
        Ok(Response::new(ContractRevision {
            contract_id: "cyrene.kernel.semantic".to_string(),
            major: 1,
            minor: 0,
        }))
    }

    async fn acquire_lease(
        &self,
        _request: Request<cy_proto::core_v1::AcquireSemanticLeaseRequest>,
    ) -> Result<Response<cy_proto::semantic_v1::Lease>, Status> {
        Err(Status::unimplemented("not needed in this test"))
    }

    async fn renew_lease(
        &self,
        _request: Request<cy_proto::core_v1::RenewLeaseRequest>,
    ) -> Result<Response<cy_proto::semantic_v1::Lease>, Status> {
        Err(Status::unimplemented("not needed in this test"))
    }

    async fn release_lease(
        &self,
        _request: Request<cy_proto::core_v1::ReleaseSemanticLeaseRequest>,
    ) -> Result<Response<cy_proto::semantic_v1::Lease>, Status> {
        Err(Status::unimplemented("not needed in this test"))
    }

    async fn start_worker(
        &self,
        _request: Request<StartWorkerRequest>,
    ) -> Result<Response<cy_proto::semantic_v1::Operation>, Status> {
        Ok(Response::new(cy_proto::semantic_v1::Operation {
            identity: Some(cy_proto::semantic_v1::Identity {
                id: "op-start-worker-1".to_string(),
                generation: 1,
            }),
            owner: None,
            executor: None,
            kind: "START_WORKER".to_string(),
            state: OperationState::Succeeded as i32,
            deadline: None,
            parent: None,
            metadata: HashMap::new(),
        }))
    }

    async fn heartbeat_worker(
        &self,
        _request: Request<cy_proto::core_v1::HeartbeatWorkerRequest>,
    ) -> Result<Response<cy_proto::semantic_v1::Worker>, Status> {
        Err(Status::unimplemented("not needed in this test"))
    }

    async fn stop_worker(
        &self,
        _request: Request<StopWorkerRequest>,
    ) -> Result<Response<cy_proto::semantic_v1::Operation>, Status> {
        Err(Status::unimplemented("not needed in this test"))
    }

    async fn create_operation(
        &self,
        _request: Request<cy_proto::core_v1::CreateOperationRequest>,
    ) -> Result<Response<cy_proto::semantic_v1::Operation>, Status> {
        Err(Status::unimplemented("not needed in this test"))
    }

    async fn report_operation(
        &self,
        _request: Request<cy_proto::core_v1::ReportOperationRequest>,
    ) -> Result<Response<cy_proto::semantic_v1::Operation>, Status> {
        Err(Status::unimplemented("not needed in this test"))
    }

    async fn cancel_operation(
        &self,
        _request: Request<cy_proto::core_v1::CancelSemanticOperationRequest>,
    ) -> Result<Response<cy_proto::semantic_v1::Operation>, Status> {
        Err(Status::unimplemented("not needed in this test"))
    }

    async fn publish_endpoint(
        &self,
        _request: Request<cy_proto::core_v1::PublishEndpointRequest>,
    ) -> Result<Response<cy_proto::semantic_v1::Endpoint>, Status> {
        Err(Status::unimplemented("not needed in this test"))
    }

    async fn authorize_endpoint(
        &self,
        _request: Request<cy_proto::core_v1::AuthorizeEndpointRequest>,
    ) -> Result<Response<cy_proto::semantic_v1::EndpointGrant>, Status> {
        Err(Status::unimplemented("not needed in this test"))
    }

    async fn revoke_endpoint(
        &self,
        _request: Request<cy_proto::core_v1::RevokeEndpointRequest>,
    ) -> Result<Response<()>, Status> {
        Ok(Response::new(()))
    }

    async fn subscribe_events(
        &self,
        _request: Request<cy_proto::core_v1::SubscribeEventsRequest>,
    ) -> Result<Response<cy_proto::semantic_v1::EventPage>, Status> {
        Err(Status::unimplemented("not needed in this test"))
    }
}

// --- End-to-End Integration Test ---

#[tokio::test]
async fn test_node_agent_uds_bridge_end_to_end() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let kernel_socket = dir.path().join("kernel.sock");

    // 1. Bind and start mock Kernel UDS gRPC Server
    let listener = UnixListener::bind(&kernel_socket)?;
    let mock_kernel = MockKernelService {
        node_id: "node-worker-alpha".to_string(),
        node_epoch: 10,
    };
    let mock_authority = MockKernelAuthorityService;

    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(KernelServiceServer::new(mock_kernel))
            .add_service(KernelAuthorityServiceServer::new(mock_authority))
            .serve_with_incoming(UnixListenerStream::new(listener))
            .await
            .unwrap();
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    // 2. Initialize Node Agent's UdsKernelCommandExecutor and Bridge
    let executor = UdsKernelCommandExecutor::new(&kernel_socket)?;
    let bridge = NodeCommandBridge::new(executor.clone());

    // 3. Test Node Discovery from local Kernel UDS
    let discovered_node = executor.discover_node().await?;
    assert_eq!(discovered_node.node_id, "node-worker-alpha");
    assert_eq!(discovered_node.node_epoch, 10);

    // 4. Establish a Fenced NodeControlSession
    let mut session = NodeControlSession::new(
        "node-worker-alpha",
        10,
        "1.0.0".to_string(),
        1,
        1,
        "",
    );
    session.hello();

    let welcome_frame = ControlPlaneToNode {
        frame_id: "welcome-1".to_string(),
        sequence_number: 1,
        session_id: "sess-12345".to_string(),
        body: Some(control_plane_to_node::Body::Welcome(NodeWelcome {
            session_id: "sess-12345".to_string(),
            selected_protocol_version: 1,
            desired_generation: 1,
            heartbeat_interval: Some(prost_types::Duration {
                seconds: 5,
                nanos: 0,
            }),
            server_time: None,
            resume_token: "res-token-1".to_string(),
            selected_contract: Some(ContractRevision {
                contract_id: "cyrene.kernel.semantic".to_string(),
                major: 1,
                minor: 0,
            }),
        })),
    };
    session.accept_welcome(welcome_frame)?;

    // 5. Forward a ReserveResources command from Control Plane through the Bridge to Kernel UDS
    let reserve_frame = ControlPlaneToNode {
        frame_id: "frame-cmd-1".to_string(),
        sequence_number: 2,
        session_id: "sess-12345".to_string(),
        body: Some(control_plane_to_node::Body::Command(
            KernelCommand {
                command_id: "cmd-res-1".to_string(),
                request: Some(kernel_command::Request::ReserveResources(
                    ReserveResourcesRequest {
                        node: Some(NodeRef {
                            node_id: "node-worker-alpha".to_string(),
                            node_epoch: 10,
                        }),
                        mutation: None,
                        requirements: None,
                        ttl: None,
                    },
                )),
            },
        )),
    };

    let response_frame = bridge.forward(&mut session, reserve_frame).await?;
    assert_eq!(response_frame.session_id, "sess-12345");

    match response_frame.body {
        Some(node_to_control_plane::Body::CommandResult(res)) => {
            assert_eq!(res.command_id, "cmd-res-1");
            match res.outcome {
                Some(kernel_command_result::Outcome::ResourceLease(lease)) => {
                    assert_eq!(lease.name, "lease-model-eval");
                    assert_eq!(lease.fence_token, 100);
                }
                other => panic!("expected ResourceLease outcome, got {other:?}"),
            }
        }
        other => panic!("expected CommandResult body, got {other:?}"),
    }

    // 6. Forward a KernelAuthorityService StartWorker command through the Bridge to Kernel UDS
    let start_worker_frame = ControlPlaneToNode {
        frame_id: "frame-cmd-2".to_string(),
        sequence_number: 3,
        session_id: "sess-12345".to_string(),
        body: Some(control_plane_to_node::Body::Command(
            KernelCommand {
                command_id: "cmd-start-worker-1".to_string(),
                request: Some(kernel_command::Request::Authority(
                    KernelAuthorityCommand {
                        request: Some(kernel_authority_command::Request::StartWorker(
                            StartWorkerRequest {
                                worker: Some(cy_proto::semantic_v1::Worker {
                                    identity: Some(cy_proto::semantic_v1::Identity {
                                        id: "worker-embed-01".to_string(),
                                        generation: 1,
                                    }),
                                    principal: None,
                                    provider: None,
                                    lease: None,
                                    state: WorkerState::Running as i32,
                                    execution_ref: String::new(),
                                    limits: HashMap::new(),
                                }),
                                context: None,
                            },
                        )),
                    },
                )),
            },
        )),
    };

    let start_worker_response = bridge.forward(&mut session, start_worker_frame).await?;
    match start_worker_response.body {
        Some(node_to_control_plane::Body::CommandResult(res)) => {
            assert_eq!(res.command_id, "cmd-start-worker-1");
            match res.outcome {
                Some(kernel_command_result::Outcome::Authority(auth_res)) => {
                    match auth_res.outcome {
                        Some(kernel_authority_command_result::Outcome::Operation(op)) => {
                            assert_eq!(op.kind, "START_WORKER");
                            assert_eq!(op.state, OperationState::Succeeded as i32);
                        }
                        other => panic!("expected Operation outcome, got {other:?}"),
                    }
                }
                other => panic!("expected Authority outcome, got {other:?}"),
            }
        }
        other => panic!("expected CommandResult body, got {other:?}"),
    }

    // 7. Verify session fencing rejects old sequence numbers or stale session IDs
    let stale_frame = ControlPlaneToNode {
        frame_id: "frame-stale".to_string(),
        sequence_number: 2, // sequence already consumed!
        session_id: "sess-12345".to_string(),
        body: Some(control_plane_to_node::Body::Command(
            KernelCommand {
                command_id: "cmd-stale".to_string(),
                request: None,
            },
        )),
    };
    let fence_err = bridge.forward(&mut session, stale_frame).await.unwrap_err();
    assert_eq!(
        fence_err,
        NodeControlSessionError::StaleControlFrame {
            received: 2,
            accepted: 3
        }
    );

    Ok(())
}
