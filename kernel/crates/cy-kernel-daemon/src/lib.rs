//! CYRENE 节点内核守护进程组合根 (Kernel Daemon Composition Root).
//!
//! 【内核守护进程职责与设计哲学】
//! 本模块作为节点内核守护进程的装配中心（Composition Root），负责将硬件探测、资源租约与沙箱运行时等各个六边形端口组合连接：
//! 1. **事实汇聚与映射**：聚合底层适配器上报的不可变硬件事实，将其严格、诚实地映射为 Core v1 Protobuf 消息（[`core_v1::KernelCapabilities`]）；
//! 2. **绝不猜测（No Guessing Invariant）**：对于探测不到的显存容量、NUMA 节点或拓扑链路，严格上报未知，绝不用启发式猜测伪造数据；
//! 3. **职责边界**：内核守护进程专职负责单机节点物理事实与资源隔离，不包含远程制品下载、全局调度仲裁或跨重启接管僵尸进程的逻辑。

#![forbid(unsafe_code)]
// Tonic owns the concrete Status representation; service helpers keep the
// canonical Result<T, Status> signature instead of boxing transport errors.
#![allow(clippy::result_large_err)]

use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use cy_adapter_client::{HardwareAdapterEndpoint, UdsHardwareAdapterRegistry};
use cy_kernel_api::{
    semantic, CgroupLimits, CleanupReport, DeviceBinding, EnforcementMode, HostInventoryProvider,
    InstalledPluginResolver, InventorySnapshot, LeaseState, NoopRuntimeJournal, ProviderError,
    ResourceLease, ResourceLeaseManager, ResourceProvider, ResourceRequest, RuntimeJournalEvent,
    RuntimeJournalRecord, RuntimeJournalSink, SandboxBackend, VerifiedInstallation,
};
use cy_proto::{core_v1, semantic_v1};
use tokio::sync::{broadcast, mpsc};
use tokio_stream::{iter, wrappers::ReceiverStream, Stream};
use tonic::{Request, Response, Status};

mod sandboxed_process;
use sandboxed_process::{SandboxedProcess, SandboxedProcessState};

const OPERATION_EVENT_HISTORY_CAPACITY: usize = 256;
const OPERATION_EVENT_SUBSCRIBER_CAPACITY: usize = 64;

/// 节点内核守护进程核心结构体
pub struct KernelDaemon {
    /// 宿主机硬件清单提供者
    inventory_provider: Arc<dyn HostInventoryProvider>,
    /// 进程外通用资源 Provider
    resource_provider: Arc<dyn ResourceProvider>,
    /// 硬件资源租约管理器
    resources: Arc<dyn ResourceLeaseManager>,
    /// 沙箱隔离后端
    sandbox: Arc<dyn SandboxBackend>,
    /// 节点唯一 ID
    node_id: String,
    /// 节点启动纪元代数 (Epoch)
    node_epoch: u64,
}

impl KernelDaemon {
    /// 构造新的内核守护进程实例
    pub fn new(
        inventory_provider: Arc<dyn HostInventoryProvider>,
        resource_provider: Arc<dyn ResourceProvider>,
        resources: Arc<dyn ResourceLeaseManager>,
        sandbox: Arc<dyn SandboxBackend>,
        node_id: impl Into<String>,
        node_epoch: u64,
    ) -> Self {
        Self {
            inventory_provider,
            resource_provider,
            resources,
            sandbox,
            node_id: node_id.into(),
            node_epoch,
        }
    }

    /// 通过版本化 UDS 连接进程外硬件适配器注册表。
    ///
    /// 该组合根不加载厂商动态库，也不执行任何厂商探测命令。适配器失联时，端口
    /// 返回 `ADAPTER_UNAVAILABLE`，由上层将节点转为不可继续分配的降级状态。
    pub fn with_hardware_adapters(
        endpoints: impl IntoIterator<Item = HardwareAdapterEndpoint>,
        resources: Arc<dyn ResourceLeaseManager>,
        sandbox: Arc<dyn SandboxBackend>,
        node_id: impl Into<String>,
        node_epoch: u64,
    ) -> Result<Self, ProviderError> {
        let adapters = Arc::new(UdsHardwareAdapterRegistry::from_endpoints(endpoints)?);
        Ok(Self::new(
            adapters.clone(),
            adapters,
            resources,
            sandbox,
            node_id,
            node_epoch,
        ))
    }

    /// 检查节点基础沙箱环境是否已就绪
    pub fn preflight_ready(&self) -> bool {
        self.sandbox.preflight().ready
            && self
                .inventory()
                .is_ok_and(|snapshot| snapshot.capabilities.ready)
    }

    /// 获取最新的硬件清单快照
    pub fn inventory(&self) -> Result<InventorySnapshot, ProviderError> {
        self.inventory_provider.probe_inventory()
    }

    /// Refreshes only current external hardware facts. A successful probe is
    /// committed to the resource ledger; a failed or expired fact leaves the
    /// last allocation state untouched and lets the caller enter DEGRADED.
    pub fn refresh_inventory_facts(&self) -> Result<InventorySnapshot, ProviderError> {
        let snapshot = self.inventory()?;
        self.resources.refresh_inventory(snapshot.clone())?;
        Ok(snapshot)
    }

    /// 申请预留硬件资源租约
    pub fn reserve(&self, request: ResourceRequest) -> Result<ResourceLease, ProviderError> {
        let snapshot = self.inventory()?;
        if !snapshot.capabilities.ready {
            return Err(ProviderError::new(
                "kernel-daemon",
                "ADAPTER_DEGRADED",
                "required hardware adapter capability is not ready",
            ));
        }
        self.resources.refresh_inventory(snapshot)?;
        self.resources.reserve(request)
    }

    /// 释放硬件资源租约
    pub fn release(&self, lease_name: &str, fence_token: u64) -> Result<(), ProviderError> {
        self.resources.release(lease_name, fence_token)
    }

    /// 读取租约当前快照，用于服务层的 fencing 校验与结果回报
    pub fn lease(&self, lease_name: &str) -> Result<ResourceLease, ProviderError> {
        self.resources.get_lease(lease_name)
    }

    /// Extend a live lease while retaining its exact resource allocation and
    /// fencing authority. The ledger performs the active-state, fence, and
    /// expiry monotonicity checks atomically.
    pub fn renew(
        &self,
        lease_name: &str,
        fence_token: u64,
        expires_at_unix_ms: u64,
    ) -> Result<ResourceLease, ProviderError> {
        self.resources
            .renew(lease_name, fence_token, expires_at_unix_ms)
    }

    /// 为租约内的所有资源合并一份沙箱绑定。
    ///
    /// 当前 SandboxBackend 接口以单份 DeviceBinding 表达资源集合，因此多资源租约在这里合并
    /// 设备节点、环境变量与 GID；任何 Provider 冲突都会 fail closed。
    pub fn binding_for_lease(&self, lease: &ResourceLease) -> Result<DeviceBinding, ProviderError> {
        let snapshot = self.inventory()?;
        let mut bindings = Vec::with_capacity(lease.allocations.len());
        for allocation in &lease.allocations {
            let resource = snapshot
                .resources
                .iter()
                .find(|resource| resource.identity == allocation.resource)
                .ok_or_else(|| {
                    ProviderError::new(
                        "kernel-daemon",
                        "LEASE_RESOURCE_NOT_IN_INVENTORY",
                        &allocation.resource.id,
                    )
                })?;
            bindings.push(
                self.resource_provider
                    .create_binding_for_generation(resource, lease.inventory_generation)?,
            );
        }
        merge_bindings(bindings)
    }

    /// 根据当前节点底层最新探测事实，组装 Core v1 规范的 [`core_v1::KernelCapabilities`] 能力事实消息。
    ///
    /// # 诚实上报原则
    /// 严格如实上报硬件探测结果，绝不针对缺失的显存、NUMA 或拓扑值进行主观猜测。
    #[allow(deprecated)]
    pub fn get_kernel_capabilities(&self) -> Result<core_v1::KernelCapabilities, ProviderError> {
        let snapshot = self.inventory()?;
        let resources = snapshot
            .resources
            .iter()
            .map(to_semantic_proto_resource)
            .collect();
        let runtime = self.sandbox.preflight();
        Ok(core_v1::KernelCapabilities {
            node: Some(core_v1::NodeRef {
                node_id: self.node_id.clone(),
                node_epoch: self.node_epoch,
            }),
            kernel_version: String::new(),
            inventory_generation: snapshot.generation,
            observed_at: Some(now_timestamp()),
            capacity: None,
            // Deprecated compatibility projection. Kernel no longer decodes
            // vendor or accelerator-specific attributes.
            accelerators: Vec::new(),
            sandbox_backends: vec![self.sandbox.backend_id().to_string()],
            enforcement: runtime
                .enforcement
                .into_iter()
                .map(|report| core_v1::EnforcementReport {
                    resource_kind: core_v1::ResourceKind::Accelerator as i32,
                    mode: to_proto_enforcement(report.mode) as i32,
                    adapter_id: report.adapter_id,
                    reason_code: report.reason_code,
                })
                .collect(),
            feature_flags: snapshot
                .capabilities
                .facts
                .into_iter()
                .filter(|fact| fact.available)
                .map(|fact| fact.name)
                .collect(),
            resources,
        })
    }
}

/// Worker heartbeat policy injected into every managed worker process.
#[derive(Debug, Clone)]
pub struct WorkerHeartbeatConfig {
    pub socket_path: PathBuf,
    pub interval: Duration,
    pub timeout: Duration,
    pub graceful_stop: Duration,
    /// Maximum time reserved for a Worker protocol-level ShutdownAck before
    /// sandboxd begins SIGTERM -> cgroup.kill -> reap.
    pub shutdown_ack_timeout: Duration,
}

impl Default for WorkerHeartbeatConfig {
    fn default() -> Self {
        Self {
            socket_path: PathBuf::from("/run/cyrene/kernel.sock"),
            interval: Duration::from_secs(5),
            timeout: Duration::from_secs(20),
            graceful_stop: Duration::from_secs(10),
            shutdown_ack_timeout: Duration::from_secs(3),
        }
    }
}

struct ManagedProcess {
    instance: SandboxedProcess,
    lease: Option<core_v1::ResourceLeaseRef>,
    semantic_worker: Option<semantic::Worker>,
    plugin: core_v1::InstalledPluginRef,
    generation: u64,
    accepted_sequence: u64,
    last_heartbeat: Instant,
    last_heartbeat_at: Option<prost_types::Timestamp>,
    runtime_state: i32,
    health: Option<core_v1::HealthReport>,
    restart_count: u32,
    watchdog_triggered: bool,
    control: Option<WorkerControlSession>,
    pending_shutdown: Option<PendingWorkerShutdown>,
}

type WorkerControlSender = mpsc::Sender<Result<core_v1::KernelToWorker, Status>>;

struct WorkerControlSession {
    connection_id: u64,
    outbound: WorkerControlSender,
}

struct PendingWorkerShutdown {
    shutdown_id: String,
    acknowledged: bool,
    drained: bool,
}

/// Core v1 KernelService 到真实资源管理器与 SandboxBackend 的最小服务适配层。
#[derive(Clone)]
pub struct KernelServiceAdapter {
    daemon: Arc<KernelDaemon>,
    resolver: Arc<dyn InstalledPluginResolver>,
    instances: Arc<Mutex<HashMap<String, ManagedProcess>>>,
    operations: Arc<Mutex<HashMap<String, core_v1::Operation>>>,
    operation_events: Arc<Mutex<VecDeque<core_v1::OperationEvent>>>,
    operation_event_sender: broadcast::Sender<core_v1::OperationEvent>,
    semantic_operations: Arc<Mutex<BTreeMap<String, semantic::Operation>>>,
    semantic_events: Arc<Mutex<VecDeque<semantic::Event>>>,
    endpoints: Arc<Mutex<BTreeMap<String, semantic::Endpoint>>>,
    endpoint_grants: Arc<Mutex<BTreeMap<String, semantic::EndpointGrant>>>,
    next_event_sequence: Arc<AtomicU64>,
    next_semantic_event_sequence: Arc<AtomicU64>,
    adapter_available: Arc<AtomicBool>,
    adapter_poll_interval: Duration,
    heartbeat: WorkerHeartbeatConfig,
    next_control_connection: Arc<AtomicU64>,
    runtime_journal: Arc<dyn RuntimeJournalSink>,
}

impl KernelServiceAdapter {
    pub fn new(daemon: Arc<KernelDaemon>, resolver: Arc<dyn InstalledPluginResolver>) -> Self {
        let (operation_event_sender, _) = broadcast::channel(OPERATION_EVENT_HISTORY_CAPACITY);
        Self {
            daemon,
            resolver,
            instances: Arc::new(Mutex::new(HashMap::new())),
            operations: Arc::new(Mutex::new(HashMap::new())),
            operation_events: Arc::new(Mutex::new(VecDeque::with_capacity(
                OPERATION_EVENT_HISTORY_CAPACITY,
            ))),
            operation_event_sender,
            semantic_operations: Arc::new(Mutex::new(BTreeMap::new())),
            semantic_events: Arc::new(Mutex::new(VecDeque::with_capacity(
                OPERATION_EVENT_HISTORY_CAPACITY,
            ))),
            endpoints: Arc::new(Mutex::new(BTreeMap::new())),
            endpoint_grants: Arc::new(Mutex::new(BTreeMap::new())),
            next_event_sequence: Arc::new(AtomicU64::new(1)),
            next_semantic_event_sequence: Arc::new(AtomicU64::new(1)),
            adapter_available: Arc::new(AtomicBool::new(true)),
            adapter_poll_interval: Duration::from_secs(5),
            heartbeat: WorkerHeartbeatConfig::default(),
            next_control_connection: Arc::new(AtomicU64::new(1)),
            runtime_journal: Arc::new(NoopRuntimeJournal),
        }
    }

    pub fn with_worker_heartbeat(mut self, heartbeat: WorkerHeartbeatConfig) -> Self {
        self.heartbeat = heartbeat;
        self
    }

    pub fn with_adapter_poll_interval(mut self, interval: Duration) -> Self {
        self.adapter_poll_interval = interval;
        self
    }

    pub fn with_runtime_journal(mut self, runtime_journal: Arc<dyn RuntimeJournalSink>) -> Self {
        self.runtime_journal = runtime_journal;
        self
    }

    pub fn server(&self) -> core_v1::kernel_service_server::KernelServiceServer<Self> {
        core_v1::kernel_service_server::KernelServiceServer::new(self.clone())
    }

    /// Canonical, transport-neutral semantic action projection. It shares the
    /// same local UDS authorization boundary and does not expose legacy
    /// installation or vendor compatibility data.
    pub fn authority_server(
        &self,
    ) -> core_v1::kernel_authority_service_server::KernelAuthorityServiceServer<Self> {
        core_v1::kernel_authority_service_server::KernelAuthorityServiceServer::new(self.clone())
    }

    pub fn lifecycle_server(
        &self,
    ) -> core_v1::plugin_lifecycle_service_server::PluginLifecycleServiceServer<Self> {
        core_v1::plugin_lifecycle_service_server::PluginLifecycleServiceServer::new(self.clone())
    }

    /// Starts the bounded watchdog loop. A missing heartbeat executes the same
    /// SIGTERM -> cgroup.kill cleanup path as an explicit termination.
    pub fn start_watchdog(&self) -> thread::JoinHandle<()> {
        let adapter = self.clone();
        thread::spawn(move || loop {
            thread::sleep(adapter.heartbeat.interval.min(Duration::from_secs(1)));
            adapter.enforce_heartbeat_deadlines();
        })
    }

    /// Continuously refreshes adapter facts independently of allocation paths.
    /// The state only transitions when a fact source disconnects/expires or
    /// subsequently recovers, so callers receive actionable DEGRADED evidence
    /// without unbounded event noise.
    pub fn start_adapter_monitor(&self) -> thread::JoinHandle<()> {
        let adapter = self.clone();
        thread::spawn(move || loop {
            thread::sleep(adapter.adapter_poll_interval);
            let result = adapter.daemon.refresh_inventory_facts();
            let ready = result.is_ok();
            let previous = adapter.adapter_available.swap(ready, Ordering::Relaxed);
            match (previous, ready) {
                (true, false) => {
                    let error = result.expect_err("adapter result is known to be an error");
                    adapter.publish_runtime_event(
                        core_v1::RuntimeEventType::AdapterDegraded,
                        "hardware-adapters",
                        error.reason_code,
                        error.message,
                    );
                }
                (false, true) => adapter.publish_runtime_event(
                    core_v1::RuntimeEventType::KernelReconciled,
                    "hardware-adapters",
                    "ADAPTER_RECOVERED",
                    "hardware adapter facts are current again",
                ),
                _ => {}
            }
        })
    }

    fn remember_operation(&self, operation: core_v1::Operation) -> core_v1::Operation {
        let mut operations = self.operations.lock().expect("operation lock poisoned");
        operations.insert(operation.name.clone(), operation.clone());
        drop(operations);
        self.publish_operation_event(operation.clone());
        operation
    }

    fn publish_operation_event(&self, operation: core_v1::Operation) {
        self.publish_event(core_v1::OperationEvent {
            event_id: String::new(),
            resume_token: String::new(),
            sequence_number: 0,
            operation: Some(operation),
            runtime_event: None,
        });
    }

    pub fn publish_runtime_event(
        &self,
        event_type: core_v1::RuntimeEventType,
        target_resource_name: impl Into<String>,
        reason_code: impl Into<String>,
        summary: impl Into<String>,
    ) {
        let target_resource_name = target_resource_name.into();
        let reason_code = reason_code.into();
        let summary = summary.into();
        self.publish_event(core_v1::OperationEvent {
            event_id: String::new(),
            resume_token: String::new(),
            sequence_number: 0,
            operation: None,
            runtime_event: Some(core_v1::RuntimeEvent {
                r#type: event_type as i32,
                target_resource_name: target_resource_name.clone(),
                reason_code: reason_code.clone(),
                summary: summary.clone(),
                observed_at: Some(now_timestamp()),
            }),
        });
        self.publish_semantic_event(
            semantic::Identity {
                id: target_resource_name,
                generation: 1,
            },
            runtime_event_kind(event_type),
            "cyrene.runtime.v1",
            format!("{reason_code}:{summary}").into_bytes(),
        );
    }

    fn publish_event(&self, mut event: core_v1::OperationEvent) {
        let sequence_number = self.next_event_sequence.fetch_add(1, Ordering::Relaxed);
        event.event_id = format!("operation-event-{sequence_number}");
        event.resume_token = sequence_number.to_string();
        event.sequence_number = sequence_number;
        let mut history = self
            .operation_events
            .lock()
            .expect("operation event history lock poisoned");
        if history.len() == OPERATION_EVENT_HISTORY_CAPACITY {
            history.pop_front();
        }
        history.push_back(event.clone());
        drop(history);
        let _ = self.operation_event_sender.send(event);
    }

    fn semantic_event_source(&self) -> semantic::Identity {
        semantic::Identity {
            id: format!("kernel/{}", self.daemon.node_id),
            generation: self.daemon.node_epoch,
        }
    }

    fn publish_semantic_event(
        &self,
        subject: semantic::Identity,
        kind: impl Into<String>,
        schema_id: impl Into<String>,
        body: impl Into<Vec<u8>>,
    ) {
        let event = semantic::Event {
            sequence: self
                .next_semantic_event_sequence
                .fetch_add(1, Ordering::Relaxed),
            source: self.semantic_event_source(),
            subject,
            kind: kind.into(),
            observed_at_unix_ms: now_unix_ms(),
            schema_id: schema_id.into(),
            body: body.into(),
        };
        if event.validate().is_err() {
            return;
        }
        let mut history = self
            .semantic_events
            .lock()
            .expect("semantic event history lock poisoned");
        if history.len() == OPERATION_EVENT_HISTORY_CAPACITY {
            history.pop_front();
        }
        history.push_back(event);
    }

    fn semantic_events_after(
        &self,
        cursor: &semantic::EventCursor,
        limit: usize,
    ) -> semantic::EventPage {
        let source = self.semantic_event_source();
        let history = self
            .semantic_events
            .lock()
            .expect("semantic event history lock poisoned");
        let oldest = history.front().map_or(0, |event| event.sequence);
        let latest = history.back().map_or(0, |event| event.sequence);
        let status = cursor.status_against(&source, oldest);
        if status != semantic::ReplayStatus::Current {
            return semantic::EventPage {
                source,
                status,
                events: Vec::new(),
                oldest_available_sequence: oldest,
                latest_available_sequence: latest,
                next_sequence: cursor.sequence,
            };
        }
        let events = history
            .iter()
            .filter(|event| event.sequence > cursor.sequence)
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
        let next_sequence = events
            .last()
            .map_or(cursor.sequence, |event| event.sequence);
        semantic::EventPage {
            source,
            status,
            events,
            oldest_available_sequence: oldest,
            latest_available_sequence: latest,
            next_sequence,
        }
    }

    fn record_runtime(
        &self,
        event: RuntimeJournalEvent,
        instance_name: Option<&str>,
        lease: Option<&core_v1::ResourceLeaseRef>,
        reason_code: &str,
    ) {
        let _ = self.runtime_journal.append(RuntimeJournalRecord {
            event,
            node_id: self.daemon.node_id.clone(),
            node_epoch: self.daemon.node_epoch,
            instance_name: instance_name.map(str::to_owned),
            lease_name: lease.map(|lease| lease.lease_name.clone()),
            fence_token: lease.map(|lease| lease.fence_token),
            reason_code: reason_code.to_string(),
        });
    }

    fn validate_node(&self, node: Option<&core_v1::NodeRef>) -> Result<(), Status> {
        let Some(node) = node else {
            return Ok(());
        };
        if node.node_id != self.daemon.node_id
            || (node.node_epoch != 0 && node.node_epoch != self.daemon.node_epoch)
        {
            return Err(Status::failed_precondition(
                "request targets another node epoch",
            ));
        }
        Ok(())
    }

    fn operation_name(&self, prefix: &str, id: &str) -> String {
        format!("operations/{prefix}-{id}")
    }

    fn lease_name(&self, mutation: Option<&core_v1::MutationContext>) -> Result<String, Status> {
        mutation
            .and_then(|mutation| {
                if !mutation.idempotency_key.is_empty() {
                    Some(mutation.idempotency_key.clone())
                } else {
                    mutation
                        .request
                        .as_ref()
                        .filter(|request| !request.request_id.is_empty())
                        .map(|request| request.request_id.clone())
                }
            })
            .filter(|value| !value.is_empty())
            .map(|value| format!("lease-{value}"))
            .ok_or_else(|| {
                Status::invalid_argument("mutation idempotency_key or request_id is required")
            })
    }

    fn operation_success(&self, name: String, target: String) -> core_v1::Operation {
        let timestamp = now_timestamp();
        self.remember_operation(core_v1::Operation {
            name,
            state: core_v1::OperationState::Succeeded as i32,
            target_resource_name: target,
            cancellable: false,
            created_at: Some(timestamp.clone()),
            updated_at: Some(timestamp),
            outcome: None,
        })
    }

    fn operation_running(&self, name: String, target: String) -> core_v1::Operation {
        let timestamp = now_timestamp();
        self.remember_operation(core_v1::Operation {
            name,
            state: core_v1::OperationState::Running as i32,
            target_resource_name: target,
            cancellable: true,
            created_at: Some(timestamp.clone()),
            updated_at: Some(timestamp),
            outcome: None,
        })
    }

    fn operation_cancelled(&self, name: String, target: String) -> core_v1::Operation {
        let timestamp = now_timestamp();
        self.remember_operation(core_v1::Operation {
            name,
            state: core_v1::OperationState::Cancelled as i32,
            target_resource_name: target,
            cancellable: false,
            created_at: Some(timestamp.clone()),
            updated_at: Some(timestamp),
            outcome: None,
        })
    }

    fn remember_semantic_operation(&self, operation: semantic::Operation) -> semantic::Operation {
        self.semantic_operations
            .lock()
            .expect("semantic operation lock poisoned")
            .insert(
                semantic_identity_key(&operation.identity),
                operation.clone(),
            );
        self.publish_semantic_event(
            operation.identity.clone(),
            semantic_operation_event_kind(operation.state),
            "cyrene.operation.v1",
            Vec::new(),
        );
        operation
    }

    fn publish_cleanup_events(&self, target: &str, report: &CleanupReport) {
        if report.oom_killed {
            self.publish_runtime_event(
                core_v1::RuntimeEventType::OomKilled,
                target,
                "OOM_KILLED",
                "sandbox telemetry recorded an OOM kill during cleanup",
            );
        }
        if report.complete {
            self.publish_runtime_event(
                core_v1::RuntimeEventType::CleanupCompleted,
                target,
                &report.reason_code,
                "worker process tree was reaped and its sandbox cleanup completed",
            );
        }
    }

    fn operation_failure(
        &self,
        name: String,
        target: String,
        error: &ProviderError,
    ) -> core_v1::Operation {
        let timestamp = now_timestamp();
        self.remember_operation(core_v1::Operation {
            name,
            state: core_v1::OperationState::Failed as i32,
            target_resource_name: target,
            cancellable: false,
            created_at: Some(timestamp.clone()),
            updated_at: Some(timestamp),
            outcome: Some(core_v1::operation::Outcome::Error(
                cy_proto::google::rpc::Status {
                    code: 9,
                    message: format!("{}: {}", error.reason_code, error.message),
                    details: Vec::new(),
                },
            )),
        })
    }

    fn release_owned_lease(&self, owned_lease: bool, lease: &ResourceLease) {
        if owned_lease {
            let _ = self.daemon.release(&lease.name, lease.fence_token);
        }
    }

    fn heartbeat_response(
        &self,
        disposition: core_v1::HeartbeatDisposition,
        sequence: u64,
        generation: u64,
    ) -> core_v1::ReportHeartbeatResponse {
        core_v1::ReportHeartbeatResponse {
            disposition: disposition as i32,
            accepted_sequence_number: sequence,
            server_time: Some(now_timestamp()),
            next_heartbeat_after: Some(prost_types::Duration {
                seconds: self.heartbeat.interval.as_secs() as i64,
                nanos: self.heartbeat.interval.subsec_nanos() as i32,
            }),
            desired_state: core_v1::DesiredPluginState::Running as i32,
            desired_generation: generation,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn accept_heartbeat(
        &self,
        plugin_instance_name: &str,
        generation: u64,
        sequence_number: u64,
        observed_at: Option<prost_types::Timestamp>,
        runtime_state: i32,
        health: Option<core_v1::HealthReport>,
        restart_count: u32,
    ) -> Result<core_v1::ReportHeartbeatResponse, Status> {
        if plugin_instance_name.is_empty() || sequence_number == 0 {
            return Err(Status::invalid_argument(
                "plugin_instance_name and a non-zero sequence_number are required",
            ));
        }
        let mut instances = self.instances.lock().expect("instance lock poisoned");
        let Some(process) = instances.get_mut(plugin_instance_name) else {
            return Ok(self.heartbeat_response(
                core_v1::HeartbeatDisposition::UnknownInstance,
                0,
                generation,
            ));
        };
        if generation != process.generation {
            return Ok(self.heartbeat_response(
                core_v1::HeartbeatDisposition::StaleGeneration,
                process.accepted_sequence,
                process.generation,
            ));
        }
        if process.watchdog_triggered {
            return Ok(core_v1::ReportHeartbeatResponse {
                disposition: core_v1::HeartbeatDisposition::Duplicate as i32,
                accepted_sequence_number: process.accepted_sequence,
                server_time: Some(now_timestamp()),
                next_heartbeat_after: None,
                desired_state: core_v1::DesiredPluginState::Stopped as i32,
                desired_generation: process.generation,
            });
        }
        if sequence_number <= process.accepted_sequence {
            return Ok(self.heartbeat_response(
                core_v1::HeartbeatDisposition::Duplicate,
                process.accepted_sequence,
                process.generation,
            ));
        }
        process.accepted_sequence = sequence_number;
        process.last_heartbeat = Instant::now();
        process.last_heartbeat_at = observed_at.or_else(|| Some(now_timestamp()));
        process.runtime_state = runtime_state;
        process.health = health;
        process.restart_count = restart_count;
        Ok(self.heartbeat_response(
            core_v1::HeartbeatDisposition::Accepted,
            process.accepted_sequence,
            process.generation,
        ))
    }

    fn register_worker_control(
        &self,
        hello: &core_v1::WorkerHello,
        outbound: WorkerControlSender,
    ) -> Result<(u64, core_v1::WorkerWelcome), Status> {
        if hello.plugin_instance_name.is_empty()
            || hello.generation == 0
            || hello.protocol_version != 1
        {
            return Err(Status::invalid_argument(
                "WorkerHello requires instance name, non-zero generation, and protocol_version=1",
            ));
        }
        let connection_id = self.next_control_connection.fetch_add(1, Ordering::Relaxed);
        let mut instances = self.instances.lock().expect("instance lock poisoned");
        let process = instances
            .get_mut(&hello.plugin_instance_name)
            .ok_or_else(|| Status::not_found("worker instance is not managed by this Kernel"))?;
        if process.generation != hello.generation {
            return Err(Status::failed_precondition("worker generation is stale"));
        }
        process.control = Some(WorkerControlSession {
            connection_id,
            outbound,
        });
        Ok((
            connection_id,
            core_v1::WorkerWelcome {
                desired_state: if process.watchdog_triggered {
                    core_v1::DesiredPluginState::Stopped as i32
                } else {
                    core_v1::DesiredPluginState::Running as i32
                },
                desired_generation: process.generation,
                next_heartbeat_after: Some(to_proto_duration(self.heartbeat.interval)),
            },
        ))
    }

    fn unregister_worker_control(&self, instance_name: &str, generation: u64, connection_id: u64) {
        let mut instances = self.instances.lock().expect("instance lock poisoned");
        let Some(process) = instances.get_mut(instance_name) else {
            return;
        };
        if process.generation == generation
            && process
                .control
                .as_ref()
                .is_some_and(|control| control.connection_id == connection_id)
        {
            process.control = None;
        }
    }

    fn accept_shutdown_ack(&self, ack: &core_v1::WorkerShutdownAck) -> Result<(), Status> {
        let mut instances = self.instances.lock().expect("instance lock poisoned");
        let process = instances
            .get_mut(&ack.plugin_instance_name)
            .ok_or_else(|| Status::not_found("worker instance is not managed by this Kernel"))?;
        if ack.generation != process.generation {
            return Err(Status::failed_precondition("worker generation is stale"));
        }
        let pending = process.pending_shutdown.as_mut().ok_or_else(|| {
            Status::failed_precondition("Kernel did not request a worker shutdown")
        })?;
        if pending.shutdown_id != ack.shutdown_id {
            return Err(Status::failed_precondition(
                "worker shutdown acknowledgement is stale",
            ));
        }
        pending.acknowledged = true;
        pending.drained = ack.drained;
        Ok(())
    }

    fn request_worker_shutdown(
        &self,
        instance_name: &str,
        reason_code: &str,
        immediate: bool,
    ) -> bool {
        if immediate {
            return false;
        }
        let shutdown_id = format!(
            "shutdown-{}",
            self.next_control_connection.fetch_add(1, Ordering::Relaxed)
        );
        let outbound = {
            let mut instances = self.instances.lock().expect("instance lock poisoned");
            let Some(process) = instances.get_mut(instance_name) else {
                return false;
            };
            let Some(control) = process.control.as_ref() else {
                return false;
            };
            process.pending_shutdown = Some(PendingWorkerShutdown {
                shutdown_id: shutdown_id.clone(),
                acknowledged: false,
                drained: false,
            });
            control.outbound.clone()
        };
        let sent = outbound.try_send(Ok(core_v1::KernelToWorker {
            body: Some(core_v1::kernel_to_worker::Body::Shutdown(
                core_v1::WorkerShutdown {
                    shutdown_id: shutdown_id.clone(),
                    mode: core_v1::StopMode::Graceful as i32,
                    ack_deadline: Some(to_proto_duration(self.heartbeat.shutdown_ack_timeout)),
                    reason_code: reason_code.to_string(),
                },
            )),
        }));
        if sent.is_err() {
            self.unregister_pending_shutdown(instance_name, &shutdown_id);
            return false;
        }
        let deadline = Instant::now() + self.heartbeat.shutdown_ack_timeout;
        loop {
            let acknowledged = self
                .instances
                .lock()
                .expect("instance lock poisoned")
                .get(instance_name)
                .and_then(|process| process.pending_shutdown.as_ref())
                .is_some_and(|pending| pending.shutdown_id == shutdown_id && pending.acknowledged);
            if acknowledged || Instant::now() >= deadline {
                return acknowledged;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn unregister_pending_shutdown(&self, instance_name: &str, shutdown_id: &str) {
        if let Some(process) = self
            .instances
            .lock()
            .expect("instance lock poisoned")
            .get_mut(instance_name)
        {
            if process
                .pending_shutdown
                .as_ref()
                .is_some_and(|pending| pending.shutdown_id == shutdown_id)
            {
                process.pending_shutdown = None;
            }
        }
    }

    fn enforce_heartbeat_deadlines(&self) {
        let overdue = {
            let instances = self.instances.lock().expect("instance lock poisoned");
            instances
                .iter()
                .filter(|(_, process)| {
                    !process.watchdog_triggered
                        && process.last_heartbeat.elapsed() > self.heartbeat.timeout
                })
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>()
        };
        for name in overdue {
            let _acknowledged = self.request_worker_shutdown(&name, "HEARTBEAT_TIMEOUT", false);
            self.publish_runtime_event(
                core_v1::RuntimeEventType::WatchdogTriggered,
                &name,
                "HEARTBEAT_TIMEOUT",
                "worker missed its mandatory heartbeat deadline",
            );
            let (lease, report) = {
                let mut instances = self.instances.lock().expect("instance lock poisoned");
                let Some(process) = instances.get_mut(&name) else {
                    continue;
                };
                process.watchdog_triggered = true;
                let lease = process.lease.clone();
                match process.instance.stop(&cy_kernel_api::StopRequest {
                    grace_period: self.heartbeat.graceful_stop,
                    immediate: false,
                }) {
                    Ok(report) => (lease, Some(report.clone())),
                    Err(_) => (lease, None),
                }
            };
            if let Some(report) = report.as_ref() {
                self.publish_cleanup_events(&name, report);
                if report.complete {
                    if let Some(lease) = lease.as_ref() {
                        if self
                            .daemon
                            .release(&lease.lease_name, lease.fence_token)
                            .is_ok()
                        {
                            self.record_runtime(
                                RuntimeJournalEvent::WatchdogReaped,
                                Some(&name),
                                Some(lease),
                                "HEARTBEAT_TIMEOUT_REAPED",
                            );
                            self.instances
                                .lock()
                                .expect("instance lock poisoned")
                                .remove(&name);
                        }
                    }
                } else {
                    self.record_runtime(
                        RuntimeJournalEvent::InstanceCleanupFailed,
                        Some(&name),
                        lease.as_ref(),
                        &report.reason_code,
                    );
                }
            } else {
                self.record_runtime(
                    RuntimeJournalEvent::InstanceCleanupFailed,
                    Some(&name),
                    lease.as_ref(),
                    "WATCHDOG_STOP_FAILED",
                );
            }
        }
    }
}

#[tonic::async_trait]
impl core_v1::kernel_service_server::KernelService for KernelServiceAdapter {
    async fn get_kernel_capabilities(
        &self,
        request: Request<core_v1::GetKernelCapabilitiesRequest>,
    ) -> Result<Response<core_v1::KernelCapabilities>, Status> {
        let request = request.into_inner();
        self.validate_node(request.node.as_ref())?;
        self.daemon
            .get_kernel_capabilities()
            .map(Response::new)
            .map_err(provider_status)
    }

    async fn acquire_lease(
        &self,
        request: Request<core_v1::AcquireLeaseRequest>,
    ) -> Result<Response<semantic_v1::Lease>, Status> {
        let request = request.into_inner();
        self.validate_node(request.node.as_ref())?;
        let lease_name = self.lease_name(request.mutation.as_ref())?;
        let generation = request
            .mutation
            .as_ref()
            .and_then(|mutation| mutation.expected_generation)
            .unwrap_or_else(|| self.daemon.resources.inventory().generation);
        let holder = semantic_identity_from_proto(request.holder, "holder")?;
        let query = semantic_query_from_proto(
            request
                .query
                .ok_or_else(|| Status::invalid_argument("resource query is required"))?,
        )?;
        let ttl = proto_duration(
            request
                .ttl
                .ok_or_else(|| Status::invalid_argument("lease ttl is required"))?,
        )?;
        if ttl.is_zero() {
            return Err(Status::invalid_argument("lease ttl must be positive"));
        }
        let limits = cgroup_limits(request.cpu.as_ref(), request.memory.as_ref())?;
        let lease = self
            .daemon
            .reserve(ResourceRequest {
                lease_name,
                expected_inventory_generation: generation,
                holder,
                query,
                expires_at_unix_ms: Some(expires_after(ttl)),
                limits,
            })
            .map_err(provider_status)?;
        let journal_lease = core_v1::ResourceLeaseRef {
            lease_name: lease.name.clone(),
            fence_token: lease.fence_token,
        };
        self.record_runtime(
            RuntimeJournalEvent::LeaseReserved,
            None,
            Some(&journal_lease),
            "LEASE_RESERVED",
        );
        Ok(Response::new(to_semantic_proto_lease(&lease)))
    }

    async fn release_lease(
        &self,
        request: Request<core_v1::ReleaseLeaseRequest>,
    ) -> Result<Response<semantic_v1::Lease>, Status> {
        let request = request.into_inner();
        let lease_identity = semantic_identity_from_proto(request.lease, "lease")?;
        let current = self
            .daemon
            .lease(&lease_identity.id)
            .map_err(provider_status)?;
        if current.generation != lease_identity.generation {
            return Err(Status::failed_precondition("stale lease generation"));
        }
        self.daemon
            .release(&lease_identity.id, request.fence_token)
            .map_err(provider_status)?;
        let journal_lease = core_v1::ResourceLeaseRef {
            lease_name: lease_identity.id.clone(),
            fence_token: request.fence_token,
        };
        self.record_runtime(
            RuntimeJournalEvent::LeaseReleased,
            None,
            Some(&journal_lease),
            "LEASE_RELEASED",
        );
        let lease = self
            .daemon
            .lease(&lease_identity.id)
            .map_err(provider_status)?;
        Ok(Response::new(to_semantic_proto_lease(&lease)))
    }

    async fn reserve_resources(
        &self,
        request: Request<core_v1::ReserveResourcesRequest>,
    ) -> Result<Response<core_v1::ResourceLease>, Status> {
        let request = request.into_inner();
        self.validate_node(request.node.as_ref())?;
        let requirements = request
            .requirements
            .ok_or_else(|| Status::invalid_argument("resource requirements are required"))?;
        let lease_name = self.lease_name(request.mutation.as_ref())?;
        let generation = request
            .mutation
            .as_ref()
            .and_then(|mutation| mutation.expected_generation)
            .unwrap_or_else(|| self.daemon.resources.inventory().generation);
        let holder = legacy_holder(request.mutation.as_ref(), &lease_name);
        let expires_at_unix_ms = request
            .ttl
            .map(proto_duration)
            .transpose()?
            .map(expires_after);
        let internal = resource_request(
            &lease_name,
            generation,
            holder,
            expires_at_unix_ms,
            &requirements,
        )?;
        let lease = self.daemon.reserve(internal).map_err(provider_status)?;
        let journal_lease = core_v1::ResourceLeaseRef {
            lease_name: lease.name.clone(),
            fence_token: lease.fence_token,
        };
        self.record_runtime(
            RuntimeJournalEvent::LeaseReserved,
            None,
            Some(&journal_lease),
            "LEASE_RESERVED",
        );
        Ok(Response::new(to_proto_lease(
            &self.daemon,
            lease,
            Some(requirements),
        )?))
    }

    async fn release_resources(
        &self,
        request: Request<core_v1::ReleaseResourcesRequest>,
    ) -> Result<Response<core_v1::ResourceLease>, Status> {
        let request = request.into_inner();
        let lease = request
            .lease
            .ok_or_else(|| Status::invalid_argument("lease reference is required"))?;
        self.daemon
            .release(&lease.lease_name, lease.fence_token)
            .map_err(provider_status)?;
        self.record_runtime(
            RuntimeJournalEvent::LeaseReleased,
            None,
            Some(&lease),
            "LEASE_RELEASED",
        );
        let lease = self
            .daemon
            .lease(&lease.lease_name)
            .map_err(provider_status)?;
        Ok(Response::new(to_proto_lease(&self.daemon, lease, None)?))
    }

    async fn launch_plugin(
        &self,
        request: Request<core_v1::LaunchPluginRequest>,
    ) -> Result<Response<core_v1::Operation>, Status> {
        let request = request.into_inner();
        self.validate_node(request.node.as_ref())?;
        let plugin = request
            .plugin
            .ok_or_else(|| Status::invalid_argument("installed plugin reference is required"))?;
        if plugin.installation_name.is_empty()
            || plugin.manifest_digest.is_empty()
            || plugin.artifact_digest.is_empty()
            || plugin.verified_signature_identity.is_empty()
        {
            return Err(Status::failed_precondition(
                "LaunchPlugin accepts only a verified InstalledPluginRef",
            ));
        }
        let instance_name = plugin.installation_name.clone();
        let operation_name = self.operation_name("launch", &instance_name);
        if self
            .instances
            .lock()
            .expect("instance lock poisoned")
            .contains_key(&instance_name)
        {
            return Err(Status::already_exists(
                "plugin instance is already managed by this Kernel",
            ));
        }
        let (lease, owned_lease) = match request.allocation {
            Some(core_v1::launch_plugin_request::Allocation::ExistingLease(lease_ref)) => (
                self.daemon
                    .lease(&lease_ref.lease_name)
                    .map_err(provider_status)?,
                false,
            ),
            Some(core_v1::launch_plugin_request::Allocation::ResourceClaim(requirements)) => {
                let lease_name = self.lease_name(request.mutation.as_ref())?;
                let generation = request
                    .mutation
                    .as_ref()
                    .and_then(|mutation| mutation.expected_generation)
                    .unwrap_or_else(|| self.daemon.resources.inventory().generation);
                let internal = resource_request(
                    &lease_name,
                    generation,
                    legacy_holder(request.mutation.as_ref(), &instance_name),
                    None,
                    &requirements,
                )?;
                (
                    self.daemon.reserve(internal).map_err(provider_status)?,
                    true,
                )
            }
            None => return Err(Status::invalid_argument("allocation is required")),
        };
        let binding = match self.daemon.binding_for_lease(&lease) {
            Ok(binding) => binding,
            Err(error) => {
                self.release_owned_lease(owned_lease, &lease);
                return Err(provider_status(error));
            }
        };
        let installation = VerifiedInstallation {
            installation_name: plugin.installation_name.clone(),
            manifest_digest: plugin.manifest_digest.clone(),
            artifact_digest: plugin.artifact_digest.clone(),
            verified_signature_identity: plugin.verified_signature_identity.clone(),
        };
        let resolved = match self
            .resolver
            .resolve_launch_plan(&installation, &instance_name)
        {
            Ok(plan) => plan,
            Err(error) => {
                self.release_owned_lease(owned_lease, &lease);
                return Err(provider_status(error));
            }
        };
        if resolved.installation != installation {
            self.release_owned_lease(owned_lease, &lease);
            return Err(Status::failed_precondition(
                "resolver returned a launch plan for another verified installation",
            ));
        }
        let mut plan = resolved.plan;
        plan.limits = lease.limits.clone();
        plan.environment = inject_heartbeat_environment(
            plan.environment,
            &self.heartbeat,
            &instance_name,
            lease.fence_token,
        )
        .map_err(|error| {
            self.release_owned_lease(owned_lease, &lease);
            provider_status(error)
        })?;
        plan.environment = binding
            .merge_environment(&plan.environment)
            .map_err(|error| {
                self.release_owned_lease(owned_lease, &lease);
                provider_status(error)
            })?;
        let mut instance = SandboxedProcess::new(self.daemon.sandbox.clone(), plan, binding);
        if let Err(error) = instance.start() {
            self.release_owned_lease(owned_lease, &lease);
            return Err(provider_status(error));
        }
        let lease_ref = core_v1::ResourceLeaseRef {
            lease_name: lease.name,
            fence_token: lease.fence_token,
        };
        self.instances
            .lock()
            .expect("instance lock poisoned")
            .insert(
                instance_name.clone(),
                ManagedProcess {
                    instance,
                    lease: Some(lease_ref.clone()),
                    semantic_worker: None,
                    plugin,
                    generation: lease_ref.fence_token,
                    accepted_sequence: 0,
                    last_heartbeat: Instant::now(),
                    last_heartbeat_at: None,
                    runtime_state: core_v1::PluginRuntimeState::Starting as i32,
                    health: None,
                    restart_count: 0,
                    watchdog_triggered: false,
                    control: None,
                    pending_shutdown: None,
                },
            );
        self.record_runtime(
            RuntimeJournalEvent::InstanceLaunched,
            Some(&instance_name),
            Some(&lease_ref),
            "WORKER_LAUNCHED",
        );
        self.publish_runtime_event(
            core_v1::RuntimeEventType::InstanceStateChanged,
            &instance_name,
            "WORKER_LAUNCHED",
            "worker process started inside its assigned sandbox",
        );
        Ok(Response::new(
            self.operation_running(operation_name, instance_name),
        ))
    }

    async fn terminate_plugin(
        &self,
        request: Request<core_v1::TerminatePluginRequest>,
    ) -> Result<Response<core_v1::Operation>, Status> {
        let request = request.into_inner();
        if request.process_name.is_empty() {
            return Err(Status::invalid_argument("process_name is required"));
        }
        let operation_name = self.operation_name("terminate", &request.process_name);
        let grace_period = request
            .grace_period
            .map(proto_duration)
            .transpose()?
            .unwrap_or(Duration::from_secs(30));
        let immediate = request.mode == core_v1::StopMode::Immediate as i32;
        let _acknowledged =
            self.request_worker_shutdown(&request.process_name, "TERMINATE_REQUESTED", immediate);
        let (lease, report) = {
            let mut instances = self.instances.lock().expect("instance lock poisoned");
            let process = instances
                .get_mut(&request.process_name)
                .ok_or_else(|| Status::not_found("plugin process is not managed by this Kernel"))?;
            let report = process
                .instance
                .stop(&cy_kernel_api::StopRequest {
                    grace_period,
                    immediate,
                })
                .map_err(provider_status)?
                .clone();
            (process.lease.clone(), report)
        };
        self.publish_cleanup_events(&request.process_name, &report);
        if !report.complete {
            self.record_runtime(
                RuntimeJournalEvent::InstanceCleanupFailed,
                Some(&request.process_name),
                lease.as_ref(),
                &report.reason_code,
            );
            let error =
                ProviderError::new("kernel-daemon", "RESOURCE_QUARANTINED", &report.reason_code);
            return Ok(Response::new(self.operation_failure(
                operation_name,
                request.process_name,
                &error,
            )));
        }
        if let Some(lease) = lease.as_ref() {
            self.daemon
                .release(&lease.lease_name, lease.fence_token)
                .map_err(provider_status)?;
        }
        self.record_runtime(
            RuntimeJournalEvent::InstanceTerminated,
            Some(&request.process_name),
            lease.as_ref(),
            "TERMINATE_COMPLETE",
        );
        self.instances
            .lock()
            .expect("instance lock poisoned")
            .remove(&request.process_name);
        Ok(Response::new(
            self.operation_success(operation_name, request.process_name),
        ))
    }

    async fn get_operation(
        &self,
        request: Request<core_v1::GetOperationRequest>,
    ) -> Result<Response<core_v1::Operation>, Status> {
        let name = request.into_inner().name;
        self.operations
            .lock()
            .expect("operation lock poisoned")
            .get(&name)
            .cloned()
            .map(Response::new)
            .ok_or_else(|| Status::not_found("operation not found"))
    }

    async fn cancel_operation(
        &self,
        request: Request<core_v1::CancelOperationRequest>,
    ) -> Result<Response<core_v1::Operation>, Status> {
        let name = request.into_inner().name;
        if name.is_empty() {
            return Err(Status::invalid_argument("operation name is required"));
        }
        let operation = self
            .operations
            .lock()
            .expect("operation lock poisoned")
            .get(&name)
            .cloned()
            .ok_or_else(|| Status::not_found("operation not found"))?;
        if operation.state != core_v1::OperationState::Running as i32 || !operation.cancellable {
            return Err(Status::failed_precondition("operation is not cancellable"));
        }
        let target = operation.target_resource_name;
        let _acknowledged = self.request_worker_shutdown(&target, "OPERATION_CANCELLED", false);
        let (lease, report) = {
            let mut instances = self.instances.lock().expect("instance lock poisoned");
            let process = instances.get_mut(&target).ok_or_else(|| {
                Status::failed_precondition("operation target is no longer managed")
            })?;
            let report = process
                .instance
                .stop(&cy_kernel_api::StopRequest {
                    grace_period: self.heartbeat.graceful_stop,
                    immediate: false,
                })
                .map_err(provider_status)?
                .clone();
            (process.lease.clone(), report)
        };
        self.publish_cleanup_events(&target, &report);
        if !report.complete {
            self.record_runtime(
                RuntimeJournalEvent::InstanceCleanupFailed,
                Some(&target),
                lease.as_ref(),
                &report.reason_code,
            );
            let error =
                ProviderError::new("kernel-daemon", "RESOURCE_QUARANTINED", &report.reason_code);
            return Ok(Response::new(self.operation_failure(name, target, &error)));
        }
        if let Some(lease) = lease.as_ref() {
            self.daemon
                .release(&lease.lease_name, lease.fence_token)
                .map_err(provider_status)?;
        }
        self.record_runtime(
            RuntimeJournalEvent::InstanceTerminated,
            Some(&target),
            lease.as_ref(),
            "CANCEL_COMPLETE",
        );
        self.instances
            .lock()
            .expect("instance lock poisoned")
            .remove(&target);
        Ok(Response::new(self.operation_cancelled(name, target)))
    }

    type WatchOperationsStream = std::pin::Pin<
        Box<dyn Stream<Item = Result<core_v1::OperationEvent, Status>> + Send + 'static>,
    >;

    async fn watch_operations(
        &self,
        request: Request<core_v1::WatchOperationsRequest>,
    ) -> Result<Response<Self::WatchOperationsStream>, Status> {
        let request = request.into_inner();
        let resume_sequence = if request.resume_token.is_empty() {
            0
        } else {
            request.resume_token.parse::<u64>().map_err(|_| {
                Status::invalid_argument("resume_token must be an event sequence number")
            })?
        };
        let names = request.operation_names;
        let receiver = self.operation_event_sender.subscribe();
        let history = self
            .operation_events
            .lock()
            .expect("operation event history lock poisoned")
            .iter()
            .filter(|event| event.sequence_number > resume_sequence)
            .filter(|event| operation_event_matches(event, &names))
            .cloned()
            .collect::<Vec<_>>();
        let last_sequence = history
            .last()
            .map(|event| event.sequence_number)
            .unwrap_or(resume_sequence);
        let (sender, stream) = mpsc::channel(OPERATION_EVENT_SUBSCRIBER_CAPACITY);
        tokio::spawn(async move {
            let mut receiver = receiver;
            let mut cursor = last_sequence;
            for event in history {
                if sender.send(Ok(event)).await.is_err() {
                    return;
                }
            }
            loop {
                match receiver.recv().await {
                    Ok(event) if event.sequence_number <= cursor => continue,
                    Ok(event) => {
                        cursor = event.sequence_number;
                        if operation_event_matches(&event, &names)
                            && sender.send(Ok(event)).await.is_err()
                        {
                            return;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        let _ = sender
                            .send(Err(Status::out_of_range(
                                "operation event subscriber lagged beyond the bounded buffer",
                            )))
                            .await;
                        return;
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(stream))))
    }
}

#[tonic::async_trait]
impl core_v1::kernel_authority_service_server::KernelAuthorityService for KernelServiceAdapter {
    async fn negotiate(
        &self,
        request: Request<core_v1::NegotiateRequest>,
    ) -> Result<Response<semantic_v1::ContractRevision>, Status> {
        let local = semantic::ContractRevision::current();
        let selected = request
            .into_inner()
            .offered
            .into_iter()
            .filter_map(semantic_contract_revision_from_proto)
            .filter_map(|offered| local.negotiate(&offered))
            .max_by_key(|revision| revision.minor)
            .ok_or_else(|| {
                semantic_status(
                    tonic::Code::FailedPrecondition,
                    "CONTRACT_INCOMPATIBLE",
                    "no offered semantic contract revision is compatible with this Kernel",
                )
            })?;
        Ok(Response::new(to_semantic_proto_contract_revision(
            &selected,
        )))
    }

    async fn acquire_lease(
        &self,
        request: Request<core_v1::AcquireSemanticLeaseRequest>,
    ) -> Result<Response<semantic_v1::Lease>, Status> {
        let request = request.into_inner();
        let context = validate_authority_context(request.context.as_ref())?;
        let holder = semantic_identity_from_proto(request.holder, "holder")?;
        let query = semantic_query_from_proto(
            request
                .query
                .ok_or_else(|| Status::invalid_argument("resource query is required"))?,
        )?;
        let ttl = proto_duration(
            request
                .ttl
                .ok_or_else(|| Status::invalid_argument("lease ttl is required"))?,
        )?;
        if ttl.is_zero() {
            return Err(Status::invalid_argument("lease ttl must be positive"));
        }
        let lease = self
            .daemon
            .reserve(ResourceRequest {
                lease_name: authority_lease_name(context),
                expected_inventory_generation: self.daemon.resources.inventory().generation,
                holder,
                query,
                expires_at_unix_ms: Some(expires_after(ttl)),
                limits: CgroupLimits::default(),
            })
            .map_err(provider_status)?;
        let journal_lease = core_v1::ResourceLeaseRef {
            lease_name: lease.name.clone(),
            fence_token: lease.fence_token,
        };
        self.record_runtime(
            RuntimeJournalEvent::LeaseReserved,
            None,
            Some(&journal_lease),
            "LEASE_RESERVED",
        );
        Ok(Response::new(to_semantic_proto_lease(&lease)))
    }

    async fn renew_lease(
        &self,
        request: Request<core_v1::RenewLeaseRequest>,
    ) -> Result<Response<semantic_v1::Lease>, Status> {
        let request = request.into_inner();
        validate_authority_context(request.context.as_ref())?;
        let lease_identity = semantic_identity_from_proto(request.lease, "lease")?;
        let ttl = proto_duration(
            request
                .ttl
                .ok_or_else(|| Status::invalid_argument("lease renewal ttl is required"))?,
        )?;
        if ttl.is_zero() {
            return Err(Status::invalid_argument(
                "lease renewal ttl must be positive",
            ));
        }
        let current = self
            .daemon
            .lease(&lease_identity.id)
            .map_err(provider_status)?;
        if current.generation != lease_identity.generation {
            return Err(Status::failed_precondition("stale lease generation"));
        }
        let lease = self
            .daemon
            .renew(&lease_identity.id, request.fence_token, expires_after(ttl))
            .map_err(provider_status)?;
        Ok(Response::new(to_semantic_proto_lease(&lease)))
    }

    async fn release_lease(
        &self,
        request: Request<core_v1::ReleaseSemanticLeaseRequest>,
    ) -> Result<Response<semantic_v1::Lease>, Status> {
        let request = request.into_inner();
        validate_authority_context(request.context.as_ref())?;
        let lease_identity = semantic_identity_from_proto(request.lease, "lease")?;
        let current = self
            .daemon
            .lease(&lease_identity.id)
            .map_err(provider_status)?;
        if current.generation != lease_identity.generation {
            return Err(semantic_status(
                tonic::Code::FailedPrecondition,
                "LEASE_GENERATION_STALE",
                "lease generation no longer has authority",
            ));
        }
        self.daemon
            .release(&lease_identity.id, request.fence_token)
            .map_err(provider_status)?;
        let journal_lease = core_v1::ResourceLeaseRef {
            lease_name: lease_identity.id.clone(),
            fence_token: request.fence_token,
        };
        self.record_runtime(
            RuntimeJournalEvent::LeaseReleased,
            None,
            Some(&journal_lease),
            "LEASE_RELEASED",
        );
        let lease = self
            .daemon
            .lease(&lease_identity.id)
            .map_err(provider_status)?;
        Ok(Response::new(to_semantic_proto_lease(&lease)))
    }

    async fn start_worker(
        &self,
        request: Request<core_v1::StartWorkerRequest>,
    ) -> Result<Response<semantic_v1::Operation>, Status> {
        let request = request.into_inner();
        validate_authority_context(request.context.as_ref())?;
        let mut worker = semantic_worker_from_proto(
            request
                .worker
                .ok_or_else(|| Status::invalid_argument("worker is required"))?,
        )?;
        if !matches!(
            worker.state,
            semantic::WorkerState::Registered | semantic::WorkerState::Starting
        ) {
            return Err(semantic_status(
                tonic::Code::FailedPrecondition,
                "STATE_TRANSITION_INVALID",
                "StartWorker requires REGISTERED or STARTING state",
            ));
        }
        if self
            .instances
            .lock()
            .expect("instance lock poisoned")
            .contains_key(&worker.identity.id)
        {
            return Err(semantic_status(
                tonic::Code::AlreadyExists,
                "WORKER_EXISTS",
                "worker identity is already managed by this Kernel",
            ));
        }
        let lease = self
            .daemon
            .lease(&worker.lease.id)
            .map_err(provider_status)?;
        if lease.generation != worker.lease.generation
            || lease.holder != worker.identity
            || lease.state != LeaseState::Active
        {
            return Err(semantic_status(
                tonic::Code::FailedPrecondition,
                "LEASE_NOT_ACTIVE",
                "worker does not hold the referenced active Lease incarnation",
            ));
        }
        let binding = self
            .daemon
            .binding_for_lease(&lease)
            .map_err(provider_status)?;
        let resolved = self
            .resolver
            .resolve_worker_launch_plan(&worker)
            .map_err(provider_status)?;
        let mut plan = resolved.plan;
        if plan.instance_name != worker.identity.id {
            return Err(semantic_status(
                tonic::Code::FailedPrecondition,
                "EXECUTION_REFERENCE_MISMATCH",
                "resolver returned a plan for another Worker identity",
            ));
        }
        plan.limits = lease.limits.clone();
        plan.environment = inject_heartbeat_environment(
            plan.environment,
            &self.heartbeat,
            &worker.identity.id,
            lease.fence_token,
        )
        .map_err(provider_status)?;
        plan.environment = binding
            .merge_environment(&plan.environment)
            .map_err(provider_status)?;
        let mut instance = SandboxedProcess::new(self.daemon.sandbox.clone(), plan, binding);
        instance.start().map_err(provider_status)?;
        worker.state = semantic::WorkerState::Starting;
        let lease_ref = core_v1::ResourceLeaseRef {
            lease_name: lease.name.clone(),
            fence_token: lease.fence_token,
        };
        self.instances
            .lock()
            .expect("instance lock poisoned")
            .insert(
                worker.identity.id.clone(),
                ManagedProcess {
                    instance,
                    lease: Some(lease_ref.clone()),
                    semantic_worker: Some(worker.clone()),
                    plugin: core_v1::InstalledPluginRef::default(),
                    generation: lease.fence_token,
                    accepted_sequence: 0,
                    last_heartbeat: Instant::now(),
                    last_heartbeat_at: None,
                    runtime_state: core_v1::PluginRuntimeState::Starting as i32,
                    health: None,
                    restart_count: 0,
                    watchdog_triggered: false,
                    control: None,
                    pending_shutdown: None,
                },
            );
        self.record_runtime(
            RuntimeJournalEvent::InstanceLaunched,
            Some(&worker.identity.id),
            Some(&lease_ref),
            "WORKER_LAUNCHED",
        );
        self.publish_semantic_event(
            worker.identity.clone(),
            "worker.starting",
            "cyrene.worker.v1",
            Vec::new(),
        );
        let operation = semantic::Operation {
            identity: semantic::Identity {
                id: format!("operation/start/{}", worker.identity.id),
                generation: worker.identity.generation,
            },
            owner: worker.principal.clone(),
            executor: worker.provider.clone(),
            kind: "worker.start".to_string(),
            state: semantic::OperationState::Running,
            deadline_unix_ms: None,
            parent: None,
            metadata: BTreeMap::from([("worker.id".to_string(), worker.identity.id.clone())]),
        };
        Ok(Response::new(to_semantic_proto_operation(
            &self.remember_semantic_operation(operation),
        )))
    }

    async fn heartbeat_worker(
        &self,
        request: Request<core_v1::HeartbeatWorkerRequest>,
    ) -> Result<Response<semantic_v1::Worker>, Status> {
        let request = request.into_inner();
        validate_authority_context(request.context.as_ref())?;
        let worker_identity = semantic_identity_from_proto(request.worker, "worker")?;
        let lease_identity = semantic_identity_from_proto(request.lease, "lease")?;
        let lease = self
            .daemon
            .lease(&lease_identity.id)
            .map_err(provider_status)?;
        if lease.generation != lease_identity.generation
            || lease.fence_token != request.fence_token
            || lease.holder != worker_identity
            || lease.state != LeaseState::Active
        {
            return Err(semantic_status(
                tonic::Code::FailedPrecondition,
                "FENCE_MISMATCH",
                "worker heartbeat no longer has active lease authority",
            ));
        }
        let mut instances = self.instances.lock().expect("instance lock poisoned");
        let process = instances.get_mut(&worker_identity.id).ok_or_else(|| {
            semantic_status(
                tonic::Code::NotFound,
                "WORKER_NOT_FOUND",
                "worker is not managed by this Kernel",
            )
        })?;
        let worker = process.semantic_worker.as_mut().ok_or_else(|| {
            semantic_status(
                tonic::Code::FailedPrecondition,
                "WORKER_COMPATIBILITY_ONLY",
                "legacy plugin process is not a semantic Worker",
            )
        })?;
        if worker.identity != worker_identity || worker.lease != lease_identity {
            return Err(semantic_status(
                tonic::Code::FailedPrecondition,
                "STALE_GENERATION",
                "worker identity or lease generation is stale",
            ));
        }
        if !worker
            .state
            .can_transition_to(semantic::WorkerState::Running)
        {
            return Err(semantic_status(
                tonic::Code::FailedPrecondition,
                "STATE_TRANSITION_INVALID",
                "worker cannot enter RUNNING from its current state",
            ));
        }
        worker.state = semantic::WorkerState::Running;
        process.last_heartbeat = Instant::now();
        process.last_heartbeat_at = Some(now_timestamp());
        let response = worker.clone();
        drop(instances);
        self.publish_semantic_event(
            response.identity.clone(),
            "worker.running",
            "cyrene.worker.v1",
            Vec::new(),
        );
        Ok(Response::new(to_semantic_proto_worker(&response)))
    }

    async fn stop_worker(
        &self,
        request: Request<core_v1::StopWorkerRequest>,
    ) -> Result<Response<semantic_v1::Operation>, Status> {
        let request = request.into_inner();
        validate_authority_context(request.context.as_ref())?;
        let worker_identity = semantic_identity_from_proto(request.worker, "worker")?;
        let lease_identity = semantic_identity_from_proto(request.lease, "lease")?;
        let lease = self
            .daemon
            .lease(&lease_identity.id)
            .map_err(provider_status)?;
        if lease.generation != lease_identity.generation
            || lease.fence_token != request.fence_token
            || lease.holder != worker_identity
            || lease.state != LeaseState::Active
        {
            return Err(semantic_status(
                tonic::Code::FailedPrecondition,
                "FENCE_MISMATCH",
                "worker stop no longer has active lease authority",
            ));
        }
        let grace_period = request
            .grace_period
            .map(proto_duration)
            .transpose()?
            .unwrap_or(self.heartbeat.graceful_stop);
        let _acknowledged =
            self.request_worker_shutdown(&worker_identity.id, "STOP_REQUESTED", false);
        let (worker, report) = {
            let mut instances = self.instances.lock().expect("instance lock poisoned");
            let process = instances.get_mut(&worker_identity.id).ok_or_else(|| {
                semantic_status(
                    tonic::Code::NotFound,
                    "WORKER_NOT_FOUND",
                    "worker is not managed by this Kernel",
                )
            })?;
            let worker = process.semantic_worker.as_mut().ok_or_else(|| {
                semantic_status(
                    tonic::Code::FailedPrecondition,
                    "WORKER_COMPATIBILITY_ONLY",
                    "legacy plugin process is not a semantic Worker",
                )
            })?;
            if worker.identity != worker_identity || worker.lease != lease_identity {
                return Err(semantic_status(
                    tonic::Code::FailedPrecondition,
                    "STALE_GENERATION",
                    "worker identity or lease generation is stale",
                ));
            }
            worker.state = semantic::WorkerState::Draining;
            let report = process
                .instance
                .stop(&cy_kernel_api::StopRequest {
                    grace_period,
                    immediate: false,
                })
                .map_err(provider_status)?
                .clone();
            worker.state = if report.complete {
                semantic::WorkerState::Stopped
            } else {
                semantic::WorkerState::Failed
            };
            (worker.clone(), report)
        };
        self.publish_cleanup_events(&worker_identity.id, &report);
        if report.complete {
            self.daemon
                .release(&lease_identity.id, request.fence_token)
                .map_err(provider_status)?;
            self.instances
                .lock()
                .expect("instance lock poisoned")
                .remove(&worker_identity.id);
        }
        self.publish_semantic_event(
            worker.identity.clone(),
            if report.complete {
                "worker.stopped"
            } else {
                "worker.failed"
            },
            "cyrene.worker.v1",
            Vec::new(),
        );
        let operation = semantic::Operation {
            identity: semantic::Identity {
                id: format!("operation/stop/{}", worker.identity.id),
                generation: worker.identity.generation,
            },
            owner: worker.principal,
            executor: worker.provider,
            kind: "worker.stop".to_string(),
            state: if report.complete {
                semantic::OperationState::Succeeded
            } else {
                semantic::OperationState::Failed
            },
            deadline_unix_ms: None,
            parent: None,
            metadata: BTreeMap::from([("worker.id".to_string(), worker.identity.id)]),
        };
        Ok(Response::new(to_semantic_proto_operation(
            &self.remember_semantic_operation(operation),
        )))
    }

    async fn create_operation(
        &self,
        request: Request<core_v1::CreateOperationRequest>,
    ) -> Result<Response<semantic_v1::Operation>, Status> {
        let request = request.into_inner();
        validate_authority_context(request.context.as_ref())?;
        let operation = semantic_operation_from_proto(
            request
                .operation
                .ok_or_else(|| Status::invalid_argument("operation is required"))?,
        )?;
        if operation.state != semantic::OperationState::Created {
            return Err(semantic_status(
                tonic::Code::FailedPrecondition,
                "STATE_TRANSITION_INVALID",
                "CreateOperation requires CREATED state",
            ));
        }
        let key = semantic_identity_key(&operation.identity);
        let mut operations = self
            .semantic_operations
            .lock()
            .expect("semantic operation lock poisoned");
        if let Some(existing) = operations.get(&key) {
            if existing == &operation {
                return Ok(Response::new(to_semantic_proto_operation(existing)));
            }
            return Err(semantic_status(
                tonic::Code::AlreadyExists,
                "OPERATION_EXISTS",
                "an operation with this identity already exists",
            ));
        }
        if operations.values().any(|existing| {
            existing.identity.id == operation.identity.id
                && existing.identity.generation > operation.identity.generation
        }) {
            return Err(semantic_status(
                tonic::Code::FailedPrecondition,
                "STALE_GENERATION",
                "operation identity generation is stale",
            ));
        }
        operations.insert(key, operation.clone());
        drop(operations);
        self.publish_semantic_event(
            operation.identity.clone(),
            "operation.created",
            "cyrene.operation.v1",
            Vec::new(),
        );
        Ok(Response::new(to_semantic_proto_operation(&operation)))
    }

    async fn report_operation(
        &self,
        request: Request<core_v1::ReportOperationRequest>,
    ) -> Result<Response<semantic_v1::Operation>, Status> {
        let request = request.into_inner();
        validate_authority_context(request.context.as_ref())?;
        let reported = semantic_operation_from_proto(
            request
                .operation
                .ok_or_else(|| Status::invalid_argument("operation is required"))?,
        )?;
        let key = semantic_identity_key(&reported.identity);
        let mut operations = self
            .semantic_operations
            .lock()
            .expect("semantic operation lock poisoned");
        let current = operations.get(&key).cloned().ok_or_else(|| {
            semantic_status(
                tonic::Code::NotFound,
                "OPERATION_NOT_FOUND",
                "operation is unknown",
            )
        })?;
        if current.owner != reported.owner
            || current.executor != reported.executor
            || current.kind != reported.kind
            || current.deadline_unix_ms != reported.deadline_unix_ms
            || current.parent != reported.parent
        {
            return Err(semantic_status(
                tonic::Code::FailedPrecondition,
                "OPERATION_IMMUTABLE_FIELDS_CHANGED",
                "operation report attempted to change immutable authority fields",
            ));
        }
        if !current.state.can_transition_to(reported.state) {
            return Err(semantic_status(
                tonic::Code::FailedPrecondition,
                "STATE_TRANSITION_INVALID",
                "operation report is not an idempotent or forward transition",
            ));
        }
        operations.insert(key, reported.clone());
        drop(operations);
        self.publish_semantic_event(
            reported.identity.clone(),
            semantic_operation_event_kind(reported.state),
            "cyrene.operation.v1",
            Vec::new(),
        );
        Ok(Response::new(to_semantic_proto_operation(&reported)))
    }

    async fn cancel_operation(
        &self,
        request: Request<core_v1::CancelSemanticOperationRequest>,
    ) -> Result<Response<semantic_v1::Operation>, Status> {
        let request = request.into_inner();
        validate_authority_context(request.context.as_ref())?;
        let identity = semantic_identity_from_proto(request.operation, "operation")?;
        let key = semantic_identity_key(&identity);
        let mut operations = self
            .semantic_operations
            .lock()
            .expect("semantic operation lock poisoned");
        let current = operations.get(&key).cloned().ok_or_else(|| {
            semantic_status(
                tonic::Code::NotFound,
                "OPERATION_NOT_FOUND",
                "operation is unknown",
            )
        })?;
        let mut cancelled = current.clone();
        if !matches!(
            current.state,
            semantic::OperationState::Cancelling | semantic::OperationState::Cancelled
        ) {
            if !current
                .state
                .can_transition_to(semantic::OperationState::Cancelling)
            {
                return Err(semantic_status(
                    tonic::Code::FailedPrecondition,
                    "STATE_TRANSITION_INVALID",
                    "operation cannot be cancelled from its current state",
                ));
            }
            cancelled.state = semantic::OperationState::Cancelling;
            operations.insert(key, cancelled.clone());
        }
        drop(operations);
        self.publish_semantic_event(
            cancelled.identity.clone(),
            semantic_operation_event_kind(cancelled.state),
            "cyrene.operation.v1",
            Vec::new(),
        );
        Ok(Response::new(to_semantic_proto_operation(&cancelled)))
    }

    async fn publish_endpoint(
        &self,
        request: Request<core_v1::PublishEndpointRequest>,
    ) -> Result<Response<semantic_v1::Endpoint>, Status> {
        let request = request.into_inner();
        validate_authority_context(request.context.as_ref())?;
        let endpoint = semantic_endpoint_from_proto(
            request
                .endpoint
                .ok_or_else(|| Status::invalid_argument("endpoint is required"))?,
        )?;
        let owner_is_managed = self
            .instances
            .lock()
            .expect("instance lock poisoned")
            .get(&endpoint.owner.id)
            .and_then(|process| process.semantic_worker.as_ref())
            .is_some_and(|worker| worker.identity == endpoint.owner);
        if !owner_is_managed {
            return Err(semantic_status(
                tonic::Code::FailedPrecondition,
                "ENDPOINT_OWNER_UNKNOWN",
                "endpoint owner is not an active managed Worker incarnation",
            ));
        }
        let endpoint_key = semantic_identity_key(&endpoint.identity);
        let mut endpoints = self.endpoints.lock().expect("endpoint lock poisoned");
        if endpoints.values().any(|current| {
            current.identity.id == endpoint.identity.id
                && current.identity.generation > endpoint.identity.generation
        }) {
            return Err(Status::failed_precondition("stale endpoint generation"));
        }
        endpoints.retain(|_, current| {
            current.identity.id != endpoint.identity.id
                || current.identity.generation >= endpoint.identity.generation
        });
        endpoints.insert(endpoint_key, endpoint.clone());
        drop(endpoints);
        self.endpoint_grants
            .lock()
            .expect("endpoint grant lock poisoned")
            .retain(|_, grant| {
                grant.endpoint.id != endpoint.identity.id || grant.endpoint == endpoint.identity
            });
        Ok(Response::new(to_semantic_proto_endpoint(&endpoint)))
    }

    async fn authorize_endpoint(
        &self,
        request: Request<core_v1::AuthorizeEndpointRequest>,
    ) -> Result<Response<semantic_v1::EndpointGrant>, Status> {
        let request = request.into_inner();
        validate_authority_context(request.context.as_ref())?;
        let grant = semantic_endpoint_grant_from_proto(
            request
                .grant
                .ok_or_else(|| Status::invalid_argument("endpoint grant is required"))?,
        )?;
        let endpoint_exists = self
            .endpoints
            .lock()
            .expect("endpoint lock poisoned")
            .get(&semantic_identity_key(&grant.endpoint))
            .is_some_and(|endpoint| endpoint.identity == grant.endpoint);
        if !endpoint_exists {
            return Err(Status::not_found(
                "endpoint grant targets an unpublished endpoint",
            ));
        }
        let lease = self
            .daemon
            .lease(&grant.lease.id)
            .map_err(provider_status)?;
        if lease.generation != grant.lease.generation {
            return Err(Status::failed_precondition("stale lease generation"));
        }
        if lease.fence_token != grant.fence_token {
            return Err(Status::failed_precondition("stale lease fence token"));
        }
        if lease.state != LeaseState::Active || lease.holder != grant.grantee {
            return Err(Status::failed_precondition(
                "endpoint grant grantee does not hold an active lease",
            ));
        }
        if grant.expires_at_unix_ms <= now_unix_ms()
            || lease
                .expires_at_unix_ms
                .is_some_and(|lease_expiry| grant.expires_at_unix_ms > lease_expiry)
        {
            return Err(Status::failed_precondition(
                "endpoint grant expiry must be active and bounded by its lease",
            ));
        }
        self.endpoint_grants
            .lock()
            .expect("endpoint grant lock poisoned")
            .insert(semantic_identity_key(&grant.identity), grant.clone());
        Ok(Response::new(to_semantic_proto_endpoint_grant(&grant)))
    }

    async fn revoke_endpoint(
        &self,
        request: Request<core_v1::RevokeEndpointRequest>,
    ) -> Result<Response<()>, Status> {
        let request = request.into_inner();
        validate_authority_context(request.context.as_ref())?;
        let grant = semantic_identity_from_proto(request.grant, "grant")?;
        self.endpoint_grants
            .lock()
            .expect("endpoint grant lock poisoned")
            .remove(&semantic_identity_key(&grant));
        Ok(Response::new(()))
    }

    async fn subscribe_events(
        &self,
        request: Request<core_v1::SubscribeEventsRequest>,
    ) -> Result<Response<semantic_v1::EventPage>, Status> {
        let request = request.into_inner();
        validate_authority_context(request.context.as_ref())?;
        let cursor = semantic_event_cursor_from_proto(request.cursor)?;
        let page_size = usize::try_from(request.page_size).unwrap_or(usize::MAX);
        if page_size == 0 || page_size > OPERATION_EVENT_HISTORY_CAPACITY {
            return Err(semantic_status(
                tonic::Code::InvalidArgument,
                "EVENT_PAGE_LIMIT_INVALID",
                "event page_size must be in 1..=256",
            ));
        }
        let page = self.semantic_events_after(&cursor, page_size);
        debug_assert!(page.validate().is_ok());
        Ok(Response::new(to_semantic_proto_event_page(&page)))
    }
}

#[tonic::async_trait]
impl core_v1::plugin_lifecycle_service_server::PluginLifecycleService for KernelServiceAdapter {
    async fn install_plugin(
        &self,
        _request: Request<core_v1::InstallPluginRequest>,
    ) -> Result<Response<core_v1::Operation>, Status> {
        Err(Status::unimplemented(
            "installation is an out-of-kernel adapter responsibility",
        ))
    }

    async fn uninstall_plugin(
        &self,
        _request: Request<core_v1::UninstallPluginRequest>,
    ) -> Result<Response<core_v1::Operation>, Status> {
        Err(Status::unimplemented(
            "installation is an out-of-kernel adapter responsibility",
        ))
    }

    async fn set_plugin_enabled(
        &self,
        _request: Request<core_v1::SetPluginEnabledRequest>,
    ) -> Result<Response<core_v1::PluginInstallation>, Status> {
        Err(Status::unimplemented(
            "plugin enablement policy belongs to the control plane",
        ))
    }

    async fn start_plugin(
        &self,
        _request: Request<core_v1::StartPluginRequest>,
    ) -> Result<Response<core_v1::Operation>, Status> {
        Err(Status::unimplemented(
            "use KernelService.LaunchPlugin after policy and installation validation",
        ))
    }

    async fn stop_plugin(
        &self,
        _request: Request<core_v1::StopPluginRequest>,
    ) -> Result<Response<core_v1::Operation>, Status> {
        Err(Status::unimplemented(
            "use KernelService.TerminatePlugin for node-local process termination",
        ))
    }

    async fn get_plugin_instance(
        &self,
        request: Request<core_v1::GetPluginInstanceRequest>,
    ) -> Result<Response<core_v1::PluginInstance>, Status> {
        let name = request.into_inner().name;
        let adapter_available = self.adapter_available.load(Ordering::Relaxed);
        self.instances
            .lock()
            .expect("instance lock poisoned")
            .get(&name)
            .map(|process| {
                Response::new(to_plugin_instance(
                    &self.daemon,
                    &name,
                    process,
                    adapter_available,
                ))
            })
            .ok_or_else(|| Status::not_found("plugin instance is not managed by this Kernel"))
    }

    async fn list_plugin_instances(
        &self,
        request: Request<core_v1::ListPluginInstancesRequest>,
    ) -> Result<Response<core_v1::ListPluginInstancesResponse>, Status> {
        let request = request.into_inner();
        let filters = request.state_filter;
        let adapter_available = self.adapter_available.load(Ordering::Relaxed);
        let plugins = self
            .instances
            .lock()
            .expect("instance lock poisoned")
            .iter()
            .filter(|(_, process)| filters.is_empty() || filters.contains(&process.runtime_state))
            .map(|(name, process)| {
                to_plugin_instance(&self.daemon, name, process, adapter_available)
            })
            .collect();
        Ok(Response::new(core_v1::ListPluginInstancesResponse {
            plugins,
            next_page_token: String::new(),
        }))
    }

    async fn report_heartbeat(
        &self,
        request: Request<core_v1::ReportHeartbeatRequest>,
    ) -> Result<Response<core_v1::ReportHeartbeatResponse>, Status> {
        let request = request.into_inner();
        Ok(Response::new(self.accept_heartbeat(
            &request.plugin_instance_name,
            request.generation,
            request.sequence_number,
            request.observed_at,
            request.runtime_state,
            request.health,
            request.restart_count,
        )?))
    }

    type ConnectWorkerStream = std::pin::Pin<
        Box<dyn Stream<Item = Result<core_v1::KernelToWorker, Status>> + Send + 'static>,
    >;

    async fn connect_worker(
        &self,
        request: Request<tonic::Streaming<core_v1::WorkerToKernel>>,
    ) -> Result<Response<Self::ConnectWorkerStream>, Status> {
        let mut inbound = request.into_inner();
        let hello = inbound.message().await?.ok_or_else(|| {
            Status::invalid_argument("WorkerHello must be the first control frame")
        })?;
        let Some(core_v1::worker_to_kernel::Body::Hello(hello)) = hello.body else {
            return Err(Status::invalid_argument(
                "WorkerHello must be the first control frame",
            ));
        };
        let (outbound, receiver) = mpsc::channel(16);
        let (connection_id, welcome) = self.register_worker_control(&hello, outbound.clone())?;
        outbound
            .send(Ok(core_v1::KernelToWorker {
                body: Some(core_v1::kernel_to_worker::Body::Welcome(welcome)),
            }))
            .await
            .map_err(|_| Status::unavailable("worker control receiver closed during handshake"))?;

        let adapter = self.clone();
        let instance_name = hello.plugin_instance_name.clone();
        let generation = hello.generation;
        tokio::spawn(async move {
            while let Ok(Some(frame)) = inbound.message().await {
                let result = match frame.body {
                    Some(core_v1::worker_to_kernel::Body::Heartbeat(heartbeat)) => adapter
                        .accept_heartbeat(
                            &heartbeat.plugin_instance_name,
                            heartbeat.generation,
                            heartbeat.sequence_number,
                            heartbeat.observed_at,
                            heartbeat.runtime_state,
                            heartbeat.health,
                            heartbeat.restart_count,
                        )
                        .map(|response| core_v1::KernelToWorker {
                            body: Some(core_v1::kernel_to_worker::Body::HeartbeatAck(
                                core_v1::WorkerHeartbeatAck {
                                    disposition: response.disposition,
                                    accepted_sequence_number: response.accepted_sequence_number,
                                    desired_state: response.desired_state,
                                    desired_generation: response.desired_generation,
                                },
                            )),
                        }),
                    Some(core_v1::worker_to_kernel::Body::ShutdownAck(ack)) => adapter
                        .accept_shutdown_ack(&ack)
                        .map(|_| core_v1::KernelToWorker {
                            body: Some(core_v1::kernel_to_worker::Body::HeartbeatAck(
                                core_v1::WorkerHeartbeatAck {
                                    disposition: core_v1::HeartbeatDisposition::Accepted as i32,
                                    accepted_sequence_number: 0,
                                    desired_state: core_v1::DesiredPluginState::Stopped as i32,
                                    desired_generation: ack.generation,
                                },
                            )),
                        }),
                    Some(core_v1::worker_to_kernel::Body::Hello(_)) | None => {
                        Err(Status::invalid_argument(
                            "WorkerHello is valid only as the first control frame",
                        ))
                    }
                };
                match result {
                    Ok(response) => {
                        if outbound.send(Ok(response)).await.is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = outbound.send(Err(error)).await;
                        break;
                    }
                }
            }
            adapter.unregister_worker_control(&instance_name, generation, connection_id);
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(receiver))))
    }

    type WatchPluginEventsStream = std::pin::Pin<
        Box<dyn Stream<Item = Result<core_v1::PluginLifecycleEvent, Status>> + Send + 'static>,
    >;

    async fn watch_plugin_events(
        &self,
        _request: Request<core_v1::WatchPluginEventsRequest>,
    ) -> Result<Response<Self::WatchPluginEventsStream>, Status> {
        Ok(Response::new(Box::pin(iter(Vec::<
            Result<core_v1::PluginLifecycleEvent, Status>,
        >::new()))))
    }
}

fn to_plugin_instance(
    daemon: &KernelDaemon,
    name: &str,
    process: &ManagedProcess,
    adapter_available: bool,
) -> core_v1::PluginInstance {
    core_v1::PluginInstance {
        name: name.to_string(),
        plugin: Some(process.plugin.clone()),
        node: Some(core_v1::NodeRef {
            node_id: daemon.node_id.clone(),
            node_epoch: daemon.node_epoch,
        }),
        generation: process.generation,
        observed_generation: process.generation,
        desired_state: if process.watchdog_triggered {
            core_v1::DesiredPluginState::Stopped as i32
        } else {
            core_v1::DesiredPluginState::Running as i32
        },
        runtime_state: managed_runtime_state(process),
        health: if adapter_available {
            process.health.clone()
        } else {
            Some(core_v1::HealthReport {
                status: core_v1::HealthStatus::Degraded as i32,
                reason_code: "ADAPTER_DEGRADED".to_string(),
                summary: "external hardware facts are unavailable or expired".to_string(),
            })
        },
        lease: process.lease.clone(),
        restart_count: process.restart_count,
        created_at: None,
        updated_at: Some(now_timestamp()),
        last_heartbeat_at: process.last_heartbeat_at.clone(),
    }
}

fn managed_runtime_state(process: &ManagedProcess) -> i32 {
    match process.instance.state() {
        SandboxedProcessState::Discovered => core_v1::PluginRuntimeState::Discovered as i32,
        SandboxedProcessState::Starting => core_v1::PluginRuntimeState::Starting as i32,
        SandboxedProcessState::Healthy => process.runtime_state,
        SandboxedProcessState::Stopping => core_v1::PluginRuntimeState::Stopping as i32,
        SandboxedProcessState::Stopped => core_v1::PluginRuntimeState::Stopped as i32,
        SandboxedProcessState::Quarantined => core_v1::PluginRuntimeState::Quarantined as i32,
    }
}

fn to_semantic_proto_identity(identity: &semantic::Identity) -> semantic_v1::Identity {
    semantic_v1::Identity {
        id: identity.id.clone(),
        generation: identity.generation,
    }
}

fn semantic_identity_from_proto(
    identity: Option<semantic_v1::Identity>,
    field: &str,
) -> Result<semantic::Identity, Status> {
    let identity =
        identity.ok_or_else(|| Status::invalid_argument(format!("{field} is required")))?;
    let identity = semantic::Identity {
        id: identity.id,
        generation: identity.generation,
    };
    identity.validate().map_err(|error| {
        Status::invalid_argument(format!("{}: {}", error.reason_code, error.message))
    })?;
    Ok(identity)
}

fn semantic_identity_key(identity: &semantic::Identity) -> String {
    format!(
        "{}:{}:{}",
        identity.id.len(),
        identity.id,
        identity.generation
    )
}

fn semantic_contract_revision_from_proto(
    revision: semantic_v1::ContractRevision,
) -> Option<semantic::ContractRevision> {
    let revision = semantic::ContractRevision {
        contract_id: revision.contract_id,
        major: revision.major,
        minor: revision.minor,
    };
    revision.validate().ok().map(|_| revision)
}

fn to_semantic_proto_contract_revision(
    revision: &semantic::ContractRevision,
) -> semantic_v1::ContractRevision {
    semantic_v1::ContractRevision {
        contract_id: revision.contract_id.clone(),
        major: revision.major,
        minor: revision.minor,
    }
}

fn validate_authority_context(
    context: Option<&core_v1::AuthorityCallContext>,
) -> Result<&core_v1::AuthorityCallContext, Status> {
    let context = context.ok_or_else(|| {
        semantic_status(
            tonic::Code::InvalidArgument,
            "AUTHORITY_CONTEXT_REQUIRED",
            "a negotiated authority call context is required",
        )
    })?;
    let offered = context.contract.clone().ok_or_else(|| {
        semantic_status(
            tonic::Code::FailedPrecondition,
            "CONTRACT_NEGOTIATION_REQUIRED",
            "a selected semantic contract revision is required",
        )
    })?;
    let offered = semantic_contract_revision_from_proto(offered).ok_or_else(|| {
        semantic_status(
            tonic::Code::InvalidArgument,
            "CONTRACT_REVISION_INVALID",
            "authority call contains an invalid semantic contract revision",
        )
    })?;
    let local = semantic::ContractRevision::current();
    if local.negotiate(&offered).as_ref() != Some(&offered) {
        return Err(semantic_status(
            tonic::Code::FailedPrecondition,
            "CONTRACT_INCOMPATIBLE",
            "authority call did not use a revision selected by this Kernel",
        ));
    }
    if context.request_id.is_empty() && context.idempotency_key.is_empty() {
        return Err(semantic_status(
            tonic::Code::InvalidArgument,
            "REQUEST_ID_REQUIRED",
            "authority call requires request_id or idempotency_key",
        ));
    }
    Ok(context)
}

fn authority_lease_name(context: &core_v1::AuthorityCallContext) -> String {
    let key = if context.idempotency_key.is_empty() {
        &context.request_id
    } else {
        &context.idempotency_key
    };
    format!("lease-{key}")
}

fn semantic_endpoint_from_proto(
    endpoint: semantic_v1::Endpoint,
) -> Result<semantic::Endpoint, Status> {
    let endpoint = semantic::Endpoint {
        identity: semantic_identity_from_proto(endpoint.identity, "endpoint identity")?,
        provider: semantic_identity_from_proto(endpoint.provider, "endpoint provider")?,
        owner: semantic_identity_from_proto(endpoint.owner, "endpoint owner")?,
        transport: endpoint.transport,
        schema_id: endpoint.schema_id,
        capabilities: endpoint
            .capabilities
            .into_iter()
            .map(|capability| semantic::Capability {
                id: capability.id,
                revision: capability.revision,
                properties: capability.properties.into_iter().collect(),
            })
            .collect(),
        public_attributes: endpoint.public_attributes.into_iter().collect(),
    };
    endpoint.validate().map_err(|error| {
        Status::invalid_argument(format!("{}: {}", error.reason_code, error.message))
    })?;
    Ok(endpoint)
}

fn to_semantic_proto_endpoint(endpoint: &semantic::Endpoint) -> semantic_v1::Endpoint {
    semantic_v1::Endpoint {
        identity: Some(to_semantic_proto_identity(&endpoint.identity)),
        provider: Some(to_semantic_proto_identity(&endpoint.provider)),
        owner: Some(to_semantic_proto_identity(&endpoint.owner)),
        transport: endpoint.transport.clone(),
        schema_id: endpoint.schema_id.clone(),
        capabilities: endpoint
            .capabilities
            .iter()
            .map(|capability| semantic_v1::Capability {
                id: capability.id.clone(),
                revision: capability.revision,
                properties: capability.properties.clone().into_iter().collect(),
            })
            .collect(),
        public_attributes: endpoint.public_attributes.clone().into_iter().collect(),
    }
}

fn semantic_endpoint_grant_from_proto(
    grant: semantic_v1::EndpointGrant,
) -> Result<semantic::EndpointGrant, Status> {
    let grant = semantic::EndpointGrant {
        identity: semantic_identity_from_proto(grant.identity, "endpoint grant identity")?,
        endpoint: semantic_identity_from_proto(grant.endpoint, "endpoint grant endpoint")?,
        grantee: semantic_identity_from_proto(grant.grantee, "endpoint grant grantee")?,
        lease: semantic_identity_from_proto(grant.lease, "endpoint grant lease")?,
        fence_token: grant.fence_token,
        expires_at_unix_ms: unix_ms_from_timestamp(
            grant
                .expires_at
                .ok_or_else(|| Status::invalid_argument("endpoint grant expiry is required"))?,
            "endpoint grant expiry",
        )?,
    };
    grant.validate().map_err(|error| {
        Status::invalid_argument(format!("{}: {}", error.reason_code, error.message))
    })?;
    Ok(grant)
}

fn to_semantic_proto_endpoint_grant(grant: &semantic::EndpointGrant) -> semantic_v1::EndpointGrant {
    semantic_v1::EndpointGrant {
        identity: Some(to_semantic_proto_identity(&grant.identity)),
        endpoint: Some(to_semantic_proto_identity(&grant.endpoint)),
        grantee: Some(to_semantic_proto_identity(&grant.grantee)),
        lease: Some(to_semantic_proto_identity(&grant.lease)),
        fence_token: grant.fence_token,
        expires_at: Some(timestamp_from_unix_ms(grant.expires_at_unix_ms)),
    }
}

fn semantic_operation_from_proto(
    operation: semantic_v1::Operation,
) -> Result<semantic::Operation, Status> {
    let state = semantic_v1::OperationState::try_from(operation.state).map_err(|_| {
        semantic_status(
            tonic::Code::InvalidArgument,
            "UNKNOWN_ENUM_VALUE",
            "operation state is unknown",
        )
    })?;
    let state = match state {
        semantic_v1::OperationState::Created => semantic::OperationState::Created,
        semantic_v1::OperationState::Pending => semantic::OperationState::Pending,
        semantic_v1::OperationState::Running => semantic::OperationState::Running,
        semantic_v1::OperationState::Succeeded => semantic::OperationState::Succeeded,
        semantic_v1::OperationState::Failed => semantic::OperationState::Failed,
        semantic_v1::OperationState::Cancelling => semantic::OperationState::Cancelling,
        semantic_v1::OperationState::Cancelled => semantic::OperationState::Cancelled,
        semantic_v1::OperationState::Lost => semantic::OperationState::Lost,
        semantic_v1::OperationState::Unspecified => {
            return Err(semantic_status(
                tonic::Code::InvalidArgument,
                "UNKNOWN_ENUM_VALUE",
                "operation state cannot be UNSPECIFIED",
            ));
        }
    };
    let operation = semantic::Operation {
        identity: semantic_identity_from_proto(operation.identity, "operation identity")?,
        owner: semantic_identity_from_proto(operation.owner, "operation owner")?,
        executor: semantic_identity_from_proto(operation.executor, "operation executor")?,
        kind: operation.kind,
        state,
        deadline_unix_ms: operation
            .deadline
            .map(|timestamp| unix_ms_from_timestamp(timestamp, "operation deadline"))
            .transpose()?,
        parent: operation
            .parent
            .map(|identity| semantic_identity_from_proto(Some(identity), "operation parent"))
            .transpose()?,
        metadata: operation.metadata.into_iter().collect(),
    };
    operation.validate().map_err(|error| {
        semantic_status(
            tonic::Code::InvalidArgument,
            &error.reason_code,
            &error.message,
        )
    })?;
    Ok(operation)
}

fn to_semantic_proto_operation(operation: &semantic::Operation) -> semantic_v1::Operation {
    semantic_v1::Operation {
        identity: Some(to_semantic_proto_identity(&operation.identity)),
        owner: Some(to_semantic_proto_identity(&operation.owner)),
        executor: Some(to_semantic_proto_identity(&operation.executor)),
        kind: operation.kind.clone(),
        state: match operation.state {
            semantic::OperationState::Created => semantic_v1::OperationState::Created,
            semantic::OperationState::Pending => semantic_v1::OperationState::Pending,
            semantic::OperationState::Running => semantic_v1::OperationState::Running,
            semantic::OperationState::Succeeded => semantic_v1::OperationState::Succeeded,
            semantic::OperationState::Failed => semantic_v1::OperationState::Failed,
            semantic::OperationState::Cancelling => semantic_v1::OperationState::Cancelling,
            semantic::OperationState::Cancelled => semantic_v1::OperationState::Cancelled,
            semantic::OperationState::Lost => semantic_v1::OperationState::Lost,
        } as i32,
        deadline: operation.deadline_unix_ms.map(timestamp_from_unix_ms),
        parent: operation.parent.as_ref().map(to_semantic_proto_identity),
        metadata: operation.metadata.clone().into_iter().collect(),
    }
}

fn semantic_worker_from_proto(worker: semantic_v1::Worker) -> Result<semantic::Worker, Status> {
    let state = semantic_v1::WorkerState::try_from(worker.state).map_err(|_| {
        semantic_status(
            tonic::Code::InvalidArgument,
            "UNKNOWN_ENUM_VALUE",
            "worker state is unknown",
        )
    })?;
    let state = match state {
        semantic_v1::WorkerState::Registered => semantic::WorkerState::Registered,
        semantic_v1::WorkerState::Starting => semantic::WorkerState::Starting,
        semantic_v1::WorkerState::Running => semantic::WorkerState::Running,
        semantic_v1::WorkerState::Draining => semantic::WorkerState::Draining,
        semantic_v1::WorkerState::Stopped => semantic::WorkerState::Stopped,
        semantic_v1::WorkerState::Failed => semantic::WorkerState::Failed,
        semantic_v1::WorkerState::Lost => semantic::WorkerState::Lost,
        semantic_v1::WorkerState::Unspecified => {
            return Err(semantic_status(
                tonic::Code::InvalidArgument,
                "UNKNOWN_ENUM_VALUE",
                "worker state cannot be UNSPECIFIED",
            ));
        }
    };
    let worker = semantic::Worker {
        identity: semantic_identity_from_proto(worker.identity, "worker identity")?,
        principal: semantic_identity_from_proto(worker.principal, "worker principal")?,
        provider: semantic_identity_from_proto(worker.provider, "worker provider")?,
        lease: semantic_identity_from_proto(worker.lease, "worker lease")?,
        state,
        execution_ref: worker.execution_ref,
        limits: worker
            .limits
            .into_iter()
            .map(|(key, quantity)| {
                (
                    key,
                    semantic::Quantity {
                        value: quantity.value,
                        unit: quantity.unit,
                    },
                )
            })
            .collect(),
    };
    worker.validate().map_err(|error| {
        semantic_status(
            tonic::Code::InvalidArgument,
            &error.reason_code,
            &error.message,
        )
    })?;
    Ok(worker)
}

fn to_semantic_proto_worker(worker: &semantic::Worker) -> semantic_v1::Worker {
    semantic_v1::Worker {
        identity: Some(to_semantic_proto_identity(&worker.identity)),
        principal: Some(to_semantic_proto_identity(&worker.principal)),
        provider: Some(to_semantic_proto_identity(&worker.provider)),
        lease: Some(to_semantic_proto_identity(&worker.lease)),
        state: match worker.state {
            semantic::WorkerState::Registered => semantic_v1::WorkerState::Registered,
            semantic::WorkerState::Starting => semantic_v1::WorkerState::Starting,
            semantic::WorkerState::Running => semantic_v1::WorkerState::Running,
            semantic::WorkerState::Draining => semantic_v1::WorkerState::Draining,
            semantic::WorkerState::Stopped => semantic_v1::WorkerState::Stopped,
            semantic::WorkerState::Failed => semantic_v1::WorkerState::Failed,
            semantic::WorkerState::Lost => semantic_v1::WorkerState::Lost,
        } as i32,
        execution_ref: worker.execution_ref.clone(),
        limits: worker
            .limits
            .iter()
            .map(|(key, quantity)| {
                (
                    key.clone(),
                    semantic_v1::Quantity {
                        value: quantity.value,
                        unit: quantity.unit.clone(),
                    },
                )
            })
            .collect(),
    }
}

fn semantic_operation_event_kind(state: semantic::OperationState) -> &'static str {
    match state {
        semantic::OperationState::Created => "operation.created",
        semantic::OperationState::Pending => "operation.pending",
        semantic::OperationState::Running => "operation.running",
        semantic::OperationState::Succeeded => "operation.succeeded",
        semantic::OperationState::Failed => "operation.failed",
        semantic::OperationState::Cancelling => "operation.cancelling",
        semantic::OperationState::Cancelled => "operation.cancelled",
        semantic::OperationState::Lost => "operation.lost",
    }
}

fn semantic_event_cursor_from_proto(
    cursor: Option<semantic_v1::EventCursor>,
) -> Result<semantic::EventCursor, Status> {
    let cursor = cursor.ok_or_else(|| {
        semantic_status(
            tonic::Code::InvalidArgument,
            "REQUIRED_FIELD_MISSING",
            "event cursor is required",
        )
    })?;
    let cursor = semantic::EventCursor {
        source: semantic_identity_from_proto(cursor.source, "event cursor source")?,
        sequence: cursor.sequence,
    };
    cursor.validate().map_err(|error| {
        semantic_status(
            tonic::Code::InvalidArgument,
            &error.reason_code,
            &error.message,
        )
    })?;
    Ok(cursor)
}

fn to_semantic_proto_event_page(page: &semantic::EventPage) -> semantic_v1::EventPage {
    semantic_v1::EventPage {
        source: Some(to_semantic_proto_identity(&page.source)),
        status: match page.status {
            semantic::ReplayStatus::Current => semantic_v1::ReplayStatus::Current,
            semantic::ReplayStatus::Gap => semantic_v1::ReplayStatus::Gap,
            semantic::ReplayStatus::SourceChanged => semantic_v1::ReplayStatus::SourceChanged,
        } as i32,
        events: page
            .events
            .iter()
            .map(|event| semantic_v1::Event {
                sequence: event.sequence,
                source: Some(to_semantic_proto_identity(&event.source)),
                subject: Some(to_semantic_proto_identity(&event.subject)),
                kind: event.kind.clone(),
                observed_at: Some(timestamp_from_unix_ms(event.observed_at_unix_ms)),
                schema_id: event.schema_id.clone(),
                body: event.body.clone(),
            })
            .collect(),
        oldest_available_sequence: page.oldest_available_sequence,
        latest_available_sequence: page.latest_available_sequence,
        next_sequence: page.next_sequence,
    }
}

fn to_semantic_proto_resource(resource: &semantic::Resource) -> semantic_v1::Resource {
    semantic_v1::Resource {
        identity: Some(to_semantic_proto_identity(&resource.identity)),
        provider: Some(to_semantic_proto_identity(&resource.provider)),
        resource_class: resource.resource_class.clone(),
        capabilities: resource
            .capabilities
            .iter()
            .map(|capability| semantic_v1::Capability {
                id: capability.id.clone(),
                revision: capability.revision,
                properties: capability.properties.clone().into_iter().collect(),
            })
            .collect(),
        capacity: resource
            .capacity
            .iter()
            .map(|(key, quantity)| {
                (
                    key.clone(),
                    semantic_v1::Quantity {
                        value: quantity.value,
                        unit: quantity.unit.clone(),
                    },
                )
            })
            .collect(),
        attributes: resource.attributes.clone().into_iter().collect(),
        state: match resource.state {
            semantic::ResourceState::Ready => semantic_v1::ResourceState::Ready,
            semantic::ResourceState::Degraded => semantic_v1::ResourceState::Degraded,
            semantic::ResourceState::Unavailable => semantic_v1::ResourceState::Unavailable,
        } as i32,
        reason_code: resource.reason_code.clone(),
        summary: resource.summary.clone(),
        links: resource
            .links
            .iter()
            .map(|link| semantic_v1::TopologyLink {
                peer: Some(to_semantic_proto_identity(&link.peer)),
                kind: link.kind.clone(),
                properties: link.properties.clone().into_iter().collect(),
            })
            .collect(),
    }
}

fn semantic_query_from_proto(
    query: semantic_v1::ResourceQuery,
) -> Result<semantic::ResourceQuery, Status> {
    let query = semantic::ResourceQuery {
        resource_class: query.resource_class,
        count: query.count,
        required_capabilities: query
            .required_capabilities
            .into_iter()
            .map(|requirement| semantic::CapabilityRequirement {
                id: requirement.id,
                minimum_revision: requirement.minimum_revision,
                required_properties: requirement.required_properties.into_iter().collect(),
            })
            .collect(),
        minimum_capacity: query
            .minimum_capacity
            .into_iter()
            .map(|(key, quantity)| {
                (
                    key,
                    semantic::Quantity {
                        value: quantity.value,
                        unit: quantity.unit,
                    },
                )
            })
            .collect(),
    };
    query.validate().map_err(|error| {
        Status::invalid_argument(format!("{}: {}", error.reason_code, error.message))
    })?;
    Ok(query)
}

/// 将内部隔离执行模式转换为 Protobuf 协议枚举
fn to_proto_enforcement(mode: EnforcementMode) -> core_v1::EnforcementMode {
    match mode {
        EnforcementMode::Hard => core_v1::EnforcementMode::Hard,
        EnforcementMode::Soft => core_v1::EnforcementMode::Soft,
        EnforcementMode::VisibilityOnly => core_v1::EnforcementMode::VisibilityOnly,
        EnforcementMode::ObserveOnly => core_v1::EnforcementMode::ObserveOnly,
        EnforcementMode::Unenforced => core_v1::EnforcementMode::Unenforced,
    }
}

fn merge_bindings(bindings: Vec<DeviceBinding>) -> Result<DeviceBinding, ProviderError> {
    let Some(first) = bindings.first().cloned() else {
        return Ok(DeviceBinding {
            resource_id: "none".to_string(),
            nodes: Vec::new(),
            environment: BTreeMap::new(),
            required_gids: Vec::new(),
            enforcement: EnforcementMode::Unenforced,
            adapter_id: "kernel-daemon".to_string(),
            reason_code: "NO_RESOURCE_BINDING".to_string(),
        });
    };
    let mut nodes = first.nodes;
    let mut environment = first.environment;
    let mut required_gids = first.required_gids;
    let enforcement = first.enforcement;
    let mut adapter_ids = vec![first.adapter_id.clone()];
    let mut resource_ids = vec![first.resource_id];
    for binding in bindings.into_iter().skip(1) {
        if binding.enforcement != enforcement {
            return Err(ProviderError::new(
                "kernel-daemon",
                "MIXED_RESOURCE_ENFORCEMENT",
                "a multi-resource binding must use one enforcement mode",
            ));
        }
        if !adapter_ids.contains(&binding.adapter_id) {
            adapter_ids.push(binding.adapter_id.clone());
        }
        resource_ids.push(binding.resource_id);
        for node in binding.nodes {
            if !nodes.iter().any(|existing| existing.path == node.path) {
                nodes.push(node);
            }
        }
        for (key, value) in binding.environment {
            if let Some(existing) = environment.get(&key) {
                if existing != &value {
                    return Err(ProviderError::new(
                        "kernel-daemon",
                        "CONFLICTING_RESOURCE_ENVIRONMENT",
                        &key,
                    ));
                }
            } else {
                environment.insert(key, value);
            }
        }
        for gid in binding.required_gids {
            if !required_gids.contains(&gid) {
                required_gids.push(gid);
            }
        }
    }
    adapter_ids.sort();
    Ok(DeviceBinding {
        resource_id: resource_ids.join(","),
        nodes,
        environment,
        required_gids,
        enforcement,
        adapter_id: adapter_ids.join(","),
        reason_code: "RESOURCE_BINDING_CREATED_BY_UDS_ADAPTERS".to_string(),
    })
}

fn resource_request(
    lease_name: &str,
    generation: u64,
    holder: semantic::Identity,
    expires_at_unix_ms: Option<u64>,
    requirements: &core_v1::ResourceRequirements,
) -> Result<ResourceRequest, Status> {
    let mut count = 0u32;
    let mut kind_capability: Option<String> = None;
    let mut required_capabilities = BTreeMap::<String, semantic::CapabilityRequirement>::new();
    let mut min_memory_bytes: Option<u64> = None;
    for accelerator in &requirements.accelerators {
        count = count
            .checked_add(accelerator.count)
            .ok_or_else(|| Status::invalid_argument("accelerator count overflow"))?;
        if accelerator.vendor != core_v1::AcceleratorVendor::Unspecified as i32
            || !accelerator.other_vendor_id.is_empty()
        {
            return Err(Status::failed_precondition(
                "legacy vendor selectors are not interpreted by Kernel; use AcquireLease with a namespaced Capability",
            ));
        }
        let requested_kind = match core_v1::AcceleratorKind::try_from(accelerator.kind)
            .map_err(|_| Status::invalid_argument("unknown accelerator kind"))?
        {
            core_v1::AcceleratorKind::Unspecified => "accelerator.compute",
            core_v1::AcceleratorKind::Gpu => "accelerator.kind.gpu",
            core_v1::AcceleratorKind::Npu => "accelerator.kind.npu",
            core_v1::AcceleratorKind::Tpu => "accelerator.kind.tpu",
            core_v1::AcceleratorKind::Other => "accelerator.kind.other",
        };
        if kind_capability
            .as_ref()
            .is_some_and(|current| current != requested_kind)
        {
            return Err(Status::invalid_argument(
                "one legacy request cannot mix accelerator kinds",
            ));
        }
        kind_capability = Some(requested_kind.to_string());
        for capability_id in &accelerator.required_features {
            if !capability_id.contains('.') {
                return Err(Status::failed_precondition(
                    "legacy bare feature names are ambiguous; use a namespaced Capability id",
                ));
            }
            required_capabilities.insert(
                capability_id.clone(),
                semantic::CapabilityRequirement {
                    id: capability_id.clone(),
                    minimum_revision: 1,
                    required_properties: BTreeMap::new(),
                },
            );
        }
        min_memory_bytes = match min_memory_bytes {
            Some(current) => Some(current.max(accelerator.min_memory_bytes_per_device)),
            None => Some(accelerator.min_memory_bytes_per_device),
        };
    }
    if count == 0 {
        return Err(Status::invalid_argument(
            "at least one accelerator resource is required",
        ));
    }
    let kind_capability = kind_capability.unwrap_or_else(|| "accelerator.compute".to_string());
    required_capabilities.insert(
        kind_capability.clone(),
        semantic::CapabilityRequirement {
            id: kind_capability,
            minimum_revision: 1,
            required_properties: BTreeMap::new(),
        },
    );
    let cpu_max_millicores = requirements
        .cpu
        .as_ref()
        .and_then(|cpu| (cpu.limit_millicores > 0).then_some(cpu.limit_millicores));
    if let Some(cpu) = requirements.cpu.as_ref() {
        if cpu.limit_millicores > 0
            && cpu.request_millicores > 0
            && cpu.request_millicores > cpu.limit_millicores
        {
            return Err(Status::invalid_argument(
                "cpu request_millicores cannot exceed limit_millicores",
            ));
        }
    }
    let memory_max_bytes = requirements
        .memory
        .as_ref()
        .and_then(|memory| (memory.limit_bytes > 0).then_some(memory.limit_bytes));
    if let Some(memory) = requirements.memory.as_ref() {
        if memory.limit_bytes > 0
            && memory.request_bytes > 0
            && memory.request_bytes > memory.limit_bytes
        {
            return Err(Status::invalid_argument(
                "memory request_bytes cannot exceed limit_bytes",
            ));
        }
    }
    Ok(ResourceRequest {
        lease_name: lease_name.to_string(),
        expected_inventory_generation: generation,
        holder,
        query: semantic::ResourceQuery {
            resource_class: "accelerator".to_string(),
            count,
            required_capabilities: required_capabilities.into_values().collect(),
            minimum_capacity: min_memory_bytes
                .filter(|value| *value > 0)
                .map(|value| {
                    BTreeMap::from([(
                        "memory.allocatable".to_string(),
                        semantic::Quantity {
                            value,
                            unit: "byte".to_string(),
                        },
                    )])
                })
                .unwrap_or_default(),
        },
        expires_at_unix_ms,
        limits: CgroupLimits {
            cpu_max_millicores,
            memory_max_bytes,
            cpuset_cpus: None,
        },
    })
}

fn inject_heartbeat_environment(
    mut environment: BTreeMap<String, String>,
    heartbeat: &WorkerHeartbeatConfig,
    instance_name: &str,
    generation: u64,
) -> Result<BTreeMap<String, String>, ProviderError> {
    let injected = [
        (
            "CYRENE_HEARTBEAT_SOCKET",
            heartbeat.socket_path.to_string_lossy().into_owned(),
        ),
        (
            "CYRENE_WORKER_CONTROL_SOCKET",
            heartbeat.socket_path.to_string_lossy().into_owned(),
        ),
        ("CYRENE_PLUGIN_INSTANCE_NAME", instance_name.to_string()),
        ("CYRENE_PLUGIN_INSTANCE_GENERATION", generation.to_string()),
        (
            "CYRENE_HEARTBEAT_INTERVAL_MS",
            heartbeat.interval.as_millis().to_string(),
        ),
    ];
    if injected
        .iter()
        .any(|(key, _)| environment.contains_key(*key))
    {
        return Err(ProviderError::new(
            "kernel-daemon",
            "RESERVED_HEARTBEAT_ENVIRONMENT",
            "installation record attempted to override Kernel heartbeat configuration",
        ));
    }
    environment.extend(injected.map(|(key, value)| (key.to_string(), value)));
    Ok(environment)
}

#[allow(deprecated)]
fn to_proto_lease(
    daemon: &KernelDaemon,
    lease: ResourceLease,
    granted: Option<core_v1::ResourceRequirements>,
) -> Result<core_v1::ResourceLease, Status> {
    let node = core_v1::NodeRef {
        node_id: daemon.node_id.clone(),
        node_epoch: daemon.node_epoch,
    };
    let enforcement = lease
        .allocations
        .iter()
        .map(|allocation| core_v1::EnforcementReport {
            resource_kind: core_v1::ResourceKind::Accelerator as i32,
            mode: to_proto_enforcement(allocation.enforcement) as i32,
            adapter_id: "resource-manager".to_string(),
            reason_code: "LEASE_ALLOCATION".to_string(),
        })
        .collect();
    Ok(core_v1::ResourceLease {
        name: lease.name,
        node: Some(node),
        state: match lease.state {
            LeaseState::Active => core_v1::LeaseState::Active,
            LeaseState::Releasing => core_v1::LeaseState::Releasing,
            LeaseState::Released => core_v1::LeaseState::Released,
            LeaseState::Expired => core_v1::LeaseState::Expired,
            LeaseState::Revoked => core_v1::LeaseState::Failed,
            LeaseState::Failed => core_v1::LeaseState::Failed,
            LeaseState::Quarantined => core_v1::LeaseState::Failed,
        } as i32,
        granted,
        accelerators: lease
            .allocations
            .into_iter()
            .map(|allocation| core_v1::AcceleratorAllocation {
                allocation_id: allocation.allocation_id,
                device_id: allocation.resource.id,
                partition_id: String::new(),
                granted_memory_bytes: allocation
                    .granted_capacity
                    .get("memory.allocatable")
                    .filter(|quantity| quantity.unit == "byte")
                    .map_or(0, |quantity| quantity.value),
                enforcement: to_proto_enforcement(allocation.enforcement) as i32,
            })
            .collect(),
        enforcement,
        expires_at: lease.expires_at_unix_ms.map(timestamp_from_unix_ms),
        fence_token: lease.fence_token,
        inventory_generation: lease.inventory_generation,
    })
}

fn to_semantic_proto_lease(lease: &ResourceLease) -> semantic_v1::Lease {
    semantic_v1::Lease {
        identity: Some(semantic_v1::Identity {
            id: lease.name.clone(),
            generation: lease.generation,
        }),
        holder: Some(to_semantic_proto_identity(&lease.holder)),
        resources: lease
            .allocations
            .iter()
            .map(|allocation| to_semantic_proto_identity(&allocation.resource))
            .collect(),
        state: match lease.state {
            LeaseState::Active => semantic_v1::LeaseState::Active,
            LeaseState::Releasing => semantic_v1::LeaseState::Releasing,
            LeaseState::Released => semantic_v1::LeaseState::Released,
            LeaseState::Expired => semantic_v1::LeaseState::Expired,
            LeaseState::Revoked => semantic_v1::LeaseState::Revoked,
            LeaseState::Failed | LeaseState::Quarantined => semantic_v1::LeaseState::Failed,
        } as i32,
        fence_token: lease.fence_token,
        expires_at: lease.expires_at_unix_ms.map(timestamp_from_unix_ms),
    }
}

fn legacy_holder(
    mutation: Option<&core_v1::MutationContext>,
    fallback: &str,
) -> semantic::Identity {
    let id = mutation
        .and_then(|mutation| mutation.request.as_ref())
        .map(|request| {
            if !request.tenant_id.is_empty() || !request.project_id.is_empty() {
                format!("legacy/{}/{}", request.tenant_id, request.project_id)
            } else if !request.request_id.is_empty() {
                format!("legacy/request/{}", request.request_id)
            } else {
                format!("legacy/{fallback}")
            }
        })
        .unwrap_or_else(|| format!("legacy/{fallback}"));
    semantic::Identity { id, generation: 1 }
}

fn cgroup_limits(
    cpu: Option<&core_v1::CpuRequirements>,
    memory: Option<&core_v1::MemoryRequirements>,
) -> Result<CgroupLimits, Status> {
    if let Some(cpu) = cpu {
        if cpu.limit_millicores > 0
            && cpu.request_millicores > 0
            && cpu.request_millicores > cpu.limit_millicores
        {
            return Err(Status::invalid_argument(
                "cpu request_millicores cannot exceed limit_millicores",
            ));
        }
    }
    if let Some(memory) = memory {
        if memory.limit_bytes > 0
            && memory.request_bytes > 0
            && memory.request_bytes > memory.limit_bytes
        {
            return Err(Status::invalid_argument(
                "memory request_bytes cannot exceed limit_bytes",
            ));
        }
    }
    Ok(CgroupLimits {
        cpu_max_millicores: cpu
            .and_then(|value| (value.limit_millicores > 0).then_some(value.limit_millicores)),
        memory_max_bytes: memory
            .and_then(|value| (value.limit_bytes > 0).then_some(value.limit_bytes)),
        cpuset_cpus: None,
    })
}

fn provider_status(error: ProviderError) -> Status {
    let code = match error.reason_code.as_str() {
        "STALE_INVENTORY_GENERATION" | "STALE_FENCE_TOKEN" | "RESOURCE_QUARANTINED" => {
            tonic::Code::FailedPrecondition
        }
        "INSUFFICIENT_RESOURCES" => tonic::Code::ResourceExhausted,
        "LEASE_NOT_FOUND" | "DEVICE_NOT_FOUND" | "RESOURCE_NOT_FOUND" => tonic::Code::NotFound,
        _ => tonic::Code::Internal,
    };
    semantic_status(code, &error.reason_code, &error.message)
}

/// Preserve the stable semantic rejection code in gRPC trailers. The human
/// message remains descriptive only; Node Agent projections copy this metadata
/// into a typed `cyrene.semantic.v1.Rejection` detail for remote consumers.
fn semantic_status(code: tonic::Code, reason_code: &str, message: &str) -> Status {
    let mut status = Status::new(code, format!("{reason_code}: {message}"));
    if let Ok(value) = reason_code.parse() {
        status.metadata_mut().insert("x-cyrene-reason-code", value);
    }
    status
}

fn proto_duration(duration: prost_types::Duration) -> Result<Duration, Status> {
    if duration.seconds < 0 || !(0..1_000_000_000).contains(&duration.nanos) {
        return Err(Status::invalid_argument(
            "duration must be non-negative and normalized",
        ));
    }
    Ok(Duration::from_secs(duration.seconds as u64)
        .saturating_add(Duration::from_nanos(duration.nanos as u64)))
}

fn unix_ms_from_timestamp(timestamp: prost_types::Timestamp, field: &str) -> Result<u64, Status> {
    if timestamp.seconds < 0
        || !(0..1_000_000_000).contains(&timestamp.nanos)
        || timestamp.nanos % 1_000_000 != 0
    {
        return Err(Status::invalid_argument(format!(
            "{field} must be non-negative, normalized, and millisecond-aligned"
        )));
    }
    let seconds = timestamp.seconds as u64;
    Ok(seconds
        .saturating_mul(1_000)
        .saturating_add((timestamp.nanos as u64) / 1_000_000))
}

fn expires_after(duration: Duration) -> u64 {
    now_unix_ms().saturating_add(duration.as_millis().min(u64::MAX as u128) as u64)
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

fn timestamp_from_unix_ms(unix_ms: u64) -> prost_types::Timestamp {
    prost_types::Timestamp {
        seconds: (unix_ms / 1_000).min(i64::MAX as u64) as i64,
        nanos: ((unix_ms % 1_000) * 1_000_000) as i32,
    }
}

fn to_proto_duration(duration: Duration) -> prost_types::Duration {
    prost_types::Duration {
        seconds: duration.as_secs().min(i64::MAX as u64) as i64,
        nanos: duration.subsec_nanos() as i32,
    }
}

fn operation_event_matches(event: &core_v1::OperationEvent, names: &[String]) -> bool {
    names.is_empty()
        || event
            .operation
            .as_ref()
            .is_some_and(|operation| names.contains(&operation.name))
}

fn runtime_event_kind(event_type: core_v1::RuntimeEventType) -> &'static str {
    match event_type {
        core_v1::RuntimeEventType::InstanceStateChanged => "worker.state.changed",
        core_v1::RuntimeEventType::WatchdogTriggered => "worker.watchdog.triggered",
        core_v1::RuntimeEventType::OomKilled => "worker.oom.killed",
        core_v1::RuntimeEventType::AdapterDegraded => "provider.degraded",
        core_v1::RuntimeEventType::CleanupCompleted => "worker.cleanup.completed",
        core_v1::RuntimeEventType::KernelReconciled => "kernel.reconciled",
        core_v1::RuntimeEventType::Unspecified => "kernel.observation",
    }
}

fn now_timestamp() -> prost_types::Timestamp {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    prost_types::Timestamp {
        seconds: elapsed.as_secs() as i64,
        nanos: elapsed.subsec_nanos() as i32,
    }
}

#[cfg(test)]
#[allow(deprecated)]
mod tests {
    use super::*;
    use cy_kernel_api::{
        CapabilityFact, CleanupReport, LaunchPlan, NodeCapabilities, ProcessCondition,
        ProcessHandle, ProcessRuntime, ResolvedLaunchPlan, StopRequest,
    };
    use cy_resource_manager::InMemoryResourceManager;
    use std::{collections::BTreeMap, path::PathBuf};
    use tokio_stream::StreamExt;

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

        fn read_health(
            &self,
            _device_id: &str,
        ) -> Result<cy_kernel_api::HealthReport, ProviderError> {
            Err(ProviderError::new("test-adapter", "UNUSED", "no resources"))
        }
    }

    #[derive(Debug, Clone)]
    struct TestHardware {
        resource: semantic::Resource,
    }

    impl HostInventoryProvider for TestHardware {
        fn probe_inventory(&self) -> Result<InventorySnapshot, ProviderError> {
            Ok(InventorySnapshot {
                generation: 1,
                resources: vec![self.resource.clone()],
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
            Ok(vec![self.resource.clone()])
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

    fn test_resource() -> semantic::Resource {
        semantic::Resource {
            identity: semantic::Identity {
                id: "resource-1".to_string(),
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

    fn semantic_lease_adapter() -> KernelServiceAdapter {
        let resource = test_resource();
        let hardware = Arc::new(TestHardware {
            resource: resource.clone(),
        });
        let daemon = Arc::new(KernelDaemon::new(
            hardware.clone(),
            hardware,
            Arc::new(InMemoryResourceManager::new("node", vec![resource])),
            Arc::new(FakeSandbox),
            "node",
            7,
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
                start_time_ticks: None,
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
            instance: SandboxedProcess::new(
                Arc::new(FakeSandbox),
                LaunchPlan {
                    instance_name: instance_name.to_string(),
                    executable: PathBuf::from("worker"),
                    args: Vec::new(),
                    environment: BTreeMap::new(),
                    cgroup_name: format!("instance-{instance_name}"),
                    limits: CgroupLimits::default(),
                },
                DeviceBinding {
                    resource_id: "none".to_string(),
                    nodes: Vec::new(),
                    environment: BTreeMap::new(),
                    required_gids: Vec::new(),
                    enforcement: EnforcementMode::Unenforced,
                    adapter_id: "test".to_string(),
                    reason_code: "TEST".to_string(),
                },
            ),
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
            last_heartbeat: Instant::now(),
            last_heartbeat_at: None,
            runtime_state: core_v1::PluginRuntimeState::Starting as i32,
            health: None,
            restart_count: 0,
            watchdog_triggered: false,
            control: None,
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
                },
            })
        }
    }

    fn semantic_worker_adapter() -> KernelServiceAdapter {
        let resource = test_resource();
        let hardware = Arc::new(TestHardware {
            resource: resource.clone(),
        });
        let daemon = Arc::new(KernelDaemon::new(
            hardware.clone(),
            hardware,
            Arc::new(InMemoryResourceManager::new("node", vec![resource])),
            Arc::new(FakeSandbox),
            "node",
            7,
        ));
        KernelServiceAdapter::new(daemon, Arc::new(TestWorkerResolver))
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
    fn semantic_authority_renews_leases_and_binds_endpoint_grants_to_the_fence() {
        use core_v1::{
            kernel_authority_service_server::KernelAuthorityService, AcquireSemanticLeaseRequest,
            AuthorizeEndpointRequest, PublishEndpointRequest, RenewLeaseRequest,
            RevokeEndpointRequest,
        };

        let adapter = semantic_lease_adapter();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let lease = runtime
            .block_on(
                adapter.acquire_lease(Request::new(AcquireSemanticLeaseRequest {
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
            .block_on(adapter.renew_lease(Request::new(RenewLeaseRequest {
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
            principal: semantic::Identity {
                id: "principal-1".to_string(),
                generation: 1,
            },
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
                adapter.publish_endpoint(Request::new(PublishEndpointRequest {
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
        let grant = runtime
            .block_on(
                adapter.authorize_endpoint(Request::new(AuthorizeEndpointRequest {
                    context: Some(authority_context("authorize-endpoint")),
                    grant: Some(semantic_v1::EndpointGrant {
                        identity: Some(semantic_v1::Identity {
                            id: "grant-1".to_string(),
                            generation: 1,
                        }),
                        endpoint: endpoint.identity.clone(),
                        grantee: renewed.holder.clone(),
                        lease: renewed.identity.clone(),
                        fence_token: renewed.fence_token,
                        expires_at: renewed.expires_at.clone(),
                    }),
                })),
            )
            .unwrap()
            .into_inner();
        assert_eq!(grant.fence_token, renewed.fence_token);

        runtime
            .block_on(adapter.revoke_endpoint(Request::new(RevokeEndpointRequest {
                context: Some(authority_context("revoke-endpoint")),
                grant: grant.identity.clone(),
            })))
            .unwrap();
        assert!(adapter.endpoint_grants.lock().unwrap().is_empty());
    }

    #[test]
    fn authority_worker_operation_and_event_paths_do_not_use_plugin_or_lro_types() {
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
                adapter.acquire_lease(Request::new(AcquireSemanticLeaseRequest {
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
            .block_on(adapter.start_worker(Request::new(StartWorkerRequest {
                context: Some(authority_context("start-worker")),
                worker: Some(worker.clone()),
            })))
            .unwrap()
            .into_inner();
        assert_eq!(started.kind, "worker.start");
        assert_eq!(started.state, semantic_v1::OperationState::Running as i32);

        let running = runtime
            .block_on(
                adapter.heartbeat_worker(Request::new(HeartbeatWorkerRequest {
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
                adapter.create_operation(Request::new(CreateOperationRequest {
                    context: Some(authority_context("create-operation")),
                    operation: Some(operation.clone()),
                })),
            )
            .unwrap();
        let mut reported = operation;
        reported.state = semantic_v1::OperationState::Running as i32;
        runtime
            .block_on(
                adapter.report_operation(Request::new(ReportOperationRequest {
                    context: Some(authority_context("report-operation")),
                    operation: Some(reported),
                })),
            )
            .unwrap();
        let cancelling = runtime
            .block_on(
                adapter.cancel_operation(Request::new(CancelSemanticOperationRequest {
                    context: Some(authority_context("cancel-operation")),
                    operation: Some(semantic_v1::Identity {
                        id: "operation-1".to_string(),
                        generation: 1,
                    }),
                })),
            )
            .unwrap()
            .into_inner();
        assert_eq!(
            cancelling.state,
            semantic_v1::OperationState::Cancelling as i32
        );

        let cursor = semantic_v1::EventCursor {
            source: Some(to_semantic_proto_identity(&adapter.semantic_event_source())),
            sequence: 0,
        };
        let events = runtime
            .block_on(
                adapter.subscribe_events(Request::new(SubscribeEventsRequest {
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
                adapter.subscribe_events(Request::new(SubscribeEventsRequest {
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
                adapter.subscribe_events(Request::new(SubscribeEventsRequest {
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
            .block_on(adapter.stop_worker(Request::new(StopWorkerRequest {
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
}
