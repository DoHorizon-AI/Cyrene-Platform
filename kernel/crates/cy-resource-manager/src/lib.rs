//! CYRENE 本地硬件资源租约与并发围栏管理器 (Resource Lease Manager).
//!
//! 【核心设计：乐观代数版本 + 围栏令牌 (Generations & Fencing)】
//! 在大规模异构 AI 集群调度中，硬件状态瞬息万变（如热拔插、故障隔离、抢占调度）。
//! 本模块实现了内存态的通用资源租约管理器 [`InMemoryResourceManager`]，具备以下核心保障：
//! 1. **原子独占分配**：同一资源同一时刻只能被单个租约持有，并发争抢下通过互斥锁与集合过滤保证绝对不发生重复分配；
//! 2. **代数版本校验 (Inventory Generation)**：租约申请必须带上客户端发起请求时所基于的代数（`expected_inventory_generation`），若硬件清单发生刷新则乐观拒绝陈旧请求；
//! 3. **围栏令牌 (Fence Token)**：每次分配赋予单调递增的 fence token，租约释放必须携带正确的令牌，彻底杜绝延迟网络包或旧任务错误释放新租约的竞态条件；
//! 4. **资源隔离封锁 (Quarantine)**：支持把不再可信的资源标记为隔离状态，阻止后续分配。

#![forbid(unsafe_code)]

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use cy_kernel_api::{
    semantic::{Resource, ResourceState},
    EnforcementMode, InventorySnapshot, LeaseState, NodeCapabilities, ProviderError,
    ResourceAllocation, ResourceLease, ResourceLeaseManager, ResourceRequest,
};

/// 内部受互斥锁保护的资源状态集
#[derive(Debug)]
struct State {
    /// 硬件清单版本代数（由外部事实源单调提供）
    generation: u64,
    /// 当前受管的所有通用资源事实 (`resource_id -> Resource`)
    resources: BTreeMap<String, Resource>,
    /// 已被租约占用的资源 ID 集合
    allocated: BTreeSet<String>,
    /// 因故障已被隔离封锁的资源 ID 集合
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
    pub fn new(node_id: impl Into<String>, resources: Vec<Resource>) -> Self {
        Self::with_next_fence_token(node_id, resources, 1)
    }

    /// Creates a fresh in-memory ledger with a persisted lower bound for its
    /// fence sequence. Runtime composition supplies a value greater than every
    /// fence recorded before a Kernel restart; old release requests can never
    /// accidentally match a newly recreated lease name.
    pub fn with_next_fence_token(
        node_id: impl Into<String>,
        resources: Vec<Resource>,
        next_fence_token: u64,
    ) -> Self {
        let resources = resources
            .into_iter()
            .map(|resource| (resource.identity.id.clone(), resource))
            .collect();
        Self {
            node_id: node_id.into(),
            state: Arc::new(Mutex::new(State {
                generation: 1,
                resources,
                allocated: BTreeSet::new(),
                quarantined: BTreeSet::new(),
                leases: BTreeMap::new(),
                next_fence_token: next_fence_token.max(1),
            })),
        }
    }

    /// 刷新硬件清单并使用 Adapter Host 提供的事实代数。
    pub fn refresh_inventory(&self, snapshot: InventorySnapshot) -> Result<(), ProviderError> {
        let mut state = self.state.lock().expect("resource state lock poisoned");
        if snapshot.generation == 0 {
            return Err(ProviderError::new(
                "resource-manager",
                "INVENTORY_GENERATION_INVALID",
                "adapter inventory generation must be non-zero",
            ));
        }
        if snapshot.generation < state.generation {
            return Err(ProviderError::new(
                "resource-manager",
                "INVENTORY_GENERATION_REGRESSION",
                &format!(
                    "incoming={}, current={}",
                    snapshot.generation, state.generation
                ),
            ));
        }
        let mut resources = snapshot
            .resources
            .into_iter()
            .map(|resource| (resource.identity.id.clone(), resource))
            .collect::<BTreeMap<_, _>>();

        if snapshot.generation == state.generation
            && !state.resources.is_empty()
            && state.resources != resources
        {
            return Err(ProviderError::new(
                "resource-manager",
                "INVENTORY_GENERATION_CONFLICT",
                "adapter changed inventory without advancing generation",
            ));
        }

        let allocated = state.allocated.iter().cloned().collect::<Vec<_>>();
        for resource_id in allocated {
            if let Some(resource) = resources.get_mut(&resource_id) {
                if resource.state != ResourceState::Ready {
                    state.quarantined.insert(resource_id.clone());
                }
                continue;
            }
            if let Some(previous) = state.resources.get(&resource_id).cloned() {
                let mut retained = previous;
                retained.state = ResourceState::Unavailable;
                retained.reason_code = "resource-missing-from-provider".to_string();
                retained.summary = "retained only until the active lease is released".to_string();
                resources.insert(resource_id.clone(), retained);
                state.quarantined.insert(resource_id.clone());
            }
        }
        for (resource_id, resource) in &resources {
            if resource.state != ResourceState::Ready {
                state.quarantined.insert(resource_id.clone());
            }
        }
        state
            .quarantined
            .retain(|resource_id| resources.contains_key(resource_id));
        state.generation = snapshot.generation;
        state.resources = resources;
        Ok(())
    }

    /// 将指定资源置入隔离封锁状态
    pub fn quarantine(&self, resource_id: &str, reason_code: &str) -> Result<(), ProviderError> {
        let mut state = self.state.lock().expect("resource state lock poisoned");
        let resource = state.resources.get_mut(resource_id).ok_or_else(|| {
            ProviderError::new("resource-manager", "RESOURCE_NOT_FOUND", resource_id)
        })?;
        resource.state = ResourceState::Unavailable;
        resource.reason_code = reason_code.to_ascii_lowercase().replace('_', "-");
        resource.summary = "resource was quarantined by the Kernel authority".to_string();
        state.quarantined.insert(resource_id.to_string());
        Ok(())
    }

    /// 检查指定资源当前是否处于已分配占用状态
    pub fn is_allocated(&self, resource_id: &str) -> bool {
        self.state
            .lock()
            .expect("resource state lock poisoned")
            .allocated
            .contains(resource_id)
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
            resources: state.resources.values().cloned().collect(),
            capabilities: NodeCapabilities {
                ready: state.quarantined.is_empty(),
                facts: Vec::new(),
                enforcement: Vec::new(),
            },
        }
    }

    fn refresh_inventory(&self, snapshot: InventorySnapshot) -> Result<(), ProviderError> {
        InMemoryResourceManager::refresh_inventory(self, snapshot)
    }

    /// 原子申请并预留硬件资源租约
    ///
    /// # 校验步骤
    /// 1. 校验 `expected_inventory_generation` 是否与当前代数完全一致；
    /// 2. 校验 `lease_name` 是否已存在（防止重复创建）；
    /// 3. 通过有界 `ResourceQuery` 筛选未分配、未隔离且状态就绪的候选资源；
    /// 4. 资源数量充足则生成新围栏令牌并原子标记占用。
    fn reserve(&self, request: ResourceRequest) -> Result<ResourceLease, ProviderError> {
        let mut state = self.state.lock().expect("resource state lock poisoned");
        expire_due_leases(&mut state, now_unix_ms());
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

        request.query.validate().map_err(|error| {
            ProviderError::new("resource-manager", error.reason_code, &error.message)
        })?;
        if request
            .expires_at_unix_ms
            .is_some_and(|expires_at| expires_at <= now_unix_ms())
        {
            return Err(ProviderError::new(
                "resource-manager",
                "LEASE_EXPIRY_INVALID",
                "lease expiry must be in the future",
            ));
        }

        let candidates = state
            .resources
            .values()
            .filter(|resource| !state.allocated.contains(&resource.identity.id))
            .filter(|resource| !state.quarantined.contains(&resource.identity.id))
            .filter(|resource| resource.state == ResourceState::Ready)
            .filter(|resource| request.query.matches(resource))
            .take(request.query.count as usize)
            .cloned()
            .collect::<Vec<_>>();

        if candidates.len() != request.query.count as usize {
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
            .map(|resource| {
                state.allocated.insert(resource.identity.id.clone());
                ResourceAllocation {
                    allocation_id: format!("{}:{}", request.lease_name, resource.identity.id),
                    resource: resource.identity.clone(),
                    granted_capacity: resource.capacity.clone(),
                    enforcement: EnforcementMode::ObserveOnly,
                }
            })
            .collect::<Vec<_>>();
        let lease = ResourceLease {
            name: request.lease_name.clone(),
            generation: 1,
            state: LeaseState::Active,
            holder: request.holder,
            allocations,
            inventory_generation: state.generation,
            fence_token,
            expires_at_unix_ms: request.expires_at_unix_ms,
            limits: request.limits,
        };
        state.leases.insert(request.lease_name, lease.clone());
        Ok(lease)
    }

    /// 读取租约当前快照
    fn get_lease(&self, lease_name: &str) -> Result<ResourceLease, ProviderError> {
        let mut state = self.state.lock().expect("resource state lock poisoned");
        expire_due_leases(&mut state, now_unix_ms());
        state
            .leases
            .get(lease_name)
            .cloned()
            .ok_or_else(|| ProviderError::new("resource-manager", "LEASE_NOT_FOUND", lease_name))
    }

    /// 释放硬件资源租约
    ///
    /// # 安全校验
    /// 必须提供与租约生成时一致的 `fence_token`，防止陈旧或并发释放请求造成状态错乱。
    fn release(&self, lease_name: &str, fence_token: u64) -> Result<(), ProviderError> {
        let mut state = self.state.lock().expect("resource state lock poisoned");
        expire_due_leases(&mut state, now_unix_ms());
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
            state.allocated.remove(&allocation.resource.id);
        }
        state
            .leases
            .get_mut(lease_name)
            .expect("lease was checked above")
            .state = LeaseState::Released;
        Ok(())
    }
}

fn expire_due_leases(state: &mut State, now_unix_ms: u64) {
    let due = state
        .leases
        .iter()
        .filter(|(_, lease)| {
            lease.state == LeaseState::Active
                && lease
                    .expires_at_unix_ms
                    .is_some_and(|expires_at| expires_at <= now_unix_ms)
        })
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    for name in due {
        let resources = state
            .leases
            .get(&name)
            .map(|lease| {
                lease
                    .allocations
                    .iter()
                    .map(|allocation| allocation.resource.id.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for resource_id in resources {
            state.allocated.remove(&resource_id);
        }
        if let Some(lease) = state.leases.get_mut(&name) {
            lease.state = LeaseState::Expired;
        }
    }
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use cy_kernel_api::semantic::{
        Capability, CapabilityRequirement, Identity, Quantity, ResourceQuery,
    };
    use std::thread;

    fn resource(id: &str) -> Resource {
        Resource {
            identity: Identity {
                id: id.to_string(),
                generation: 1,
            },
            provider: Identity {
                id: "test-provider".to_string(),
                generation: 1,
            },
            resource_class: "accelerator".to_string(),
            capabilities: vec![
                Capability {
                    id: "accelerator.compute".to_string(),
                    revision: 1,
                    properties: BTreeMap::new(),
                },
                Capability {
                    id: "vendor.test.compute".to_string(),
                    revision: 1,
                    properties: BTreeMap::new(),
                },
            ],
            capacity: BTreeMap::from([
                (
                    "memory.total".to_string(),
                    Quantity {
                        value: 40 * 1024 * 1024 * 1024,
                        unit: "byte".to_string(),
                    },
                ),
                (
                    "memory.allocatable".to_string(),
                    Quantity {
                        value: 40 * 1024 * 1024 * 1024,
                        unit: "byte".to_string(),
                    },
                ),
            ]),
            attributes: BTreeMap::new(),
            state: ResourceState::Ready,
            reason_code: "test-ready".to_string(),
            summary: "healthy".to_string(),
            links: Vec::new(),
        }
    }

    fn request(name: impl Into<String>, generation: u64) -> ResourceRequest {
        ResourceRequest {
            lease_name: name.into(),
            expected_inventory_generation: generation,
            holder: Identity {
                id: "worker/test".to_string(),
                generation: 1,
            },
            query: ResourceQuery {
                resource_class: "accelerator".to_string(),
                count: 1,
                required_capabilities: vec![CapabilityRequirement {
                    id: "accelerator.compute".to_string(),
                    minimum_revision: 1,
                    required_properties: BTreeMap::new(),
                }],
                minimum_capacity: BTreeMap::from([(
                    "memory.allocatable".to_string(),
                    Quantity {
                        value: 1,
                        unit: "byte".to_string(),
                    },
                )]),
            },
            expires_at_unix_ms: None,
            limits: Default::default(),
        }
    }

    #[test]
    fn concurrent_reservation_never_duplicates_a_resource() {
        let manager = Arc::new(InMemoryResourceManager::new(
            "node-1",
            vec![resource("resource-0")],
        ));
        let generation = manager.inventory().generation;
        let mut workers = Vec::new();
        for index in 0..100 {
            let manager = Arc::clone(&manager);
            workers.push(thread::spawn(move || {
                manager.reserve(request(format!("lease-{index}"), generation))
            }));
        }
        let successful = workers
            .into_iter()
            .filter_map(|worker| worker.join().unwrap().ok())
            .collect::<Vec<_>>();
        assert_eq!(successful.len(), 1);
        assert!(manager.is_allocated("resource-0"));
    }

    #[test]
    fn stale_generation_and_fence_are_rejected() {
        let manager = InMemoryResourceManager::new("node-1", vec![resource("resource-0")]);
        let lease = manager.reserve(request("lease-1", 1)).unwrap();
        assert_eq!(
            manager
                .release(&lease.name, lease.fence_token + 1)
                .unwrap_err()
                .reason_code,
            "STALE_FENCE_TOKEN"
        );
        manager.release(&lease.name, lease.fence_token).unwrap();
        assert!(!manager.is_allocated("resource-0"));
    }

    #[test]
    fn recovered_fence_floor_invalidates_a_prior_kernel_epoch() {
        let manager = InMemoryResourceManager::with_next_fence_token(
            "node-1",
            vec![resource("resource-0")],
            42,
        );
        let lease = manager.reserve(request("lease-after-restart", 1)).unwrap();
        assert_eq!(lease.fence_token, 42);
        assert_eq!(
            manager.release(&lease.name, 41).unwrap_err().reason_code,
            "STALE_FENCE_TOKEN"
        );
    }

    #[test]
    fn adapter_generation_regression_and_unversioned_changes_are_rejected() {
        let manager = InMemoryResourceManager::new("node-1", vec![resource("resource-0")]);
        let error = manager
            .refresh_inventory(InventorySnapshot {
                generation: 0,
                resources: vec![resource("resource-0")],
                capabilities: NodeCapabilities {
                    ready: true,
                    facts: Vec::new(),
                    enforcement: Vec::new(),
                },
            })
            .unwrap_err();
        assert_eq!(error.reason_code, "INVENTORY_GENERATION_INVALID");

        let error = manager
            .refresh_inventory(InventorySnapshot {
                generation: 1,
                resources: vec![resource("resource-other")],
                capabilities: NodeCapabilities {
                    ready: true,
                    facts: Vec::new(),
                    enforcement: Vec::new(),
                },
            })
            .unwrap_err();
        assert_eq!(error.reason_code, "INVENTORY_GENERATION_CONFLICT");
    }

    #[test]
    fn capability_query_is_generic_and_lease_expiry_releases_authority() {
        let manager = InMemoryResourceManager::new("node-1", vec![resource("resource-0")]);
        let mut lease_request = request("lease-expiring", 1);
        lease_request.query.required_capabilities = vec![CapabilityRequirement {
            id: "vendor.test.compute".to_string(),
            minimum_revision: 1,
            required_properties: BTreeMap::new(),
        }];
        lease_request.expires_at_unix_ms = Some(now_unix_ms().saturating_add(25));
        let lease = manager.reserve(lease_request).unwrap();
        thread::sleep(std::time::Duration::from_millis(40));

        assert_eq!(
            manager.get_lease(&lease.name).unwrap().state,
            LeaseState::Expired
        );
        assert!(!manager.is_allocated("resource-0"));
    }
}
