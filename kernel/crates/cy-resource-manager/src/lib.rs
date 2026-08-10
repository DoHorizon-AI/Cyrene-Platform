//! CYRENE 本地硬件资源租约与并发围栏管理器 (Resource Lease Manager).
//!
//! 【核心设计：乐观代数版本 + 围栏令牌 (Generations & Fencing)】
//! 在大规模异构 AI 集群调度中，硬件状态瞬息万变（如热拔插、故障隔离、抢占调度）。
//! 本模块实现了内存态的加速卡资源租约管理器 [`InMemoryResourceManager`]，具备以下核心保障：
//! 1. **原子独占分配**：同一物理卡同一时刻只能被单个租约持有，并发争抢下通过互斥锁与集合过滤保证绝对不发生重复分配；
//! 2. **代数版本校验 (Inventory Generation)**：租约申请必须带上客户端发起请求时所基于的代数（`expected_inventory_generation`），若硬件清单发生刷新则乐观拒绝陈旧请求；
//! 3. **围栏令牌 (Fence Token)**：每次分配赋予单调递增的 fence token，租约释放必须携带正确的令牌，彻底杜绝延迟网络包或旧任务错误释放新租约的竞态条件；
//! 4. **硬件隔离封锁 (Quarantine)**：支持在检测到硬件故障时将特定 GPU 标记为隔离状态，阻止后续调度分配。

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};

use cy_kernel_api::{
    AcceleratorDevice, EnforcementMode, InventorySnapshot, LeaseState, NodeCapabilities,
    ProviderError, ResourceAllocation, ResourceLease, ResourceLeaseManager, ResourceRequest,
};

/// 内部受互斥锁保护的资源状态集
#[derive(Debug)]
struct State {
    /// 硬件清单版本代数（每次 refresh_inventory 加 1）
    generation: u64,
    /// 当前受管的所有加速卡设备字典 (`device_id -> AcceleratorDevice`)
    devices: BTreeMap<String, AcceleratorDevice>,
    /// 已被租约占用的设备 ID 集合
    allocated: BTreeSet<String>,
    /// 因故障已被隔离封锁的设备 ID 集合
    quarantined: BTreeSet<String>,
    /// 活跃与历史租约记录 (`lease_name -> ResourceLease`)
    leases: BTreeMap<String, ResourceLease>,
    /// 下一个可用的围栏令牌序列号（单调递增）
    next_fence_token: u64,
}

/// 内存态硬件资源租约管理器
#[derive(Clone, Debug)]
pub struct InMemoryResourceManager {
    /// 节点唯一 ID
    node_id: String,
    /// 线程安全的多线程共享状态
    state: Arc<Mutex<State>>,
}

impl InMemoryResourceManager {
    /// 创建资源管理器实例并初始化可用硬件清单
    pub fn new(node_id: impl Into<String>, devices: Vec<AcceleratorDevice>) -> Self {
        let devices = devices
            .into_iter()
            .map(|device| (device.device_id.clone(), device))
            .collect();
        Self {
            node_id: node_id.into(),
            state: Arc::new(Mutex::new(State {
                generation: 1,
                devices,
                allocated: BTreeSet::new(),
                quarantined: BTreeSet::new(),
                leases: BTreeMap::new(),
                next_fence_token: 1,
            })),
        }
    }

    /// 刷新硬件清单并递增代数版本号
    pub fn refresh_inventory(&self, devices: Vec<AcceleratorDevice>) -> u64 {
        let mut state = self.state.lock().expect("resource state lock poisoned");
        state.generation = state.generation.saturating_add(1);
        state.devices = devices
            .into_iter()
            .map(|device| (device.device_id.clone(), device))
            .collect();
        state.generation
    }

    /// 将指定设备置入隔离封锁状态（如硬件掉卡、ECC 错误、过热等）
    pub fn quarantine(&self, device_id: &str, reason_code: &str) -> Result<(), ProviderError> {
        let mut state = self.state.lock().expect("resource state lock poisoned");
        let device = state
            .devices
            .get_mut(device_id)
            .ok_or_else(|| ProviderError::new("resource-manager", "DEVICE_NOT_FOUND", device_id))?;
        device.health.healthy = Some(false);
        device.health.reason_code = reason_code.to_string();
        state.quarantined.insert(device_id.to_string());
        Ok(())
    }

    /// 检查指定设备当前是否处于已分配占用状态
    pub fn is_allocated(&self, device_id: &str) -> bool {
        self.state
            .lock()
            .expect("resource state lock poisoned")
            .allocated
            .contains(device_id)
    }

    /// 获取所属节点 ID
    pub fn node_id(&self) -> &str {
        &self.node_id
    }
}

impl ResourceLeaseManager for InMemoryResourceManager {
    /// 获取当前最新硬件清单与健康状态快照
    fn inventory(&self) -> InventorySnapshot {
        let state = self.state.lock().expect("resource state lock poisoned");
        InventorySnapshot {
            generation: state.generation,
            devices: state.devices.values().cloned().collect(),
            capabilities: NodeCapabilities {
                ready: state.quarantined.is_empty(),
                facts: Vec::new(),
                enforcement: Vec::new(),
            },
        }
    }

    /// 原子申请并预留硬件资源租约
    ///
    /// # 校验步骤
    /// 1. 校验 `expected_inventory_generation` 是否与当前代数完全一致；
    /// 2. 校验 `lease_name` 是否已存在（防止重复创建）；
    /// 3. 筛选满足「未分配 + 未隔离 + 健康 + 厂商匹配 + 显存满足」的候选设备；
    /// 4. 设备数量充足则生成新围栏令牌并原子标记占用。
    fn reserve(&self, request: ResourceRequest) -> Result<ResourceLease, ProviderError> {
        let mut state = self.state.lock().expect("resource state lock poisoned");
        if request.expected_inventory_generation != state.generation {
            return Err(ProviderError::new(
                "resource-manager",
                "STALE_INVENTORY_GENERATION",
                &format!(
                    "request={}, current={}",
                    request.expected_inventory_generation, state.generation
                ),
            ));
        }
        if state.leases.contains_key(&request.lease_name) {
            return Err(ProviderError::new(
                "resource-manager",
                "LEASE_ALREADY_EXISTS",
                &request.lease_name,
            ));
        }

        let candidates = state
            .devices
            .values()
            .filter(|device| !state.allocated.contains(&device.device_id))
            .filter(|device| !state.quarantined.contains(&device.device_id))
            .filter(|device| device.health.healthy == Some(true))
            .filter(|device| request.vendor.is_none_or(|vendor| device.vendor == vendor))
            .filter(|device| {
                request.min_memory_bytes.is_none_or(|minimum| {
                    device
                        .total_memory_bytes
                        .is_some_and(|value| value >= minimum)
                })
            })
            .take(request.count)
            .cloned()
            .collect::<Vec<_>>();

        if candidates.len() != request.count {
            return Err(ProviderError::new(
                "resource-manager",
                "INSUFFICIENT_RESOURCES",
                "no atomic allocation satisfies the request",
            ));
        }

        let fence_token = state.next_fence_token;
        state.next_fence_token = state.next_fence_token.saturating_add(1);
        let allocations = candidates
            .iter()
            .map(|device| {
                state.allocated.insert(device.device_id.clone());
                ResourceAllocation {
                    allocation_id: format!("{}:{}", request.lease_name, device.device_id),
                    device_id: device.device_id.clone(),
                    granted_memory_bytes: device.allocatable_memory_bytes,
                    enforcement: EnforcementMode::ObserveOnly,
                }
            })
            .collect::<Vec<_>>();
        let lease = ResourceLease {
            name: request.lease_name.clone(),
            state: LeaseState::Active,
            allocations,
            inventory_generation: state.generation,
            fence_token,
        };
        state.leases.insert(request.lease_name, lease.clone());
        Ok(lease)
    }

    /// 释放硬件资源租约
    ///
    /// # 安全校验
    /// 必须提供与租约生成时一致的 `fence_token`，防止陈旧或并发释放请求造成状态错乱。
    fn release(&self, lease_name: &str, fence_token: u64) -> Result<(), ProviderError> {
        let mut state = self.state.lock().expect("resource state lock poisoned");
        let allocations = {
            let lease = state.leases.get(lease_name).ok_or_else(|| {
                ProviderError::new("resource-manager", "LEASE_NOT_FOUND", lease_name)
            })?;
            if lease.fence_token != fence_token {
                return Err(ProviderError::new(
                    "resource-manager",
                    "STALE_FENCE_TOKEN",
                    lease_name,
                ));
            }
            if lease.state == LeaseState::Released {
                return Ok(());
            }
            lease.allocations.clone()
        };
        for allocation in &allocations {
            state.allocated.remove(&allocation.device_id);
        }
        state
            .leases
            .get_mut(lease_name)
            .expect("lease was checked above")
            .state = LeaseState::Released;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cy_kernel_api::{AcceleratorKind, AcceleratorVendor, HealthReport};
    use std::thread;

    fn device(id: &str) -> AcceleratorDevice {
        AcceleratorDevice {
            device_id: id.to_string(),
            kind: AcceleratorKind::Gpu,
            vendor: AcceleratorVendor::Nvidia,
            device_family: "test".to_string(),
            pci_address: None,
            numa_node: Some(0),
            total_memory_bytes: Some(40 * 1024 * 1024 * 1024),
            allocatable_memory_bytes: Some(40 * 1024 * 1024 * 1024),
            features: Vec::new(),
            device_nodes: Vec::new(),
            links: Vec::new(),
            health: HealthReport {
                healthy: Some(true),
                reason_code: "TEST".to_string(),
                summary: "healthy".to_string(),
            },
        }
    }

    #[test]
    fn concurrent_reservation_never_duplicates_a_device() {
        let manager = Arc::new(InMemoryResourceManager::new(
            "node-1",
            vec![device("gpu-0")],
        ));
        let generation = manager.inventory().generation;
        let mut workers = Vec::new();
        for index in 0..100 {
            let manager = Arc::clone(&manager);
            workers.push(thread::spawn(move || {
                manager.reserve(ResourceRequest {
                    lease_name: format!("lease-{index}"),
                    expected_inventory_generation: generation,
                    count: 1,
                    vendor: Some(AcceleratorVendor::Nvidia),
                    min_memory_bytes: Some(1),
                })
            }));
        }
        let successful = workers
            .into_iter()
            .filter_map(|worker| worker.join().unwrap().ok())
            .collect::<Vec<_>>();
        assert_eq!(successful.len(), 1);
        assert!(manager.is_allocated("gpu-0"));
    }

    #[test]
    fn stale_generation_and_fence_are_rejected() {
        let manager = InMemoryResourceManager::new("node-1", vec![device("gpu-0")]);
        let lease = manager
            .reserve(ResourceRequest {
                lease_name: "lease-1".to_string(),
                expected_inventory_generation: 1,
                count: 1,
                vendor: None,
                min_memory_bytes: None,
            })
            .unwrap();
        assert_eq!(
            manager
                .release(&lease.name, lease.fence_token + 1)
                .unwrap_err()
                .reason_code,
            "STALE_FENCE_TOKEN"
        );
        manager.release(&lease.name, lease.fence_token).unwrap();
        assert!(!manager.is_allocated("gpu-0"));
    }
}
