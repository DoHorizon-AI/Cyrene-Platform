// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: execution_assignment_tck.rs                                ║
// ║ Module: cy_execution_control integration tests                      ║
// ║ Role: Canonical Kernel Lease to Runtime assignment conformance.      ║
// ║                                                                      ║
// ║ 模块：cy_execution_control 集成测试                                  ║
// ║ 职责：验证 canonical Kernel Lease 到 Runtime assignment 的真实路由。  ║
// ╚══════════════════════════════════════════════════════════════════════╝

#![cfg(unix)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cy_execution_control::{
    AuthenticatedAgent, DispatchError, ExecutionControlService, ExecutionController,
    ExecutionDispatchRequest, ExecutionReleaseRequest, InMemoryExecutionIntentStore,
    IntentDisposition, PeerAuthenticator, RuntimeLauncher,
};
use cy_execution_fabric::{
    execution_capability, validate_assignment, DevelopmentEnrollmentProvider,
    ExecutionPlacementRequest, ExecutionTargetCandidate, NetworkRequirements, PlacementPolicy,
    RuntimeAssignmentBuilder, RuntimeScope,
};
use cy_kernel_api::{
    AuthorityCallContext, CleanupReport, DeviceBinding, EnforcementMode, HostInventoryProvider,
    InstalledPluginResolver, InventorySnapshot, LaunchPlan, NamespaceId, NodeCapabilities,
    ProcessHandle, ProcessRuntime, ProviderError, ResourceLeaseManager, ResourceProvider,
    SandboxBackend, StopRequest, VerifiedInstallation,
};
use cy_kernel_contract as semantic;
use cy_kernel_daemon::{
    peer_cred::{inject_authority_principal, PeerCredAccept},
    KernelDaemon, KernelServiceAdapter,
};
use cy_node_agent::{NodeCommandBridge, NodeControlSession, UdsKernelCommandExecutor};
use cy_proto::core_v1::{
    self, control_plane_to_node, node_control_service_client::NodeControlServiceClient,
    node_control_service_server::NodeControlServiceServer, node_to_control_plane, AssignmentAck,
    AssignmentAckDisposition, ExecutionAgentHello, NodeToControlPlane,
};
use cy_proto::semantic_v1;
use cy_resource_manager::InMemoryResourceManager;
use tempfile::tempdir;
use tokio::net::{TcpListener, UnixListener};
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::{ReceiverStream, TcpListenerStream, UnixListenerStream};
use tonic::metadata::MetadataMap;
use tonic::transport::{Endpoint, Server};
use tonic::{Code, Request};

const NODE_ID: &str = "node-alpha";
const NODE_EPOCH: u64 = 1;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn selected_node_acquires_one_real_kernel_lease_and_dispatches_once() {
    exercise_dispatch(false, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn offline_runtime_launches_after_one_durable_kernel_lease() {
    exercise_dispatch(true, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unknown_launch_reconciles_without_a_second_lease() {
    exercise_dispatch(true, true).await;
}

struct ProtocolLauncher {
    endpoint: String,
    runtime: semantic::Identity,
    resources: Arc<InMemoryResourceManager>,
    controller: ExecutionController,
    unknown: bool,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    lease: Mutex<Option<semantic::Lease>>,
}

impl RuntimeLauncher for ProtocolLauncher {
    fn launch<'a>(
        &'a self,
        _node: &'a core_v1::NodeRef,
        assignment: &'a core_v1::RuntimeAssignment,
        lease: &'a semantic::Lease,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), DispatchError>> + Send + 'a>>
    {
        Box::pin(async move {
            assert_eq!(self.resources.leases().len(), 1);
            assert_eq!(lease.holder, self.runtime);
            assert_eq!(
                self.controller
                    .intent_disposition(&assignment.assignment_id)
                    .unwrap(),
                Some(IntentDisposition::LeaseAcquired)
            );
            *self.lease.lock().unwrap() = Some(lease.clone());
            *self.task.lock().unwrap() = Some(tokio::spawn(run_runtime_protocol(
                self.endpoint.clone(),
                self.runtime.clone(),
            )));
            if self.unknown {
                Err(DispatchError {
                    reason_code: "UNKNOWN_REQUIRES_RECONCILIATION".to_string(),
                    message: "launch reply lost".to_string(),
                    reconciliation_required: true,
                })
            } else {
                Ok(())
            }
        })
    }

    fn reconcile<'a>(
        &'a self,
        _node: &'a core_v1::NodeRef,
        _assignment: &'a core_v1::RuntimeAssignment,
        lease: &'a semantic::Lease,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), DispatchError>> + Send + 'a>>
    {
        Box::pin(async move {
            assert_eq!(self.lease.lock().unwrap().as_ref(), Some(lease));
            assert!(self.task.lock().unwrap().is_some());
            Ok(())
        })
    }
}

impl Drop for ProtocolLauncher {
    fn drop(&mut self) {
        if let Some(task) = self.task.lock().unwrap().take() {
            task.abort();
        }
    }
}

async fn exercise_dispatch(offline: bool, unknown_launch: bool) {
    let temporary = tempdir().unwrap();
    let kernel_socket = temporary.path().join("kernel.sock");
    let resource = test_resource();
    let resources = Arc::new(InMemoryResourceManager::new(
        NODE_ID,
        vec![resource.clone()],
    ));
    let kernel_task = start_kernel(&kernel_socket, resource.clone(), Arc::clone(&resources)).await;

    let runtime = semantic::Identity {
        id: "runtime-alpha".to_string(),
        generation: 1,
    };
    let service = ExecutionControlService::new(
        Arc::new(MetadataAuthenticator {
            runtime: runtime.clone(),
        }),
        Arc::new(DevelopmentEnrollmentProvider::new(
            ["single-use-enrollment".to_string()],
            120_000,
        )),
        Duration::from_secs(5),
        Duration::from_secs(5),
    )
    .unwrap();
    let (control_endpoint, control_task) = start_control(service.clone()).await;
    let host_task = tokio::spawn(run_host(control_endpoint.clone(), kernel_socket.clone()));
    wait_for(|| {
        service.has_host_session(&core_v1::NodeRef {
            node_id: NODE_ID.to_string(),
            node_epoch: NODE_EPOCH,
        })
    })
    .await;
    let runtime_task = if offline {
        None
    } else {
        Some(tokio::spawn(run_runtime_protocol(
            control_endpoint.clone(),
            runtime.clone(),
        )))
    };

    if !offline {
        wait_for(|| {
            service.has_host_session(&core_v1::NodeRef {
                node_id: NODE_ID.to_string(),
                node_epoch: NODE_EPOCH,
            }) && service.has_runtime_session(&runtime)
        })
        .await;
    }

    let workload_identity = if offline {
        assert!(!service.has_runtime_session(&runtime));
        let scope = RuntimeScope {
            runtime: runtime.clone(),
            organization_id: "organization-alpha".to_string(),
            workspace_id: "workspace-alpha".to_string(),
        };
        let node = core_v1::NodeRef {
            node_id: NODE_ID.to_string(),
            node_epoch: NODE_EPOCH,
        };
        let identity = service
            .prepare_runtime_workload(
                &node,
                scope.clone(),
                "single-use-enrollment",
                "user-alpha",
                vec!["operation.report".to_string()],
            )
            .unwrap();
        assert_eq!(
            identity,
            service
                .prepare_runtime_workload(
                    &node,
                    scope,
                    "single-use-enrollment",
                    "user-alpha",
                    vec!["operation.report".to_string()]
                )
                .unwrap()
        );
        identity
    } else {
        service
            .workload_identity(&runtime, "user-alpha", vec!["operation.report".to_string()])
            .unwrap()
    };
    let now = now_unix_ms();
    let candidate = candidate(resource, now);
    let request = ExecutionDispatchRequest {
        placement: placement_request(now),
        candidates: vec![candidate],
        assignment: RuntimeAssignmentBuilder::new(
            "assignment-alpha",
            runtime.clone(),
            semantic::Identity {
                id: "operation-alpha".to_string(),
                generation: 1,
            },
            "attempt-alpha",
            workload_identity,
            core_v1::RuntimeProfile {
                image_digest: format!("sha256:{}", "1".repeat(64)),
                resolved_digest: format!("sha256:{}", "2".repeat(64)),
            },
            Vec::new(),
        ),
        acquire_command_id: "command-acquire-alpha".to_string(),
        intent_payload_digest: format!("sha256:{}", "a".repeat(64)),
        acquire_context: context("acquire-alpha"),
        release_command_id: "command-rollback-alpha".to_string(),
        release_context: context("rollback-alpha"),
        lease_ttl: Duration::from_secs(30),
    };
    let controller = ExecutionController::new(
        service,
        Duration::from_secs(5),
        Arc::new(InMemoryExecutionIntentStore::default()),
    )
    .unwrap();
    let launcher = ProtocolLauncher {
        endpoint: control_endpoint,
        runtime: runtime.clone(),
        resources: resources.clone(),
        controller: controller.clone(),
        unknown: unknown_launch,
        task: Mutex::new(None),
        lease: Mutex::new(None),
    };
    let dispatched = if offline {
        controller
            .dispatch_with_launcher(request.clone(), &launcher)
            .await
    } else {
        controller.dispatch(request.clone()).await
    };
    if unknown_launch {
        assert!(dispatched.unwrap_err().reconciliation_required);
        assert_eq!(
            controller.intent_disposition("assignment-alpha").unwrap(),
            Some(IntentDisposition::UnknownRequiresReconciliation)
        );
        let observed_lease = launcher.lease.lock().unwrap().clone().unwrap();
        let receipt = controller
            .reconcile_with_launcher(request.clone(), observed_lease, &launcher)
            .await
            .unwrap();
        assert!(matches!(
            receipt.ack_disposition,
            AssignmentAckDisposition::Accepted | AssignmentAckDisposition::Duplicate
        ));
        assert_eq!(
            controller.intent_disposition("assignment-alpha").unwrap(),
            Some(IntentDisposition::Completed)
        );
        assert_eq!(
            controller
                .dispatch_with_launcher(request, &launcher)
                .await
                .unwrap_err()
                .reason_code,
            "EXECUTION_INTENT_ALREADY_RECORDED"
        );
        assert_eq!(resources.leases().len(), 1);
        host_task.abort();
        control_task.abort();
        kernel_task.abort();
        return;
    }
    let receipt = dispatched.unwrap();

    assert_eq!(receipt.node.node_id, NODE_ID);
    assert_eq!(receipt.lease.state, semantic::LeaseState::Active);
    assert_eq!(receipt.lease.holder, runtime);
    assert_eq!(receipt.lease.resources.len(), 1);
    assert_ne!(receipt.lease.fence_token, 0);
    assert_eq!(receipt.ack_disposition, AssignmentAckDisposition::Accepted);
    assert_eq!(resources.leases().len(), 1);
    assert!(ResourceLeaseManager::is_allocated(
        resources.as_ref(),
        &resources.leases()[0].name
    ));
    assert_eq!(
        controller.intent_disposition("assignment-alpha").unwrap(),
        Some(IntentDisposition::Completed)
    );

    let duplicate = controller.dispatch(request).await.unwrap_err();
    assert_eq!(duplicate.reason_code, "EXECUTION_INTENT_ALREADY_RECORDED");
    assert_eq!(resources.leases().len(), 1);

    controller
        .release(ExecutionReleaseRequest {
            assignment_id: "assignment-alpha".to_string(),
            node: receipt.node,
            lease: receipt.lease,
            command_id: "command-release-alpha".to_string(),
            context: context("release-alpha"),
        })
        .await
        .unwrap();
    assert!(!ResourceLeaseManager::is_allocated(
        resources.as_ref(),
        &resources.leases()[0].name
    ));
    assert_eq!(
        controller.intent_disposition("assignment-alpha").unwrap(),
        Some(IntentDisposition::Released)
    );

    host_task.abort();
    if let Some(task) = runtime_task {
        task.abort();
    }
    control_task.abort();
    kernel_task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rollback_retries_exact_release_after_same_node_host_reconnect() {
    let temporary = tempdir().unwrap();
    let kernel_socket = temporary.path().join("kernel.sock");
    let resource = test_resource();
    let resources = Arc::new(InMemoryResourceManager::new(
        NODE_ID,
        vec![resource.clone()],
    ));
    let kernel_task = start_kernel(&kernel_socket, resource.clone(), Arc::clone(&resources)).await;

    let runtime = semantic::Identity {
        id: "runtime-rollback".to_string(),
        generation: 1,
    };
    let service = ExecutionControlService::new(
        Arc::new(MetadataAuthenticator {
            runtime: runtime.clone(),
        }),
        Arc::new(DevelopmentEnrollmentProvider::new(
            ["single-use-enrollment".to_string()],
            120_000,
        )),
        Duration::from_secs(5),
        Duration::from_secs(5),
    )
    .unwrap();
    let (control_endpoint, control_task) = start_control(service.clone()).await;
    let first_host_task = tokio::spawn(run_host(control_endpoint.clone(), kernel_socket.clone()));
    wait_for(|| {
        service.has_host_session(&core_v1::NodeRef {
            node_id: NODE_ID.to_string(),
            node_epoch: NODE_EPOCH,
        })
    })
    .await;
    let (assignment_seen_tx, assignment_seen_rx) = oneshot::channel();
    let (reject_tx, reject_rx) = oneshot::channel();
    let runtime_task = tokio::spawn(run_rejecting_runtime_protocol(
        control_endpoint.clone(),
        runtime.clone(),
        assignment_seen_tx,
        reject_rx,
    ));
    wait_for(|| service.has_runtime_session(&runtime)).await;

    let workload_identity = service
        .workload_identity(
            &runtime,
            "user-rollback",
            vec!["operation.report".to_string()],
        )
        .unwrap();
    let now = now_unix_ms();
    let request = ExecutionDispatchRequest {
        placement: placement_request(now),
        candidates: vec![candidate(resource, now)],
        assignment: RuntimeAssignmentBuilder::new(
            "assignment-rollback",
            runtime.clone(),
            semantic::Identity {
                id: "operation-rollback".to_string(),
                generation: 1,
            },
            "attempt-rollback",
            workload_identity,
            core_v1::RuntimeProfile {
                image_digest: format!("sha256:{}", "3".repeat(64)),
                resolved_digest: format!("sha256:{}", "4".repeat(64)),
            },
            Vec::new(),
        ),
        acquire_command_id: "command-acquire-rollback".to_string(),
        intent_payload_digest: format!("sha256:{}", "b".repeat(64)),
        acquire_context: context("acquire-rollback"),
        release_command_id: "command-release-rollback".to_string(),
        release_context: context("release-rollback"),
        lease_ttl: Duration::from_secs(30),
    };
    let controller = ExecutionController::new(
        service,
        Duration::from_secs(5),
        Arc::new(InMemoryExecutionIntentStore::default()),
    )
    .unwrap();
    let dispatch_task = {
        let controller = controller.clone();
        tokio::spawn(async move { controller.dispatch(request).await })
    };

    assignment_seen_rx.await.unwrap();
    assert!(ResourceLeaseManager::is_allocated(
        resources.as_ref(),
        &resources.leases()[0].name
    ));
    let (replacement_ready_tx, replacement_ready_rx) = oneshot::channel();
    let replacement_host_task = tokio::spawn(run_host_with_ready(
        control_endpoint,
        kernel_socket,
        replacement_ready_tx,
    ));
    replacement_ready_rx.await.unwrap();
    reject_tx.send(()).unwrap();

    let error = dispatch_task.await.unwrap().unwrap_err();
    assert_eq!(error.reason_code, "ASSIGNMENT_REJECTED_BY_TEST");
    assert!(!error.reconciliation_required);
    assert_eq!(resources.leases().len(), 1);
    assert!(!ResourceLeaseManager::is_allocated(
        resources.as_ref(),
        &resources.leases()[0].name
    ));
    assert_eq!(
        controller
            .intent_disposition("assignment-rollback")
            .unwrap(),
        Some(IntentDisposition::Failed)
    );

    first_host_task.abort();
    replacement_host_task.abort();
    runtime_task.abort();
    control_task.abort();
    kernel_task.abort();
}

#[derive(Debug)]
struct MetadataAuthenticator {
    runtime: semantic::Identity,
}

impl PeerAuthenticator for MetadataAuthenticator {
    fn authenticate(
        &self,
        _certificate_chain: &[Vec<u8>],
        metadata: &MetadataMap,
    ) -> Result<AuthenticatedAgent, cy_execution_control::DispatchError> {
        match metadata
            .get("x-cyrene-test-agent")
            .and_then(|value| value.to_str().ok())
        {
            Some("host") => Ok(AuthenticatedAgent::Host {
                node: core_v1::NodeRef {
                    node_id: NODE_ID.to_string(),
                    node_epoch: NODE_EPOCH,
                },
            }),
            Some("runtime") => Ok(AuthenticatedAgent::Runtime {
                runtime: self.runtime.clone(),
                node: core_v1::NodeRef {
                    node_id: NODE_ID.to_string(),
                    node_epoch: NODE_EPOCH,
                },
            }),
            _ => Err(cy_execution_control::DispatchError {
                reason_code: "TEST_PEER_REJECTED".to_string(),
                message: "test transport identity is missing".to_string(),
                reconciliation_required: false,
            }),
        }
    }
}

async fn start_control(service: ExecutionControlService) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        Server::builder()
            .add_service(NodeControlServiceServer::new(service))
            .serve_with_incoming(TcpListenerStream::new(listener))
            .await
            .unwrap();
    });
    (format!("http://{address}"), task)
}

async fn start_kernel(
    socket: &PathBuf,
    resource: semantic::Resource,
    resources: Arc<InMemoryResourceManager>,
) -> tokio::task::JoinHandle<()> {
    let hardware = Arc::new(TestHardware {
        resources: vec![resource],
    });
    let daemon = Arc::new(KernelDaemon::new(
        hardware.clone(),
        hardware,
        resources,
        Arc::new(TestSandbox),
        NODE_ID,
        NODE_EPOCH,
    ));
    let adapter = KernelServiceAdapter::new(daemon, Arc::new(UnusedResolver));
    let listener = UnixListener::bind(socket).unwrap();
    tokio::spawn(async move {
        Server::builder()
            .add_service(
                core_v1::kernel_authority_service_server::KernelAuthorityServiceServer::with_interceptor(
                    adapter.clone(),
                    inject_authority_principal,
                ),
            )
            .add_service(adapter.server())
            .serve_with_incoming(PeerCredAccept::new(UnixListenerStream::new(listener)))
            .await
            .unwrap();
    })
}

async fn run_host(endpoint: String, kernel_socket: PathBuf) {
    run_host_session(endpoint, kernel_socket, None).await;
}

async fn run_host_with_ready(endpoint: String, kernel_socket: PathBuf, ready: oneshot::Sender<()>) {
    run_host_session(endpoint, kernel_socket, Some(ready)).await;
}

async fn run_host_session(
    endpoint: String,
    kernel_socket: PathBuf,
    mut ready: Option<oneshot::Sender<()>>,
) {
    let executor = UdsKernelCommandExecutor::new(kernel_socket).unwrap();
    let node = executor.discover_node().await.unwrap();
    let bridge = NodeCommandBridge::new(executor);
    let mut session =
        NodeControlSession::new(node.node_id, node.node_epoch, "assignment-tck", 1, 1, "");
    let channel = Endpoint::from_shared(endpoint)
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut client = NodeControlServiceClient::new(channel);
    let (outbound, inbound) = mpsc::channel(16);
    outbound.send(session.hello()).await.unwrap();
    let mut request = Request::new(ReceiverStream::new(inbound));
    request
        .metadata_mut()
        .insert("x-cyrene-test-agent", "host".parse().unwrap());
    let mut incoming = client.connect(request).await.unwrap().into_inner();
    let welcome = incoming.message().await.unwrap().unwrap();
    session.accept_welcome(welcome).unwrap();
    if let Some(ready) = ready.take() {
        ready.send(()).unwrap();
    }
    loop {
        match incoming.message().await {
            Ok(Some(frame)) => {
                let result = bridge.forward(&mut session, frame).await.unwrap();
                outbound.send(result).await.unwrap();
            }
            Ok(None) => return,
            Err(status) if status.code() == Code::Aborted => return,
            Err(status) => panic!("Host control stream failed: {status}"),
        }
    }
}

async fn run_rejecting_runtime_protocol(
    endpoint: String,
    runtime: semantic::Identity,
    assignment_seen: oneshot::Sender<()>,
    reject: oneshot::Receiver<()>,
) {
    let channel = Endpoint::from_shared(endpoint)
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut client = NodeControlServiceClient::new(channel);
    let (outbound, inbound) = mpsc::channel(16);
    outbound
        .send(NodeToControlPlane {
            frame_id: "runtime-rollback-hello".to_string(),
            sequence_number: 1,
            session_id: String::new(),
            ack_sequence_number: 0,
            body: Some(node_to_control_plane::Body::ExecutionAgentHello(
                runtime_hello(&runtime),
            )),
        })
        .await
        .unwrap();
    let mut request = Request::new(ReceiverStream::new(inbound));
    request
        .metadata_mut()
        .insert("x-cyrene-test-agent", "runtime".parse().unwrap());
    let mut incoming = client.connect(request).await.unwrap().into_inner();
    let welcome = incoming.message().await.unwrap().unwrap();
    let session_id = match welcome.body {
        Some(control_plane_to_node::Body::ExecutionAgentWelcome(welcome)) => welcome.session_id,
        _ => panic!("Runtime Agent did not receive ExecutionAgentWelcome"),
    };
    while let Some(frame) = incoming.message().await.unwrap() {
        let Some(control_plane_to_node::Body::RuntimeAssignment(assignment)) = frame.body else {
            continue;
        };
        validate_assignment(&runtime, &assignment, now_unix_ms()).unwrap();
        assignment_seen.send(()).unwrap();
        reject.await.unwrap();
        outbound
            .send(NodeToControlPlane {
                frame_id: "runtime-rollback-rejection".to_string(),
                sequence_number: 2,
                session_id,
                ack_sequence_number: frame.sequence_number,
                body: Some(node_to_control_plane::Body::AssignmentAck(AssignmentAck {
                    assignment_id: assignment.assignment_id,
                    runtime: Some(core_v1::RuntimeRef {
                        identity: Some(identity_to_proto(&runtime)),
                    }),
                    disposition: AssignmentAckDisposition::Rejected as i32,
                    rejection: Some(semantic_v1::Rejection {
                        reason_code: "ASSIGNMENT_REJECTED_BY_TEST".to_string(),
                        message: "intentional rollback path".to_string(),
                    }),
                })),
            })
            .await
            .unwrap();
        return;
    }
}

async fn run_runtime_protocol(endpoint: String, runtime: semantic::Identity) {
    let channel = Endpoint::from_shared(endpoint)
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut client = NodeControlServiceClient::new(channel);
    let (outbound, inbound) = mpsc::channel(16);
    outbound
        .send(NodeToControlPlane {
            frame_id: "runtime-hello".to_string(),
            sequence_number: 1,
            session_id: String::new(),
            ack_sequence_number: 0,
            body: Some(node_to_control_plane::Body::ExecutionAgentHello(
                runtime_hello(&runtime),
            )),
        })
        .await
        .unwrap();
    let mut request = Request::new(ReceiverStream::new(inbound));
    request
        .metadata_mut()
        .insert("x-cyrene-test-agent", "runtime".parse().unwrap());
    let mut incoming = client.connect(request).await.unwrap().into_inner();
    let welcome = incoming.message().await.unwrap().unwrap();
    let session_id = match welcome.body {
        Some(control_plane_to_node::Body::ExecutionAgentWelcome(welcome)) => welcome.session_id,
        _ => panic!("Runtime Agent did not receive ExecutionAgentWelcome"),
    };
    let mut sequence = 2;
    while let Some(frame) = incoming.message().await.unwrap() {
        let Some(control_plane_to_node::Body::RuntimeAssignment(assignment)) = frame.body else {
            continue;
        };
        validate_assignment(&runtime, &assignment, now_unix_ms()).unwrap();
        outbound
            .send(NodeToControlPlane {
                frame_id: format!("runtime-{sequence}"),
                sequence_number: sequence,
                session_id: session_id.clone(),
                ack_sequence_number: frame.sequence_number,
                body: Some(node_to_control_plane::Body::AssignmentAck(AssignmentAck {
                    assignment_id: assignment.assignment_id,
                    runtime: Some(core_v1::RuntimeRef {
                        identity: Some(identity_to_proto(&runtime)),
                    }),
                    disposition: AssignmentAckDisposition::Accepted as i32,
                    rejection: None,
                })),
            })
            .await
            .unwrap();
        sequence += 1;
    }
}

fn runtime_hello(runtime: &semantic::Identity) -> ExecutionAgentHello {
    ExecutionAgentHello {
        runtime: Some(core_v1::RuntimeRef {
            identity: Some(identity_to_proto(runtime)),
        }),
        scope: Some(core_v1::AccountScope {
            user_id: String::new(),
            organization_id: "organization-alpha".to_string(),
            workspace_id: "workspace-alpha".to_string(),
        }),
        attachment_type: core_v1::ExecutionAttachmentType::ContainerAgent as i32,
        persistence_class: core_v1::PersistenceClass::Ephemeral as i32,
        agent_version: "assignment-tck".to_string(),
        min_protocol_version: 2,
        max_protocol_version: 2,
        resume_token: String::new(),
        capabilities: vec![execution_capability(
            core_v1::ExecutionAttachmentType::ContainerAgent,
            false,
            core_v1::RestartCapability::None,
        )],
        enrollment_proof: "single-use-enrollment".to_string(),
        node: Some(core_v1::ExecutionNodeDescriptor {
            node: Some(core_v1::NodeRef {
                node_id: NODE_ID.to_string(),
                node_epoch: NODE_EPOCH,
            }),
            node_type: "container".to_string(),
            persistent: Some(false),
        }),
        restart_capability: core_v1::RestartCapability::None as i32,
    }
}

fn placement_request(now: u64) -> ExecutionPlacementRequest {
    ExecutionPlacementRequest {
        capability_requirements: Vec::new(),
        resource_query: semantic::ResourceQuery {
            resource_class: "accelerator".to_string(),
            count: 1,
            required_capabilities: Vec::new(),
            minimum_capacity: BTreeMap::new(),
        },
        allowed_attachments: BTreeSet::from([core_v1::ExecutionAttachmentType::ContainerAgent]),
        persistent: Some(false),
        restart_capability: Some(core_v1::RestartCapability::None),
        checkpoint_resume: false,
        network: NetworkRequirements::default(),
        artifacts: Vec::new(),
        artifact_policy_scope: String::new(),
        policy: PlacementPolicy::default(),
        latest_start_unix_ms: Some(now + 30_000),
        now_unix_ms: now,
    }
}

fn candidate(resource: semantic::Resource, now: u64) -> ExecutionTargetCandidate {
    let provider = semantic::Provider {
        identity: resource.provider.clone(),
        state: semantic::ProviderState::Ready,
        capabilities: Vec::new(),
    };
    ExecutionTargetCandidate {
        node: core_v1::NodeRef {
            node_id: NODE_ID.to_string(),
            node_epoch: NODE_EPOCH,
        },
        lifecycle_state: core_v1::NodeLifecycleState::Online,
        attachment: core_v1::ExecutionAttachmentType::ContainerAgent,
        persistent: false,
        restart_capability: core_v1::RestartCapability::None,
        capabilities: vec![execution_capability(
            core_v1::ExecutionAttachmentType::ContainerAgent,
            false,
            core_v1::RestartCapability::None,
        )],
        provider: provider.clone(),
        provider_snapshot: semantic::ProviderSnapshot {
            provider: provider.identity,
            snapshot_generation: 1,
            resources: vec![resource],
            workers: Vec::new(),
            endpoints: Vec::new(),
            sampled_at_unix_ms: now,
            expires_at_unix_ms: now + 60_000,
        },
        residency: "local".to_string(),
        trust_domain: "workspace-alpha".to_string(),
        classifications: BTreeSet::new(),
        policy_tags: BTreeSet::new(),
        artifact_destination_peer_id: "peer-node-alpha".to_string(),
        artifact_quotes: Vec::new(),
        execution_cost_microunits: 1,
        available_at_unix_ms: now,
        reliability_score: 100,
    }
}

fn context(id: &str) -> AuthorityCallContext {
    AuthorityCallContext {
        contract: semantic::ContractRevision::current(),
        namespace: NamespaceId::default(),
        request_id: id.to_string(),
        idempotency_key: id.to_string(),
    }
}

fn test_resource() -> semantic::Resource {
    semantic::Resource {
        identity: semantic::Identity {
            id: "resource-alpha".to_string(),
            generation: 1,
        },
        provider: semantic::Identity {
            id: "provider-alpha".to_string(),
            generation: 1,
        },
        resource_class: "accelerator".to_string(),
        capabilities: Vec::new(),
        capacity: BTreeMap::new(),
        attributes: BTreeMap::new(),
        state: semantic::ResourceState::Ready,
        reason_code: "ready".to_string(),
        summary: "ready".to_string(),
        links: Vec::new(),
    }
}

fn identity_to_proto(identity: &semantic::Identity) -> semantic_v1::Identity {
    semantic_v1::Identity {
        id: identity.id.clone(),
        generation: identity.generation,
    }
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
        .try_into()
        .unwrap()
}

async fn wait_for(mut ready: impl FnMut() -> bool) {
    for _ in 0..100 {
        if ready() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for authenticated Agent sessions");
}

#[derive(Debug, Clone)]
struct TestHardware {
    resources: Vec<semantic::Resource>,
}

impl HostInventoryProvider for TestHardware {
    fn probe_inventory(&self) -> Result<InventorySnapshot, ProviderError> {
        Ok(InventorySnapshot {
            generation: 1,
            resources: self.resources.clone(),
            capabilities: NodeCapabilities {
                ready: true,
                facts: Vec::new(),
                enforcement: Vec::new(),
            },
        })
    }
}

impl ResourceProvider for TestHardware {
    fn adapter_id(&self) -> &str {
        "test-provider"
    }

    fn probe_resources(&self) -> Result<Vec<semantic::Resource>, ProviderError> {
        Ok(self.resources.clone())
    }

    fn create_binding(
        &self,
        resource: &semantic::Resource,
    ) -> Result<DeviceBinding, ProviderError> {
        Ok(DeviceBinding {
            resource_id: resource.identity.id.clone(),
            nodes: Vec::new(),
            environment: BTreeMap::new(),
            joinable_environment_keys: BTreeSet::new(),
            required_gids: Vec::new(),
            enforcement: EnforcementMode::Hard,
            adapter_id: self.adapter_id().to_string(),
            reason_code: "test-binding".to_string(),
        })
    }

    fn read_health(
        &self,
        _resource_id: &str,
    ) -> Result<cy_kernel_api::HealthReport, ProviderError> {
        Ok(cy_kernel_api::HealthReport {
            healthy: Some(true),
            reason_code: "ready".to_string(),
            summary: "ready".to_string(),
        })
    }
}

#[derive(Debug)]
struct TestSandbox;

impl ProcessRuntime for TestSandbox {
    fn preflight(&self) -> NodeCapabilities {
        NodeCapabilities {
            ready: true,
            facts: Vec::new(),
            enforcement: Vec::new(),
        }
    }

    fn launch(
        &self,
        _plan: &LaunchPlan,
        _binding: &DeviceBinding,
    ) -> Result<ProcessHandle, ProviderError> {
        Err(ProviderError::new(
            "test-sandbox",
            "UNUSED",
            "assignment TCK does not start a Kernel Worker",
        ))
    }

    fn stop(
        &self,
        _handle: &ProcessHandle,
        _request: &StopRequest,
    ) -> Result<CleanupReport, ProviderError> {
        Err(ProviderError::new(
            "test-sandbox",
            "UNUSED",
            "assignment TCK has no Kernel Worker to stop",
        ))
    }
}

impl SandboxBackend for TestSandbox {
    fn backend_id(&self) -> &str {
        "test-sandbox"
    }
}

struct UnusedResolver;

impl InstalledPluginResolver for UnusedResolver {
    fn resolve_launch_plan(
        &self,
        _installation: &VerifiedInstallation,
        _instance_name: &str,
    ) -> Result<cy_kernel_api::ResolvedLaunchPlan, ProviderError> {
        Err(ProviderError::new(
            "test-resolver",
            "UNUSED",
            "assignment TCK does not resolve a Kernel Worker",
        ))
    }
}
