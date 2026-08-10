//! CYRENE 节点内核守护进程组合根 (Kernel Daemon Composition Root).
//!
//! 【内核守护进程职责与设计哲学】
//! 本模块作为节点内核守护进程的装配中心（Composition Root），负责将硬件探测、资源租约与沙箱运行时等各个六边形端口组合连接：
//! 1. **事实汇聚与映射**：聚合底层适配器上报的不可变硬件事实，将其严格、诚实地映射为 Core v1 Protobuf 消息（[`core_v1::KernelCapabilities`]）；
//! 2. **绝不猜测（No Guessing Invariant）**：对于探测不到的显存容量、NUMA 节点或拓扑链路，严格上报未知，绝不用启发式猜测伪造数据；
//! 3. **职责边界**：内核守护进程专职负责单机节点物理事实与资源隔离，不包含远程制品下载、全局调度仲裁或跨重启接管僵尸进程的逻辑。

use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use cy_hardware_discovery::NvidiaSmiProvider;
use cy_kernel_api::{
    AcceleratorKind, AcceleratorLinkType, AcceleratorProvider, AcceleratorVendor, DeviceBinding,
    EnforcementMode, HostInventoryProvider, InventorySnapshot, LaunchPlan, LeaseState,
    ProviderError, ResourceLease, ResourceLeaseManager, ResourceRequest, SandboxBackend,
};
use cy_plugin_supervisor::ManagedInstance;
use cy_proto::core_v1;
use tokio_stream::{iter, Stream};
use tonic::{Request, Response, Status};

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

    /// 检查节点基础沙箱环境是否已就绪
    pub fn preflight_ready(&self) -> bool {
        self.sandbox.preflight().ready
    }

    /// 获取最新的硬件清单快照
    pub fn inventory(&self) -> Result<InventorySnapshot, ProviderError> {
        self.inventory_provider.probe_inventory()
    }

    /// 申请预留硬件资源租约
    pub fn reserve(&self, request: ResourceRequest) -> Result<ResourceLease, ProviderError> {
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
            bindings.push(self.accelerator_provider.create_binding(device)?);
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

    /// 获取内置 NVIDIA 适配器引用
    pub fn nvidia_provider(&self) -> &Arc<dyn AcceleratorProvider> {
        &self.accelerator_provider
    }
}

/// 已验证安装记录解析器。P2 只消费安装阶段已经校验过的记录，不负责 OCI 下载或签名验证。
pub trait InstalledPluginResolver: Send + Sync {
    fn resolve_launch_plan(
        &self,
        plugin: &core_v1::InstalledPluginRef,
        instance_name: &str,
    ) -> Result<LaunchPlan, ProviderError>;
}

struct ManagedProcess {
    instance: ManagedInstance,
    lease: Option<core_v1::ResourceLeaseRef>,
}

/// Core v1 KernelService 到真实资源管理器与 SandboxBackend 的最小服务适配层。
pub struct KernelServiceAdapter {
    daemon: Arc<KernelDaemon>,
    resolver: Arc<dyn InstalledPluginResolver>,
    instances: Mutex<HashMap<String, ManagedProcess>>,
    operations: Mutex<HashMap<String, core_v1::Operation>>,
}

impl KernelServiceAdapter {
    pub fn new(daemon: Arc<KernelDaemon>, resolver: Arc<dyn InstalledPluginResolver>) -> Self {
        Self {
            daemon,
            resolver,
            instances: Mutex::new(HashMap::new()),
            operations: Mutex::new(HashMap::new()),
        }
    }

    pub fn server(self) -> core_v1::kernel_service_server::KernelServiceServer<Self> {
        core_v1::kernel_service_server::KernelServiceServer::new(self)
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
                if owned_lease {
                    let _ = self.daemon.release(&lease.name, lease.fence_token);
                }
                return Err(provider_status(error));
            }
        };
        let mut plan = match self.resolver.resolve_launch_plan(&plugin, &instance_name) {
            Ok(plan) => plan,
            Err(error) => {
                if owned_lease {
                    let _ = self.daemon.release(&lease.name, lease.fence_token);
                }
                return Err(provider_status(error));
            }
        };
        plan.environment = binding
            .merge_environment(&plan.environment)
            .map_err(|error| {
                if owned_lease {
                    let _ = self.daemon.release(&lease.name, lease.fence_token);
                }
                provider_status(error)
            })?;
        let mut instance = ManagedInstance::new(self.daemon.sandbox.clone(), plan, binding);
        if let Err(error) = instance.start() {
            if owned_lease {
                let _ = self.daemon.release(&lease.name, lease.fence_token);
            }
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
fn to_proto_enforcement(mode: cy_kernel_api::EnforcementMode) -> core_v1::EnforcementMode {
    match mode {
        cy_kernel_api::EnforcementMode::Hard => core_v1::EnforcementMode::Hard,
        cy_kernel_api::EnforcementMode::Soft => core_v1::EnforcementMode::Soft,
        cy_kernel_api::EnforcementMode::VisibilityOnly => core_v1::EnforcementMode::VisibilityOnly,
        cy_kernel_api::EnforcementMode::ObserveOnly => core_v1::EnforcementMode::ObserveOnly,
        cy_kernel_api::EnforcementMode::Unenforced => core_v1::EnforcementMode::Unenforced,
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
    let adapter_id = first.adapter_id.clone();
    let mut device_ids = vec![first.device_id];
    for binding in bindings.into_iter().skip(1) {
        if binding.enforcement != enforcement || binding.adapter_id != adapter_id {
            return Err(ProviderError::new(
                "kernel-daemon",
                "MIXED_DEVICE_ENFORCEMENT",
                "a multi-device binding must have one adapter and enforcement mode",
            ));
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
    Ok(DeviceBinding {
        device_id: device_ids.join(","),
        nodes,
        environment,
        required_gids,
        enforcement,
        adapter_id,
        reason_code: "DEVICE_BINDING_CREATED".to_string(),
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
    Ok(ResourceRequest {
        lease_name: lease_name.to_string(),
        expected_inventory_generation: generation,
        count,
        vendor,
        min_memory_bytes: min_memory_bytes.filter(|value| *value > 0),
    })
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

#[allow(dead_code)]
fn _default_nvidia_provider() -> Arc<dyn AcceleratorProvider> {
    Arc::new(NvidiaSmiProvider::new("nvidia-smi"))
}
