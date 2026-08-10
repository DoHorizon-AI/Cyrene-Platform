//! CYRENE 节点内核守护进程组合根 (Kernel Daemon Composition Root).
//!
//! 【内核守护进程职责与设计哲学】
//! 本模块作为节点内核守护进程的装配中心（Composition Root），负责将硬件探测、资源租约与沙箱运行时等各个六边形端口组合连接：
//! 1. **事实汇聚与映射**：聚合底层适配器上报的不可变硬件事实，将其严格、诚实地映射为 Core v1 Protobuf 消息（[`core_v1::KernelCapabilities`]）；
//! 2. **绝不猜测（No Guessing Invariant）**：对于探测不到的显存容量、NUMA 节点或拓扑链路，严格上报未知，绝不用启发式猜测伪造数据；
//! 3. **职责边界**：内核守护进程专职负责单机节点物理事实与资源隔离，不包含远程制品下载、全局调度仲裁或跨重启接管僵尸进程的逻辑。

#![forbid(unsafe_code)]

use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use cy_adapter_client::{HardwareAdapterEndpoint, UdsHardwareAdapterRegistry};
use cy_kernel_api::{
    AcceleratorKind, AcceleratorLinkType, AcceleratorProvider, AcceleratorVendor, CgroupLimits,
    DeviceBinding, EnforcementMode, HostInventoryProvider, InstalledPluginResolver,
    InventorySnapshot, LeaseState, ProviderError, ResourceLease, ResourceLeaseManager,
    ResourceRequest, SandboxBackend, VerifiedInstallation,
};
use cy_proto::core_v1;
use tokio_stream::{iter, Stream};
use tonic::{Request, Response, Status};

mod sandboxed_process;
use sandboxed_process::{SandboxedProcess, SandboxedProcessState};

/// 节点内核守护进程核心结构体
pub struct KernelDaemon {
    /// 宿主机硬件清单提供者
    inventory_provider: Arc<dyn HostInventoryProvider>,
    /// 硬件加速卡提供者
    accelerator_provider: Arc<dyn AcceleratorProvider>,
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
        accelerator_provider: Arc<dyn AcceleratorProvider>,
        resources: Arc<dyn ResourceLeaseManager>,
        sandbox: Arc<dyn SandboxBackend>,
        node_id: impl Into<String>,
        node_epoch: u64,
    ) -> Self {
        Self {
            inventory_provider,
            accelerator_provider,
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

    /// 为租约内的所有加速卡合并一份设备绑定。
    ///
    /// 当前 SandboxBackend 接口以单份 DeviceBinding 表达设备集合，因此多卡租约在这里合并
    /// 设备节点、环境变量与 GID；任何厂商适配器冲突都会 fail closed。
    pub fn binding_for_lease(&self, lease: &ResourceLease) -> Result<DeviceBinding, ProviderError> {
        let snapshot = self.inventory()?;
        let mut bindings = Vec::with_capacity(lease.allocations.len());
        for allocation in &lease.allocations {
            let device = snapshot
                .devices
                .iter()
                .find(|device| device.device_id == allocation.device_id)
                .ok_or_else(|| {
                    ProviderError::new(
                        "kernel-daemon",
                        "LEASE_DEVICE_NOT_IN_INVENTORY",
                        &allocation.device_id,
                    )
                })?;
            bindings.push(
                self.accelerator_provider
                    .create_binding_for_generation(device, lease.inventory_generation)?,
            );
        }
        merge_bindings(bindings)
    }

    /// 根据当前节点底层最新探测事实，组装 Core v1 规范的 [`core_v1::KernelCapabilities`] 能力事实消息。
    ///
    /// # 诚实上报原则
    /// 严格如实上报硬件探测结果，绝不针对缺失的显存、NUMA 或拓扑值进行主观猜测。
    pub fn get_kernel_capabilities(&self) -> Result<core_v1::KernelCapabilities, ProviderError> {
        let snapshot = self.inventory()?;
        let accelerators = snapshot
            .devices
            .iter()
            .map(|device| core_v1::AcceleratorDevice {
                device_id: device.device_id.clone(),
                kind: to_proto_kind(device.kind) as i32,
                vendor: to_proto_vendor(device.vendor) as i32,
                other_vendor_id: String::new(),
                device_family: device.device_family.clone(),
                pci_address: device.pci_address.clone().unwrap_or_default(),
                total_memory_bytes: device.total_memory_bytes.unwrap_or_default(),
                allocatable_memory_bytes: device.allocatable_memory_bytes.unwrap_or_default(),
                features: device.features.clone(),
                partitions: Vec::new(),
                health: Some(core_v1::HealthReport {
                    status: device.health.healthy.map_or(
                        core_v1::HealthStatus::Unknown as i32,
                        |healthy| {
                            if healthy {
                                core_v1::HealthStatus::Healthy as i32
                            } else {
                                core_v1::HealthStatus::Degraded as i32
                            }
                        },
                    ),
                    reason_code: device.health.reason_code.clone(),
                    summary: device.health.summary.clone(),
                }),
                numa_node: device.numa_node,
                links: device
                    .links
                    .iter()
                    .map(|link| core_v1::AcceleratorLink {
                        peer_device_id: link.peer_device_id.clone(),
                        link_type: to_proto_link_type(link.link_type) as i32,
                        link_count: link.link_count.unwrap_or_default(),
                        width: link.width.unwrap_or_default(),
                        bandwidth_bytes_per_second: link
                            .bandwidth_bytes_per_second
                            .unwrap_or_default(),
                        stable: link.stable,
                    })
                    .collect(),
            })
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
            accelerators,
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
}

impl Default for WorkerHeartbeatConfig {
    fn default() -> Self {
        Self {
            socket_path: PathBuf::from("/run/cyrene/kernel.sock"),
            interval: Duration::from_secs(5),
            timeout: Duration::from_secs(20),
            graceful_stop: Duration::from_secs(10),
        }
    }
}

struct ManagedProcess {
    instance: SandboxedProcess,
    lease: Option<core_v1::ResourceLeaseRef>,
    plugin: core_v1::InstalledPluginRef,
    generation: u64,
    accepted_sequence: u64,
    last_heartbeat: Instant,
    last_heartbeat_at: Option<prost_types::Timestamp>,
    runtime_state: i32,
    health: Option<core_v1::HealthReport>,
    restart_count: u32,
    watchdog_triggered: bool,
}

/// Core v1 KernelService 到真实资源管理器与 SandboxBackend 的最小服务适配层。
#[derive(Clone)]
pub struct KernelServiceAdapter {
    daemon: Arc<KernelDaemon>,
    resolver: Arc<dyn InstalledPluginResolver>,
    instances: Arc<Mutex<HashMap<String, ManagedProcess>>>,
    operations: Arc<Mutex<HashMap<String, core_v1::Operation>>>,
    heartbeat: WorkerHeartbeatConfig,
}

impl KernelServiceAdapter {
    pub fn new(daemon: Arc<KernelDaemon>, resolver: Arc<dyn InstalledPluginResolver>) -> Self {
        Self {
            daemon,
            resolver,
            instances: Arc::new(Mutex::new(HashMap::new())),
            operations: Arc::new(Mutex::new(HashMap::new())),
            heartbeat: WorkerHeartbeatConfig::default(),
        }
    }

    pub fn with_worker_heartbeat(mut self, heartbeat: WorkerHeartbeatConfig) -> Self {
        self.heartbeat = heartbeat;
        self
    }

    pub fn server(&self) -> core_v1::kernel_service_server::KernelServiceServer<Self> {
        core_v1::kernel_service_server::KernelServiceServer::new(self.clone())
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

    fn remember_operation(&self, operation: core_v1::Operation) -> core_v1::Operation {
        let mut operations = self.operations.lock().expect("operation lock poisoned");
        operations.insert(operation.name.clone(), operation.clone());
        operation
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
            let lease = {
                let mut instances = self.instances.lock().expect("instance lock poisoned");
                let Some(process) = instances.get_mut(&name) else {
                    continue;
                };
                process.watchdog_triggered = true;
                match process.instance.stop(&cy_kernel_api::StopRequest {
                    grace_period: self.heartbeat.graceful_stop,
                    immediate: false,
                }) {
                    Ok(report) if report.complete => process.lease.clone(),
                    Ok(_) | Err(_) => None,
                }
            };
            if let Some(lease) = lease {
                if self
                    .daemon
                    .release(&lease.lease_name, lease.fence_token)
                    .is_ok()
                {
                    self.instances
                        .lock()
                        .expect("instance lock poisoned")
                        .remove(&name);
                }
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
        let internal = resource_request(&lease_name, generation, &requirements)?;
        let lease = self.daemon.reserve(internal).map_err(provider_status)?;
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
                let internal = resource_request(&lease_name, generation, &requirements)?;
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
        };
        let mut plan = match self
            .resolver
            .resolve_launch_plan(&installation, &instance_name)
        {
            Ok(plan) => plan,
            Err(error) => {
                self.release_owned_lease(owned_lease, &lease);
                return Err(provider_status(error));
            }
        };
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
        self.instances
            .lock()
            .expect("instance lock poisoned")
            .insert(
                instance_name.clone(),
                ManagedProcess {
                    instance,
                    lease: Some(core_v1::ResourceLeaseRef {
                        lease_name: lease.name,
                        fence_token: lease.fence_token,
                    }),
                    plugin,
                    generation: lease.fence_token,
                    accepted_sequence: 0,
                    last_heartbeat: Instant::now(),
                    last_heartbeat_at: None,
                    runtime_state: core_v1::PluginRuntimeState::Starting as i32,
                    health: None,
                    restart_count: 0,
                    watchdog_triggered: false,
                },
            );
        Ok(Response::new(
            self.operation_success(operation_name, instance_name),
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
        let lease = {
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
            if !report.complete {
                let error = ProviderError::new(
                    "kernel-daemon",
                    "RESOURCE_QUARANTINED",
                    &report.reason_code,
                );
                return Ok(Response::new(self.operation_failure(
                    operation_name,
                    request.process_name,
                    &error,
                )));
            }
            process.lease.clone()
        };
        if let Some(lease) = lease {
            self.daemon
                .release(&lease.lease_name, lease.fence_token)
                .map_err(provider_status)?;
        }
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
        _request: Request<core_v1::CancelOperationRequest>,
    ) -> Result<Response<core_v1::Operation>, Status> {
        Err(Status::unimplemented(
            "operation cancellation is not part of the P2 kernel baseline",
        ))
    }

    type WatchOperationsStream = std::pin::Pin<
        Box<dyn Stream<Item = Result<core_v1::OperationEvent, Status>> + Send + 'static>,
    >;

    async fn watch_operations(
        &self,
        _request: Request<core_v1::WatchOperationsRequest>,
    ) -> Result<Response<Self::WatchOperationsStream>, Status> {
        Ok(Response::new(Box::pin(iter(Vec::<
            Result<core_v1::OperationEvent, Status>,
        >::new()))))
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
        self.instances
            .lock()
            .expect("instance lock poisoned")
            .get(&name)
            .map(|process| Response::new(to_plugin_instance(&self.daemon, &name, process)))
            .ok_or_else(|| Status::not_found("plugin instance is not managed by this Kernel"))
    }

    async fn list_plugin_instances(
        &self,
        request: Request<core_v1::ListPluginInstancesRequest>,
    ) -> Result<Response<core_v1::ListPluginInstancesResponse>, Status> {
        let request = request.into_inner();
        let filters = request.state_filter;
        let plugins = self
            .instances
            .lock()
            .expect("instance lock poisoned")
            .iter()
            .filter(|(_, process)| filters.is_empty() || filters.contains(&process.runtime_state))
            .map(|(name, process)| to_plugin_instance(&self.daemon, name, process))
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
        if request.plugin_instance_name.is_empty() || request.sequence_number == 0 {
            return Err(Status::invalid_argument(
                "plugin_instance_name and a non-zero sequence_number are required",
            ));
        }
        let mut instances = self.instances.lock().expect("instance lock poisoned");
        let Some(process) = instances.get_mut(&request.plugin_instance_name) else {
            return Ok(Response::new(self.heartbeat_response(
                core_v1::HeartbeatDisposition::UnknownInstance,
                0,
                request.generation,
            )));
        };
        if request.generation != process.generation {
            return Ok(Response::new(self.heartbeat_response(
                core_v1::HeartbeatDisposition::StaleGeneration,
                process.accepted_sequence,
                process.generation,
            )));
        }
        if process.watchdog_triggered {
            return Ok(Response::new(core_v1::ReportHeartbeatResponse {
                disposition: core_v1::HeartbeatDisposition::Duplicate as i32,
                accepted_sequence_number: process.accepted_sequence,
                server_time: Some(now_timestamp()),
                next_heartbeat_after: None,
                desired_state: core_v1::DesiredPluginState::Stopped as i32,
                desired_generation: process.generation,
            }));
        }
        if request.sequence_number <= process.accepted_sequence {
            return Ok(Response::new(self.heartbeat_response(
                core_v1::HeartbeatDisposition::Duplicate,
                process.accepted_sequence,
                process.generation,
            )));
        }
        process.accepted_sequence = request.sequence_number;
        process.last_heartbeat = Instant::now();
        process.last_heartbeat_at = request.observed_at.or_else(|| Some(now_timestamp()));
        process.runtime_state = request.runtime_state;
        process.health = request.health;
        process.restart_count = request.restart_count;
        Ok(Response::new(self.heartbeat_response(
            core_v1::HeartbeatDisposition::Accepted,
            process.accepted_sequence,
            process.generation,
        )))
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
        health: process.health.clone(),
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

/// 将内部芯片类型转换为 Protobuf 协议枚举
fn to_proto_kind(kind: AcceleratorKind) -> core_v1::AcceleratorKind {
    match kind {
        AcceleratorKind::Gpu => core_v1::AcceleratorKind::Gpu,
        AcceleratorKind::Npu => core_v1::AcceleratorKind::Npu,
        AcceleratorKind::Tpu => core_v1::AcceleratorKind::Tpu,
        AcceleratorKind::Other => core_v1::AcceleratorKind::Other,
    }
}

/// 将内部厂商类型转换为 Protobuf 协议枚举
fn to_proto_vendor(vendor: AcceleratorVendor) -> core_v1::AcceleratorVendor {
    match vendor {
        AcceleratorVendor::Nvidia => core_v1::AcceleratorVendor::Nvidia,
        AcceleratorVendor::Amd => core_v1::AcceleratorVendor::Amd,
        AcceleratorVendor::HuaweiAscend => core_v1::AcceleratorVendor::HuaweiAscend,
        AcceleratorVendor::Intel => core_v1::AcceleratorVendor::Intel,
        AcceleratorVendor::Other => core_v1::AcceleratorVendor::Other,
    }
}

/// 将内部互联总线类型转换为 Protobuf 协议枚举
fn to_proto_link_type(link_type: AcceleratorLinkType) -> core_v1::AcceleratorLinkType {
    match link_type {
        AcceleratorLinkType::Pcie => core_v1::AcceleratorLinkType::Pcie,
        AcceleratorLinkType::Nvlink => core_v1::AcceleratorLinkType::Nvlink,
        AcceleratorLinkType::Xgmi => core_v1::AcceleratorLinkType::Xgmi,
        AcceleratorLinkType::Other => core_v1::AcceleratorLinkType::Other,
    }
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
            device_id: "none".to_string(),
            nodes: Vec::new(),
            environment: BTreeMap::new(),
            required_gids: Vec::new(),
            enforcement: EnforcementMode::Unenforced,
            adapter_id: "kernel-daemon".to_string(),
            reason_code: "NO_ACCELERATOR_ALLOCATION".to_string(),
        });
    };
    let mut nodes = first.nodes;
    let mut environment = first.environment;
    let mut required_gids = first.required_gids;
    let enforcement = first.enforcement;
    let mut adapter_ids = vec![first.adapter_id.clone()];
    let mut device_ids = vec![first.device_id];
    for binding in bindings.into_iter().skip(1) {
        if binding.enforcement != enforcement {
            return Err(ProviderError::new(
                "kernel-daemon",
                "MIXED_DEVICE_ENFORCEMENT",
                "a multi-device binding must use one enforcement mode",
            ));
        }
        if !adapter_ids.contains(&binding.adapter_id) {
            adapter_ids.push(binding.adapter_id.clone());
        }
        device_ids.push(binding.device_id);
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
                        "CONFLICTING_DEVICE_ENVIRONMENT",
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
        device_id: device_ids.join(","),
        nodes,
        environment,
        required_gids,
        enforcement,
        adapter_id: adapter_ids.join(","),
        reason_code: "DEVICE_BINDING_CREATED_BY_UDS_ADAPTERS".to_string(),
    })
}

fn resource_request(
    lease_name: &str,
    generation: u64,
    requirements: &core_v1::ResourceRequirements,
) -> Result<ResourceRequest, Status> {
    let mut count = 0usize;
    let mut vendor = None;
    let mut min_memory_bytes: Option<u64> = None;
    for accelerator in &requirements.accelerators {
        count = count
            .checked_add(accelerator.count as usize)
            .ok_or_else(|| Status::invalid_argument("accelerator count overflow"))?;
        if accelerator.vendor != core_v1::AcceleratorVendor::Unspecified as i32 {
            let requested_vendor = core_v1::AcceleratorVendor::try_from(accelerator.vendor)
                .map_err(|_| Status::invalid_argument("unknown accelerator vendor"))?;
            let requested_vendor = match requested_vendor {
                core_v1::AcceleratorVendor::Nvidia => AcceleratorVendor::Nvidia,
                core_v1::AcceleratorVendor::Amd => AcceleratorVendor::Amd,
                core_v1::AcceleratorVendor::HuaweiAscend => AcceleratorVendor::HuaweiAscend,
                core_v1::AcceleratorVendor::Intel => AcceleratorVendor::Intel,
                core_v1::AcceleratorVendor::Other | core_v1::AcceleratorVendor::Unspecified => {
                    AcceleratorVendor::Other
                }
            };
            if vendor.is_some_and(|current| current != requested_vendor) {
                return Err(Status::invalid_argument(
                    "multiple accelerator vendors are not supported in one P2 request",
                ));
            }
            vendor = Some(requested_vendor);
        }
        min_memory_bytes = match min_memory_bytes {
            Some(current) => Some(current.max(accelerator.min_memory_bytes_per_device)),
            None => Some(accelerator.min_memory_bytes_per_device),
        };
    }
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
        count,
        vendor,
        min_memory_bytes: min_memory_bytes.filter(|value| *value > 0),
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
            LeaseState::Failed => core_v1::LeaseState::Failed,
            LeaseState::Quarantined => core_v1::LeaseState::Failed,
        } as i32,
        granted,
        accelerators: lease
            .allocations
            .into_iter()
            .map(|allocation| core_v1::AcceleratorAllocation {
                allocation_id: allocation.allocation_id,
                device_id: allocation.device_id,
                partition_id: String::new(),
                granted_memory_bytes: allocation.granted_memory_bytes.unwrap_or_default(),
                enforcement: to_proto_enforcement(allocation.enforcement) as i32,
            })
            .collect(),
        enforcement,
        expires_at: None,
        fence_token: lease.fence_token,
        inventory_generation: lease.inventory_generation,
    })
}

fn provider_status(error: ProviderError) -> Status {
    let message = format!("{}: {}", error.reason_code, error.message);
    match error.reason_code.as_str() {
        "STALE_INVENTORY_GENERATION" | "STALE_FENCE_TOKEN" | "RESOURCE_QUARANTINED" => {
            Status::failed_precondition(message)
        }
        "INSUFFICIENT_RESOURCES" => Status::resource_exhausted(message),
        "LEASE_NOT_FOUND" | "DEVICE_NOT_FOUND" => Status::not_found(message),
        _ => Status::internal(message),
    }
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
mod tests {
    use super::*;
    use cy_kernel_api::{
        CapabilityFact, CleanupReport, LaunchPlan, NodeCapabilities, ProcessCondition,
        ProcessHandle, ProcessRuntime, StopRequest,
    };
    use cy_resource_manager::InMemoryResourceManager;
    use std::{collections::BTreeMap, path::PathBuf};

    #[derive(Debug)]
    struct EmptyHardware;

    impl HostInventoryProvider for EmptyHardware {
        fn probe_inventory(&self) -> Result<InventorySnapshot, ProviderError> {
            Ok(InventorySnapshot {
                generation: 1,
                devices: Vec::new(),
                capabilities: NodeCapabilities {
                    ready: true,
                    facts: Vec::new(),
                    enforcement: Vec::new(),
                },
            })
        }
    }

    impl AcceleratorProvider for EmptyHardware {
        fn adapter_id(&self) -> &str {
            "test-adapter"
        }

        fn probe_inventory(&self) -> Result<Vec<cy_kernel_api::AcceleratorDevice>, ProviderError> {
            Ok(Vec::new())
        }

        fn create_binding(
            &self,
            _device: &cy_kernel_api::AcceleratorDevice,
        ) -> Result<DeviceBinding, ProviderError> {
            Err(ProviderError::new(
                "test-adapter",
                "UNUSED",
                "no accelerators",
            ))
        }

        fn read_health(
            &self,
            _device_id: &str,
        ) -> Result<cy_kernel_api::HealthReport, ProviderError> {
            Err(ProviderError::new(
                "test-adapter",
                "UNUSED",
                "no accelerators",
            ))
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

    struct UnusedResolver;

    impl InstalledPluginResolver for UnusedResolver {
        fn resolve_launch_plan(
            &self,
            _installation: &VerifiedInstallation,
            _instance_name: &str,
        ) -> Result<LaunchPlan, ProviderError> {
            Err(ProviderError::new(
                "test",
                "UNUSED",
                "not launched in this test",
            ))
        }
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
            });
        adapter.instances.lock().unwrap().insert(
            "worker_1".to_string(),
            ManagedProcess {
                instance: SandboxedProcess::new(
                    Arc::new(FakeSandbox),
                    LaunchPlan {
                        instance_name: "worker_1".to_string(),
                        executable: PathBuf::from("worker"),
                        args: Vec::new(),
                        environment: BTreeMap::new(),
                        cgroup_name: "instance-worker_1".to_string(),
                        limits: CgroupLimits::default(),
                    },
                    DeviceBinding {
                        device_id: "none".to_string(),
                        nodes: Vec::new(),
                        environment: BTreeMap::new(),
                        required_gids: Vec::new(),
                        enforcement: EnforcementMode::Unenforced,
                        adapter_id: "test".to_string(),
                        reason_code: "TEST".to_string(),
                    },
                ),
                lease: None,
                plugin: core_v1::InstalledPluginRef {
                    installation_name: "worker_1".to_string(),
                    plugin_id: "test".to_string(),
                    version: "1".to_string(),
                    component_id: "test".to_string(),
                    manifest_digest: "sha256:test".to_string(),
                    artifact_digest: "sha256:test".to_string(),
                    verified_signature_identity: "test".to_string(),
                },
                generation: 99,
                accepted_sequence: 0,
                last_heartbeat: Instant::now(),
                last_heartbeat_at: None,
                runtime_state: core_v1::PluginRuntimeState::Starting as i32,
                health: None,
                restart_count: 0,
                watchdog_triggered: false,
            },
        );
        adapter
    }

    #[test]
    fn resource_limits_are_mapped_without_relaxing_request_validation() {
        let request = resource_request(
            "lease-1",
            4,
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
                accelerators: Vec::new(),
            },
        )
        .unwrap();
        assert_eq!(request.limits.cpu_max_millicores, Some(750));
        assert_eq!(request.limits.memory_max_bytes, Some(2048));

        let error = resource_request(
            "lease-2",
            4,
            &core_v1::ResourceRequirements {
                cpu: Some(core_v1::CpuRequirements {
                    request_millicores: 751,
                    limit_millicores: 750,
                }),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert_eq!(error.code(), tonic::Code::InvalidArgument);
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
    fn multi_adapter_bindings_preserve_provenance_without_relaxing_enforcement() {
        let merged = merge_bindings(vec![
            DeviceBinding {
                device_id: "nvidia-0".to_string(),
                nodes: Vec::new(),
                environment: BTreeMap::new(),
                required_gids: vec![44],
                enforcement: EnforcementMode::Hard,
                adapter_id: "nvidia".to_string(),
                reason_code: "TEST".to_string(),
            },
            DeviceBinding {
                device_id: "amd-0".to_string(),
                nodes: Vec::new(),
                environment: BTreeMap::new(),
                required_gids: vec![45],
                enforcement: EnforcementMode::Hard,
                adapter_id: "amd".to_string(),
                reason_code: "TEST".to_string(),
            },
        ])
        .unwrap();

        assert_eq!(merged.device_id, "nvidia-0,amd-0");
        assert_eq!(merged.adapter_id, "amd,nvidia");
        assert_eq!(merged.required_gids, vec![44, 45]);
        assert_eq!(merged.enforcement, EnforcementMode::Hard);
    }

    #[test]
    fn multi_adapter_bindings_reject_mixed_enforcement() {
        let error = merge_bindings(vec![
            DeviceBinding {
                device_id: "nvidia-0".to_string(),
                nodes: Vec::new(),
                environment: BTreeMap::new(),
                required_gids: Vec::new(),
                enforcement: EnforcementMode::Hard,
                adapter_id: "nvidia".to_string(),
                reason_code: "TEST".to_string(),
            },
            DeviceBinding {
                device_id: "virtual-0".to_string(),
                nodes: Vec::new(),
                environment: BTreeMap::new(),
                required_gids: Vec::new(),
                enforcement: EnforcementMode::VisibilityOnly,
                adapter_id: "virtual".to_string(),
                reason_code: "TEST".to_string(),
            },
        ])
        .unwrap_err();

        assert_eq!(error.reason_code, "MIXED_DEVICE_ENFORCEMENT");
    }
}
