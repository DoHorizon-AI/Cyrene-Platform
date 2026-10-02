// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: kernel/crates/cy-kernel-daemon/tests/runtime_update_maintenance.rs ║
// ║ Module: CYRENE Platform                                             ║
// ║ Role: Core v2 maintenance-gate UDS integration acceptance test.       ║
// ║                                                                      ║
// ║ 模块职责：通过 Core v2 UDS gRPC 验收维护门禁与 Kernel authority。     ║
// ╚══════════════════════════════════════════════════════════════════════╝
//! Live Core v2 UDS gRPC acceptance for readiness and maintenance fencing.
//!
//! The test runs the real `KernelServiceAdapter`, `KernelDaemon`, resource
//! manager, and durable `RuntimeMaintenance` gate. Its hardware facts and
//! sandbox process are deliberately simulated test fixtures; this does not
//! claim host GPU, sandboxd, or whole-deployment evidence.
//! 中文：本测试通过真实 Core v2 authority 服务和 Unix Domain Socket 验收就绪检查及维护门禁。Kernel、资源管理器与持久门禁使用真实实现；硬件事实与 sandbox 进程明确为模拟测试资源，不代表宿主 GPU、sandboxd 或整套部署已经验收。

#![cfg(unix)]

use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::Arc,
};

use cy_kernel_api::{
    semantic, CapabilityFact, CgroupLimits, CleanupReport, DeviceBinding, EnforcementMode,
    HostInventoryProvider, InstalledPluginResolver, InventorySnapshot, LaunchPlan,
    NodeCapabilities, ProcessCondition, ProcessHandle, ProcessRuntime, ProviderError,
    ResolvedLaunchPlan, ResourceProvider, SandboxBackend, StopRequest, VerifiedInstallation,
};
use cy_kernel_daemon::{
    peer_cred::{inject_authority_principal, PeerCredAccept},
    KernelDaemon, KernelServiceAdapter,
};
use cy_proto::{core_v2, semantic_v1};
use cy_resource_manager::InMemoryResourceManager;
use cy_runtime_maintenance::{RuntimeMaintenance, TrustedActivitySourceCatalog};
use hyper_util::rt::TokioIo;
use tokio::{
    net::{UnixListener, UnixStream},
    task::JoinHandle,
    time::{sleep, Duration},
};
use tokio_stream::wrappers::UnixListenerStream;
use tonic::transport::{Channel, Endpoint, Server, Uri};
use tower::service_fn;

type CoreV2Client = core_v2::kernel_authority_service_client::KernelAuthorityServiceClient<Channel>;
type ServerResult = Result<(), tonic::transport::Error>;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn core_v2_uds_readiness_admission_race_and_restart_recovery() {
    let directory = tempfile::tempdir().expect("create isolated acceptance directory");
    assert_root_peer_namespace(directory.path());

    let state_dir = directory.path().join("maintenance-state");
    let operator_path = directory.path().join("private/operator.token");
    let catalog = empty_catalog();
    let gate = RuntimeMaintenance::open(&state_dir, catalog.clone())
        .expect("open isolated durable maintenance gate");
    let operator_token = gate
        .initialize_operator_capability(&operator_path)
        .expect("create isolated operator capability");

    let socket = directory.path().join("authority-first.sock");
    let adapter = simulated_kernel_adapter(gate);
    let (shutdown, server) = start_authority_server(&socket, adapter);
    let mut client = connect_client(socket.clone()).await;

    // A lease with no Worker is enough to prove that a live allocation blocks
    // a Core Runtime apply.
    let lease = acquire_lease(&mut client, "lease-only-worker").await;
    let lease_only = get_readiness(&mut client).await;
    assert_readiness(
        &lease_only,
        core_v2::UpdateReadinessStatus::IdleRuntimeRequiresUnload,
    );
    assert_eq!(lease_only.active_task_count, 0);
    assert_eq!(lease_only.active_worker_count, 0);
    assert!(lease_only.active_lease_or_allocation_count > 0);
    let begin_while_leased = begin_maintenance(
        &mut client,
        &operator_token,
        &lease_only,
        "lease-only-blocked",
        true,
    )
    .await;
    assert_eq!(
        begin_while_leased.status,
        core_v2::UpdateReadinessStatus::IdleRuntimeRequiresUnload as i32
    );
    assert!(begin_while_leased.maintenance_token.is_empty());

    // A registered Worker is an idle loaded runtime: there are no Product
    // tasks, but Kernel and resource facts must still block Core Runtime apply.
    let worker = start_worker(&mut client, &lease).await;
    let idle_worker = get_readiness(&mut client).await;
    assert_readiness(
        &idle_worker,
        core_v2::UpdateReadinessStatus::IdleRuntimeRequiresUnload,
    );
    assert_eq!(idle_worker.active_task_count, 0);
    assert!(idle_worker.active_worker_count > 0);
    assert!(idle_worker.active_lease_or_allocation_count > 0);
    let begin_while_worker_idle = begin_maintenance(
        &mut client,
        &operator_token,
        &idle_worker,
        "idle-worker-blocked",
        true,
    )
    .await;
    assert_eq!(
        begin_while_worker_idle.status,
        core_v2::UpdateReadinessStatus::IdleRuntimeRequiresUnload as i32
    );
    assert!(begin_while_worker_idle.maintenance_token.is_empty());

    stop_worker(&mut client, &lease, &worker).await;
    release_lease(&mut client, &lease, "release-idle-worker").await;

    let ready = get_readiness(&mut client).await;
    assert_readiness(&ready, core_v2::UpdateReadinessStatus::Ready);
    assert_eq!(ready.active_task_count, 0);
    assert_eq!(ready.active_worker_count, 0);
    assert_eq!(ready.active_lease_or_allocation_count, 0);
    assert!(ready.requires_restart_confirmation);

    let confirmation_required = begin_maintenance(
        &mut client,
        &operator_token,
        &ready,
        "confirmation-required",
        false,
    )
    .await;
    assert_eq!(
        confirmation_required.status,
        core_v2::UpdateReadinessStatus::UserConfirmationRequired as i32
    );
    assert!(confirmation_required.maintenance_token.is_empty());

    // Race the real RPCs at the authority socket. The cross-process admission
    // lock must let either a lease or maintenance begin win, never both.
    let race_ready = get_readiness(&mut client).await;
    assert_readiness(&race_ready, core_v2::UpdateReadinessStatus::Ready);
    let race_begin_request =
        begin_request(&operator_token, &race_ready, "concurrent-maintenance", true);
    let race_lease_request = lease_request("concurrent-worker");
    let mut begin_client = connect_client(socket.clone()).await;
    let mut lease_client = connect_client(socket.clone()).await;
    let barrier = Arc::new(tokio::sync::Barrier::new(3));
    let begin_barrier = Arc::clone(&barrier);
    let begin_task = tokio::spawn(async move {
        begin_barrier.wait().await;
        begin_client.begin_maintenance(race_begin_request).await
    });
    let lease_barrier = Arc::clone(&barrier);
    let lease_task = tokio::spawn(async move {
        lease_barrier.wait().await;
        lease_client.acquire_lease(race_lease_request).await
    });
    barrier.wait().await;
    let race_begin = begin_task
        .await
        .expect("maintenance race task must join")
        .expect("BeginMaintenance returns an explicit decision")
        .into_inner();
    let race_lease = lease_task.await.expect("lease race task must join");
    let begin_won = !race_begin.maintenance_token.is_empty()
        && race_begin.status == core_v2::UpdateReadinessStatus::MaintenanceActive as i32;
    let lease_won = race_lease.is_ok();
    assert_ne!(
        begin_won, lease_won,
        "serialized BeginMaintenance and AcquireLease must have exactly one winner; begin={race_begin:?}, lease={race_lease:?}"
    );

    let mut recovery_request_id = "post-race-cleanup".to_string();
    let mut recovery_token = String::new();
    if begin_won {
        recovery_request_id = "concurrent-maintenance".to_string();
        recovery_token = race_begin.maintenance_token.clone();
    } else {
        let lease = race_lease
            .expect("the lease side won the admission race")
            .into_inner();
        release_lease_response(&mut client, lease, "release-race-lease").await;
    }

    if begin_won {
        let failed_end = end_maintenance(
            &mut client,
            &operator_token,
            &recovery_request_id,
            &recovery_token,
            core_v2::MaintenanceOutcome::Failed,
            false,
        )
        .await;
        assert!(!failed_end.unlocked);
        assert_eq!(
            failed_end.status,
            core_v2::UpdateReadinessStatus::MaintenanceActive as i32
        );
        let rollback_end = end_maintenance(
            &mut client,
            &operator_token,
            &recovery_request_id,
            &recovery_token,
            core_v2::MaintenanceOutcome::RolledBack,
            true,
        )
        .await;
        assert!(rollback_end.unlocked);
        assert_eq!(
            rollback_end.status,
            core_v2::UpdateReadinessStatus::Ready as i32
        );
    }

    let ready_for_restart = get_readiness(&mut client).await;
    assert_readiness(&ready_for_restart, core_v2::UpdateReadinessStatus::Ready);
    let final_begin = begin_maintenance(
        &mut client,
        &operator_token,
        &ready_for_restart,
        "restart-recovery",
        true,
    )
    .await;
    assert_eq!(
        final_begin.status,
        core_v2::UpdateReadinessStatus::MaintenanceActive as i32
    );
    assert!(!final_begin.maintenance_token.is_empty());

    let unhealthy_end = end_maintenance(
        &mut client,
        &operator_token,
        "restart-recovery",
        &final_begin.maintenance_token,
        core_v2::MaintenanceOutcome::Success,
        false,
    )
    .await;
    assert!(!unhealthy_end.unlocked);
    assert_eq!(
        unhealthy_end.status,
        core_v2::UpdateReadinessStatus::MaintenanceActive as i32
    );

    // Stop and recreate the authority service with a freshly opened gate over
    // the same state directory. The unresolved transaction must survive.
    drop(client);
    stop_authority_server(shutdown, server).await;
    sleep(Duration::from_millis(10)).await;

    let reopened_gate = RuntimeMaintenance::open(&state_dir, catalog)
        .expect("reopen persistent maintenance journal after authority restart");
    let reopened_operator_token = reopened_gate
        .initialize_operator_capability(&operator_path)
        .expect("read the same isolated operator capability");
    assert_eq!(reopened_operator_token, operator_token);

    let restarted_socket = directory.path().join("authority-restarted.sock");
    let restarted_adapter = simulated_kernel_adapter(reopened_gate);
    let (restarted_shutdown, restarted_server) =
        start_authority_server(&restarted_socket, restarted_adapter);
    let mut restarted_client = connect_client(restarted_socket).await;

    let recovered = get_readiness(&mut restarted_client).await;
    assert_readiness(
        &recovered,
        core_v2::UpdateReadinessStatus::MaintenanceActive,
    );
    let blocked_lease = restarted_client
        .acquire_lease(lease_request("recovery-blocked-worker"))
        .await
        .expect_err("recovered maintenance must keep lease admission closed");
    assert_eq!(blocked_lease.code(), tonic::Code::FailedPrecondition);
    let blocked_worker = restarted_client
        .start_worker(worker_request_without_lease("recovery-blocked-worker"))
        .await
        .expect_err("recovered maintenance must keep Worker admission closed");
    assert_eq!(blocked_worker.code(), tonic::Code::FailedPrecondition);

    let recovered_end = end_maintenance(
        &mut restarted_client,
        &reopened_operator_token,
        "restart-recovery",
        &final_begin.maintenance_token,
        core_v2::MaintenanceOutcome::RolledBack,
        true,
    )
    .await;
    assert!(recovered_end.unlocked);
    assert_eq!(
        recovered_end.status,
        core_v2::UpdateReadinessStatus::Ready as i32
    );
    let ready_after_recovery = get_readiness(&mut restarted_client).await;
    assert_readiness(&ready_after_recovery, core_v2::UpdateReadinessStatus::Ready);

    // Admission is available again after a healthy rollback receipt.
    let post_recovery_lease = acquire_lease(&mut restarted_client, "post-recovery-worker").await;
    release_lease(
        &mut restarted_client,
        &post_recovery_lease,
        "post-recovery-release",
    )
    .await;

    drop(restarted_client);
    stop_authority_server(restarted_shutdown, restarted_server).await;
}

fn assert_root_peer_namespace(directory: &Path) {
    use std::os::unix::fs::MetadataExt;

    let metadata = directory
        .metadata()
        .expect("inspect isolated test namespace directory");
    assert_eq!(
        metadata.uid(),
        0,
        "run this authority acceptance under `unshare --user --map-root-user` so SO_PEERCRED carries test-namespace uid 0"
    );
    assert_eq!(
        metadata.gid(),
        0,
        "run this authority acceptance under `unshare --user --map-root-user` so SO_PEERCRED carries test-namespace gid 0"
    );
}

fn empty_catalog() -> TrustedActivitySourceCatalog {
    TrustedActivitySourceCatalog {
        schema_version: 1,
        generation: 1,
        sources: Vec::new(),
    }
}

fn simulated_kernel_adapter(gate: RuntimeMaintenance) -> KernelServiceAdapter {
    let resources = vec![simulated_accelerator()];
    let hardware = Arc::new(SimulatedTestHardware {
        resources: resources.clone(),
    });
    let daemon = Arc::new(KernelDaemon::new(
        hardware.clone(),
        hardware,
        Arc::new(InMemoryResourceManager::new("acceptance-node", resources)),
        Arc::new(SimulatedTestSandbox),
        "acceptance-node",
        1,
    ));
    KernelServiceAdapter::new(daemon, Arc::new(SimulatedWorkerResolver))
        .with_runtime_maintenance(gate)
}

fn start_authority_server(
    socket_path: &Path,
    adapter: KernelServiceAdapter,
) -> (tokio::sync::oneshot::Sender<()>, JoinHandle<ServerResult>) {
    let listener = UnixListener::bind(socket_path).expect("bind isolated authority UDS");
    let (shutdown, shutdown_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        Server::builder()
            .add_service(
                core_v2::kernel_authority_service_server::KernelAuthorityServiceServer::
                    with_interceptor(adapter, inject_authority_principal),
            )
            .serve_with_incoming_shutdown(
                PeerCredAccept::new(UnixListenerStream::new(listener)),
                async {
                    let _ = shutdown_rx.await;
                },
            )
            .await
    });
    (shutdown, server)
}

async fn stop_authority_server(
    shutdown: tokio::sync::oneshot::Sender<()>,
    server: JoinHandle<ServerResult>,
) {
    let _ = shutdown.send(());
    server
        .await
        .expect("authority server task must join")
        .expect("authority server must shut down cleanly");
}

async fn connect_client(socket_path: PathBuf) -> CoreV2Client {
    let channel = Endpoint::try_from("http://[::]:50051")
        .expect("construct local endpoint")
        .connect_with_connector(service_fn(move |_: Uri| {
            let path = socket_path.clone();
            async move { UnixStream::connect(path).await.map(TokioIo::new) }
        }))
        .await
        .expect("connect Core v2 client to authority UDS");
    core_v2::kernel_authority_service_client::KernelAuthorityServiceClient::new(channel)
}

async fn get_readiness(client: &mut CoreV2Client) -> core_v2::UpdateReadinessResponse {
    client
        .get_update_readiness(core_v2::UpdateReadinessRequest {
            target_kind: core_v2::MaintenanceTargetKind::CoreRuntime as i32,
            expected_activity_sources: Vec::new(),
            expected_catalog_generation: 1,
            requires_restart: true,
        })
        .await
        .expect("GetUpdateReadiness must return a decision")
        .into_inner()
}

fn assert_readiness(
    response: &core_v2::UpdateReadinessResponse,
    expected: core_v2::UpdateReadinessStatus,
) {
    assert_eq!(response.status, expected as i32, "readiness={response:?}");
    assert_eq!(response.install_catalog_generation, 1);
    assert_eq!(response.unknown_activity_sources, Vec::<String>::new());
}

async fn begin_maintenance(
    client: &mut CoreV2Client,
    operator_token: &str,
    readiness: &core_v2::UpdateReadinessResponse,
    request_id: &str,
    user_confirmed_restart: bool,
) -> core_v2::BeginMaintenanceResponse {
    client
        .begin_maintenance(begin_request(
            operator_token,
            readiness,
            request_id,
            user_confirmed_restart,
        ))
        .await
        .expect("BeginMaintenance must return an explicit decision")
        .into_inner()
}

fn begin_request(
    operator_token: &str,
    readiness: &core_v2::UpdateReadinessResponse,
    request_id: &str,
    user_confirmed_restart: bool,
) -> core_v2::BeginMaintenanceRequest {
    core_v2::BeginMaintenanceRequest {
        request_id: request_id.to_string(),
        target_kind: core_v2::MaintenanceTargetKind::CoreRuntime as i32,
        expected_gate_generation: readiness.gate_generation,
        user_confirmed_restart,
        expected_activity_sources: Vec::new(),
        expected_catalog_generation: readiness.install_catalog_generation,
        operator_token: operator_token.to_string(),
        plan_id: format!("plan-{request_id}"),
        plan_digest: format!("sha256:{}", "a".repeat(64)),
        component_artifact_digests: HashMap::from([(
            "cyrene.kernel".to_string(),
            format!("sha256:{}", "b".repeat(64)),
        )]),
    }
}

async fn end_maintenance(
    client: &mut CoreV2Client,
    operator_token: &str,
    request_id: &str,
    maintenance_token: &str,
    outcome: core_v2::MaintenanceOutcome,
    healthy: bool,
) -> core_v2::EndMaintenanceResponse {
    client
        .end_maintenance(core_v2::EndMaintenanceRequest {
            request_id: request_id.to_string(),
            maintenance_token: maintenance_token.to_string(),
            outcome: outcome as i32,
            healthy,
            operator_token: operator_token.to_string(),
        })
        .await
        .expect("EndMaintenance must return an explicit unlock decision")
        .into_inner()
}

async fn acquire_lease(client: &mut CoreV2Client, worker_id: &str) -> semantic_v1::Lease {
    client
        .acquire_lease(lease_request(worker_id))
        .await
        .expect("AcquireLease must succeed while maintenance is open")
        .into_inner()
}

fn lease_request(worker_id: &str) -> core_v2::AcquireLeaseRequest {
    core_v2::AcquireLeaseRequest {
        context: Some(authority_context(&format!("acquire-{worker_id}"))),
        holder: Some(semantic_v1::Identity {
            id: worker_id.to_string(),
            generation: 1,
        }),
        query: Some(semantic_v1::ResourceQuery {
            resource_class: "accelerator".to_string(),
            count: 1,
            required_capabilities: vec![semantic_v1::CapabilityRequirement {
                id: "accelerator.compute".to_string(),
                minimum_revision: 1,
                required_properties: HashMap::new(),
            }],
            minimum_capacity: HashMap::new(),
        }),
        ttl: Some(prost_types::Duration {
            seconds: 60,
            nanos: 0,
        }),
    }
}

async fn start_worker(
    client: &mut CoreV2Client,
    lease: &semantic_v1::Lease,
) -> semantic_v1::Identity {
    let identity = lease
        .holder
        .clone()
        .expect("lease returns its holder identity");
    let operation = client
        .start_worker(core_v2::StartWorkerRequest {
            context: Some(authority_context(&format!("start-{}", identity.id))),
            worker: Some(worker_message(identity.clone(), lease)),
        })
        .await
        .expect("StartWorker must create the simulated idle Worker")
        .into_inner();
    assert_eq!(operation.kind, "worker.start");
    assert_eq!(operation.state, semantic_v1::OperationState::Running as i32);
    identity
}

async fn stop_worker(
    client: &mut CoreV2Client,
    lease: &semantic_v1::Lease,
    worker: &semantic_v1::Identity,
) {
    client
        .stop_worker(core_v2::StopWorkerRequest {
            context: Some(authority_context(&format!("stop-{}", worker.id))),
            worker: Some(worker.clone()),
            lease: lease.identity.clone(),
            fence_token: lease.fence_token,
            grace_period: Some(prost_types::Duration {
                seconds: 0,
                nanos: 0,
            }),
        })
        .await
        .expect("StopWorker must release the simulated ManagedProcess");
}

async fn release_lease(client: &mut CoreV2Client, lease: &semantic_v1::Lease, request_id: &str) {
    release_lease_response(client, lease.clone(), request_id).await;
}

async fn release_lease_response(
    client: &mut CoreV2Client,
    lease: semantic_v1::Lease,
    request_id: &str,
) {
    client
        .release_lease(core_v2::ReleaseLeaseRequest {
            context: Some(authority_context(request_id)),
            lease: lease.identity,
            fence_token: lease.fence_token,
        })
        .await
        .expect("ReleaseLease must remain available while clearing test runtime state");
}

fn worker_request_without_lease(worker_id: &str) -> core_v2::StartWorkerRequest {
    let worker = semantic_v1::Worker {
        identity: Some(semantic_v1::Identity {
            id: worker_id.to_string(),
            generation: 1,
        }),
        principal: Some(peer_identity()),
        provider: Some(semantic_v1::Identity {
            id: "test-provider".to_string(),
            generation: 1,
        }),
        lease: Some(semantic_v1::Identity {
            id: "not-created-lease".to_string(),
            generation: 1,
        }),
        state: semantic_v1::WorkerState::Registered as i32,
        execution_ref: "simulated-test-execution".to_string(),
        limits: HashMap::new(),
    };
    core_v2::StartWorkerRequest {
        context: Some(authority_context(&format!("blocked-start-{worker_id}"))),
        worker: Some(worker),
    }
}

fn worker_message(
    identity: semantic_v1::Identity,
    lease: &semantic_v1::Lease,
) -> semantic_v1::Worker {
    semantic_v1::Worker {
        identity: Some(identity),
        principal: Some(peer_identity()),
        provider: Some(semantic_v1::Identity {
            id: "test-provider".to_string(),
            generation: 1,
        }),
        lease: lease.identity.clone(),
        state: semantic_v1::WorkerState::Registered as i32,
        execution_ref: "simulated-test-execution".to_string(),
        limits: HashMap::new(),
    }
}

fn peer_identity() -> semantic_v1::Identity {
    semantic_v1::Identity {
        id: "unix-principal/uid-0/gid-0".to_string(),
        generation: 1,
    }
}

fn authority_context(request_id: &str) -> core_v2::AuthorityCallContext {
    core_v2::AuthorityCallContext {
        namespace: "maintenance-acceptance".to_string(),
        contract: Some(semantic_v1::ContractRevision {
            contract_id: "cyrene.kernel.semantic".to_string(),
            major: 1,
            minor: 0,
        }),
        request_id: request_id.to_string(),
        idempotency_key: request_id.to_string(),
    }
}

#[derive(Debug, Clone)]
struct SimulatedTestHardware {
    resources: Vec<semantic::Resource>,
}

impl HostInventoryProvider for SimulatedTestHardware {
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

impl ResourceProvider for SimulatedTestHardware {
    fn adapter_id(&self) -> &str {
        "simulated-acceptance-hardware"
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
            joinable_environment_keys: Default::default(),
            required_gids: Vec::new(),
            enforcement: EnforcementMode::Hard,
            adapter_id: self.adapter_id().to_string(),
            reason_code: "SIMULATED_ACCEPTANCE_BINDING".to_string(),
        })
    }

    fn read_health(
        &self,
        _resource_id: &str,
    ) -> Result<cy_kernel_api::HealthReport, ProviderError> {
        Ok(cy_kernel_api::HealthReport {
            healthy: Some(true),
            reason_code: "SIMULATED_ACCEPTANCE_READY".to_string(),
            summary: "simulated acceptance fixture".to_string(),
        })
    }
}

fn simulated_accelerator() -> semantic::Resource {
    semantic::Resource {
        identity: semantic::Identity {
            id: "simulated-accelerator-0".to_string(),
            generation: 1,
        },
        provider: semantic::Identity {
            id: "test-provider".to_string(),
            generation: 1,
        },
        resource_class: "accelerator".to_string(),
        capabilities: vec![semantic::Capability {
            id: "accelerator.compute".to_string(),
            revision: 1,
            properties: BTreeMap::new(),
        }],
        capacity: BTreeMap::from([(
            "memory.allocatable".to_string(),
            semantic::Quantity {
                value: 1024,
                unit: "byte".to_string(),
            },
        )]),
        attributes: BTreeMap::new(),
        state: semantic::ResourceState::Ready,
        reason_code: "SIMULATED_ACCEPTANCE_READY".to_string(),
        summary: "simulated accelerator for Core maintenance acceptance".to_string(),
        links: Vec::new(),
    }
}

#[derive(Debug)]
struct SimulatedTestSandbox;

impl ProcessRuntime for SimulatedTestSandbox {
    fn preflight(&self) -> NodeCapabilities {
        NodeCapabilities {
            ready: true,
            facts: vec![CapabilityFact {
                name: "simulated-sandbox".to_string(),
                available: true,
                required: true,
                detail: "in-process acceptance fixture; no host sandboxd".to_string(),
            }],
            enforcement: Vec::new(),
        }
    }

    fn launch(
        &self,
        _plan: &LaunchPlan,
        _binding: &DeviceBinding,
    ) -> Result<ProcessHandle, ProviderError> {
        Ok(ProcessHandle {
            pid: 1,
            cgroup_path: PathBuf::from("/simulated/acceptance"),
            start_time_ticks: Some(1),
            transport_socket: None,
        })
    }

    fn stop(
        &self,
        _handle: &ProcessHandle,
        _request: &StopRequest,
    ) -> Result<CleanupReport, ProviderError> {
        Ok(CleanupReport {
            complete: true,
            exit_code: Some(0),
            oom_killed: false,
            conditions: Vec::<ProcessCondition>::new(),
            reason_code: "SIMULATED_ACCEPTANCE_STOP".to_string(),
        })
    }
}

impl SandboxBackend for SimulatedTestSandbox {
    fn backend_id(&self) -> &str {
        "simulated-acceptance-sandbox"
    }
}

struct SimulatedWorkerResolver;

impl InstalledPluginResolver for SimulatedWorkerResolver {
    fn resolve_launch_plan(
        &self,
        _installation: &VerifiedInstallation,
        _instance_name: &str,
    ) -> Result<ResolvedLaunchPlan, ProviderError> {
        Err(ProviderError::new(
            "simulated-acceptance-resolver",
            "LEGACY_LAUNCH_UNUSED",
            "this acceptance uses only Core v2 semantic Worker calls",
        ))
    }

    fn resolve_worker_launch_plan(
        &self,
        worker: &semantic::Worker,
    ) -> Result<ResolvedLaunchPlan, ProviderError> {
        Ok(ResolvedLaunchPlan {
            installation: VerifiedInstallation {
                installation_name: "simulated-acceptance-worker".to_string(),
                manifest_digest: "sha256:simulated".to_string(),
                artifact_digest: "sha256:simulated".to_string(),
                verified_signature_identity: "simulated-test-fixture".to_string(),
            },
            plan: LaunchPlan {
                instance_name: worker.identity.id.clone(),
                executable: PathBuf::from("simulated-worker"),
                args: Vec::new(),
                environment: BTreeMap::new(),
                cgroup_name: format!("simulated-{}", worker.identity.id),
                limits: CgroupLimits::default(),
                working_dir: None,
                transport_socket: None,
            },
        })
    }
}
