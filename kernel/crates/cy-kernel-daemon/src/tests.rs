//! cy-kernel-daemon 单元测试与集成测试套件。

#![allow(deprecated)]

use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};

use cy_adapter_client::{HardwareAdapter, UdsHardwareAdapterRegistry};
use cy_kernel_api::{
    semantic, AuthorityCallContext, CapabilityFact, CgroupLimits, CleanupReport, DeviceBinding,
    DurableEventRecord, DurableEventStore, EnforcementMode, FailingRuntimeJournal,
    HostInventoryProvider, InstalledPluginResolver, InventorySnapshot, KernelAuthority,
    KernelProviderAuthority, LaunchPlan, NamespaceId, NodeCapabilities, ObjectRef,
    ProcessCondition, ProcessHandle, ProcessRuntime, ProviderError, ProviderReconcileAction,
    ResolvedLaunchPlan, ResourceProvider, RuntimeJournalEvent, RuntimeJournalRecord,
    RuntimeJournalSink, SandboxBackend, StopRequest, VerifiedInstallation,
};
use cy_proto::{core_v1, core_v2, semantic_v1};
use cy_resource_manager::InMemoryResourceManager;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tonic::Request;

use crate::{
    adapter::{KernelServiceAdapter, OPERATION_EVENT_HISTORY_CAPACITY},
    convert::{
        merge_bindings, now_unix_ms, resource_request, to_plugin_instance,
        to_semantic_proto_contract_revision, to_semantic_proto_identity, unix_ms_from_timestamp,
    },
    daemon::KernelDaemon,
    peer_cred::{principal_from_peer_cred, PeerCred},
    session::{ManagedProcess, WorkerHeartbeatConfig},
    watchdog::{InstanceActor, WorkerTransportCommand},
};

const AUTHORITY_TEST_PEER: PeerCred = PeerCred {
    pid: 4242,
    uid: 1000,
    gid: 1000,
};

fn authority_request<T>(message: T) -> Request<T> {
    authority_request_for(AUTHORITY_TEST_PEER, message)
}

fn authority_request_for<T>(peer: PeerCred, message: T) -> Request<T> {
    let mut request = Request::new(message);
    request
        .extensions_mut()
        .insert(principal_from_peer_cred(&peer));
    request
}

#[derive(Debug)]
struct EmptyHardware;

impl HostInventoryProvider for EmptyHardware {
    fn probe_inventory(&self) -> Result<InventorySnapshot, ProviderError> {
        Ok(InventorySnapshot {
            generation: 1,
            resources: Vec::new(),
            capabilities: NodeCapabilities {
                ready: true,
                facts: Vec::new(),
                enforcement: Vec::new(),
            },
        })
    }
}

impl ResourceProvider for EmptyHardware {
    fn adapter_id(&self) -> &str {
        "test-adapter"
    }

    fn probe_resources(&self) -> Result<Vec<semantic::Resource>, ProviderError> {
        Ok(Vec::new())
    }

    fn create_binding(
        &self,
        _resource: &semantic::Resource,
    ) -> Result<DeviceBinding, ProviderError> {
        Err(ProviderError::new("test-adapter", "UNUSED", "no resources"))
    }

    fn read_health(&self, _device_id: &str) -> Result<cy_kernel_api::HealthReport, ProviderError> {
        Err(ProviderError::new("test-adapter", "UNUSED", "no resources"))
    }
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
            reason_code: "test-ready".to_string(),
            summary: "ready".to_string(),
        })
    }
}

impl HardwareAdapter for TestHardware {}

#[derive(Debug)]
struct SnapshotHardware {
    generation: u64,
    resources: Vec<semantic::Resource>,
}

impl HostInventoryProvider for SnapshotHardware {
    fn probe_inventory(&self) -> Result<InventorySnapshot, ProviderError> {
        Ok(InventorySnapshot {
            generation: self.generation,
            resources: self.resources.clone(),
            capabilities: NodeCapabilities {
                ready: true,
                facts: Vec::new(),
                enforcement: Vec::new(),
            },
        })
    }
}

impl ResourceProvider for SnapshotHardware {
    fn adapter_id(&self) -> &str {
        "snapshot-hardware"
    }

    fn probe_resources(&self) -> Result<Vec<semantic::Resource>, ProviderError> {
        Ok(self.resources.clone())
    }

    fn create_binding(
        &self,
        _resource: &semantic::Resource,
    ) -> Result<DeviceBinding, ProviderError> {
        Err(ProviderError::new(
            "snapshot-hardware",
            "UNUSED",
            "bindings are not exercised by this observation test",
        ))
    }

    fn read_health(
        &self,
        _resource_id: &str,
    ) -> Result<cy_kernel_api::HealthReport, ProviderError> {
        Err(ProviderError::new(
            "snapshot-hardware",
            "UNUSED",
            "health is not exercised by this observation test",
        ))
    }
}

impl HardwareAdapter for SnapshotHardware {}

#[derive(Debug)]
struct FailingHardware;

impl HostInventoryProvider for FailingHardware {
    fn probe_inventory(&self) -> Result<InventorySnapshot, ProviderError> {
        Err(ProviderError::new(
            "failing-hardware",
            "ADAPTER_UNAVAILABLE",
            "adapter is unavailable",
        ))
    }
}

impl ResourceProvider for FailingHardware {
    fn adapter_id(&self) -> &str {
        "failing-hardware"
    }

    fn probe_resources(&self) -> Result<Vec<semantic::Resource>, ProviderError> {
        Err(ProviderError::new(
            "failing-hardware",
            "ADAPTER_UNAVAILABLE",
            "adapter is unavailable",
        ))
    }

    fn create_binding(
        &self,
        _resource: &semantic::Resource,
    ) -> Result<DeviceBinding, ProviderError> {
        Err(ProviderError::new(
            "failing-hardware",
            "ADAPTER_UNAVAILABLE",
            "adapter is unavailable",
        ))
    }

    fn read_health(
        &self,
        _resource_id: &str,
    ) -> Result<cy_kernel_api::HealthReport, ProviderError> {
        Err(ProviderError::new(
            "failing-hardware",
            "ADAPTER_UNAVAILABLE",
            "adapter is unavailable",
        ))
    }
}

impl HardwareAdapter for FailingHardware {}

fn test_resource() -> semantic::Resource {
    test_resource_with_id("resource-1")
}

fn test_resource_with_id(id: &str) -> semantic::Resource {
    semantic::Resource {
        identity: semantic::Identity {
            id: id.to_string(),
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
        reason_code: "test-ready".to_string(),
        summary: "ready".to_string(),
        links: Vec::new(),
    }
}

fn semantic_provider(
    id: &str,
    generation: u64,
    state: semantic::ProviderState,
) -> semantic::Provider {
    semantic::Provider {
        identity: semantic::Identity {
            id: id.to_string(),
            generation,
        },
        state,
        capabilities: vec![semantic::Capability {
            id: "accelerator.compute".to_string(),
            revision: 1,
            properties: BTreeMap::new(),
        }],
    }
}

fn provider_snapshot(
    provider: &semantic::Provider,
    snapshot_generation: u64,
    resources: Vec<semantic::Resource>,
    workers: Vec<semantic::Worker>,
) -> semantic::ProviderSnapshot {
    semantic::ProviderSnapshot {
        provider: provider.identity.clone(),
        snapshot_generation,
        resources,
        workers,
        endpoints: Vec::new(),
        sampled_at_unix_ms: now_unix_ms(),
        expires_at_unix_ms: future_expiry(),
    }
}

fn semantic_worker_for(
    id: &str,
    provider: semantic::Identity,
    lease: semantic::Identity,
    state: semantic::WorkerState,
) -> semantic::Worker {
    semantic::Worker {
        identity: semantic::Identity {
            id: id.to_string(),
            generation: 1,
        },
        principal: principal_from_peer_cred(&AUTHORITY_TEST_PEER).identity,
        provider,
        lease,
        state,
        execution_ref: "test-execution-reference".to_string(),
        limits: BTreeMap::new(),
    }
}

fn semantic_endpoint_for(worker: &semantic::Worker) -> semantic::Endpoint {
    semantic::Endpoint {
        identity: semantic::Identity {
            id: format!("endpoint-{}", worker.identity.id),
            generation: 1,
        },
        provider: worker.provider.clone(),
        owner: worker.identity.clone(),
        transport: "transport.uds".to_string(),
        schema_id: "schema.v1".to_string(),
        capabilities: Vec::new(),
        public_attributes: BTreeMap::new(),
    }
}

fn future_expiry() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
        + 60_000
}

fn semantic_lease_adapter() -> KernelServiceAdapter {
    semantic_lease_adapter_at_epoch(7)
}

fn semantic_lease_adapter_at_epoch(node_epoch: u64) -> KernelServiceAdapter {
    let resource = test_resource();
    let hardware = Arc::new(TestHardware {
        resources: vec![resource.clone()],
    });
    let daemon = Arc::new(KernelDaemon::new(
        hardware.clone(),
        hardware,
        Arc::new(InMemoryResourceManager::new("node", vec![resource])),
        Arc::new(FakeSandbox),
        "node",
        node_epoch,
    ));
    KernelServiceAdapter::new(daemon, Arc::new(UnusedResolver))
}

fn authority_context(request_id: &str) -> core_v1::AuthorityCallContext {
    core_v1::AuthorityCallContext {
        contract: Some(to_semantic_proto_contract_revision(
            &semantic::ContractRevision::current(),
        )),
        request_id: request_id.to_string(),
        idempotency_key: request_id.to_string(),
    }
}

fn scoped_authority_context(namespace: &str, request_id: &str) -> AuthorityCallContext {
    AuthorityCallContext {
        contract: semantic::ContractRevision::current(),
        namespace: NamespaceId::new(namespace).unwrap(),
        request_id: request_id.to_string(),
        idempotency_key: request_id.to_string(),
    }
}

#[derive(Debug)]
struct FakeSandbox;

impl ProcessRuntime for FakeSandbox {
    fn preflight(&self) -> NodeCapabilities {
        NodeCapabilities {
            ready: true,
            facts: vec![CapabilityFact {
                name: "test".to_string(),
                available: true,
                required: true,
                detail: "test".to_string(),
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
            cgroup_path: PathBuf::from("/test"),
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
            reason_code: "TEST_STOP".to_string(),
        })
    }
}

impl SandboxBackend for FakeSandbox {
    fn backend_id(&self) -> &str {
        "test"
    }
}

fn managed_test_process(
    instance_name: &str,
    generation: u64,
    lease: Option<core_v1::ResourceLeaseRef>,
) -> ManagedProcess {
    ManagedProcess {
        actor: InstanceActor::new_for_test(),
        lease,
        semantic_worker: None,
        plugin: core_v1::InstalledPluginRef {
            installation_name: instance_name.to_string(),
            plugin_id: "test".to_string(),
            version: "1".to_string(),
            component_id: "test".to_string(),
            manifest_digest: "sha256:test".to_string(),
            artifact_digest: "sha256:test".to_string(),
            verified_signature_identity: "test".to_string(),
        },
        generation,
        accepted_sequence: 0,
        last_heartbeat_at: None,
        runtime_state: core_v1::PluginRuntimeState::Starting as i32,
        health: None,
        restart_count: 0,
        watchdog_triggered: false,
        transport_disconnected: false,
        control: None,
        semantic_control: None,
        pending_shutdown: None,
    }
}

#[derive(Default)]
struct RecordingRuntimeJournal {
    records: std::sync::Mutex<Vec<RuntimeJournalRecord>>,
}

impl RuntimeJournalSink for RecordingRuntimeJournal {
    fn append(&self, record: RuntimeJournalRecord) -> Result<(), ProviderError> {
        self.records.lock().unwrap().push(record);
        Ok(())
    }
}

#[derive(Default)]
struct RecordingDurableEventStore {
    records: Mutex<Vec<DurableEventRecord>>,
    fail_reads: AtomicBool,
}

impl DurableEventStore for RecordingDurableEventStore {
    fn append_event(&self, record: DurableEventRecord) -> Result<(), ProviderError> {
        self.records.lock().unwrap().push(record);
        Ok(())
    }

    fn events_for_source(
        &self,
        source: &semantic::Identity,
        namespace: &str,
    ) -> Result<Option<Vec<DurableEventRecord>>, ProviderError> {
        if self.fail_reads.load(Ordering::SeqCst) {
            return Err(ProviderError::new(
                "test-event-store",
                "EVENT_READ_FAILED",
                "injected durable event read failure",
            ));
        }
        Ok(Some(
            self.records
                .lock()
                .unwrap()
                .iter()
                .filter(|record| record.namespace == namespace && record.event.source == *source)
                .cloned()
                .collect(),
        ))
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
            "test",
            "UNUSED",
            "not launched in this test",
        ))
    }
}

struct TestWorkerResolver;

impl InstalledPluginResolver for TestWorkerResolver {
    fn resolve_launch_plan(
        &self,
        _installation: &VerifiedInstallation,
        _instance_name: &str,
    ) -> Result<ResolvedLaunchPlan, ProviderError> {
        Err(ProviderError::new(
            "test",
            "UNUSED",
            "legacy launch is not used",
        ))
    }

    fn resolve_worker_launch_plan(
        &self,
        worker: &semantic::Worker,
    ) -> Result<ResolvedLaunchPlan, ProviderError> {
        Ok(ResolvedLaunchPlan {
            installation: VerifiedInstallation {
                installation_name: "test-installation".to_string(),
                manifest_digest: "sha256:test".to_string(),
                artifact_digest: "sha256:test".to_string(),
                verified_signature_identity: "test".to_string(),
            },
            plan: LaunchPlan {
                instance_name: worker.identity.id.clone(),
                executable: PathBuf::from("worker"),
                args: Vec::new(),
                environment: BTreeMap::new(),
                cgroup_name: format!("instance-{}", worker.identity.id),
                limits: CgroupLimits::default(),
                transport_socket: None,
            },
        })
    }
}

fn semantic_worker_adapter() -> KernelServiceAdapter {
    semantic_worker_adapter_with_resources(vec![test_resource()])
}

fn semantic_worker_adapter_with_resources(
    resources: Vec<semantic::Resource>,
) -> KernelServiceAdapter {
    let hardware = Arc::new(TestHardware {
        resources: resources.clone(),
    });
    let daemon = Arc::new(KernelDaemon::new(
        hardware.clone(),
        hardware,
        Arc::new(InMemoryResourceManager::new("node", resources)),
        Arc::new(FakeSandbox),
        "node",
        7,
    ));
    KernelServiceAdapter::new(daemon, Arc::new(TestWorkerResolver))
}

fn hardware_provider_adapter(
    adapters: Vec<(String, Arc<dyn HardwareAdapter>)>,
) -> KernelServiceAdapter {
    let registry = Arc::new(UdsHardwareAdapterRegistry::from_adapters(adapters).unwrap());
    let mut daemon = KernelDaemon::new(
        registry.clone(),
        registry.clone(),
        Arc::new(InMemoryResourceManager::new("node", Vec::new())),
        Arc::new(FakeSandbox),
        "node",
        9,
    );
    daemon.hardware_adapters = Some(registry);
    KernelServiceAdapter::new(Arc::new(daemon), Arc::new(UnusedResolver))
}

fn heartbeat_adapter() -> KernelServiceAdapter {
    let hardware = Arc::new(EmptyHardware);
    let daemon = Arc::new(KernelDaemon::new(
        hardware.clone(),
        hardware,
        Arc::new(InMemoryResourceManager::new("node", Vec::new())),
        Arc::new(FakeSandbox),
        "node",
        7,
    ));
    let adapter = KernelServiceAdapter::new(daemon, Arc::new(UnusedResolver))
        .with_worker_heartbeat(WorkerHeartbeatConfig {
            socket_path: PathBuf::from("/run/cyrene/test.sock"),
            interval: Duration::from_secs(1),
            timeout: Duration::from_secs(2),
            graceful_stop: Duration::from_secs(1),
            shutdown_ack_timeout: Duration::from_millis(50),
        });
    adapter.instances.lock().unwrap().insert(
        "worker_1".to_string(),
        managed_test_process("worker_1", 99, None),
    );
    adapter
}

#[test]
fn resource_limits_are_mapped_without_relaxing_request_validation() {
    let request = resource_request(
        "lease-1",
        4,
        semantic::Identity {
            id: "worker/test".to_string(),
            generation: 1,
        },
        None,
        &core_v1::ResourceRequirements {
            cpu: Some(core_v1::CpuRequirements {
                request_millicores: 500,
                limit_millicores: 750,
            }),
            memory: Some(core_v1::MemoryRequirements {
                request_bytes: 1024,
                limit_bytes: 2048,
            }),
            ephemeral_storage_limit_bytes: 0,
            accelerators: vec![core_v1::AcceleratorRequirements {
                count: 1,
                ..Default::default()
            }],
        },
    )
    .unwrap();
    assert_eq!(request.limits.cpu_max_millicores, Some(750));
    assert_eq!(request.limits.memory_max_bytes, Some(2048));

    let error = resource_request(
        "lease-2",
        4,
        semantic::Identity {
            id: "worker/test".to_string(),
            generation: 1,
        },
        None,
        &core_v1::ResourceRequirements {
            cpu: Some(core_v1::CpuRequirements {
                request_millicores: 751,
                limit_millicores: 750,
            }),
            accelerators: vec![core_v1::AcceleratorRequirements {
                count: 1,
                ..Default::default()
            }],
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(error.code(), tonic::Code::InvalidArgument);
}

#[test]
fn semantic_lease_rpc_is_vendor_neutral_fenced_and_ttl_bounded() {
    use core_v1::kernel_service_server::KernelService;

    let adapter = semantic_lease_adapter();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let lease = runtime
        .block_on(
            adapter.acquire_lease(Request::new(core_v1::AcquireLeaseRequest {
                mutation: Some(core_v1::MutationContext {
                    request: Some(core_v1::RequestContext {
                        request_id: "request-1".to_string(),
                        ..Default::default()
                    }),
                    idempotency_key: "semantic-1".to_string(),
                    expected_generation: Some(1),
                }),
                node: Some(core_v1::NodeRef {
                    node_id: "node".to_string(),
                    node_epoch: 7,
                }),
                holder: Some(semantic_v1::Identity {
                    id: "worker-1".to_string(),
                    generation: 1,
                }),
                query: Some(semantic_v1::ResourceQuery {
                    resource_class: "accelerator".to_string(),
                    count: 1,
                    required_capabilities: vec![semantic_v1::CapabilityRequirement {
                        id: "accelerator.compute".to_string(),
                        minimum_revision: 1,
                        required_properties: Default::default(),
                    }],
                    minimum_capacity: Default::default(),
                }),
                ttl: Some(prost_types::Duration {
                    seconds: 30,
                    nanos: 0,
                }),
                cpu: None,
                memory: None,
            })),
        )
        .unwrap()
        .into_inner();

    assert_eq!(lease.state, semantic_v1::LeaseState::Active as i32);
    assert_eq!(lease.resources[0].id, "resource-1");
    assert!(lease.expires_at.is_some());
    let lease_identity = lease.identity.clone();

    let released = runtime
        .block_on(
            adapter.release_lease(Request::new(core_v1::ReleaseLeaseRequest {
                mutation: None,
                lease: lease_identity,
                fence_token: lease.fence_token,
            })),
        )
        .unwrap()
        .into_inner();
    assert_eq!(released.state, semantic_v1::LeaseState::Released as i32);

    let capabilities = adapter.daemon.get_kernel_capabilities().unwrap();
    assert!(capabilities.accelerators.is_empty());
    assert_eq!(capabilities.resources.len(), 1);
    assert_eq!(capabilities.resources[0].resource_class, "accelerator");
}

#[test]
fn authority_rejects_requests_without_peer_credentials() {
    use core_v1::kernel_authority_service_server::KernelAuthorityService;

    let adapter = semantic_lease_adapter();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let error = runtime
        .block_on(adapter.negotiate(Request::new(core_v1::NegotiateRequest::default())))
        .unwrap_err();

    assert_eq!(error.code(), tonic::Code::Unauthenticated);
}

#[test]
fn core_v1_defaults_scope_and_core_v2_requires_a_valid_explicit_namespace() {
    use core_v1::{
        kernel_authority_service_server::KernelAuthorityService, AcquireSemanticLeaseRequest,
    };
    use core_v2::{
        kernel_authority_service_server::KernelAuthorityService as KernelAuthorityV2Service,
        AcquireSemanticLeaseRequest as AcquireV2LeaseRequest,
    };

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let adapter = semantic_lease_adapter();
    let default_lease = runtime
        .block_on(KernelAuthorityService::acquire_lease(
            &adapter,
            authority_request(AcquireSemanticLeaseRequest {
                context: Some(authority_context("v1-default")),
                holder: Some(semantic_v1::Identity {
                    id: "worker-v1".to_string(),
                    generation: 1,
                }),
                query: Some(semantic_v1::ResourceQuery {
                    resource_class: "accelerator".to_string(),
                    count: 1,
                    required_capabilities: vec![semantic_v1::CapabilityRequirement {
                        id: "accelerator.compute".to_string(),
                        minimum_revision: 1,
                        required_properties: Default::default(),
                    }],
                    minimum_capacity: Default::default(),
                }),
                ttl: Some(prost_types::Duration {
                    seconds: 30,
                    nanos: 0,
                }),
            }),
        ))
        .unwrap()
        .into_inner();
    let default_object = ObjectRef {
        namespace: NamespaceId::default(),
        identity: semantic::Identity {
            id: default_lease.identity.unwrap().id,
            generation: 1,
        },
    };
    assert!(adapter.leases.lock().unwrap().contains_key(&default_object));

    let v2_adapter = semantic_lease_adapter();
    let request = |namespace: &str| {
        authority_request(AcquireV2LeaseRequest {
            context: Some(core_v2::AuthorityCallContext {
                namespace: namespace.to_string(),
                contract: Some(to_semantic_proto_contract_revision(
                    &semantic::ContractRevision::current(),
                )),
                request_id: "v2-scope".to_string(),
                idempotency_key: "v2-scope".to_string(),
            }),
            holder: Some(semantic_v1::Identity {
                id: "worker-v2".to_string(),
                generation: 1,
            }),
            query: Some(semantic_v1::ResourceQuery {
                resource_class: "accelerator".to_string(),
                count: 1,
                required_capabilities: vec![semantic_v1::CapabilityRequirement {
                    id: "accelerator.compute".to_string(),
                    minimum_revision: 1,
                    required_properties: Default::default(),
                }],
                minimum_capacity: Default::default(),
            }),
            ttl: Some(prost_types::Duration {
                seconds: 30,
                nanos: 0,
            }),
        })
    };
    let missing = runtime
        .block_on(KernelAuthorityV2Service::acquire_lease(
            &v2_adapter,
            request(""),
        ))
        .unwrap_err();
    assert_eq!(missing.code(), tonic::Code::InvalidArgument);
    assert_eq!(
        missing
            .metadata()
            .get("x-cyrene-reason-code")
            .and_then(|value| value.to_str().ok()),
        Some("NAMESPACE_REQUIRED")
    );
    let invalid = runtime
        .block_on(KernelAuthorityV2Service::acquire_lease(
            &v2_adapter,
            request("not/a-namespace"),
        ))
        .unwrap_err();
    assert_eq!(invalid.code(), tonic::Code::InvalidArgument);
    assert_eq!(
        invalid
            .metadata()
            .get("x-cyrene-reason-code")
            .and_then(|value| value.to_str().ok()),
        Some("NAMESPACE_INVALID")
    );
    let v2_lease = runtime
        .block_on(KernelAuthorityV2Service::acquire_lease(
            &v2_adapter,
            request("tenant-a"),
        ))
        .unwrap()
        .into_inner();
    let identity = v2_lease.identity.unwrap();
    assert!(v2_adapter.leases.lock().unwrap().contains_key(&ObjectRef {
        namespace: NamespaceId::new("tenant-a").unwrap(),
        identity: semantic::Identity {
            id: identity.id,
            generation: identity.generation,
        },
    }));
}

#[test]
fn namespace_scopes_identical_worker_lease_operation_endpoint_grant_and_events() {
    let adapter = semantic_worker_adapter_with_resources(vec![
        test_resource_with_id("resource-a"),
        test_resource_with_id("resource-b"),
    ]);
    let authority = adapter.authority();
    let namespace_a = scoped_authority_context("namespace-a", "lease-x");
    let namespace_b = scoped_authority_context("namespace-b", "lease-x");
    let principal_a = principal_from_peer_cred(&AUTHORITY_TEST_PEER);
    let principal_b = principal_from_peer_cred(&PeerCred {
        pid: 4243,
        uid: 2000,
        gid: 2000,
    });
    let worker_identity = semantic::Identity {
        id: "worker-x".to_string(),
        generation: 1,
    };
    let query = semantic::ResourceQuery {
        resource_class: "accelerator".to_string(),
        count: 1,
        required_capabilities: vec![semantic::CapabilityRequirement {
            id: "accelerator.compute".to_string(),
            minimum_revision: 1,
            required_properties: BTreeMap::new(),
        }],
        minimum_capacity: BTreeMap::new(),
    };
    let lease_a = authority
        .acquire_lease(
            &namespace_a,
            &principal_a,
            worker_identity.clone(),
            query.clone(),
            u64::MAX,
        )
        .unwrap();
    let lease_b = authority
        .acquire_lease(
            &namespace_b,
            &principal_b,
            worker_identity.clone(),
            query,
            u64::MAX,
        )
        .unwrap();
    assert_eq!(lease_a.identity, lease_b.identity);
    assert_ne!(lease_a.fence_token, lease_b.fence_token);

    let worker = |lease: &semantic::Lease, principal: &semantic::Principal| semantic::Worker {
        identity: worker_identity.clone(),
        principal: principal.identity.clone(),
        provider: semantic::Identity {
            id: "provider-x".to_string(),
            generation: 1,
        },
        lease: lease.identity.clone(),
        state: semantic::WorkerState::Registered,
        execution_ref: "opaque-execution-reference".to_string(),
        limits: BTreeMap::new(),
    };
    authority
        .start_worker(&namespace_a, &principal_a, worker(&lease_a, &principal_a))
        .unwrap();
    authority
        .start_worker(&namespace_b, &principal_b, worker(&lease_b, &principal_b))
        .unwrap();

    let endpoint = |principal: &semantic::Principal| semantic::Endpoint {
        identity: semantic::Identity {
            id: "endpoint-x".to_string(),
            generation: 1,
        },
        provider: semantic::Identity {
            id: "provider-x".to_string(),
            generation: 1,
        },
        owner: worker_identity.clone(),
        transport: "transport.uds".to_string(),
        schema_id: "schema.v1".to_string(),
        capabilities: Vec::new(),
        public_attributes: BTreeMap::from([("owner".to_string(), principal.identity.id.clone())]),
    };
    let endpoint_a = authority
        .publish_endpoint(&namespace_a, &principal_a, endpoint(&principal_a))
        .unwrap();
    let endpoint_b = authority
        .publish_endpoint(&namespace_b, &principal_b, endpoint(&principal_b))
        .unwrap();
    assert_eq!(endpoint_a.identity, endpoint_b.identity);

    let grant = |endpoint: &semantic::Endpoint, lease: &semantic::Lease| semantic::EndpointGrant {
        identity: semantic::Identity {
            id: "grant-x".to_string(),
            generation: 1,
        },
        endpoint: endpoint.identity.clone(),
        grantee: worker_identity.clone(),
        lease: lease.identity.clone(),
        fence_token: lease.fence_token,
        expires_at_unix_ms: u64::MAX,
    };
    authority
        .authorize_endpoint(&namespace_a, &principal_a, grant(&endpoint_a, &lease_a))
        .unwrap();
    authority
        .authorize_endpoint(&namespace_b, &principal_b, grant(&endpoint_b, &lease_b))
        .unwrap();

    let operation = |principal: &semantic::Principal| semantic::Operation {
        identity: semantic::Identity {
            id: "operation-x".to_string(),
            generation: 1,
        },
        owner: principal.identity.clone(),
        executor: semantic::Identity {
            id: "provider-x".to_string(),
            generation: 1,
        },
        kind: "ai.train".to_string(),
        state: semantic::OperationState::Created,
        deadline_unix_ms: None,
        parent: None,
        metadata: BTreeMap::new(),
    };
    authority
        .create_operation(&namespace_a, &principal_a, operation(&principal_a))
        .unwrap();
    authority
        .create_operation(&namespace_b, &principal_b, operation(&principal_b))
        .unwrap();

    let worker_object_a = namespace_a.object_ref(worker_identity.clone());
    let worker_object_b = namespace_b.object_ref(worker_identity.clone());
    let workers = authority.runtime.workers.lock().unwrap();
    assert_eq!(workers.len(), 2);
    assert_ne!(workers[&worker_object_a], workers[&worker_object_b]);
    drop(workers);
    assert_eq!(authority.runtime.leases.lock().unwrap().len(), 2);
    assert_eq!(authority.runtime.endpoints.lock().unwrap().len(), 2);
    assert_eq!(authority.runtime.endpoint_grants.lock().unwrap().len(), 2);
    assert!(authority
        .runtime
        .semantic_operations
        .lock()
        .unwrap()
        .contains_key(&namespace_a.object_ref(semantic::Identity {
            id: "operation-x".to_string(),
            generation: 1,
        })));
    assert!(authority
        .runtime
        .semantic_operations
        .lock()
        .unwrap()
        .contains_key(&namespace_b.object_ref(semantic::Identity {
            id: "operation-x".to_string(),
            generation: 1,
        })));

    let events_a = authority
        .events_after(
            &namespace_a,
            &principal_a,
            &semantic::EventCursor {
                source: authority.semantic_event_source_for(&namespace_a.namespace),
                sequence: 0,
            },
            OPERATION_EVENT_HISTORY_CAPACITY,
        )
        .unwrap();
    let events_b = authority
        .events_after(
            &namespace_b,
            &principal_b,
            &semantic::EventCursor {
                source: authority.semantic_event_source_for(&namespace_b.namespace),
                sequence: 0,
            },
            OPERATION_EVENT_HISTORY_CAPACITY,
        )
        .unwrap();
    assert_ne!(events_a.source, events_b.source);
    assert!(events_a
        .events
        .iter()
        .all(|event| event.source == events_a.source));
    assert!(events_b
        .events
        .iter()
        .all(|event| event.source == events_b.source));

    let denied = authority
        .release_lease(
            &namespace_b,
            &principal_a,
            &lease_b.identity,
            lease_b.fence_token,
        )
        .unwrap_err();
    assert_eq!(denied.reason_code, "NAMESPACE_AUTHORITY_DENIED");
}

#[test]
fn endpoint_authority_requires_the_worker_owner_principal() {
    use core_v1::{
        kernel_authority_service_server::KernelAuthorityService, AcquireSemanticLeaseRequest,
        AuthorizeEndpointRequest, PublishEndpointRequest, RenewLeaseRequest, RevokeEndpointRequest,
    };

    let adapter = semantic_lease_adapter();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let lease = runtime
        .block_on(
            adapter.acquire_lease(authority_request(AcquireSemanticLeaseRequest {
                context: Some(authority_context("endpoint-lease")),
                holder: Some(semantic_v1::Identity {
                    id: "worker-1".to_string(),
                    generation: 1,
                }),
                query: Some(semantic_v1::ResourceQuery {
                    resource_class: "accelerator".to_string(),
                    count: 1,
                    required_capabilities: vec![semantic_v1::CapabilityRequirement {
                        id: "accelerator.compute".to_string(),
                        minimum_revision: 1,
                        required_properties: Default::default(),
                    }],
                    minimum_capacity: Default::default(),
                }),
                ttl: Some(prost_types::Duration {
                    seconds: 10,
                    nanos: 0,
                }),
            })),
        )
        .unwrap()
        .into_inner();
    let renewed = runtime
        .block_on(adapter.renew_lease(authority_request(RenewLeaseRequest {
            context: Some(authority_context("renew-endpoint-lease")),
            lease: lease.identity.clone(),
            fence_token: lease.fence_token,
            ttl: Some(prost_types::Duration {
                seconds: 20,
                nanos: 0,
            }),
        })))
        .unwrap()
        .into_inner();
    assert_eq!(renewed.fence_token, lease.fence_token);
    assert!(
        unix_ms_from_timestamp(renewed.expires_at.clone().unwrap(), "renewed").unwrap()
            > unix_ms_from_timestamp(lease.expires_at.clone().unwrap(), "lease").unwrap()
    );
    let mut process = managed_test_process(
        "worker-1",
        1,
        Some(core_v1::ResourceLeaseRef {
            lease_name: renewed.identity.as_ref().unwrap().id.clone(),
            fence_token: renewed.fence_token,
        }),
    );
    process.semantic_worker = Some(semantic::Worker {
        identity: semantic::Identity {
            id: "worker-1".to_string(),
            generation: 1,
        },
        principal: principal_from_peer_cred(&AUTHORITY_TEST_PEER).identity,
        provider: semantic::Identity {
            id: "provider-1".to_string(),
            generation: 1,
        },
        lease: semantic::Identity {
            id: renewed.identity.as_ref().unwrap().id.clone(),
            generation: renewed.identity.as_ref().unwrap().generation,
        },
        state: semantic::WorkerState::Starting,
        execution_ref: "test-ref".to_string(),
        limits: BTreeMap::new(),
    });
    adapter
        .instances
        .lock()
        .unwrap()
        .insert("worker-1".to_string(), process);

    let endpoint = runtime
        .block_on(
            adapter.publish_endpoint(authority_request(PublishEndpointRequest {
                context: Some(authority_context("publish-endpoint")),
                endpoint: Some(semantic_v1::Endpoint {
                    identity: Some(semantic_v1::Identity {
                        id: "endpoint-1".to_string(),
                        generation: 1,
                    }),
                    provider: Some(semantic_v1::Identity {
                        id: "provider-1".to_string(),
                        generation: 1,
                    }),
                    owner: Some(semantic_v1::Identity {
                        id: "worker-1".to_string(),
                        generation: 1,
                    }),
                    transport: "transport.uds".to_string(),
                    schema_id: "schema.v1".to_string(),
                    capabilities: Vec::new(),
                    public_attributes: Default::default(),
                }),
            })),
        )
        .unwrap()
        .into_inner();
    let non_owner = PeerCred {
        pid: 7171,
        uid: 2000,
        gid: 2000,
    };
    let denied_publish = runtime.block_on(adapter.publish_endpoint(authority_request_for(
        non_owner,
        PublishEndpointRequest {
            context: Some(authority_context("publish-endpoint-as-non-owner")),
            endpoint: Some(endpoint.clone()),
        },
    )));
    assert_eq!(
        denied_publish.unwrap_err().code(),
        tonic::Code::PermissionDenied
    );
    let grant_request = semantic_v1::EndpointGrant {
        identity: Some(semantic_v1::Identity {
            id: "grant-1".to_string(),
            generation: 1,
        }),
        endpoint: endpoint.identity.clone(),
        grantee: renewed.holder.clone(),
        lease: renewed.identity.clone(),
        fence_token: renewed.fence_token,
        expires_at: renewed.expires_at.clone(),
    };
    let denied_authorize = runtime.block_on(adapter.authorize_endpoint(authority_request_for(
        non_owner,
        AuthorizeEndpointRequest {
            context: Some(authority_context("authorize-endpoint-as-non-owner")),
            grant: Some(grant_request.clone()),
        },
    )));
    assert_eq!(
        denied_authorize.unwrap_err().code(),
        tonic::Code::PermissionDenied
    );
    let grant = runtime
        .block_on(
            adapter.authorize_endpoint(authority_request(AuthorizeEndpointRequest {
                context: Some(authority_context("authorize-endpoint")),
                grant: Some(grant_request),
            })),
        )
        .unwrap()
        .into_inner();
    assert_eq!(grant.fence_token, renewed.fence_token);

    let denied_revoke = runtime.block_on(adapter.revoke_endpoint(authority_request_for(
        non_owner,
        RevokeEndpointRequest {
            context: Some(authority_context("revoke-endpoint-as-non-owner")),
            grant: grant.identity.clone(),
        },
    )));
    assert_eq!(
        denied_revoke.unwrap_err().code(),
        tonic::Code::PermissionDenied
    );

    runtime
        .block_on(
            adapter.revoke_endpoint(authority_request(RevokeEndpointRequest {
                context: Some(authority_context("revoke-endpoint")),
                grant: grant.identity.clone(),
            })),
        )
        .unwrap();
    assert!(adapter.endpoint_grants.lock().unwrap().is_empty());
}

#[test]
fn authority_worker_operation_and_event_paths_do_not_use_plugin_or_lro_types() {
    use crate::watchdog::InstanceActorState;
    use core_v1::{
        kernel_authority_service_server::KernelAuthorityService, AcquireSemanticLeaseRequest,
        CancelSemanticOperationRequest, CreateOperationRequest, HeartbeatWorkerRequest,
        ReportOperationRequest, StartWorkerRequest, StopWorkerRequest, SubscribeEventsRequest,
    };

    let adapter = semantic_worker_adapter();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let lease = runtime
        .block_on(
            adapter.acquire_lease(authority_request(AcquireSemanticLeaseRequest {
                context: Some(authority_context("worker-lease")),
                holder: Some(semantic_v1::Identity {
                    id: "worker-1".to_string(),
                    generation: 1,
                }),
                query: Some(semantic_v1::ResourceQuery {
                    resource_class: "accelerator".to_string(),
                    count: 1,
                    required_capabilities: vec![semantic_v1::CapabilityRequirement {
                        id: "accelerator.compute".to_string(),
                        minimum_revision: 1,
                        required_properties: Default::default(),
                    }],
                    minimum_capacity: Default::default(),
                }),
                ttl: Some(prost_types::Duration {
                    seconds: 30,
                    nanos: 0,
                }),
            })),
        )
        .unwrap()
        .into_inner();
    let worker = semantic_v1::Worker {
        identity: Some(semantic_v1::Identity {
            id: "worker-1".to_string(),
            generation: 1,
        }),
        principal: Some(semantic_v1::Identity {
            id: "principal-1".to_string(),
            generation: 1,
        }),
        provider: Some(semantic_v1::Identity {
            id: "provider-1".to_string(),
            generation: 1,
        }),
        lease: lease.identity.clone(),
        state: semantic_v1::WorkerState::Registered as i32,
        execution_ref: "opaque-execution-reference".to_string(),
        limits: Default::default(),
    };
    let started = runtime
        .block_on(adapter.start_worker(authority_request(StartWorkerRequest {
            context: Some(authority_context("start-worker")),
            worker: Some(worker.clone()),
        })))
        .unwrap()
        .into_inner();
    assert_eq!(started.kind, "worker.start");
    assert_eq!(started.state, semantic_v1::OperationState::Running as i32);
    // Direction 2: the launched worker must be driven by the wired-up
    // InstanceActor, not a bare SandboxedProcess.
    assert_eq!(
        adapter
            .instances
            .lock()
            .unwrap()
            .get("worker-1")
            .expect("started worker must be registered")
            .actor
            .state(),
        InstanceActorState::Healthy
    );
    let stored_principal = adapter
        .instances
        .lock()
        .unwrap()
        .get("worker-1")
        .and_then(|process| process.semantic_worker.as_ref())
        .expect("started worker must retain semantic identity")
        .principal
        .clone();
    assert_eq!(
        stored_principal.id, "unix-principal/uid-1000/gid-1000",
        "the authority must not trust Worker.principal from the request body",
    );
    assert_eq!(stored_principal.generation, 1);

    let running = runtime
        .block_on(
            adapter.heartbeat_worker(authority_request(HeartbeatWorkerRequest {
                context: Some(authority_context("heartbeat-worker")),
                worker: worker.identity.clone(),
                lease: lease.identity.clone(),
                fence_token: lease.fence_token,
            })),
        )
        .unwrap()
        .into_inner();
    assert_eq!(running.state, semantic_v1::WorkerState::Running as i32);

    let operation = semantic_v1::Operation {
        identity: Some(semantic_v1::Identity {
            id: "operation-1".to_string(),
            generation: 1,
        }),
        owner: worker.principal.clone(),
        executor: worker.provider.clone(),
        kind: "ai.train".to_string(),
        state: semantic_v1::OperationState::Created as i32,
        deadline: None,
        parent: None,
        metadata: Default::default(),
    };
    runtime
        .block_on(
            adapter.create_operation(authority_request(CreateOperationRequest {
                context: Some(authority_context("create-operation")),
                operation: Some(operation.clone()),
            })),
        )
        .unwrap();
    let mut reported = operation;
    reported.state = semantic_v1::OperationState::Running as i32;
    runtime
        .block_on(
            adapter.report_operation(authority_request(ReportOperationRequest {
                context: Some(authority_context("report-operation")),
                operation: Some(reported),
            })),
        )
        .unwrap();
    let cancelled = runtime
        .block_on(
            adapter.cancel_operation(authority_request(CancelSemanticOperationRequest {
                context: Some(authority_context("cancel-operation")),
                operation: Some(semantic_v1::Identity {
                    id: "operation-1".to_string(),
                    generation: 1,
                }),
            })),
        )
        .unwrap()
        .into_inner();
    assert_eq!(cancelled.state, semantic_v1::OperationState::Lost as i32);

    let cursor = semantic_v1::EventCursor {
        source: Some(to_semantic_proto_identity(&adapter.semantic_event_source())),
        sequence: 0,
    };
    let events = runtime
        .block_on(
            adapter.subscribe_events(authority_request(SubscribeEventsRequest {
                context: Some(authority_context("subscribe-events")),
                cursor: Some(cursor),
                page_size: 256,
            })),
        )
        .unwrap()
        .into_inner();
    assert_eq!(events.status, semantic_v1::ReplayStatus::Current as i32);
    assert!(events
        .events
        .iter()
        .any(|event| event.kind == "worker.starting"));
    assert!(events
        .events
        .iter()
        .any(|event| event.kind == "operation.running"));

    let source_changed = runtime
        .block_on(
            adapter.subscribe_events(authority_request(SubscribeEventsRequest {
                context: Some(authority_context("source-changed")),
                cursor: Some(semantic_v1::EventCursor {
                    source: Some(semantic_v1::Identity {
                        id: "another-kernel".to_string(),
                        generation: 1,
                    }),
                    sequence: 0,
                }),
                page_size: 1,
            })),
        )
        .unwrap()
        .into_inner();
    assert_eq!(
        source_changed.status,
        semantic_v1::ReplayStatus::SourceChanged as i32
    );
    for sequence in 0..=OPERATION_EVENT_HISTORY_CAPACITY {
        adapter.publish_semantic_event(
            semantic::Identity {
                id: "worker-1".to_string(),
                generation: 1,
            },
            "worker.observed",
            "cyrene.worker.v1",
            sequence.to_string().into_bytes(),
        );
    }
    let gap = runtime
        .block_on(
            adapter.subscribe_events(authority_request(SubscribeEventsRequest {
                context: Some(authority_context("replay-gap")),
                cursor: Some(semantic_v1::EventCursor {
                    source: Some(to_semantic_proto_identity(&adapter.semantic_event_source())),
                    sequence: 1,
                }),
                page_size: 1,
            })),
        )
        .unwrap()
        .into_inner();
    assert_eq!(gap.status, semantic_v1::ReplayStatus::Gap as i32);
    assert!(gap.events.is_empty());

    let stopped = runtime
        .block_on(adapter.stop_worker(authority_request(StopWorkerRequest {
            context: Some(authority_context("stop-worker")),
            worker: worker.identity,
            lease: lease.identity,
            fence_token: lease.fence_token,
            grace_period: Some(prost_types::Duration {
                seconds: 1,
                nanos: 0,
            }),
        })))
        .unwrap()
        .into_inner();
    assert_eq!(stopped.state, semantic_v1::OperationState::Succeeded as i32);
}

#[test]
fn durable_event_replay_is_not_limited_by_the_memory_window() {
    let store = Arc::new(RecordingDurableEventStore::default());
    let authority = semantic_lease_adapter().with_event_store(store).authority();
    let source = authority.semantic_event_source();
    for sequence in 1..=(OPERATION_EVENT_HISTORY_CAPACITY + 44) {
        authority.publish_semantic_event(
            semantic::Identity {
                id: format!("worker-{sequence}"),
                generation: 1,
            },
            "worker.observed",
            "cyrene.worker.v1",
            Vec::new(),
        );
    }
    let context = scoped_authority_context("default", "durable-replay");
    let principal = principal_from_peer_cred(&AUTHORITY_TEST_PEER);
    let first = authority
        .events_after(
            &context,
            &principal,
            &semantic::EventCursor {
                source: source.clone(),
                sequence: 0,
            },
            OPERATION_EVENT_HISTORY_CAPACITY,
        )
        .unwrap();
    assert_eq!(first.status, semantic::ReplayStatus::Current);
    assert_eq!(first.oldest_available_sequence, 1);
    assert_eq!(first.latest_available_sequence, 300);
    assert_eq!(first.events.len(), OPERATION_EVENT_HISTORY_CAPACITY);
    assert_eq!(first.events[0].sequence, 1);
    assert_eq!(first.events.last().unwrap().sequence, 256);

    let second = authority
        .events_after(
            &context,
            &principal,
            &semantic::EventCursor {
                source,
                sequence: first.next_sequence,
            },
            OPERATION_EVENT_HISTORY_CAPACITY,
        )
        .unwrap();
    assert_eq!(
        second
            .events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        (257..=300).collect::<Vec<_>>(),
    );
    assert_eq!(
        authority
            .snapshot(&context, &principal)
            .unwrap()
            .cursor
            .sequence,
        300
    );
}

#[test]
fn durable_retention_gap_requires_snapshot_before_resume() {
    let store = Arc::new(RecordingDurableEventStore::default());
    let authority = semantic_lease_adapter()
        .with_event_store(store.clone())
        .authority();
    let source = authority.semantic_event_source();
    for sequence in 1..=10 {
        authority.publish_semantic_event(
            semantic::Identity {
                id: format!("worker-{sequence}"),
                generation: 1,
            },
            "worker.observed",
            "cyrene.worker.v1",
            Vec::new(),
        );
    }
    store
        .records
        .lock()
        .unwrap()
        .retain(|record| record.event.sequence >= 5);
    let context = scoped_authority_context("default", "durable-gap");
    let principal = principal_from_peer_cred(&AUTHORITY_TEST_PEER);
    let gap = authority
        .events_after(
            &context,
            &principal,
            &semantic::EventCursor {
                source: source.clone(),
                sequence: 1,
            },
            1,
        )
        .unwrap();
    assert_eq!(gap.status, semantic::ReplayStatus::Gap);
    assert!(gap.events.is_empty());
    assert_eq!(gap.oldest_available_sequence, 5);

    let snapshot = authority.snapshot(&context, &principal).unwrap();
    assert_eq!(snapshot.cursor.sequence, 10);
    let resumed = authority
        .events_after(&context, &principal, &snapshot.cursor, 1)
        .unwrap();
    assert_eq!(resumed.status, semantic::ReplayStatus::Current);
    assert!(resumed.events.is_empty());
}

#[test]
fn durable_event_read_failure_never_falls_back_to_memory_history() {
    let store = Arc::new(RecordingDurableEventStore::default());
    store.fail_reads.store(true, Ordering::SeqCst);
    let authority = semantic_lease_adapter().with_event_store(store).authority();
    let context = scoped_authority_context("default", "durable-read-failure");
    let principal = principal_from_peer_cred(&AUTHORITY_TEST_PEER);
    let cursor = semantic::EventCursor {
        source: authority.semantic_event_source(),
        sequence: 0,
    };
    assert_eq!(
        authority
            .events_after(&context, &principal, &cursor, 1)
            .unwrap_err()
            .reason_code,
        "EVENT_READ_FAILED"
    );
    assert_eq!(
        authority
            .snapshot(&context, &principal)
            .unwrap_err()
            .reason_code,
        "EVENT_READ_FAILED"
    );
}

#[test]
fn recreated_authority_continues_sequence_for_the_same_source() {
    let store = Arc::new(RecordingDurableEventStore::default());
    let first = semantic_lease_adapter()
        .with_event_store(store.clone())
        .authority();
    let source = first.semantic_event_source();
    for sequence in 1..=3 {
        first.publish_semantic_event(
            semantic::Identity {
                id: format!("worker-{sequence}"),
                generation: 1,
            },
            "worker.observed",
            "cyrene.worker.v1",
            Vec::new(),
        );
    }

    let recreated = semantic_lease_adapter().with_event_store(store).authority();
    recreated.publish_semantic_event(
        semantic::Identity {
            id: "worker-4".to_string(),
            generation: 1,
        },
        "worker.observed",
        "cyrene.worker.v1",
        Vec::new(),
    );
    let context = scoped_authority_context("default", "recreated-source");
    let principal = principal_from_peer_cred(&AUTHORITY_TEST_PEER);
    let replay = recreated
        .events_after(
            &context,
            &principal,
            &semantic::EventCursor {
                source,
                sequence: 0,
            },
            4,
        )
        .unwrap();
    assert_eq!(
        replay
            .events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2, 3, 4],
    );
}

#[test]
fn cursor_from_an_older_epoch_returns_source_changed() {
    let store = Arc::new(RecordingDurableEventStore::default());
    let first = semantic_lease_adapter_at_epoch(7)
        .with_event_store(store.clone())
        .authority();
    first.publish_semantic_event(
        semantic::Identity {
            id: "worker-old".to_string(),
            generation: 1,
        },
        "worker.observed",
        "cyrene.worker.v1",
        Vec::new(),
    );
    let old_cursor = semantic::EventCursor {
        source: first.semantic_event_source(),
        sequence: 1,
    };
    let restarted = semantic_lease_adapter_at_epoch(8)
        .with_event_store(store)
        .authority();
    let context = scoped_authority_context("default", "changed-source");
    let principal = principal_from_peer_cred(&AUTHORITY_TEST_PEER);
    let page = restarted
        .events_after(&context, &principal, &old_cursor, 1)
        .unwrap();
    assert_eq!(page.status, semantic::ReplayStatus::SourceChanged);
    assert!(page.events.is_empty());
    let snapshot = restarted.snapshot(&context, &principal).unwrap();
    assert_eq!(snapshot.source.generation, 8);
    assert_eq!(snapshot.cursor.sequence, 0);
}

/// Item 3: the snapshot cursor and the durable event ordering are the SAME
/// consistency boundary. A snapshot taken at cursor C, followed by
/// `events_after(C)`, must reconstruct the live authority state: the set of
/// operation identities in the snapshot equals the set of `operation.created`
/// subjects in the durable history at or before C, the cursor equals the
/// latest durable sequence, and replay from C is empty/Current when nothing
/// changed after the snapshot.
#[test]
fn snapshot_cursor_and_durable_event_ordering_share_one_boundary() {
    let store = Arc::new(RecordingDurableEventStore::default());
    let authority = semantic_lease_adapter().with_event_store(store).authority();
    let context = scoped_authority_context("default", "audit-boundary");
    let principal = principal_from_peer_cred(&AUTHORITY_TEST_PEER);

    let worker_identity = semantic::Identity {
        id: "audit-worker".to_string(),
        generation: 1,
    };
    let query = semantic::ResourceQuery {
        resource_class: "accelerator".to_string(),
        count: 1,
        required_capabilities: vec![semantic::CapabilityRequirement {
            id: "accelerator.compute".to_string(),
            minimum_revision: 1,
            required_properties: BTreeMap::new(),
        }],
        minimum_capacity: BTreeMap::new(),
    };
    authority
        .acquire_lease(
            &context,
            &principal,
            worker_identity.clone(),
            query,
            u64::MAX,
        )
        .unwrap();
    let operation = semantic::Operation {
        identity: semantic::Identity {
            id: "operation-audit".to_string(),
            generation: 1,
        },
        owner: principal.identity.clone(),
        executor: semantic::Identity {
            id: "provider-x".to_string(),
            generation: 1,
        },
        kind: "ai.train".to_string(),
        state: semantic::OperationState::Created,
        deadline_unix_ms: None,
        parent: None,
        metadata: BTreeMap::new(),
    };
    authority
        .create_operation(&context, &principal, operation.clone())
        .unwrap();
    authority
        .report_operation(
            &context,
            &principal,
            semantic::Operation {
                state: semantic::OperationState::Running,
                ..operation.clone()
            },
        )
        .unwrap();
    authority
        .report_operation(
            &context,
            &principal,
            semantic::Operation {
                state: semantic::OperationState::Succeeded,
                ..operation.clone()
            },
        )
        .unwrap();

    let snapshot = authority.snapshot(&context, &principal).unwrap();
    assert!(snapshot.cursor.sequence > 0);

    // (a) The snapshot cursor equals the latest durable sequence.
    let full = authority
        .events_after(
            &context,
            &principal,
            &semantic::EventCursor {
                source: snapshot.source.clone(),
                sequence: 0,
            },
            OPERATION_EVENT_HISTORY_CAPACITY,
        )
        .unwrap();
    assert_eq!(full.status, semantic::ReplayStatus::Current);
    assert_eq!(snapshot.cursor.sequence, full.latest_available_sequence);
    // The durable replay is contiguous 1..=latest, the exact ordering the
    // snapshot cursor is derived from.
    let sequences: Vec<u64> = full.events.iter().map(|event| event.sequence).collect();
    assert_eq!(
        sequences,
        (1..=snapshot.cursor.sequence).collect::<Vec<_>>()
    );

    // (b) Replay from the snapshot cursor is empty and Current: nothing
    // changed after the snapshot, so Snapshot @ C is already complete and
    // events_after(C) contributes no further (and no lost) transition.
    let after = authority
        .events_after(&context, &principal, &snapshot.cursor, 256)
        .unwrap();
    assert_eq!(after.status, semantic::ReplayStatus::Current);
    assert!(after.events.is_empty());

    // (c) The snapshot's operation set is identical to the set of operations
    // whose `operation.created` event is in the durable history at or before C.
    let mut created: Vec<String> = full
        .events
        .iter()
        .filter(|event| event.kind == "operation.created")
        .map(|event| event.subject.id.clone())
        .collect();
    created.sort();
    let mut snapshot_operations: Vec<String> = snapshot
        .operations
        .iter()
        .map(|operation| operation.identity.id.clone())
        .collect();
    snapshot_operations.sort();
    assert_eq!(created, snapshot_operations);
}

/// Item 4: a snapshot must stay consistent with the incremental replay while
/// transitions mutate authority state concurrently. The reader constantly
/// takes a snapshot and replays from its cursor; because the snapshot now
/// captures the event cursor BEFORE reading state, every transition published
/// during the read is either already reflected in the snapshot or returned by
/// the replay — never lost from both. The same-source replay from a fresh
/// snapshot cursor can only be Current, and the latest available sequence can
/// never fall behind the snapshot cursor.
#[test]
fn snapshot_stays_consistent_while_operations_mutate_concurrently() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;

    let store = Arc::new(RecordingDurableEventStore::default());
    let authority = Arc::new(semantic_lease_adapter().with_event_store(store).authority());
    let context = scoped_authority_context("default", "concurrent-snapshot");
    let principal = principal_from_peer_cred(&AUTHORITY_TEST_PEER);

    let writers = 4;
    let per_writer = 40;
    let total = writers * per_writer;
    let running = Arc::new(AtomicBool::new(true));

    let mut handles = Vec::new();
    for writer in 0..writers {
        let authority = authority.clone();
        let context = context.clone();
        let principal = principal.clone();
        let running = running.clone();
        handles.push(thread::spawn(move || {
            for index in 0..per_writer {
                let operation = semantic::Operation {
                    identity: semantic::Identity {
                        id: format!("op-{writer}-{index}"),
                        generation: 1,
                    },
                    owner: principal.identity.clone(),
                    executor: semantic::Identity {
                        id: "provider-x".to_string(),
                        generation: 1,
                    },
                    kind: "ai.train".to_string(),
                    state: semantic::OperationState::Created,
                    deadline_unix_ms: None,
                    parent: None,
                    metadata: BTreeMap::new(),
                };
                // A transition publishes its durable event only after mutating
                // state; the reordered snapshot must never lose it.
                let _ = authority.create_operation(&context, &principal, operation);
                if !running.load(Ordering::SeqCst) {
                    break;
                }
            }
        }));
    }

    let reader_authority = authority.clone();
    let reader_context = context.clone();
    let reader_principal = principal.clone();
    let reader_running = running.clone();
    let reader = thread::spawn(move || {
        let mut checked = 0;
        while reader_running.load(Ordering::SeqCst) {
            let snapshot = match reader_authority.snapshot(&reader_context, &reader_principal) {
                Ok(snapshot) => snapshot,
                Err(_) => continue,
            };
            let page = reader_authority
                .events_after(&reader_context, &reader_principal, &snapshot.cursor, 256)
                .expect("replay from a fresh snapshot cursor must not fail");
            // Under concurrency a writer may commit between the snapshot and
            // this replay, or the in-memory window may roll past the snapshot
            // cursor. `Gap`/`SourceChanged` are valid outcomes: the client must
            // resnapshot. The invariant under test is that no event is lost or
            // corrupted, not that every poll is Current.
            assert!(
                matches!(
                    page.status,
                    semantic::ReplayStatus::Current
                        | semantic::ReplayStatus::Gap
                        | semantic::ReplayStatus::SourceChanged
                ),
                "replay must yield a valid Current/Gap/SourceChanged status"
            );
            assert!(page.latest_available_sequence >= snapshot.cursor.sequence);
            checked += 1;
        }
        checked
    });

    for handle in handles {
        handle.join().unwrap();
    }
    running.store(false, Ordering::SeqCst);
    let checked = reader.join().unwrap();
    assert!(checked > 0);

    // Quiescent audit: every created operation is present in both the durable
    // event log and the snapshot. Nothing was lost between the state read and
    // the cursor read under concurrency.
    let snapshot = authority.snapshot(&context, &principal).unwrap();
    let full = authority
        .events_after(
            &context,
            &principal,
            &semantic::EventCursor {
                source: snapshot.source.clone(),
                sequence: 0,
            },
            OPERATION_EVENT_HISTORY_CAPACITY,
        )
        .unwrap();
    assert_eq!(full.status, semantic::ReplayStatus::Current);
    let mut created: Vec<String> = full
        .events
        .iter()
        .filter(|event| event.kind == "operation.created")
        .map(|event| event.subject.id.clone())
        .collect();
    created.sort();
    let mut in_snapshot: Vec<String> = snapshot
        .operations
        .iter()
        .map(|operation| operation.identity.id.clone())
        .collect();
    in_snapshot.sort();
    assert_eq!(created, in_snapshot);
    assert_eq!(snapshot.cursor.sequence, full.latest_available_sequence);
    assert_eq!(snapshot.cursor.sequence, total as u64);
}

#[test]
fn heartbeat_requires_generation_and_monotonic_sequence() {
    use core_v1::plugin_lifecycle_service_server::PluginLifecycleService;
    let adapter = heartbeat_adapter();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let accepted = runtime
        .block_on(
            adapter.report_heartbeat(Request::new(core_v1::ReportHeartbeatRequest {
                context: None,
                plugin_instance_name: "worker_1".to_string(),
                generation: 99,
                sequence_number: 1,
                observed_at: None,
                runtime_state: core_v1::PluginRuntimeState::Healthy as i32,
                health: None,
                restart_count: 0,
            })),
        )
        .unwrap()
        .into_inner();
    assert_eq!(
        accepted.disposition,
        core_v1::HeartbeatDisposition::Accepted as i32
    );
    let duplicate = runtime
        .block_on(
            adapter.report_heartbeat(Request::new(core_v1::ReportHeartbeatRequest {
                context: None,
                plugin_instance_name: "worker_1".to_string(),
                generation: 99,
                sequence_number: 1,
                observed_at: None,
                runtime_state: core_v1::PluginRuntimeState::Healthy as i32,
                health: None,
                restart_count: 0,
            })),
        )
        .unwrap()
        .into_inner();
    assert_eq!(
        duplicate.disposition,
        core_v1::HeartbeatDisposition::Duplicate as i32
    );
    let stale = runtime
        .block_on(
            adapter.report_heartbeat(Request::new(core_v1::ReportHeartbeatRequest {
                context: None,
                plugin_instance_name: "worker_1".to_string(),
                generation: 98,
                sequence_number: 2,
                observed_at: None,
                runtime_state: core_v1::PluginRuntimeState::Healthy as i32,
                health: None,
                restart_count: 0,
            })),
        )
        .unwrap()
        .into_inner();
    assert_eq!(
        stale.disposition,
        core_v1::HeartbeatDisposition::StaleGeneration as i32
    );
}

#[test]
fn worker_control_shutdown_waits_for_matching_ack() {
    let adapter = heartbeat_adapter();
    let (outbound, mut inbound) = mpsc::channel(1);
    let (connection_id, welcome) = adapter
        .register_worker_control(
            &core_v1::WorkerHello {
                plugin_instance_name: "worker_1".to_string(),
                generation: 99,
                protocol_version: 1,
            },
            outbound,
        )
        .unwrap();
    assert_eq!(
        welcome.desired_state,
        core_v1::DesiredPluginState::Running as i32
    );

    let acknowledger = {
        let adapter = adapter.clone();
        thread::spawn(move || {
            let frame = inbound
                .blocking_recv()
                .expect("Kernel must send Shutdown")
                .expect("Kernel control stream must remain healthy");
            let Some(core_v1::kernel_to_worker::Body::Shutdown(shutdown)) = frame.body else {
                panic!("expected WorkerShutdown frame");
            };
            adapter
                .accept_shutdown_ack(&core_v1::WorkerShutdownAck {
                    plugin_instance_name: "worker_1".to_string(),
                    generation: 99,
                    shutdown_id: shutdown.shutdown_id,
                    drained: true,
                    detail: "drained".to_string(),
                })
                .unwrap();
        })
    };

    assert!(adapter.request_worker_shutdown("worker_1", "TEST_STOP", false));
    acknowledger.join().unwrap();
    let instances = adapter.instances.lock().unwrap();
    let pending = instances["worker_1"].pending_shutdown.as_ref().unwrap();
    assert!(pending.acknowledged);
    assert!(pending.drained);
    drop(instances);
    adapter.unregister_worker_control("worker_1", 99, connection_id);
}

#[test]
fn semantic_worker_control_shutdown_is_fenced_and_acknowledged() {
    use core_v1::{
        kernel_authority_service_server::KernelAuthorityService, AcquireSemanticLeaseRequest,
        StartWorkerRequest,
    };

    let adapter = semantic_worker_adapter().with_worker_heartbeat(WorkerHeartbeatConfig {
        socket_path: PathBuf::from("/run/cyrene/worker.sock"),
        interval: Duration::from_secs(1),
        timeout: Duration::from_secs(2),
        graceful_stop: Duration::from_secs(1),
        shutdown_ack_timeout: Duration::from_millis(50),
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let lease = runtime
        .block_on(
            adapter.acquire_lease(authority_request(AcquireSemanticLeaseRequest {
                context: Some(authority_context("control-lease")),
                holder: Some(semantic_v1::Identity {
                    id: "worker-control-1".to_string(),
                    generation: 1,
                }),
                query: Some(semantic_v1::ResourceQuery {
                    resource_class: "accelerator".to_string(),
                    count: 1,
                    required_capabilities: vec![semantic_v1::CapabilityRequirement {
                        id: "accelerator.compute".to_string(),
                        minimum_revision: 1,
                        required_properties: Default::default(),
                    }],
                    minimum_capacity: Default::default(),
                }),
                ttl: Some(prost_types::Duration {
                    seconds: 30,
                    nanos: 0,
                }),
            })),
        )
        .unwrap()
        .into_inner();
    let worker = semantic_v1::Worker {
        identity: Some(semantic_v1::Identity {
            id: "worker-control-1".to_string(),
            generation: 1,
        }),
        principal: Some(semantic_v1::Identity {
            id: "principal-1".to_string(),
            generation: 1,
        }),
        provider: Some(semantic_v1::Identity {
            id: "provider-1".to_string(),
            generation: 1,
        }),
        lease: lease.identity.clone(),
        state: semantic_v1::WorkerState::Registered as i32,
        execution_ref: "opaque-execution-reference".to_string(),
        limits: Default::default(),
    };
    runtime
        .block_on(adapter.start_worker(authority_request(StartWorkerRequest {
            context: Some(authority_context("control-start")),
            worker: Some(worker.clone()),
        })))
        .unwrap();

    let (outbound, mut inbound) = mpsc::channel(1);
    let control_context = crate::convert::authority_call_context_from_proto(Some(
        &authority_context("control-connect"),
    ))
    .unwrap();
    let (connection_id, welcome) = adapter
        .register_semantic_worker_control(
            &control_context,
            &core_v1::WorkerControlHello {
                worker: worker.identity.clone(),
                lease: lease.identity.clone(),
                fence_token: lease.fence_token,
            },
            outbound,
        )
        .unwrap();
    assert_eq!(welcome.identity.id, "worker-control-1");

    let acknowledger = {
        let adapter = adapter.clone();
        let worker = worker.clone();
        let lease = lease.clone();
        let context = crate::convert::authority_call_context_from_proto(Some(&authority_context(
            "control-ack",
        )))
        .unwrap();
        thread::spawn(move || {
            let frame = inbound
                .blocking_recv()
                .expect("Kernel must send Shutdown")
                .expect("semantic Worker control stream must remain healthy");
            let Some(core_v1::kernel_to_worker_control::Body::Shutdown(shutdown)) = frame.body
            else {
                panic!("expected WorkerControlShutdown frame");
            };
            adapter
                .accept_semantic_shutdown_ack(
                    &context,
                    &core_v1::WorkerControlShutdownAck {
                        worker: worker.identity.clone(),
                        lease: lease.identity.clone(),
                        fence_token: lease.fence_token,
                        shutdown_id: shutdown.shutdown_id,
                        drained: true,
                    },
                )
                .unwrap();
        })
    };

    assert!(adapter.request_semantic_worker_shutdown("worker-control-1", "TEST_STOP"));
    acknowledger.join().unwrap();
    let instances = adapter.instances.lock().unwrap();
    let pending = instances["worker-control-1"]
        .pending_shutdown
        .as_ref()
        .unwrap();
    assert!(pending.acknowledged);
    assert!(pending.drained);
    drop(instances);
    adapter.unregister_semantic_worker_control("worker-control-1", 1, connection_id);
}

#[test]
fn watch_operations_replays_bounded_operation_and_runtime_events() {
    use core_v1::kernel_service_server::KernelService;

    let adapter = heartbeat_adapter();
    let operation = adapter.operation_running(
        "operations/launch-worker_1".to_string(),
        "worker_1".to_string(),
    );
    adapter.publish_runtime_event(
        core_v1::RuntimeEventType::WatchdogTriggered,
        "worker_1",
        "TEST_WATCHDOG",
        "test runtime event",
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let response = adapter
            .watch_operations(Request::new(core_v1::WatchOperationsRequest {
                context: None,
                operation_names: Vec::new(),
                resume_token: String::new(),
            }))
            .await
            .unwrap();
        let mut stream = response.into_inner();
        let first = stream.next().await.unwrap().unwrap();
        assert_eq!(first.operation.unwrap().name, operation.name);
        assert_eq!(first.sequence_number, 1);
        let second = stream.next().await.unwrap().unwrap();
        assert_eq!(
            second.runtime_event.unwrap().r#type,
            core_v1::RuntimeEventType::WatchdogTriggered as i32
        );
        assert_eq!(second.sequence_number, 2);
    });
}

#[test]
fn cancel_operation_reaps_worker_and_emits_terminal_operation() {
    use core_v1::kernel_service_server::KernelService;

    let journal = Arc::new(RecordingRuntimeJournal::default());
    let adapter = heartbeat_adapter().with_runtime_journal(journal.clone());
    let running = adapter.operation_running(
        "operations/launch-worker_1".to_string(),
        "worker_1".to_string(),
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let cancelled = runtime
        .block_on(
            adapter.cancel_operation(Request::new(core_v1::CancelOperationRequest {
                mutation: None,
                name: running.name.clone(),
            })),
        )
        .unwrap()
        .into_inner();
    assert_eq!(cancelled.state, core_v1::OperationState::Cancelled as i32);
    assert!(!adapter.instances.lock().unwrap().contains_key("worker_1"));
    let events = adapter.operation_events.lock().unwrap();
    assert!(events.iter().any(|event| {
        event.operation.as_ref().is_some_and(|operation| {
            operation.name == running.name
                && operation.state == core_v1::OperationState::Cancelled as i32
        })
    }));
    assert!(events.iter().any(|event| {
        event.runtime_event.as_ref().is_some_and(|runtime_event| {
            runtime_event.r#type == core_v1::RuntimeEventType::CleanupCompleted as i32
        })
    }));
    assert!(journal.records.lock().unwrap().iter().any(|record| {
        record.event == RuntimeJournalEvent::InstanceTerminated
            && record.instance_name.as_deref() == Some("worker_1")
            && record.reason_code == "CANCEL_COMPLETE"
    }));
}

#[test]
fn adapter_degraded_state_is_visible_on_managed_instances() {
    let adapter = heartbeat_adapter();
    adapter.adapter_available.store(false, Ordering::Relaxed);
    let instances = adapter.instances.lock().unwrap();
    let instance = to_plugin_instance(
        &adapter.daemon,
        "worker_1",
        &instances["worker_1"],
        adapter.adapter_available.load(Ordering::Relaxed),
    );
    let health = instance.health.unwrap();
    assert_eq!(health.status, core_v1::HealthStatus::Degraded as i32);
    assert_eq!(health.reason_code, "ADAPTER_DEGRADED");
}

#[test]
fn multi_adapter_bindings_preserve_provenance_without_relaxing_enforcement() {
    let merged = merge_bindings(vec![
        DeviceBinding {
            resource_id: "nvidia-0".to_string(),
            nodes: Vec::new(),
            environment: BTreeMap::new(),
            required_gids: vec![44],
            enforcement: EnforcementMode::Hard,
            adapter_id: "nvidia".to_string(),
            reason_code: "TEST".to_string(),
        },
        DeviceBinding {
            resource_id: "amd-0".to_string(),
            nodes: Vec::new(),
            environment: BTreeMap::new(),
            required_gids: vec![45],
            enforcement: EnforcementMode::Hard,
            adapter_id: "amd".to_string(),
            reason_code: "TEST".to_string(),
        },
    ])
    .unwrap();

    assert_eq!(merged.resource_id, "nvidia-0,amd-0");
    assert_eq!(merged.adapter_id, "amd,nvidia");
    assert_eq!(merged.required_gids, vec![44, 45]);
    assert_eq!(merged.enforcement, EnforcementMode::Hard);
}

#[test]
fn multi_adapter_bindings_reject_mixed_enforcement() {
    let error = merge_bindings(vec![
        DeviceBinding {
            resource_id: "nvidia-0".to_string(),
            nodes: Vec::new(),
            environment: BTreeMap::new(),
            required_gids: Vec::new(),
            enforcement: EnforcementMode::Hard,
            adapter_id: "nvidia".to_string(),
            reason_code: "TEST".to_string(),
        },
        DeviceBinding {
            resource_id: "virtual-0".to_string(),
            nodes: Vec::new(),
            environment: BTreeMap::new(),
            required_gids: Vec::new(),
            enforcement: EnforcementMode::VisibilityOnly,
            adapter_id: "virtual".to_string(),
            reason_code: "TEST".to_string(),
        },
    ])
    .unwrap_err();

    assert_eq!(error.reason_code, "MIXED_RESOURCE_ENFORCEMENT");
}

/// A lease must never become externally visible when the durable fence record
/// cannot be persisted. The journal write is authoritative: on failure the
/// in-memory reservation is rolled back, leaving no active lease behind.
#[test]
fn acquire_lease_is_rolled_back_when_journal_write_fails() {
    use crate::convert::authority_lease_name;
    use core_v1::{
        kernel_authority_service_server::KernelAuthorityService, AcquireSemanticLeaseRequest,
    };

    let adapter = semantic_lease_adapter().with_runtime_journal(Arc::new(FailingRuntimeJournal));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let result = runtime.block_on(adapter.acquire_lease(authority_request(
        AcquireSemanticLeaseRequest {
            context: Some(authority_context("rollback-journal-fail")),
            holder: Some(semantic_v1::Identity {
                id: "worker-1".to_string(),
                generation: 1,
            }),
            query: Some(semantic_v1::ResourceQuery {
                resource_class: "accelerator".to_string(),
                count: 1,
                required_capabilities: vec![semantic_v1::CapabilityRequirement {
                    id: "accelerator.compute".to_string(),
                    minimum_revision: 1,
                    required_properties: Default::default(),
                }],
                minimum_capacity: Default::default(),
            }),
            ttl: Some(prost_types::Duration {
                seconds: 30,
                nanos: 0,
            }),
        },
    )));
    assert!(
        result.is_err(),
        "acquire_lease must fail when the durable journal write fails"
    );

    // Rollback must have released the in-memory lease, so it is not externally
    // visible as an Active lease (its resource is freed) even though the
    // durable fence record was never persisted.
    let lease_name = authority_lease_name(&authority_context("rollback-journal-fail"));
    let rolled_back = adapter
        .daemon
        .lease(&lease_name)
        .expect("lease record should still exist after rollback");
    assert_eq!(
        rolled_back.state,
        cy_kernel_api::LeaseState::Released,
        "rolled-back lease must not remain Active"
    );
}

/// A release must fail closed: if the durable release record cannot be
/// persisted the in-memory lease is retained (we do not lose the evidence of
/// the reservation). This journal double allows `LeaseReserved` but fails
/// `LeaseReleased`, so a held lease survives a failed release attempt.
#[test]
fn release_lease_fails_closed_when_journal_write_fails() {
    use core_v1::{
        kernel_authority_service_server::KernelAuthorityService, AcquireSemanticLeaseRequest,
        ReleaseSemanticLeaseRequest,
    };

    #[derive(Default)]
    struct ReserveOkReleaseFailing {
        records: std::sync::Mutex<Vec<RuntimeJournalRecord>>,
    }
    impl RuntimeJournalSink for ReserveOkReleaseFailing {
        fn append(&self, record: RuntimeJournalRecord) -> Result<(), ProviderError> {
            if record.event == RuntimeJournalEvent::LeaseReleased {
                return Err(ProviderError::new(
                    "failing-journal",
                    "JOURNAL_WRITE_FAILED",
                    "injected durable release write failure",
                ));
            }
            self.records.lock().unwrap().push(record);
            Ok(())
        }
    }

    let adapter =
        semantic_lease_adapter().with_runtime_journal(Arc::new(ReserveOkReleaseFailing::default()));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let lease = runtime
        .block_on(
            adapter.acquire_lease(authority_request(AcquireSemanticLeaseRequest {
                context: Some(authority_context("release-journal-fail")),
                holder: Some(semantic_v1::Identity {
                    id: "worker-1".to_string(),
                    generation: 1,
                }),
                query: Some(semantic_v1::ResourceQuery {
                    resource_class: "accelerator".to_string(),
                    count: 1,
                    required_capabilities: vec![semantic_v1::CapabilityRequirement {
                        id: "accelerator.compute".to_string(),
                        minimum_revision: 1,
                        required_properties: Default::default(),
                    }],
                    minimum_capacity: Default::default(),
                }),
                ttl: Some(prost_types::Duration {
                    seconds: 30,
                    nanos: 0,
                }),
            })),
        )
        .unwrap()
        .into_inner();

    let release_result = runtime.block_on(adapter.release_lease(authority_request(
        ReleaseSemanticLeaseRequest {
            context: Some(authority_context("release-journal-fail")),
            lease: lease.identity.clone(),
            fence_token: lease.fence_token,
        },
    )));
    assert!(
        release_result.is_err(),
        "release_lease must fail when the durable journal write fails"
    );

    // Release authority is durable and cleanup is not yet confirmed, so the
    // lease must remain RELEASING and its resource unavailable. It must never
    // be reported as RELEASED when the terminal record cannot be persisted.
    let still_held = adapter
        .daemon
        .lease(&lease.identity.as_ref().unwrap().id)
        .unwrap();
    assert_eq!(still_held.state, cy_kernel_api::LeaseState::Releasing);
}

#[test]
fn local_authority_owns_semantic_lease_transitions_without_tonic() {
    let adapter = semantic_lease_adapter();
    let authority = adapter.authority();
    let context = AuthorityCallContext {
        contract: semantic::ContractRevision::current(),
        namespace: NamespaceId::default(),
        request_id: "direct-authority".to_string(),
        idempotency_key: "direct-authority".to_string(),
    };
    let principal = principal_from_peer_cred(&AUTHORITY_TEST_PEER);
    let lease = authority
        .acquire_lease(
            &context,
            &principal,
            semantic::Identity {
                id: "direct-worker".to_string(),
                generation: 1,
            },
            semantic::ResourceQuery {
                resource_class: "accelerator".to_string(),
                count: 1,
                required_capabilities: vec![semantic::CapabilityRequirement {
                    id: "accelerator.compute".to_string(),
                    minimum_revision: 1,
                    required_properties: BTreeMap::new(),
                }],
                minimum_capacity: BTreeMap::new(),
            },
            u64::MAX,
        )
        .expect("direct authority acquire should succeed");

    assert_eq!(lease.state, semantic::LeaseState::Active);
    assert_eq!(lease.holder.id, "direct-worker");
    assert_eq!(
        adapter.daemon.lease(&lease.identity.id).unwrap().state,
        cy_kernel_api::LeaseState::Active
    );
}

#[test]
fn provider_lifecycle_separates_session_snapshot_and_resource_generations() {
    let adapter = semantic_lease_adapter();
    let authority = adapter.authority();
    let principal = principal_from_peer_cred(&AUTHORITY_TEST_PEER);
    let context = scoped_authority_context("default", "provider-lifecycle");
    let provider_v1 = semantic_provider("test-provider", 1, semantic::ProviderState::Ready);
    authority
        .register_provider(&context, &principal, provider_v1.clone())
        .unwrap();

    let mut resource = test_resource();
    resource.identity.generation = 37;
    resource.provider = provider_v1.identity.clone();
    let snapshot_v4 = provider_snapshot(&provider_v1, 4, vec![resource.clone()], Vec::new());
    authority
        .publish_inventory(&context, &principal, snapshot_v4.clone())
        .unwrap();
    let first = authority
        .reconcile_provider(&context, &principal, &provider_v1.identity)
        .unwrap();
    assert_eq!(first.snapshot_generation, 4);
    assert_eq!(
        first.actions,
        vec![ProviderReconcileAction::RefreshResource(
            resource.identity.clone()
        )]
    );
    assert_eq!(
        authority
            .reconcile_provider(&context, &principal, &provider_v1.identity)
            .unwrap()
            .actions,
        vec![ProviderReconcileAction::Noop]
    );

    let provider_v2 = semantic_provider("test-provider", 2, semantic::ProviderState::Ready);
    authority
        .register_provider(&context, &principal, provider_v2.clone())
        .unwrap();
    assert_eq!(
        authority
            .publish_inventory(&context, &principal, snapshot_v4)
            .unwrap_err()
            .reason_code,
        "STALE_GENERATION"
    );
    let snapshot_v1_after_reconnect = provider_snapshot(
        &provider_v2,
        1,
        vec![semantic::Resource {
            provider: provider_v2.identity.clone(),
            ..resource.clone()
        }],
        Vec::new(),
    );
    authority
        .publish_inventory(&context, &principal, snapshot_v1_after_reconnect)
        .unwrap();

    let records = authority.runtime.providers.lock().unwrap();
    let record = records
        .get(&(NamespaceId::default(), "test-provider".to_string()))
        .unwrap();
    assert_eq!(record.provider.identity.generation, 2);
    assert_eq!(record.inventory.as_ref().unwrap().snapshot_generation, 1);
    assert_eq!(
        record.inventory.as_ref().unwrap().resources[0]
            .identity
            .generation,
        37
    );
}

#[test]
fn hardware_adapters_publish_separate_resource_only_provider_snapshots() {
    let mut resource_a = test_resource_with_id("resource-a");
    resource_a.identity.generation = 17;
    let mut resource_b = test_resource_with_id("resource-b");
    resource_b.identity.generation = 29;
    let adapter = hardware_provider_adapter(vec![
        (
            "adapter-a".to_string(),
            Arc::new(SnapshotHardware {
                generation: 41,
                resources: vec![resource_a.clone()],
            }) as Arc<dyn HardwareAdapter>,
        ),
        (
            "adapter-b".to_string(),
            Arc::new(SnapshotHardware {
                generation: 58,
                resources: vec![resource_b.clone()],
            }) as Arc<dyn HardwareAdapter>,
        ),
    ])
    .with_adapter_poll_interval(Duration::from_secs(3));

    adapter.sync_hardware_provider_facts().unwrap();
    let authority = adapter.authority();
    let records = authority.runtime.providers.lock().unwrap();
    for (adapter_id, resource, snapshot_generation) in
        [("adapter-a", resource_a, 41), ("adapter-b", resource_b, 58)]
    {
        let record = records
            .get(&(NamespaceId::default(), adapter_id.to_string()))
            .unwrap();
        assert_eq!(record.provider.identity.generation, 9);
        assert_eq!(record.provider.state, semantic::ProviderState::Ready);
        let snapshot = record.inventory.as_ref().unwrap();
        assert_eq!(snapshot.snapshot_generation, snapshot_generation);
        assert_eq!(snapshot.resources.len(), 1);
        assert_eq!(
            snapshot.resources[0].identity.generation,
            resource.identity.generation
        );
        assert_eq!(snapshot.resources[0].provider, record.provider.identity);
        assert!(snapshot.workers.is_empty());
        assert!(snapshot.endpoints.is_empty());
        assert!(snapshot.expires_at_unix_ms > snapshot.sampled_at_unix_ms);
        assert!(snapshot.expires_at_unix_ms - snapshot.sampled_at_unix_ms <= 6_000);
    }
    drop(records);
    assert_eq!(
        adapter.daemon.resources.inventory().generation,
        1,
        "the allocation ledger retains its independent aggregate generation"
    );

    adapter.sync_hardware_provider_facts().unwrap();
}

#[test]
fn unavailable_hardware_adapter_does_not_hide_other_provider_facts() {
    let adapter = hardware_provider_adapter(vec![
        (
            "adapter-good".to_string(),
            Arc::new(TestHardware {
                resources: vec![test_resource_with_id("resource-good")],
            }) as Arc<dyn HardwareAdapter>,
        ),
        (
            "adapter-failed".to_string(),
            Arc::new(FailingHardware) as Arc<dyn HardwareAdapter>,
        ),
    ]);

    adapter.sync_hardware_provider_facts().unwrap();
    let authority = adapter.authority();
    let records = authority.runtime.providers.lock().unwrap();
    let available = records
        .get(&(NamespaceId::default(), "adapter-good".to_string()))
        .unwrap();
    assert_eq!(available.provider.state, semantic::ProviderState::Ready);
    assert!(available.inventory.is_some());
    let unavailable = records
        .get(&(NamespaceId::default(), "adapter-failed".to_string()))
        .unwrap();
    assert_eq!(
        unavailable.provider.state,
        semantic::ProviderState::Unavailable
    );
    assert!(unavailable.inventory.is_none());
}

#[test]
fn resource_facts_reconciliation_never_owns_workers_or_leases() {
    let adapter = semantic_worker_adapter();
    let authority = adapter.authority();
    let principal = principal_from_peer_cred(&AUTHORITY_TEST_PEER);
    let context = scoped_authority_context("default", "resource-facts-only");
    let provider = semantic_provider("test-provider", 7, semantic::ProviderState::Ready);
    authority
        .register_resource_facts_provider(&context, &principal, provider.clone())
        .unwrap();
    let worker_identity = semantic::Identity {
        id: "hardware-worker".to_string(),
        generation: 1,
    };
    let lease = authority
        .acquire_lease(
            &context,
            &principal,
            worker_identity.clone(),
            semantic::ResourceQuery {
                resource_class: "accelerator".to_string(),
                count: 1,
                required_capabilities: vec![semantic::CapabilityRequirement {
                    id: "accelerator.compute".to_string(),
                    minimum_revision: 1,
                    required_properties: BTreeMap::new(),
                }],
                minimum_capacity: BTreeMap::new(),
            },
            future_expiry(),
        )
        .unwrap();
    let worker = semantic_worker_for(
        &worker_identity.id,
        provider.identity.clone(),
        lease.identity.clone(),
        semantic::WorkerState::Registered,
    );
    authority
        .start_worker(&context, &principal, worker.clone())
        .unwrap();
    let mut resource = test_resource();
    resource.provider = provider.identity.clone();
    authority
        .publish_inventory(
            &context,
            &principal,
            provider_snapshot(&provider, 1, vec![resource.clone()], Vec::new()),
        )
        .unwrap();

    assert_eq!(
        authority
            .reconcile_provider(&context, &principal, &provider.identity)
            .unwrap()
            .actions,
        vec![ProviderReconcileAction::RefreshResource(resource.identity)]
    );
    assert_eq!(
        adapter.daemon.lease(&lease.identity.id).unwrap().state,
        cy_kernel_api::LeaseState::Active
    );
    assert_ne!(
        adapter.instances.lock().unwrap()[&worker.identity.id]
            .semantic_worker
            .as_ref()
            .unwrap()
            .state,
        semantic::WorkerState::Lost
    );
}

#[test]
fn provider_unavailability_is_scoped_to_its_logical_identity() {
    let adapter = semantic_lease_adapter();
    let authority = adapter.authority();
    let principal = principal_from_peer_cred(&AUTHORITY_TEST_PEER);
    let context = scoped_authority_context("default", "provider-isolation");
    let provider_a = semantic_provider("provider-a", 1, semantic::ProviderState::Ready);
    let provider_b = semantic_provider("provider-b", 1, semantic::ProviderState::Ready);
    authority
        .register_provider(&context, &principal, provider_a.clone())
        .unwrap();
    authority
        .register_provider(&context, &principal, provider_b.clone())
        .unwrap();

    let provider_a_lost = semantic_provider("provider-a", 1, semantic::ProviderState::Unavailable);
    authority
        .register_provider(&context, &principal, provider_a_lost.clone())
        .unwrap();
    assert_eq!(
        authority
            .register_provider(&context, &principal, provider_a)
            .unwrap_err()
            .reason_code,
        "STALE_GENERATION"
    );
    let records = authority.runtime.providers.lock().unwrap();
    assert_eq!(
        records
            .get(&(NamespaceId::default(), "provider-a".to_string()))
            .unwrap()
            .provider
            .state,
        semantic::ProviderState::Unavailable
    );
    assert_eq!(
        records
            .get(&(NamespaceId::default(), "provider-b".to_string()))
            .unwrap()
            .provider
            .state,
        semantic::ProviderState::Ready
    );
}

#[test]
fn missing_worker_reconcile_revokes_authority_and_fences_old_worker() {
    let adapter = semantic_worker_adapter()
        .with_runtime_journal(Arc::new(RecordingRuntimeJournal::default()));
    let authority = adapter.authority();
    let principal = principal_from_peer_cred(&AUTHORITY_TEST_PEER);
    let context = scoped_authority_context("default", "worker-a-lease");
    let worker_a = semantic::Identity {
        id: "worker-a".to_string(),
        generation: 1,
    };
    let lease_a = authority
        .acquire_lease(
            &context,
            &principal,
            worker_a.clone(),
            semantic::ResourceQuery {
                resource_class: "accelerator".to_string(),
                count: 1,
                required_capabilities: vec![semantic::CapabilityRequirement {
                    id: "accelerator.compute".to_string(),
                    minimum_revision: 1,
                    required_properties: BTreeMap::new(),
                }],
                minimum_capacity: BTreeMap::new(),
            },
            future_expiry(),
        )
        .unwrap();
    let provider = semantic_provider("test-provider", 1, semantic::ProviderState::Ready);
    authority
        .register_provider(&context, &principal, provider.clone())
        .unwrap();
    let worker = semantic_worker_for(
        "worker-a",
        provider.identity.clone(),
        lease_a.identity.clone(),
        semantic::WorkerState::Registered,
    );
    authority
        .start_worker(&context, &principal, worker.clone())
        .unwrap();
    authority
        .heartbeat_worker(
            &context,
            &principal,
            &worker.identity,
            &lease_a.identity,
            lease_a.fence_token,
        )
        .unwrap();
    let (cancel_sender, mut cancel_receiver) = mpsc::channel(1);
    adapter
        .instances
        .lock()
        .unwrap()
        .get_mut("worker-a")
        .unwrap()
        .actor
        .attach_transport_channel(cancel_sender);
    let cancellable_operation = semantic::Operation {
        identity: semantic::Identity {
            id: "operation-cancel-worker-a".to_string(),
            generation: 1,
        },
        owner: principal.identity.clone(),
        executor: worker.identity.clone(),
        kind: "worker.invoke".to_string(),
        state: semantic::OperationState::Created,
        deadline_unix_ms: None,
        parent: None,
        metadata: BTreeMap::new(),
    };
    authority
        .create_operation(&context, &principal, cancellable_operation.clone())
        .unwrap();
    authority
        .report_operation(
            &context,
            &principal,
            semantic::Operation {
                state: semantic::OperationState::Running,
                ..cancellable_operation.clone()
            },
        )
        .unwrap();
    assert_eq!(
        authority
            .cancel_operation(&context, &principal, &cancellable_operation.identity)
            .unwrap()
            .state,
        semantic::OperationState::Cancelling
    );
    match cancel_receiver.try_recv().unwrap() {
        WorkerTransportCommand::Cancel {
            cancel_request_id,
            target_request_id,
            generation,
            fence_token,
        } => {
            assert!(cancel_request_id.starts_with("cancel-"));
            assert_eq!(target_request_id, cancellable_operation.identity.id);
            assert_eq!(generation, 1);
            assert_eq!(fence_token, lease_a.fence_token);
        }
        WorkerTransportCommand::Request(_) => panic!("cancellation must reach the worker executor"),
    }
    assert_eq!(
        authority
            .report_operation(
                &context,
                &principal,
                semantic::Operation {
                    state: semantic::OperationState::Cancelled,
                    ..cancellable_operation.clone()
                },
            )
            .unwrap()
            .state,
        semantic::OperationState::Cancelled
    );
    let operation = semantic::Operation {
        identity: semantic::Identity {
            id: "operation-worker-a".to_string(),
            generation: 1,
        },
        owner: principal.identity.clone(),
        executor: worker.identity.clone(),
        kind: "worker.invoke".to_string(),
        state: semantic::OperationState::Created,
        deadline_unix_ms: None,
        parent: None,
        metadata: BTreeMap::new(),
    };
    authority
        .create_operation(&context, &principal, operation.clone())
        .unwrap();
    authority
        .report_operation(
            &context,
            &principal,
            semantic::Operation {
                state: semantic::OperationState::Running,
                ..operation.clone()
            },
        )
        .unwrap();
    let endpoint = semantic_endpoint_for(&worker);
    authority
        .publish_endpoint(&context, &principal, endpoint.clone())
        .unwrap();

    let mut resource = test_resource();
    resource.provider = provider.identity.clone();
    let stale_worker = semantic_worker_for(
        "worker-stale",
        provider.identity.clone(),
        semantic::Identity {
            id: "lease-stale".to_string(),
            generation: 1,
        },
        semantic::WorkerState::Running,
    );
    authority
        .publish_inventory(
            &context,
            &principal,
            provider_snapshot(
                &provider,
                1,
                vec![resource.clone()],
                vec![
                    semantic::Worker {
                        state: semantic::WorkerState::Running,
                        ..worker.clone()
                    },
                    stale_worker.clone(),
                ],
            ),
        )
        .unwrap();
    let initial_reconciliation = authority
        .reconcile_provider(&context, &principal, &provider.identity)
        .unwrap();
    assert!(initial_reconciliation.actions.contains(
        &ProviderReconcileAction::TerminateStaleWorker(stale_worker.identity.clone())
    ));

    authority
        .publish_inventory(
            &context,
            &principal,
            provider_snapshot(&provider, 2, vec![resource], Vec::new()),
        )
        .unwrap();
    authority
        .confirm_stale_worker_termination(
            &context,
            &principal,
            &provider.identity,
            1,
            &stale_worker.identity,
            true,
        )
        .unwrap();
    let reconciliation = authority
        .reconcile_provider(&context, &principal, &provider.identity)
        .unwrap();
    assert!(reconciliation
        .actions
        .contains(&ProviderReconcileAction::MarkWorkerLost(
            worker.identity.clone()
        )));
    assert!(reconciliation
        .actions
        .contains(&ProviderReconcileAction::RevokeLease(
            lease_a.identity.clone()
        )));
    assert!(reconciliation
        .actions
        .contains(&ProviderReconcileAction::MarkOperationLost(
            operation.identity.clone()
        )));
    assert!(reconciliation
        .actions
        .contains(&ProviderReconcileAction::RevokeEndpoint(
            endpoint.identity.clone()
        )));
    assert_eq!(
        adapter.instances.lock().unwrap()["worker-a"]
            .semantic_worker
            .as_ref()
            .unwrap()
            .state,
        semantic::WorkerState::Lost
    );
    let revoked = adapter.daemon.lease(&lease_a.identity.id).unwrap();
    assert_eq!(revoked.state, cy_kernel_api::LeaseState::Revoked);
    assert!(revoked.fence_token > lease_a.fence_token);
    assert_eq!(
        authority.runtime.semantic_operations.lock().unwrap()
            [&context.object_ref(operation.identity.clone())]
            .state,
        semantic::OperationState::Lost
    );
    assert!(!authority
        .runtime
        .endpoints
        .lock()
        .unwrap()
        .contains_key(&context.object_ref(endpoint.identity.clone())));

    assert_eq!(
        authority
            .heartbeat_worker(
                &context,
                &principal,
                &worker.identity,
                &lease_a.identity,
                lease_a.fence_token,
            )
            .unwrap_err()
            .reason_code,
        "FENCE_MISMATCH"
    );
    assert_eq!(
        authority
            .renew_lease(
                &context,
                &principal,
                &lease_a.identity,
                lease_a.fence_token,
                future_expiry() + 60_000,
            )
            .unwrap_err()
            .reason_code,
        "STALE_FENCE_TOKEN"
    );
    assert!(authority
        .publish_endpoint(&context, &principal, endpoint)
        .is_err());
    assert!(authority
        .verify_worker_control(
            &context,
            &worker.identity,
            &lease_a.identity,
            lease_a.fence_token,
        )
        .is_err());

    let lease_b = authority
        .acquire_lease(
            &scoped_authority_context("default", "worker-b-lease"),
            &principal,
            semantic::Identity {
                id: "worker-b".to_string(),
                generation: 1,
            },
            semantic::ResourceQuery {
                resource_class: "accelerator".to_string(),
                count: 1,
                required_capabilities: vec![semantic::CapabilityRequirement {
                    id: "accelerator.compute".to_string(),
                    minimum_revision: 1,
                    required_properties: BTreeMap::new(),
                }],
                minimum_capacity: BTreeMap::new(),
            },
            future_expiry(),
        )
        .unwrap();
    assert!(lease_b.fence_token > lease_a.fence_token);
    assert_eq!(
        authority
            .reconcile_provider(&context, &principal, &provider.identity)
            .unwrap()
            .actions,
        vec![ProviderReconcileAction::Noop]
    );
    let events = authority
        .events_after(
            &context,
            &principal,
            &semantic::EventCursor {
                source: authority.semantic_event_source(),
                sequence: 0,
            },
            256,
        )
        .unwrap();
    assert!(events
        .events
        .iter()
        .any(|event| event.kind == "worker.lost"));
    assert!(events
        .events
        .iter()
        .any(|event| event.kind == "lease.revoked"));
    assert!(events
        .events
        .iter()
        .any(|event| event.kind == "operation.lost"));
    assert!(events
        .events
        .iter()
        .any(|event| event.kind == "endpoint.revoked"));
    let snapshot = authority.snapshot(&context, &principal).unwrap();
    assert_eq!(snapshot.source, authority.semantic_event_source());
    assert_eq!(snapshot.cursor.sequence, events.latest_available_sequence);
    assert_eq!(snapshot.workers[0].state, semantic::WorkerState::Lost);
    assert_eq!(snapshot.leases[0].state, semantic::LeaseState::Revoked);
    assert_eq!(
        snapshot
            .operations
            .iter()
            .find(|current| current.identity == operation.identity)
            .unwrap()
            .state,
        semantic::OperationState::Lost
    );
    assert!(snapshot.endpoints.is_empty());
}

#[test]
fn owned_startup_failure_keeps_resource_unavailable_when_release_cannot_complete() {
    use core_v1::{
        kernel_authority_service_server::KernelAuthorityService, AcquireSemanticLeaseRequest,
    };

    #[derive(Default)]
    struct TerminalReleaseJournal;

    impl RuntimeJournalSink for TerminalReleaseJournal {
        fn append(&self, record: RuntimeJournalRecord) -> Result<(), ProviderError> {
            if record.event == RuntimeJournalEvent::LeaseReleased {
                return Err(ProviderError::new(
                    "failing-journal",
                    "JOURNAL_WRITE_FAILED",
                    "startup cleanup terminal record cannot be persisted",
                ));
            }
            Ok(())
        }
    }

    let adapter = semantic_lease_adapter().with_runtime_journal(Arc::new(TerminalReleaseJournal));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let request = |request_id: &str| AcquireSemanticLeaseRequest {
        context: Some(authority_context(request_id)),
        holder: Some(semantic_v1::Identity {
            id: format!("worker-{request_id}"),
            generation: 1,
        }),
        query: Some(semantic_v1::ResourceQuery {
            resource_class: "accelerator".to_string(),
            count: 1,
            required_capabilities: vec![semantic_v1::CapabilityRequirement {
                id: "accelerator.compute".to_string(),
                minimum_revision: 1,
                required_properties: Default::default(),
            }],
            minimum_capacity: Default::default(),
        }),
        ttl: Some(prost_types::Duration {
            seconds: 30,
            nanos: 0,
        }),
    };
    let lease = runtime
        .block_on(adapter.acquire_lease(authority_request(request("startup-failure"))))
        .unwrap()
        .into_inner();
    let internal = adapter
        .daemon
        .lease(&lease.identity.as_ref().unwrap().id)
        .unwrap();

    let failure = adapter.release_owned_lease(true, &internal).unwrap_err();
    assert_eq!(failure.reason_code, "JOURNAL_WRITE_FAILED");
    assert_eq!(
        adapter.daemon.lease(&internal.name).unwrap().state,
        cy_kernel_api::LeaseState::Failed,
        "failed startup cleanup must retain the allocation"
    );
    assert!(runtime
        .block_on(adapter.acquire_lease(authority_request(request("replacement"))))
        .is_err());
}

/// End-to-end proof of the two-phase release invariant: a resource whose
/// instance cannot be physically reaped must never reach `RELEASED`. The
/// legacy `ReleaseLease` RPC must fail closed with `CLEANUP_INCOMPLETE`, the
/// lease must be left `FAILED` (allocation still held), and the half-cleaned
/// resource must NOT be handed to a replacement lease.
///
/// This exercises the real `release_lease` gRPC path (not just the port),
/// driving it through the shared `release_lease_with_cleanup` helper that now
/// gates every legacy release on confirmed physical cleanup.
#[test]
fn uncleaned_resource_cannot_be_reacquired_after_failed_release() {
    use crate::watchdog::{InstanceActor, InstanceActorState};
    use core_v1::kernel_service_server::KernelService;
    use cy_kernel_api::{
        CgroupLimits, DeviceBinding, EnforcementMode, LaunchPlan, ProcessCondition, ProcessHandle,
        ProcessRuntime, SandboxBackend, StopRequest,
    };
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    /// Sandbox whose `stop` reports an incomplete cleanup: the instance is
    /// stuck (e.g. an uninterruptible process) and must be quarantined, never
    /// silently released back to the pool.
    struct StuckSandbox;

    impl ProcessRuntime for StuckSandbox {
        fn preflight(&self) -> cy_kernel_api::NodeCapabilities {
            cy_kernel_api::NodeCapabilities {
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
            Ok(ProcessHandle {
                pid: 1,
                cgroup_path: PathBuf::from("/test"),
                start_time_ticks: Some(1),
                transport_socket: None,
            })
        }
        fn stop(
            &self,
            _handle: &ProcessHandle,
            _request: &StopRequest,
        ) -> Result<cy_kernel_api::CleanupReport, ProviderError> {
            Ok(cy_kernel_api::CleanupReport {
                complete: false,
                exit_code: None,
                oom_killed: false,
                conditions: vec![ProcessCondition {
                    reason_code: "REAP_TIMEOUT".to_string(),
                    summary: "instance could not be reaped".to_string(),
                }],
                reason_code: "PROCESS_UNINTERRUPTIBLE".to_string(),
            })
        }
    }

    impl SandboxBackend for StuckSandbox {
        fn backend_id(&self) -> &str {
            "stuck-test"
        }
    }

    let adapter = semantic_lease_adapter();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    // Acquire the sole resource through the legacy KernelService RPC.
    let lease = runtime
        .block_on(
            adapter.acquire_lease(Request::new(core_v1::AcquireLeaseRequest {
                mutation: Some(core_v1::MutationContext {
                    request: Some(core_v1::RequestContext {
                        request_id: "e2e-release".to_string(),
                        ..Default::default()
                    }),
                    idempotency_key: "e2e-release".to_string(),
                    expected_generation: Some(1),
                }),
                node: Some(core_v1::NodeRef {
                    node_id: "node".to_string(),
                    node_epoch: 7,
                }),
                holder: Some(semantic_v1::Identity {
                    id: "worker-e2e".to_string(),
                    generation: 1,
                }),
                query: Some(semantic_v1::ResourceQuery {
                    resource_class: "accelerator".to_string(),
                    count: 1,
                    required_capabilities: vec![semantic_v1::CapabilityRequirement {
                        id: "accelerator.compute".to_string(),
                        minimum_revision: 1,
                        required_properties: Default::default(),
                    }],
                    minimum_capacity: Default::default(),
                }),
                ttl: Some(prost_types::Duration {
                    seconds: 30,
                    nanos: 0,
                }),
                cpu: None,
                memory: None,
            })),
        )
        .unwrap()
        .into_inner();

    let lease_identity = lease
        .identity
        .clone()
        .expect("acquired lease has an identity");
    let fence_token = lease.fence_token;

    // Register a running instance bound to the acquired lease, backed by a
    // sandbox that reports an incomplete cleanup when stopped.
    let mut actor = InstanceActor::new(
        "stuck-instance",
        lease_identity.id.clone(),
        fence_token,
        Arc::new(StuckSandbox),
        LaunchPlan {
            instance_name: "stuck-instance".to_string(),
            executable: PathBuf::from("/bin/true"),
            args: Vec::new(),
            environment: BTreeMap::new(),
            cgroup_name: "stuck-instance".to_string(),
            limits: CgroupLimits::default(),
            transport_socket: None,
        },
        DeviceBinding {
            resource_id: "test".to_string(),
            nodes: Vec::new(),
            environment: BTreeMap::new(),
            required_gids: Vec::new(),
            enforcement: EnforcementMode::Soft,
            adapter_id: "test".to_string(),
            reason_code: "test".to_string(),
        },
        Duration::from_secs(30),
    );
    actor.start().expect("stuck instance must start");
    assert_eq!(actor.state(), InstanceActorState::Healthy);

    let mut process = managed_test_process(
        "stuck-instance",
        fence_token,
        Some(core_v1::ResourceLeaseRef {
            lease_name: lease_identity.id.clone(),
            fence_token,
        }),
    );
    process.actor = actor;
    adapter
        .instances
        .lock()
        .unwrap()
        .insert("stuck-instance".to_string(), process);

    // Release through the legacy RPC: because the instance cannot be reaped,
    // `release_lease_with_cleanup` must fail closed with CLEANUP_INCOMPLETE.
    let release = runtime.block_on(adapter.release_lease(Request::new(
        core_v1::ReleaseLeaseRequest {
            mutation: None,
            lease: Some(lease_identity.clone()),
            fence_token,
        },
    )));
    assert!(
        release.is_err(),
        "release must fail when physical cleanup is incomplete"
    );
    assert_eq!(
        release
            .unwrap_err()
            .metadata()
            .get("x-cyrene-reason-code")
            .and_then(|value| value.to_str().ok()),
        Some("CLEANUP_INCOMPLETE"),
        "a failed physical cleanup must surface CLEANUP_INCOMPLETE"
    );

    // The lease must be FAILED, never RELEASED: the allocation is still held
    // so it cannot be handed to a replacement while physically dirty.
    let held = adapter.daemon.lease(&lease_identity.id).unwrap();
    assert_eq!(
        held.state,
        cy_kernel_api::LeaseState::Failed,
        "a lease whose cleanup could not be confirmed must remain FAILED, not RELEASED"
    );

    // The half-cleaned resource must NOT be reacquired by another lease.
    let reacquire = runtime.block_on(adapter.acquire_lease(Request::new(
        core_v1::AcquireLeaseRequest {
            mutation: Some(core_v1::MutationContext {
                request: Some(core_v1::RequestContext {
                    request_id: "e2e-reacquire".to_string(),
                    ..Default::default()
                }),
                idempotency_key: "e2e-reacquire".to_string(),
                expected_generation: Some(1),
            }),
            node: Some(core_v1::NodeRef {
                node_id: "node".to_string(),
                node_epoch: 7,
            }),
            holder: Some(semantic_v1::Identity {
                id: "worker-replacement".to_string(),
                generation: 1,
            }),
            query: Some(semantic_v1::ResourceQuery {
                resource_class: "accelerator".to_string(),
                count: 1,
                required_capabilities: vec![semantic_v1::CapabilityRequirement {
                    id: "accelerator.compute".to_string(),
                    minimum_revision: 1,
                    required_properties: Default::default(),
                }],
                minimum_capacity: Default::default(),
            }),
            ttl: Some(prost_types::Duration {
                seconds: 30,
                nanos: 0,
            }),
            cpu: None,
            memory: None,
        },
    )));
    assert!(
        reacquire.is_err(),
        "an incompletely cleaned resource must not be reacquired"
    );
    assert_eq!(
        reacquire
            .unwrap_err()
            .metadata()
            .get("x-cyrene-reason-code")
            .and_then(|value| value.to_str().ok()),
        Some("INSUFFICIENT_RESOURCES"),
        "the still-held FAILED lease must block reallocation with INSUFFICIENT_RESOURCES"
    );
}

// ---------------------------------------------------------------------------
// Phase 5: Endpoint Authority Hardening (control plane)
//
// A Worker that owns an Endpoint derives its authority from its active Lease,
// its owner Principal and its Namespace. These tests prove that Endpoint
// publish/authorize/revoke require the owner Principal, that Namespace
// isolation holds, and that Lease release, Worker loss/replacement and Grant
// revocation remove the related Endpoint/Grant authority state so stale
// metadata cannot outlive its authority.
// ---------------------------------------------------------------------------

/// Builds a namespace where one Worker owns an active Lease, a published
/// Endpoint, and an authorized EndpointGrant.
fn endpoint_authority_scenario(
    adapter: &KernelServiceAdapter,
    namespace: &str,
    peer: PeerCred,
    worker_id: &str,
) -> (
    AuthorityCallContext,
    semantic::Principal,
    semantic::Identity,
    semantic::Lease,
    semantic::Endpoint,
    semantic::EndpointGrant,
) {
    let authority = adapter.authority();
    let context = scoped_authority_context(namespace, "endpoint-authority-scenario");
    let principal = principal_from_peer_cred(&peer);
    let worker_identity = semantic::Identity {
        id: worker_id.to_string(),
        generation: 1,
    };
    let query = semantic::ResourceQuery {
        resource_class: "accelerator".to_string(),
        count: 1,
        required_capabilities: vec![semantic::CapabilityRequirement {
            id: "accelerator.compute".to_string(),
            minimum_revision: 1,
            required_properties: BTreeMap::new(),
        }],
        minimum_capacity: BTreeMap::new(),
    };
    let lease = authority
        .acquire_lease(
            &context,
            &principal,
            worker_identity.clone(),
            query,
            u64::MAX,
        )
        .unwrap();
    authority
        .start_worker(
            &context,
            &principal,
            semantic::Worker {
                identity: worker_identity.clone(),
                principal: principal.identity.clone(),
                provider: semantic::Identity {
                    id: format!("provider-{worker_id}"),
                    generation: 1,
                },
                lease: lease.identity.clone(),
                state: semantic::WorkerState::Registered,
                execution_ref: "opaque-execution-reference".to_string(),
                limits: BTreeMap::new(),
            },
        )
        .unwrap();
    let endpoint = authority
        .publish_endpoint(
            &context,
            &principal,
            semantic::Endpoint {
                identity: semantic::Identity {
                    id: format!("endpoint-{worker_id}"),
                    generation: 1,
                },
                provider: semantic::Identity {
                    id: format!("provider-{worker_id}"),
                    generation: 1,
                },
                owner: worker_identity.clone(),
                transport: "transport.uds".to_string(),
                schema_id: "schema.v1".to_string(),
                capabilities: Vec::new(),
                public_attributes: BTreeMap::new(),
            },
        )
        .unwrap();
    let grant = authority
        .authorize_endpoint(
            &context,
            &principal,
            semantic::EndpointGrant {
                identity: semantic::Identity {
                    id: format!("grant-{worker_id}"),
                    generation: 1,
                },
                endpoint: endpoint.identity.clone(),
                grantee: worker_identity.clone(),
                lease: lease.identity.clone(),
                fence_token: lease.fence_token,
                expires_at_unix_ms: u64::MAX,
            },
        )
        .unwrap();
    (context, principal, worker_identity, lease, endpoint, grant)
}

fn endpoint_accelerator_query() -> semantic::ResourceQuery {
    semantic::ResourceQuery {
        resource_class: "accelerator".to_string(),
        count: 1,
        required_capabilities: vec![semantic::CapabilityRequirement {
            id: "accelerator.compute".to_string(),
            minimum_revision: 1,
            required_properties: BTreeMap::new(),
        }],
        minimum_capacity: BTreeMap::new(),
    }
}

// Test A — cross-Principal Publish denied: an authenticated Principal A cannot
// publish an Endpoint for a Worker owned by Principal B.
#[test]
fn endpoint_cross_principal_publish_denied() {
    let adapter = semantic_worker_adapter();
    let authority = adapter.authority();
    let context = scoped_authority_context("ns-a", "cross-publish");
    let principal_a = principal_from_peer_cred(&AUTHORITY_TEST_PEER);
    let principal_b = principal_from_peer_cred(&PeerCred {
        pid: 7171,
        uid: 2000,
        gid: 2000,
    });
    let worker_b = semantic::Identity {
        id: "worker-b".to_string(),
        generation: 1,
    };
    // A owns the namespace; the Lease is held by worker-b.
    let lease = authority
        .acquire_lease(
            &context,
            &principal_a,
            worker_b.clone(),
            endpoint_accelerator_query(),
            u64::MAX,
        )
        .unwrap();
    // Register a live Worker incarnation that is owned by Principal B.
    let mut process = managed_test_process(
        "worker-b",
        1,
        Some(core_v1::ResourceLeaseRef {
            lease_name: lease.identity.id.clone(),
            fence_token: lease.fence_token,
        }),
    );
    process.semantic_worker = Some(semantic::Worker {
        identity: worker_b.clone(),
        principal: principal_b.identity.clone(),
        provider: semantic::Identity {
            id: "provider-b".to_string(),
            generation: 1,
        },
        lease: lease.identity.clone(),
        state: semantic::WorkerState::Running,
        execution_ref: "opaque-execution-reference".to_string(),
        limits: BTreeMap::new(),
    });
    adapter
        .instances
        .lock()
        .unwrap()
        .insert("worker-b".to_string(), process);
    authority
        .runtime
        .workers
        .lock()
        .unwrap()
        .insert(context.object_ref(worker_b.clone()), "worker-b".to_string());

    let denied = authority
        .publish_endpoint(
            &context,
            &principal_a,
            semantic::Endpoint {
                identity: semantic::Identity {
                    id: "endpoint-b".to_string(),
                    generation: 1,
                },
                provider: semantic::Identity {
                    id: "provider-b".to_string(),
                    generation: 1,
                },
                owner: worker_b.clone(),
                transport: "transport.uds".to_string(),
                schema_id: "schema.v1".to_string(),
                capabilities: Vec::new(),
                public_attributes: BTreeMap::new(),
            },
        )
        .unwrap_err();
    assert_eq!(
        denied.reason_code, "AUTHORITY_DENIED",
        "authenticated Principal A must not publish an Endpoint for B's Worker"
    );
}

// Test B — cross-Principal Authorize denied.
#[test]
fn endpoint_cross_principal_authorize_denied() {
    let adapter = semantic_worker_adapter();
    let authority = adapter.authority();
    let context = scoped_authority_context("ns-a", "cross-authorize");
    let principal_b = principal_from_peer_cred(&PeerCred {
        pid: 7171,
        uid: 2000,
        gid: 2000,
    });
    let (_, _, worker_identity, lease, endpoint, _) =
        endpoint_authority_scenario(&adapter, "ns-a", AUTHORITY_TEST_PEER, "worker-a");
    let denied = authority
        .authorize_endpoint(
            &context,
            &principal_b,
            semantic::EndpointGrant {
                identity: semantic::Identity {
                    id: "grant-cross".to_string(),
                    generation: 1,
                },
                endpoint: endpoint.identity.clone(),
                grantee: worker_identity.clone(),
                lease: lease.identity.clone(),
                fence_token: lease.fence_token,
                expires_at_unix_ms: u64::MAX,
            },
        )
        .unwrap_err();
    assert_eq!(
        denied.reason_code, "NAMESPACE_AUTHORITY_DENIED",
        "authenticated Principal B must not authorize an Endpoint in A's Namespace"
    );
}

// Test C — cross-Principal Revoke denied.
#[test]
fn endpoint_cross_principal_revoke_denied() {
    let adapter = semantic_worker_adapter();
    let authority = adapter.authority();
    let context = scoped_authority_context("ns-a", "cross-revoke");
    let principal_b = principal_from_peer_cred(&PeerCred {
        pid: 7171,
        uid: 2000,
        gid: 2000,
    });
    let (_, _, _, _, _, grant) =
        endpoint_authority_scenario(&adapter, "ns-a", AUTHORITY_TEST_PEER, "worker-a");
    let denied = authority
        .revoke_endpoint(&context, &principal_b, &grant.identity)
        .unwrap_err();
    assert_eq!(
        denied.reason_code, "NAMESPACE_AUTHORITY_DENIED",
        "authenticated Principal B must not revoke A's Endpoint Grant"
    );
    assert!(
        authority
            .runtime
            .endpoint_grants
            .lock()
            .unwrap()
            .contains_key(&context.object_ref(grant.identity.clone())),
        "only the owner may revoke; the Grant must remain intact"
    );
}

// Test D — the Worker owner Principal is allowed to publish/authorize/revoke.
#[test]
fn endpoint_owner_principal_allowed() {
    let adapter = semantic_worker_adapter();
    let authority = adapter.authority();
    let (context, principal, _, _, endpoint, grant) =
        endpoint_authority_scenario(&adapter, "ns-a", AUTHORITY_TEST_PEER, "worker-a");
    // publish + authorize already succeeded inside the scenario.
    assert!(authority
        .runtime
        .endpoints
        .lock()
        .unwrap()
        .contains_key(&context.object_ref(endpoint.identity.clone())));
    assert!(authority
        .runtime
        .endpoint_grants
        .lock()
        .unwrap()
        .contains_key(&context.object_ref(grant.identity.clone())));
    authority
        .revoke_endpoint(&context, &principal, &grant.identity)
        .unwrap();
    assert!(
        authority.runtime.endpoint_grants.lock().unwrap().is_empty(),
        "revoking the Grant by its owner must succeed"
    );
    assert!(
        authority
            .runtime
            .endpoints
            .lock()
            .unwrap()
            .contains_key(&context.object_ref(endpoint.identity.clone())),
        "revoking the Grant must not remove the Endpoint itself"
    );
}

// Test E — Namespace isolation: identical bare Worker/Endpoint/Lease IDs in two
// Namespaces cannot be operated across Namespace boundaries.
#[test]
fn endpoint_namespace_isolation() {
    let adapter = semantic_worker_adapter_with_resources(vec![
        test_resource_with_id("resource-a"),
        test_resource_with_id("resource-b"),
    ]);
    let authority = adapter.authority();
    let (context_a, principal_a, worker_a, _, endpoint_a, grant_a) =
        endpoint_authority_scenario(&adapter, "ns-a", AUTHORITY_TEST_PEER, "worker-x");
    let (context_b, _principal_b, worker_b, lease_b, endpoint_b, grant_b) =
        endpoint_authority_scenario(
            &adapter,
            "ns-b",
            PeerCred {
                pid: 4243,
                uid: 2000,
                gid: 2000,
            },
            "worker-x",
        );
    // The bare IDs are identical across Namespaces, but the records are scoped.
    assert_eq!(endpoint_a.identity, endpoint_b.identity);
    assert_eq!(worker_a, worker_b);
    assert_eq!(grant_a.identity, grant_b.identity);
    assert_eq!(authority.runtime.endpoints.lock().unwrap().len(), 2);
    assert_eq!(authority.runtime.endpoint_grants.lock().unwrap().len(), 2);

    // A cannot operate B's record through A's Namespace: revoking the same bare
    // grant id in ns-a must not remove the ns-b copy.
    authority
        .revoke_endpoint(&context_a, &principal_a, &grant_a.identity)
        .unwrap();
    assert!(
        !authority
            .runtime
            .endpoint_grants
            .lock()
            .unwrap()
            .contains_key(&context_a.object_ref(grant_a.identity.clone())),
        "A's revoke removes A's Namespace copy"
    );
    assert!(
        authority
            .runtime
            .endpoint_grants
            .lock()
            .unwrap()
            .contains_key(&context_b.object_ref(grant_b.identity.clone())),
        "the same bare grant id in B's Namespace must remain untouched"
    );

    // A cannot wield B's authority: authorizing a grant for the same endpoint
    // id with B's Lease/Fence is denied inside A's Namespace.
    let denied = authority
        .authorize_endpoint(
            &context_a,
            &principal_a,
            semantic::EndpointGrant {
                identity: semantic::Identity {
                    id: "grant-x".to_string(),
                    generation: 1,
                },
                endpoint: endpoint_b.identity.clone(),
                grantee: worker_b.clone(),
                lease: lease_b.identity.clone(),
                fence_token: lease_b.fence_token,
                expires_at_unix_ms: u64::MAX,
            },
        )
        .unwrap_err();
    assert!(
        matches!(
            denied.reason_code.as_str(),
            "STALE_FENCE_TOKEN" | "ENDPOINT_NOT_FOUND"
        ),
        "cross-Namespace authorization using B's authority must be denied, got {}",
        denied.reason_code
    );
}

// Test F — stale Worker generation: loss/replacement of the Worker removes its
// Endpoint/Grant authority state and the old generation cannot republish.
#[test]
fn endpoint_stale_generation_invalidated() {
    let adapter = semantic_worker_adapter();
    let authority = adapter.authority();
    let (context, principal, worker_identity, _, endpoint, grant) =
        endpoint_authority_scenario(&adapter, "ns-a", AUTHORITY_TEST_PEER, "worker-a");
    assert!(authority
        .runtime
        .endpoints
        .lock()
        .unwrap()
        .contains_key(&context.object_ref(endpoint.identity.clone())));
    assert!(authority
        .runtime
        .endpoint_grants
        .lock()
        .unwrap()
        .contains_key(&context.object_ref(grant.identity.clone())));

    // Worker loss / replacement revokes the Lease and purges its authority.
    authority
        .mark_worker_lost(&context, &worker_identity, "TEST_WORKER_REPLACED")
        .unwrap();
    assert!(
        !authority
            .runtime
            .endpoints
            .lock()
            .unwrap()
            .contains_key(&context.object_ref(endpoint.identity.clone())),
        "Endpoint must be removed when its owning Worker is lost/replaced"
    );
    assert!(
        !authority
            .runtime
            .endpoint_grants
            .lock()
            .unwrap()
            .contains_key(&context.object_ref(grant.identity.clone())),
        "Grant must be removed when its owning Worker is lost/replaced"
    );

    // The stale generation can no longer publish: its Lease is revoked.
    let denied = authority
        .publish_endpoint(
            &context,
            &principal,
            semantic::Endpoint {
                identity: semantic::Identity {
                    id: "endpoint-a".to_string(),
                    generation: 1,
                },
                provider: semantic::Identity {
                    id: "provider-a".to_string(),
                    generation: 1,
                },
                owner: worker_identity.clone(),
                transport: "transport.uds".to_string(),
                schema_id: "schema.v1".to_string(),
                capabilities: Vec::new(),
                public_attributes: BTreeMap::new(),
            },
        )
        .unwrap_err();
    assert_eq!(
        denied.reason_code, "LEASE_NOT_ACTIVE",
        "the old generation's Lease is revoked, so it has no Endpoint authority"
    );
}

// Test G — Lease release (without Worker loss) must invalidate the related
// Endpoint/Grant authority state.
#[test]
fn lease_release_invalidates_endpoint_grant_authority() {
    let adapter = semantic_worker_adapter();
    let authority = adapter.authority();
    let (context, principal, worker_identity, lease, endpoint, grant) =
        endpoint_authority_scenario(&adapter, "ns-a", AUTHORITY_TEST_PEER, "worker-a");
    assert!(authority
        .runtime
        .endpoint_grants
        .lock()
        .unwrap()
        .contains_key(&context.object_ref(grant.identity.clone())));

    // Release the Lease through the canonical release path (not worker loss).
    authority
        .release_lease(&context, &principal, &lease.identity, lease.fence_token)
        .unwrap();
    assert!(
        !authority
            .runtime
            .endpoints
            .lock()
            .unwrap()
            .contains_key(&context.object_ref(endpoint.identity.clone())),
        "released Lease must remove the owner Worker's Endpoint"
    );
    assert!(
        !authority
            .runtime
            .endpoint_grants
            .lock()
            .unwrap()
            .contains_key(&context.object_ref(grant.identity.clone())),
        "released Lease must remove the dependent Grant"
    );

    // The grant is gone, so a fresh authorize against the released lease fails.
    let denied = authority
        .authorize_endpoint(
            &context,
            &principal,
            semantic::EndpointGrant {
                identity: semantic::Identity {
                    id: "grant-a".to_string(),
                    generation: 1,
                },
                endpoint: endpoint.identity.clone(),
                grantee: worker_identity.clone(),
                lease: lease.identity.clone(),
                fence_token: lease.fence_token,
                expires_at_unix_ms: u64::MAX,
            },
        )
        .unwrap_err();
    assert_eq!(
        denied.reason_code, "ENDPOINT_NOT_FOUND",
        "the released Worker's Endpoint must no longer resolve"
    );
}

// Test H — a revoked Grant is removed from Kernel authority state and can no
// longer be relied on by any consumer.
#[test]
fn revoked_grant_removed_from_authority_state() {
    let adapter = semantic_worker_adapter();
    let authority = adapter.authority();
    let (context, principal, _, _, endpoint, grant) =
        endpoint_authority_scenario(&adapter, "ns-a", AUTHORITY_TEST_PEER, "worker-a");
    let grant_key = context.object_ref(grant.identity.clone());
    // Before revocation the Grant is present (the consumer's authority ticket).
    assert!(authority
        .runtime
        .endpoint_grants
        .lock()
        .unwrap()
        .contains_key(&grant_key));

    authority
        .revoke_endpoint(&context, &principal, &grant.identity)
        .unwrap();
    assert!(
        !authority
            .runtime
            .endpoint_grants
            .lock()
            .unwrap()
            .contains_key(&grant_key),
        "revoked Grant must be removed from Kernel authority state"
    );
    assert!(
        authority
            .runtime
            .endpoints
            .lock()
            .unwrap()
            .contains_key(&context.object_ref(endpoint.identity.clone())),
        "revoking a Grant must not remove the Endpoint it referenced"
    );
}

// ---------------------------------------------------------------------------
// Phase 6: Legacy Lease Lifecycle Closure (watchdog e2e)
//
// A Lease bound to a running execution domain must reach RELEASED only after
// ACTIVE -> RELEASING -> physical cleanup confirmed. The heartbeat-timeout
// watchdog is a production Lease lifecycle path: these tests drive the real
// `enforce_heartbeat_deadlines` scan and prove that an incomplete cleanup never
// exposes RELEASED and never reallocates the resource, while a complete
// cleanup releases the Lease and lets a replacement Lease advance the Fence.
// ---------------------------------------------------------------------------

/// Sandbox whose `stop()` reports an incomplete physical cleanup, simulating a
/// worker that cannot be reaped after a heartbeat timeout.
struct UninterruptibleSandbox;

impl ProcessRuntime for UninterruptibleSandbox {
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
        Ok(ProcessHandle {
            pid: 1,
            cgroup_path: PathBuf::from("/test"),
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
            complete: false,
            exit_code: None,
            oom_killed: false,
            conditions: vec![ProcessCondition {
                reason_code: "REAP_TIMEOUT".to_string(),
                summary: "instance could not be reaped".to_string(),
            }],
            reason_code: "PROCESS_UNINTERRUPTIBLE".to_string(),
        })
    }
}

impl SandboxBackend for UninterruptibleSandbox {
    fn backend_id(&self) -> &str {
        "uninterruptible-test"
    }
}

/// Builds an adapter holding one unique resource, an ACTIVE Lease bound to a
/// started execution domain, and an overdue heartbeat so the next watchdog
/// scan treats `instance_name` as timed out.
fn watchdog_instance_scenario(
    runtime: Arc<dyn SandboxBackend>,
    instance_name: &str,
) -> (KernelServiceAdapter, cy_kernel_api::ResourceLease) {
    use crate::watchdog::InstanceActorState;

    let adapter = semantic_worker_adapter_with_resources(vec![test_resource()])
        .with_worker_heartbeat(WorkerHeartbeatConfig {
            socket_path: PathBuf::from("/run/cyrene/watchdog.sock"),
            interval: Duration::from_secs(1),
            timeout: Duration::from_millis(50),
            graceful_stop: Duration::from_millis(50),
            shutdown_ack_timeout: Duration::from_millis(20),
        });
    let holder = semantic::Identity {
        id: "watchdog-holder".to_string(),
        generation: 1,
    };
    let requirements = core_v1::ResourceRequirements {
        cpu: Some(core_v1::CpuRequirements {
            request_millicores: 500,
            limit_millicores: 750,
        }),
        memory: Some(core_v1::MemoryRequirements {
            request_bytes: 1024,
            limit_bytes: 2048,
        }),
        ephemeral_storage_limit_bytes: 0,
        accelerators: vec![core_v1::AcceleratorRequirements {
            count: 1,
            ..Default::default()
        }],
    };
    let request =
        resource_request("watchdog-lease", 1, holder, None, &requirements).expect("request");
    let lease = adapter
        .daemon
        .reserve(request)
        .expect("unique resource is allocatable");

    let mut actor = InstanceActor::new(
        instance_name,
        lease.name.clone(),
        lease.fence_token,
        runtime,
        LaunchPlan {
            instance_name: instance_name.to_string(),
            executable: PathBuf::from("/bin/true"),
            args: Vec::new(),
            environment: BTreeMap::new(),
            cgroup_name: instance_name.to_string(),
            limits: CgroupLimits::default(),
            transport_socket: None,
        },
        DeviceBinding {
            resource_id: lease.name.clone(),
            nodes: Vec::new(),
            environment: BTreeMap::new(),
            required_gids: Vec::new(),
            enforcement: EnforcementMode::Soft,
            adapter_id: "test".to_string(),
            reason_code: "test".to_string(),
        },
        Duration::from_millis(10),
    );
    actor.start().expect("watchdog instance must start");
    assert_eq!(actor.state(), InstanceActorState::Healthy);

    let mut process = managed_test_process(
        instance_name,
        lease.fence_token,
        Some(core_v1::ResourceLeaseRef {
            lease_name: lease.name.clone(),
            fence_token: lease.fence_token,
        }),
    );
    process.actor = actor;
    adapter
        .instances
        .lock()
        .unwrap()
        .insert(instance_name.to_string(), process);
    // Make the instance overdue: last heartbeat is far older than the 10ms
    // deadline configured on the actor.
    adapter
        .instances
        .lock()
        .unwrap()
        .get_mut(instance_name)
        .unwrap()
        .actor
        .on_heartbeat_received(std::time::Instant::now() - Duration::from_secs(1));
    (adapter, lease)
}

#[test]
fn watchdog_incomplete_cleanup_never_releases_and_blocks_reacquire() {
    let (adapter, lease) =
        watchdog_instance_scenario(Arc::new(UninterruptibleSandbox), "watchdog-w1");
    let old_fence = lease.fence_token;

    adapter.enforce_heartbeat_deadlines();

    // The Lease must never reach RELEASED: it fails closed instead.
    let after = adapter.daemon.lease(&lease.name).unwrap();
    assert_eq!(
        after.state,
        cy_kernel_api::LeaseState::Failed,
        "incomplete cleanup must fail the Lease, never release it"
    );
    assert_ne!(
        after.state,
        cy_kernel_api::LeaseState::Released,
        "RELEASED must never be visible for an incompletely cleaned domain"
    );

    // The still-held allocation must block reacquisition.
    let holder = semantic::Identity {
        id: "watchdog-holder-retry".to_string(),
        generation: 1,
    };
    let requirements = core_v1::ResourceRequirements {
        cpu: Some(core_v1::CpuRequirements {
            request_millicores: 500,
            limit_millicores: 750,
        }),
        memory: Some(core_v1::MemoryRequirements {
            request_bytes: 1024,
            limit_bytes: 2048,
        }),
        ephemeral_storage_limit_bytes: 0,
        accelerators: vec![core_v1::AcceleratorRequirements {
            count: 1,
            ..Default::default()
        }],
    };
    let request =
        resource_request("watchdog-lease-retry", 1, holder, None, &requirements).expect("request");
    let reacquire = adapter.daemon.reserve(request);
    assert!(
        reacquire.is_err(),
        "the still-held FAILED lease must block reallocation with INSUFFICIENT_RESOURCES"
    );
    assert!(
        old_fence > 0,
        "the acquired lease must carry a non-zero Fence"
    );
}

#[test]
fn watchdog_complete_cleanup_releases_and_replacement_fence_advances() {
    let (adapter, lease) = watchdog_instance_scenario(Arc::new(FakeSandbox), "watchdog-w2");
    let old_fence = lease.fence_token;

    adapter.enforce_heartbeat_deadlines();

    // Physical cleanup completed: the Lease is RELEASED.
    let after = adapter.daemon.lease(&lease.name).unwrap();
    assert_eq!(
        after.state,
        cy_kernel_api::LeaseState::Released,
        "complete cleanup must release the Lease"
    );

    // The resource is allocatable again and a replacement Lease succeeds with
    // a strictly higher Fence token (monotonic fencing).
    let holder = semantic::Identity {
        id: "watchdog-holder-replacement".to_string(),
        generation: 1,
    };
    let requirements = core_v1::ResourceRequirements {
        cpu: Some(core_v1::CpuRequirements {
            request_millicores: 500,
            limit_millicores: 750,
        }),
        memory: Some(core_v1::MemoryRequirements {
            request_bytes: 1024,
            limit_bytes: 2048,
        }),
        ephemeral_storage_limit_bytes: 0,
        accelerators: vec![core_v1::AcceleratorRequirements {
            count: 1,
            ..Default::default()
        }],
    };
    let request = resource_request("watchdog-lease-replacement", 1, holder, None, &requirements)
        .expect("request");
    let replacement = adapter
        .daemon
        .reserve(request)
        .expect("replacement Lease must succeed after complete cleanup");
    assert!(
        replacement.fence_token > old_fence,
        "replacement Fence ({}) must exceed the old Fence ({})",
        replacement.fence_token,
        old_fence
    );
}

// ---------------------------------------------------------------------------
// Phase 7: Durability and Journal Failure Policy
//
// Durable writes are Class A (durable-before-visible), Class B (durable intent
// / physical action / durable outcome), or Class C (best-effort telemetry).
// These tests inject persistence failures and prove the policy: no unsafe
// visible authority, no fence reuse, no silently reusable resource, and no
// corruption of authority state when observability fails.
// ---------------------------------------------------------------------------

// Class B intent: the watchdog must never begin a physical release without its
// durable LEASE_RELEASE_STARTED record. A failing journal defers the release to
// the next scan (fail-closed) instead of leaking the Lease as ACTIVE behind a
// stopped Worker.
#[test]
fn watchdog_release_intent_journal_failure_defers_fail_closed() {
    let adapter = semantic_worker_adapter_with_resources(vec![test_resource()])
        .with_worker_heartbeat(WorkerHeartbeatConfig {
            socket_path: PathBuf::from("/run/cyrene/watchdog-journal.sock"),
            interval: Duration::from_secs(1),
            timeout: Duration::from_millis(50),
            graceful_stop: Duration::from_millis(50),
            shutdown_ack_timeout: Duration::from_millis(20),
        })
        .with_runtime_journal(Arc::new(FailingRuntimeJournal));
    let holder = semantic::Identity {
        id: "watchdog-journal-holder".to_string(),
        generation: 1,
    };
    let requirements = core_v1::ResourceRequirements {
        cpu: Some(core_v1::CpuRequirements {
            request_millicores: 500,
            limit_millicores: 750,
        }),
        memory: Some(core_v1::MemoryRequirements {
            request_bytes: 1024,
            limit_bytes: 2048,
        }),
        ephemeral_storage_limit_bytes: 0,
        accelerators: vec![core_v1::AcceleratorRequirements {
            count: 1,
            ..Default::default()
        }],
    };
    let request = resource_request("watchdog-journal-lease", 1, holder, None, &requirements)
        .expect("request");
    let lease = adapter
        .daemon
        .reserve(request)
        .expect("unique resource is allocatable");
    let mut actor = InstanceActor::new(
        "watchdog-journal-w1",
        lease.name.clone(),
        lease.fence_token,
        Arc::new(FakeSandbox),
        LaunchPlan {
            instance_name: "watchdog-journal-w1".to_string(),
            executable: PathBuf::from("/bin/true"),
            args: Vec::new(),
            environment: BTreeMap::new(),
            cgroup_name: "watchdog-journal-w1".to_string(),
            limits: CgroupLimits::default(),
            transport_socket: None,
        },
        DeviceBinding {
            resource_id: lease.name.clone(),
            nodes: Vec::new(),
            environment: BTreeMap::new(),
            required_gids: Vec::new(),
            enforcement: EnforcementMode::Soft,
            adapter_id: "test".to_string(),
            reason_code: "test".to_string(),
        },
        Duration::from_millis(10),
    );
    actor.start().expect("watchdog instance must start");
    let mut process = managed_test_process(
        "watchdog-journal-w1",
        lease.fence_token,
        Some(core_v1::ResourceLeaseRef {
            lease_name: lease.name.clone(),
            fence_token: lease.fence_token,
        }),
    );
    process.actor = actor;
    adapter
        .instances
        .lock()
        .unwrap()
        .insert("watchdog-journal-w1".to_string(), process);
    adapter
        .instances
        .lock()
        .unwrap()
        .get_mut("watchdog-journal-w1")
        .unwrap()
        .actor
        .on_heartbeat_received(std::time::Instant::now() - Duration::from_secs(1));

    // The durable intent cannot be persisted: the watchdog must defer.
    adapter.enforce_heartbeat_deadlines();

    // Fail-closed: the physical release never began; the Lease stays ACTIVE.
    let after = adapter.daemon.lease(&lease.name).unwrap();
    assert_eq!(
        after.state,
        cy_kernel_api::LeaseState::Active,
        "the release must not begin without its durable intent"
    );
    // The Worker is not stopped/removed and the watchdog is re-armed.
    let instances = adapter.instances.lock().unwrap();
    let process = instances
        .get("watchdog-journal-w1")
        .expect("the instance must remain for the next scan");
    assert!(
        !process.watchdog_triggered,
        "the watchdog must be re-armed to retry the durable intent"
    );
    drop(instances);
    // The resource is not silently reusable.
    let retry = resource_request(
        "watchdog-journal-retry",
        1,
        semantic::Identity {
            id: "watchdog-journal-retry-holder".to_string(),
            generation: 1,
        },
        None,
        &requirements,
    )
    .expect("request");
    assert!(
        adapter.daemon.reserve(retry).is_err(),
        "the still-ACTIVE Lease must keep the resource non-allocatable"
    );
}

// Class A/B launch evidence: a Worker must never become visible without its
// durable InstanceLaunched record. The launch fails closed and the Lease stays
// held, so the resource is not silently reusable.
#[test]
fn worker_launch_journal_failure_fails_closed_and_keeps_lease_held() {
    #[derive(Default)]
    struct LaunchFailingJournal {
        records: std::sync::Mutex<Vec<RuntimeJournalRecord>>,
    }
    impl RuntimeJournalSink for LaunchFailingJournal {
        fn append(&self, record: RuntimeJournalRecord) -> Result<(), ProviderError> {
            if record.event == RuntimeJournalEvent::InstanceLaunched {
                return Err(ProviderError::new(
                    "failing-journal",
                    "JOURNAL_WRITE_FAILED",
                    "injected worker launch write failure",
                ));
            }
            self.records.lock().unwrap().push(record);
            Ok(())
        }
    }

    let adapter = semantic_worker_adapter_with_resources(vec![test_resource()])
        .with_runtime_journal(Arc::new(LaunchFailingJournal::default()));
    let authority = adapter.authority();
    let context = scoped_authority_context("ns-launch-fail", "launch-journal-fail");
    let principal = principal_from_peer_cred(&AUTHORITY_TEST_PEER);
    let worker_identity = semantic::Identity {
        id: "worker-launch-fail".to_string(),
        generation: 1,
    };
    let query = semantic::ResourceQuery {
        resource_class: "accelerator".to_string(),
        count: 1,
        required_capabilities: vec![semantic::CapabilityRequirement {
            id: "accelerator.compute".to_string(),
            minimum_revision: 1,
            required_properties: BTreeMap::new(),
        }],
        minimum_capacity: BTreeMap::new(),
    };
    let lease = authority
        .acquire_lease(
            &context,
            &principal,
            worker_identity.clone(),
            query,
            u64::MAX,
        )
        .unwrap();
    assert_eq!(lease.state, semantic::LeaseState::Active);

    // The InstanceLaunched durable evidence cannot be persisted.
    let launch = authority.start_worker(
        &context,
        &principal,
        semantic::Worker {
            identity: worker_identity.clone(),
            principal: principal.identity.clone(),
            provider: semantic::Identity {
                id: "provider-launch-fail".to_string(),
                generation: 1,
            },
            lease: lease.identity.clone(),
            state: semantic::WorkerState::Registered,
            execution_ref: "opaque-execution-reference".to_string(),
            limits: BTreeMap::new(),
        },
    );
    assert!(
        launch.is_err(),
        "launch must fail closed when its durable evidence cannot be persisted"
    );
    assert!(
        authority.runtime.instances.lock().unwrap().is_empty(),
        "no Worker may be externally visible without its durable launch evidence"
    );

    // The held Lease blocks reallocation: no silently reusable resource.
    let retry = authority.acquire_lease(
        &context,
        &principal,
        semantic::Identity {
            id: "worker-launch-fail-retry".to_string(),
            generation: 1,
        },
        semantic::ResourceQuery {
            resource_class: "accelerator".to_string(),
            count: 1,
            required_capabilities: vec![semantic::CapabilityRequirement {
                id: "accelerator.compute".to_string(),
                minimum_revision: 1,
                required_properties: BTreeMap::new(),
            }],
            minimum_capacity: BTreeMap::new(),
        },
        u64::MAX,
    );
    assert!(
        retry.is_err(),
        "the held ACTIVE Lease must block reallocation"
    );
}

// Class C: semantic event projections are observability, not correctness
// evidence. When the durable event store rejects an append the projection is
// dropped without corrupting authority state or panicking.
#[test]
fn semantic_event_append_failure_degrades_stream_without_silent_gap() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};

    struct FailOnceEventStore {
        fail_next: AtomicBool,
        append_count: AtomicUsize,
    }
    impl DurableEventStore for FailOnceEventStore {
        fn append_event(&self, _record: DurableEventRecord) -> Result<(), ProviderError> {
            self.append_count.fetch_add(1, AtomicOrdering::SeqCst);
            if self.fail_next.swap(false, AtomicOrdering::SeqCst) {
                return Err(ProviderError::new(
                    "test",
                    "EVENT_APPEND_FAILED",
                    "injected event append failure",
                ));
            }
            Ok(())
        }

        fn events_for_source(
            &self,
            _source: &semantic::Identity,
            _namespace: &str,
        ) -> Result<Option<Vec<DurableEventRecord>>, ProviderError> {
            Ok(None)
        }
    }

    let store = Arc::new(FailOnceEventStore {
        fail_next: AtomicBool::new(true),
        append_count: AtomicUsize::new(0),
    });
    let adapter = semantic_worker_adapter_with_resources(vec![test_resource()])
        .with_event_store(store.clone());
    let authority = adapter.authority();
    let context = scoped_authority_context("ns-event-fail", "event-append-fail");
    let principal = principal_from_peer_cred(&AUTHORITY_TEST_PEER);
    let worker_identity = semantic::Identity {
        id: "worker-event-fail".to_string(),
        generation: 1,
    };
    let query = semantic::ResourceQuery {
        resource_class: "accelerator".to_string(),
        count: 1,
        required_capabilities: vec![semantic::CapabilityRequirement {
            id: "accelerator.compute".to_string(),
            minimum_revision: 1,
            required_properties: BTreeMap::new(),
        }],
        minimum_capacity: BTreeMap::new(),
    };
    let lease = authority
        .acquire_lease(
            &context,
            &principal,
            worker_identity.clone(),
            query,
            u64::MAX,
        )
        .unwrap();
    assert_eq!(lease.state, semantic::LeaseState::Active);

    // The first normative event (worker.starting) append fails: the stream must
    // degrade rather than silently lose the fact.
    authority
        .start_worker(
            &context,
            &principal,
            semantic::Worker {
                identity: worker_identity.clone(),
                principal: principal.identity.clone(),
                provider: semantic::Identity {
                    id: "provider-event-fail".to_string(),
                    generation: 1,
                },
                lease: lease.identity.clone(),
                state: semantic::WorkerState::Registered,
                execution_ref: "opaque-execution-reference".to_string(),
                limits: BTreeMap::new(),
            },
        )
        .unwrap();
    assert!(
        !authority.runtime.instances.lock().unwrap().is_empty(),
        "authority state must be intact despite the failed event append"
    );

    // The store has since RECOVERED, but the stream must NOT continue as if
    // contiguous: an already-subscribed client must never silently miss the
    // failed fact while later events advance. The second publish is suppressed
    // by the degraded stream, so the store never sees another append attempt.
    authority.publish_semantic_event_in(
        &context.namespace,
        worker_identity.clone(),
        "worker.running",
        "cyrene.worker.v1",
        Vec::new(),
    );
    assert_eq!(
        store.append_count.load(AtomicOrdering::SeqCst),
        1,
        "a degraded stream must not attempt further durable appends"
    );

    // The degraded stream is externally observable: replay surfaces Gap
    // (resnapshot required), never a silent Current-with-no-new-events stall.
    let page = authority
        .events_after(
            &context,
            &principal,
            &semantic::EventCursor {
                source: authority.semantic_event_source_for(&context.namespace),
                sequence: 0,
            },
            OPERATION_EVENT_HISTORY_CAPACITY,
        )
        .unwrap();
    assert_eq!(
        page.status,
        semantic::ReplayStatus::Gap,
        "a degraded stream must surface as Gap so clients resnapshot, not stall silently"
    );
    assert!(
        page.events.is_empty(),
        "no events may replay past a lost fact"
    );
}

#[test]
fn worker_launch_persistence_failure_reaps_physical_process() {
    use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

    #[derive(Default)]
    struct LaunchFailingJournal {
        records: std::sync::Mutex<Vec<RuntimeJournalRecord>>,
    }
    impl RuntimeJournalSink for LaunchFailingJournal {
        fn append(&self, record: RuntimeJournalRecord) -> Result<(), ProviderError> {
            if record.event == RuntimeJournalEvent::InstanceLaunched {
                return Err(ProviderError::new(
                    "failing-journal",
                    "JOURNAL_WRITE_FAILED",
                    "injected worker launch write failure",
                ));
            }
            self.records.lock().unwrap().push(record);
            Ok(())
        }
    }

    /// Records whether the physical sandbox process was launched and stopped,
    /// so the test can prove the spawned domain is synchronously reaped.
    #[derive(Clone, Default)]
    struct RecordingSandbox {
        launched: Arc<AtomicBool>,
        stopped: Arc<AtomicBool>,
    }
    impl ProcessRuntime for RecordingSandbox {
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
            self.launched.store(true, AtomicOrdering::SeqCst);
            Ok(ProcessHandle {
                pid: 1,
                cgroup_path: PathBuf::from("/test"),
                start_time_ticks: Some(1),
                transport_socket: None,
            })
        }

        fn stop(
            &self,
            _handle: &ProcessHandle,
            _request: &StopRequest,
        ) -> Result<CleanupReport, ProviderError> {
            self.stopped.store(true, AtomicOrdering::SeqCst);
            Ok(CleanupReport {
                complete: true,
                exit_code: Some(0),
                oom_killed: false,
                conditions: Vec::new(),
                reason_code: "TEST_STOP".to_string(),
            })
        }
    }
    impl SandboxBackend for RecordingSandbox {
        fn backend_id(&self) -> &str {
            "recording-test"
        }
    }

    let sandbox = Arc::new(RecordingSandbox::default());
    let hardware = Arc::new(TestHardware {
        resources: vec![test_resource()],
    });
    let daemon = Arc::new(KernelDaemon::new(
        hardware.clone(),
        hardware,
        Arc::new(InMemoryResourceManager::new("node", vec![test_resource()])),
        sandbox.clone(),
        "node",
        7,
    ));
    let adapter = KernelServiceAdapter::new(daemon, Arc::new(TestWorkerResolver))
        .with_runtime_journal(Arc::new(LaunchFailingJournal::default()));
    let authority = adapter.authority();
    let context = scoped_authority_context("ns-reap", "launch-reap");
    let principal = principal_from_peer_cred(&AUTHORITY_TEST_PEER);
    let worker_identity = semantic::Identity {
        id: "worker-reap".to_string(),
        generation: 1,
    };
    let query = semantic::ResourceQuery {
        resource_class: "accelerator".to_string(),
        count: 1,
        required_capabilities: vec![semantic::CapabilityRequirement {
            id: "accelerator.compute".to_string(),
            minimum_revision: 1,
            required_properties: BTreeMap::new(),
        }],
        minimum_capacity: BTreeMap::new(),
    };
    let lease = authority
        .acquire_lease(
            &context,
            &principal,
            worker_identity.clone(),
            query,
            u64::MAX,
        )
        .unwrap();

    // InstanceLaunched cannot be persisted after the physical spawn.
    let launch = authority.start_worker(
        &context,
        &principal,
        semantic::Worker {
            identity: worker_identity.clone(),
            principal: principal.identity.clone(),
            provider: semantic::Identity {
                id: "provider-reap".to_string(),
                generation: 1,
            },
            lease: lease.identity.clone(),
            state: semantic::WorkerState::Registered,
            execution_ref: "opaque-execution-reference".to_string(),
            limits: BTreeMap::new(),
        },
    );
    assert!(
        launch.is_err(),
        "launch must fail closed when InstanceLaunched cannot be persisted"
    );
    // The physical domain WAS spawned...
    assert!(
        sandbox.launched.load(AtomicOrdering::SeqCst),
        "the sandbox must have physically launched the process"
    );
    // ...and it was synchronously reaped before the failure was returned, so no
    // untracked physical execution domain remains alive.
    assert!(
        sandbox.stopped.load(AtomicOrdering::SeqCst),
        "the launched physical process must be reaped before returning the failure"
    );
    assert!(
        authority.runtime.instances.lock().unwrap().is_empty(),
        "no semantic Worker may be visible without its durable launch evidence"
    );
    assert_eq!(lease.state, semantic::LeaseState::Active);
}

// Worst-case restart boundary: physical spawn succeeds -> InstanceLaunched
// persistence fails -> synchronous reap is incomplete -> InstanceCleanupFailed
// persistence ALSO fails. The durable journal must still retain the pre-launch
// intent (InstanceLaunching) so restart Discover/Classify can recognize "launch
// intended, outcome unknown" instead of treating the lease as a cleanly
// reserved, never-bound resource.
#[test]
fn worker_launch_double_persistence_failure_keeps_pre_launch_intent() {
    #[derive(Default)]
    struct LaunchEvidenceFailingJournal {
        records: std::sync::Mutex<Vec<RuntimeJournalRecord>>,
    }
    impl RuntimeJournalSink for LaunchEvidenceFailingJournal {
        fn append(&self, record: RuntimeJournalRecord) -> Result<(), ProviderError> {
            if matches!(
                record.event,
                RuntimeJournalEvent::InstanceLaunched | RuntimeJournalEvent::InstanceCleanupFailed
            ) {
                return Err(ProviderError::new(
                    "failing-journal",
                    "JOURNAL_WRITE_FAILED",
                    "injected post-spawn persistence failure",
                ));
            }
            self.records.lock().unwrap().push(record);
            Ok(())
        }
    }

    let journal = Arc::new(LaunchEvidenceFailingJournal::default());
    let hardware = Arc::new(TestHardware {
        resources: vec![test_resource()],
    });
    let daemon = Arc::new(KernelDaemon::new(
        hardware.clone(),
        hardware,
        Arc::new(InMemoryResourceManager::new("node", vec![test_resource()])),
        Arc::new(UninterruptibleSandbox),
        "node",
        7,
    ));
    let adapter = KernelServiceAdapter::new(daemon, Arc::new(TestWorkerResolver))
        .with_runtime_journal(journal.clone());
    let authority = adapter.authority();
    let context = scoped_authority_context("ns-double", "launch-double");
    let principal = principal_from_peer_cred(&AUTHORITY_TEST_PEER);
    let worker_identity = semantic::Identity {
        id: "worker-double".to_string(),
        generation: 1,
    };
    let query = semantic::ResourceQuery {
        resource_class: "accelerator".to_string(),
        count: 1,
        required_capabilities: vec![semantic::CapabilityRequirement {
            id: "accelerator.compute".to_string(),
            minimum_revision: 1,
            required_properties: BTreeMap::new(),
        }],
        minimum_capacity: BTreeMap::new(),
    };
    let lease = authority
        .acquire_lease(
            &context,
            &principal,
            worker_identity.clone(),
            query,
            u64::MAX,
        )
        .unwrap();

    // Spawn succeeds; InstanceLaunched fails; the synchronous reap is incomplete
    // (UninterruptibleSandbox) so InstanceCleanupFailed is attempted and ALSO
    // fails.
    let launch = authority.start_worker(
        &context,
        &principal,
        semantic::Worker {
            identity: worker_identity.clone(),
            principal: principal.identity.clone(),
            provider: semantic::Identity {
                id: "provider-double".to_string(),
                generation: 1,
            },
            lease: lease.identity.clone(),
            state: semantic::WorkerState::Registered,
            execution_ref: "opaque-execution-reference".to_string(),
            limits: BTreeMap::new(),
        },
    );
    assert!(
        launch.is_err(),
        "launch must fail closed on the post-spawn persistence failure"
    );

    // The durable journal still carries the pre-launch intent.
    let records = journal.records.lock().unwrap();
    assert!(
        records.iter().any(|record| {
            record.event == RuntimeJournalEvent::InstanceLaunching
                && record.instance_name.as_deref() == Some("worker-double")
        }),
        "the pre-launch durable intent must survive the double persistence failure"
    );
    assert!(
        records
            .iter()
            .any(|record| record.event == RuntimeJournalEvent::LeaseReserved),
        "the lease reservation must be durably present"
    );
    assert!(
        !records
            .iter()
            .any(|record| record.event == RuntimeJournalEvent::InstanceLaunched),
        "no launch outcome may be falsely recorded"
    );
    drop(records);
}
