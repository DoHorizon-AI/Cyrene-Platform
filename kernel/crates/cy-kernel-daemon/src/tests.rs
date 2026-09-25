// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: kernel/crates/cy-kernel-daemon/src/tests.rs
// ║ Module: CYRENE Platform
// ║ Role: Kernel daemon semantic authority, lifecycle, recovery, endpoint, and event regression tests.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Kernel daemon 语义权威、生命周期、恢复、Endpoint 与 Event 回归测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! cy-kernel-daemon 单元测试与集成测试套件。

#![allow(deprecated)]

mod authority;
mod endpoint;
mod golden;
mod lifecycle;
mod recovery;
mod service_supervision_wire;

use authority::{
    hardware_provider_adapter, heartbeat_adapter, semantic_worker_adapter,
    semantic_worker_adapter_with_resources,
};

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

fn watch_event(response: core_v1::WatchEventsResponse) -> semantic_v1::Event {
    match response.body {
        Some(core_v1::watch_events_response::Body::Event(event)) => event,
        Some(core_v1::watch_events_response::Body::Continuity(continuity)) => {
            panic!(
                "watch stream returned continuity frame: {:?}",
                continuity.status
            )
        }
        None => panic!("watch stream returned an empty response"),
    }
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
            environment: resource
                .attributes
                .get("test.binding.selector")
                .map(|value| BTreeMap::from([("TEST_DEVICE_SELECTION".to_string(), value.clone())]))
                .unwrap_or_default(),
            joinable_environment_keys: Default::default(),
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
        connection_ref: "uds://runtime/direct-worker".to_string(),
        credential_ref: None,
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
                working_dir: None,
                transport_socket: None,
            },
        })
    }
}
