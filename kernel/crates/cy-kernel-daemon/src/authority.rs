// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: kernel/crates/cy-kernel-daemon/src/authority.rs
// ║ Module: CYRENE Platform
// ║ Role: Kernel authority state, lease/worker/endpoint lifecycle, and provider fact reconciliation.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Kernel 权威状态、租约/Worker/Endpoint 生命周期与 Provider 事实协调。
// ╚══════════════════════════════════════════════════════════════════════╝
//! Transport-independent storage and coordination for canonical authority actions.

mod authority_actions;
mod provider_actions;

use std::{
    collections::{btree_map::Entry, BTreeMap, HashMap, VecDeque},
    sync::{atomic::AtomicU64, Arc, Mutex},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use cy_kernel_api::{
    semantic, AuthorityCallContext, AuthoritySnapshot, CgroupLimits, DurableEventRecord,
    DurableEventStore, InstalledPluginResolver, KernelAuthority, KernelProviderAuthority,
    LeaseState, NamespaceId, NoopRuntimeJournal, ObjectRef, ProviderError, ProviderReconcileAction,
    ProviderReconcileResult, ResourceLease, ResourceRequest, RuntimeJournalEvent,
    RuntimeJournalRecord, RuntimeJournalSink, RuntimeProcessEvidence,
};
use cy_proto::core_v1;

use crate::{
    adapter::OPERATION_EVENT_HISTORY_CAPACITY,
    convert::{inject_heartbeat_environment, now_timestamp, semantic_operation_event_kind},
    daemon::KernelDaemon,
    session::{ManagedProcess, WorkerHeartbeatConfig},
    watchdog::{InstanceActor, WorkerCancelAck},
};

/// Canonical state is owned by the local Kernel authority rather than by a
/// transport service implementation. Compatibility projections borrow this
/// runtime through `KernelServiceAdapter` during the migration.
/// 规范状态由本地 Kernel authority 持有，而不是由传输服务实现持有。迁移期间，兼容性投影通过 KernelServiceAdapter 借用此 runtime。
#[doc(hidden)]
pub struct AuthorityRuntime {
    pub(crate) daemon: Arc<KernelDaemon>,
    pub(crate) resolver: Arc<dyn InstalledPluginResolver>,
    pub(crate) instances: Arc<Mutex<HashMap<String, ManagedProcess>>>,
    /// The legacy process map is keyed by a private runtime name. Canonical
    /// authority lookup always enters through this typed namespace index.
    /// 旧版进程映射以私有 runtime 名称为键。规范 authority 查询始终通过类型化 namespace 索引进入。
    pub(crate) workers: Arc<Mutex<BTreeMap<ObjectRef, String>>>,
    pub(crate) leases: Arc<Mutex<BTreeMap<ObjectRef, String>>>,
    pub(crate) semantic_operations: Arc<Mutex<BTreeMap<ObjectRef, semantic::Operation>>>,
    pub(crate) semantic_events: Arc<Mutex<BTreeMap<NamespaceId, NamespaceEventHistory>>>,
    pub(crate) endpoints: Arc<Mutex<BTreeMap<ObjectRef, semantic::Endpoint>>>,
    pub(crate) endpoint_grants: Arc<Mutex<BTreeMap<ObjectRef, semantic::EndpointGrant>>>,
    /// Provider records are keyed by namespace and logical ID, never by the
    /// transport socket or session-generation-bearing Identity.
    /// Provider 记录以 namespace 和逻辑 ID 为键，绝不以传输套接字或包含 session generation 的 Identity 为键。
    pub(crate) providers: Arc<Mutex<BTreeMap<(NamespaceId, String), ProviderRecord>>>,
    /// First mutation binds a namespace to its authenticated Principal. This
    /// minimal local policy prevents an unrelated UDS peer from controlling it.
    /// 首次状态修改会将 namespace 绑定到经过认证的 Principal。这项最小本地策略可防止无关 UDS 对端控制该 namespace。
    pub(crate) namespace_owners: Arc<Mutex<BTreeMap<NamespaceId, semantic::Identity>>>,
    pub(crate) heartbeat: WorkerHeartbeatConfig,
    pub(crate) next_control_connection: Arc<AtomicU64>,
    pub(crate) runtime_journal: Arc<dyn RuntimeJournalSink>,
    pub(crate) event_store: Arc<dyn DurableEventStore>,
    pub(crate) event_notifier: Arc<tokio::sync::Notify>,
}

#[derive(Clone)]
pub(crate) struct ProviderRecord {
    pub(crate) provider: semantic::Provider,
    pub(crate) principal: semantic::Principal,
    inventory_scope: ProviderInventoryScope,
    pub(crate) inventory: Option<semantic::ProviderSnapshot>,
    pub(crate) reconciled_snapshot_generation: Option<u64>,
    pub(crate) pending_stale_workers: Vec<(semantic::Identity, u64)>,
}

/// Hardware observations describe resources only. This stays internal so the
/// external Provider projection continues to carry the full contract shape.
/// 硬件观测只描述资源。此信息保持内部可见，使外部 Provider 投影仍能携带完整契约形状。
#[derive(Clone, Copy, PartialEq, Eq)]
enum ProviderInventoryScope {
    Full,
    ResourceFactsOnly,
}

pub(crate) struct NamespaceEventHistory {
    next_sequence: Option<u64>,
    events: VecDeque<semantic::Event>,
}

/// Production local authority for one Kernel process. It has no tonic request
/// or response dependency; transports project requests onto this owner.
/// 单个 Kernel 进程使用的生产级本地 authority。它不依赖 tonic 请求或响应；传输层会将请求投影到此 owner。
#[derive(Clone)]
pub struct LocalKernelAuthority {
    pub(crate) runtime: Arc<AuthorityRuntime>,
}

impl LocalKernelAuthority {
    fn schedule_cancellation_deadline(
        &self,
        context: AuthorityCallContext,
        operation: semantic::Identity,
        receipt: tokio::sync::oneshot::Receiver<WorkerCancelAck>,
    ) {
        let authority = self.clone();
        let ack_timeout = self.runtime.heartbeat.shutdown_ack_timeout;
        let completion_timeout = self.runtime.heartbeat.graceful_stop;
        std::thread::spawn(move || {
            if receipt.blocking_recv().is_err() {
                authority.mark_cancelling_operation_lost(&context, &operation);
                return;
            }
            // A receipt only proves delivery. The executor must still report a
            // terminal Operation state before the bounded completion deadline.
            // receipt 只能证明消息已送达。executor 仍必须在有界完成期限内报告 Operation 的终态。
            std::thread::sleep(completion_timeout.max(ack_timeout));
            authority.mark_cancelling_operation_lost(&context, &operation);
        });
    }

    fn mark_cancelling_operation_lost(
        &self,
        context: &AuthorityCallContext,
        identity: &semantic::Identity,
    ) -> semantic::Operation {
        let operation = {
            let mut operations = self
                .runtime
                .semantic_operations
                .lock()
                .expect("semantic operation lock poisoned");
            let operation = operations
                .get_mut(&context.object_ref(identity.clone()))
                .expect("operation was checked before cancellation delivery");
            if operation.state == semantic::OperationState::Cancelling {
                operation.state = semantic::OperationState::Lost;
            }
            operation.clone()
        };
        if operation.state == semantic::OperationState::Lost {
            self.publish_semantic_event_in(
                &context.namespace,
                operation.identity.clone(),
                "operation.lost",
                "cyrene.operation.v1",
                Vec::new(),
            );
        }
        operation
    }

    pub(crate) fn confirm_stale_worker_termination(
        &self,
        context: &AuthorityCallContext,
        principal: &semantic::Principal,
        provider: &semantic::Identity,
        snapshot_generation: u64,
        worker: &semantic::Identity,
        terminated: bool,
    ) -> Result<(), semantic::Rejection> {
        self.validate_context(context)?;
        let key = (context.namespace.clone(), provider.id.clone());
        let mut providers = self
            .runtime
            .providers
            .lock()
            .expect("provider state lock poisoned");
        let record = providers.get_mut(&key).ok_or_else(|| {
            Self::rejection(
                "PROVIDER_NOT_FOUND",
                "provider has not registered this session",
            )
        })?;
        if record.provider.identity != *provider {
            return Err(Self::rejection(
                "STALE_GENERATION",
                "termination result targets a stale provider session",
            ));
        }
        if record.principal != *principal {
            return Err(Self::rejection(
                "AUTHORITY_DENIED",
                "termination result caller does not own the registered Provider session",
            ));
        }
        let Some(position) =
            record
                .pending_stale_workers
                .iter()
                .position(|(candidate, generation)| {
                    candidate == worker && *generation == snapshot_generation
                })
        else {
            return Err(Self::rejection(
                "RECONCILIATION_RESULT_UNKNOWN",
                "termination result does not match a pending stale Worker action",
            ));
        };
        if !terminated {
            return Ok(());
        }
        if record.inventory.as_ref().is_some_and(|snapshot| {
            snapshot
                .workers
                .iter()
                .any(|observed| observed.identity == *worker)
        }) {
            return Err(Self::rejection(
                "PROVIDER_REALITY_STALE",
                "a successful termination result requires a later complete snapshot without the Worker",
            ));
        }
        record.pending_stale_workers.remove(position);
        Ok(())
    }

    pub(crate) fn new(
        daemon: Arc<KernelDaemon>,
        resolver: Arc<dyn InstalledPluginResolver>,
        runtime_journal: Arc<dyn RuntimeJournalSink>,
    ) -> Self {
        Self {
            runtime: Arc::new(AuthorityRuntime {
                daemon,
                resolver,
                instances: Arc::new(Mutex::new(HashMap::new())),
                workers: Arc::new(Mutex::new(BTreeMap::new())),
                leases: Arc::new(Mutex::new(BTreeMap::new())),
                semantic_operations: Arc::new(Mutex::new(BTreeMap::new())),
                semantic_events: Arc::new(Mutex::new(BTreeMap::new())),
                endpoints: Arc::new(Mutex::new(BTreeMap::new())),
                endpoint_grants: Arc::new(Mutex::new(BTreeMap::new())),
                providers: Arc::new(Mutex::new(BTreeMap::new())),
                namespace_owners: Arc::new(Mutex::new(BTreeMap::new())),
                heartbeat: WorkerHeartbeatConfig::default(),
                next_control_connection: Arc::new(AtomicU64::new(1)),
                runtime_journal,
                event_store: Arc::new(NoopRuntimeJournal),
                event_notifier: Arc::new(tokio::sync::Notify::new()),
            }),
        }
    }

    pub(crate) fn set_worker_heartbeat(&mut self, heartbeat: WorkerHeartbeatConfig) {
        Arc::get_mut(&mut self.runtime)
            .expect("authority cannot be reconfigured after it is shared")
            .heartbeat = heartbeat;
    }

    pub(crate) fn set_runtime_journal(&mut self, runtime_journal: Arc<dyn RuntimeJournalSink>) {
        Arc::get_mut(&mut self.runtime)
            .expect("authority cannot be reconfigured after it is shared")
            .runtime_journal = runtime_journal;
    }

    pub(crate) fn set_event_store(&mut self, event_store: Arc<dyn DurableEventStore>) {
        Arc::get_mut(&mut self.runtime)
            .expect("authority cannot be reconfigured after it is shared")
            .event_store = event_store;
    }

    /// Returns a source-scoped authority snapshot. Clients use this after a
    /// replay gap or source change before resuming from the returned cursor.
    /// 返回按 source 限定的 authority 快照。发生 replay 缺口或 source 变化后，客户端使用此快照，再从返回的 cursor 继续读取。
    pub fn snapshot(
        &self,
        context: &AuthorityCallContext,
        principal: &semantic::Principal,
    ) -> Result<AuthoritySnapshot, semantic::Rejection> {
        self.validate_context(context)?;
        self.require_namespace_owner(context, principal)?;
        // Capture the event consistency boundary BEFORE reading authority
        // state. A transition publishes its durable event only after mutating
        // state, so reading state after the cursor guarantees the snapshot is a
        // superset of every event already counted in `cursor`. This makes
        // `Snapshot @ C + read_events(C)` reconstruct current state without
        // losing a transition that publishes between the state read and the
        // cursor read (the single-mutex hole that previously let a Worker or
        // Operation vanish from both the snapshot and the incremental replay).
        // 在读取 authority 状态之前，先捕获事件一致性边界。状态转换会先修改状态，再发布其耐久事件；因此，在读取 cursor 之后再读取状态，可保证快照包含 cursor 已计入的全部事件。这样 Snapshot @ C + read_events(C) 可以重建当前状态，不会丢失在读取状态和 cursor 之间发布的转换；此前单互斥锁方案会让 Worker 或 Operation 同时从快照和增量 replay 中消失。
        let source = self.semantic_event_source_for(&context.namespace);
        let cursor = semantic::EventCursor {
            source: source.clone(),
            sequence: self
                .event_history_for(&context.namespace, &source)?
                .last()
                .map_or(0, |event| event.sequence),
        };
        let providers = self
            .runtime
            .providers
            .lock()
            .expect("provider state lock poisoned")
            .iter()
            .filter(|((namespace, _), _)| namespace == &context.namespace)
            .map(|(_, record)| record.provider.clone())
            .collect::<Vec<_>>();
        let worker_names = self
            .runtime
            .workers
            .lock()
            .expect("worker scope lock poisoned")
            .iter()
            .filter(|(object, _)| object.namespace == context.namespace)
            .map(|(_, instance_name)| instance_name.clone())
            .collect::<Vec<_>>();
        let instances = self
            .runtime
            .instances
            .lock()
            .expect("instance lock poisoned");
        let workers = worker_names
            .iter()
            .filter_map(|instance_name| {
                instances
                    .get(instance_name)
                    .and_then(|process| process.semantic_worker.clone())
            })
            .collect::<Vec<_>>();
        drop(instances);
        let lease_objects = self
            .runtime
            .leases
            .lock()
            .expect("lease scope lock poisoned")
            .iter()
            .filter(|(object, _)| object.namespace == context.namespace)
            .map(|(object, name)| (object.clone(), name.clone()))
            .collect::<Vec<_>>();
        let mut leases = Vec::with_capacity(lease_objects.len());
        for (object, name) in lease_objects {
            let lease = self
                .runtime
                .daemon
                .lease(&name)
                .map_err(Self::provider_rejection)?;
            leases.push(Self::semantic_lease(&object, &lease));
        }
        let operations = self
            .runtime
            .semantic_operations
            .lock()
            .expect("semantic operation lock poisoned")
            .iter()
            .filter(|(object, _)| object.namespace == context.namespace)
            .map(|(_, operation)| operation.clone())
            .collect::<Vec<_>>();
        let endpoints = self
            .runtime
            .endpoints
            .lock()
            .expect("endpoint lock poisoned")
            .iter()
            .filter(|(object, _)| object.namespace == context.namespace)
            .map(|(_, endpoint)| endpoint.clone())
            .collect::<Vec<_>>();
        let endpoint_grants = self
            .runtime
            .endpoint_grants
            .lock()
            .expect("endpoint grant lock poisoned")
            .iter()
            .filter(|(object, _)| object.namespace == context.namespace)
            .map(|(_, grant)| grant.clone())
            .collect::<Vec<_>>();
        Ok(AuthoritySnapshot {
            source,
            cursor,
            providers,
            workers,
            leases,
            operations,
            endpoints,
            endpoint_grants,
        })
    }

    pub(crate) fn verify_worker_control(
        &self,
        context: &AuthorityCallContext,
        worker_identity: &semantic::Identity,
        lease_identity: &semantic::Identity,
        fence_token: u64,
    ) -> Result<semantic::Worker, semantic::Rejection> {
        self.validate_context(context)?;
        let lease = self.lease_for(context, lease_identity)?;
        if lease.generation != lease_identity.generation
            || lease.fence_token != fence_token
            || lease.holder != *worker_identity
            || lease.state != LeaseState::Active
        {
            return Err(Self::rejection(
                "FENCE_MISMATCH",
                "worker control no longer has active lease authority",
            ));
        }
        let worker_name = self.worker_instance_name(context, worker_identity)?;
        let instances = self
            .runtime
            .instances
            .lock()
            .expect("instance lock poisoned");
        let process = instances.get(&worker_name).ok_or_else(|| {
            Self::rejection("WORKER_NOT_FOUND", "worker is not managed by this Kernel")
        })?;
        let worker = process.semantic_worker.as_ref().ok_or_else(|| {
            Self::rejection(
                "WORKER_COMPATIBILITY_ONLY",
                "legacy plugin process cannot use semantic Worker control",
            )
        })?;
        if worker.identity != *worker_identity || worker.lease != *lease_identity {
            return Err(Self::rejection(
                "STALE_GENERATION",
                "worker identity or lease generation is stale",
            ));
        }
        Ok(worker.clone())
    }

    pub(crate) fn accept_worker_control_heartbeat(
        &self,
        context: &AuthorityCallContext,
        worker_identity: semantic::Identity,
        lease_identity: semantic::Identity,
        fence_token: u64,
    ) -> Result<semantic::Worker, semantic::Rejection> {
        self.verify_worker_control(context, &worker_identity, &lease_identity, fence_token)?;
        let worker_name = self.worker_instance_name(context, &worker_identity)?;
        let mut instances = self
            .runtime
            .instances
            .lock()
            .expect("instance lock poisoned");
        let process = instances.get_mut(&worker_name).ok_or_else(|| {
            Self::rejection("WORKER_NOT_FOUND", "worker is not managed by this Kernel")
        })?;
        let worker = process.semantic_worker.as_mut().ok_or_else(|| {
            Self::rejection(
                "WORKER_COMPATIBILITY_ONLY",
                "legacy plugin process cannot use semantic Worker control",
            )
        })?;
        if !worker
            .state
            .can_transition_to(semantic::WorkerState::Running)
        {
            return Err(Self::rejection(
                "STATE_TRANSITION_INVALID",
                "worker cannot enter RUNNING from its current state",
            ));
        }
        worker.state = semantic::WorkerState::Running;
        process.actor.on_heartbeat_received(Instant::now());
        process.last_heartbeat_at = Some(now_timestamp());
        let worker = worker.clone();
        drop(instances);
        self.publish_semantic_event_in(
            &context.namespace,
            worker.identity.clone(),
            "worker.running",
            "cyrene.worker.v1",
            Vec::new(),
        );
        Ok(worker)
    }

    /// Removes the authority metadata for a Worker that no longer holds valid
    /// authority: its published Endpoints are deleted, and every EndpointGrant
    /// that references one of those Endpoints or names the Worker as grantee is
    /// dropped. Callers invoke this whenever the Worker's Lease is revoked,
    /// released, or expired, or the Worker is lost/replaced, so stale
    /// Endpoint/Grant state cannot outlive the authority it depends on. The
    /// removed Endpoint identities are returned so callers can emit the
    /// corresponding revocation reconcile actions.
    /// 移除不再具有有效 authority 的 Worker 元数据：删除其已发布 Endpoint，并丢弃引用这些 Endpoint 或将该 Worker 设为被授权方的所有 EndpointGrant。调用方会在 Worker 的 Lease 被撤销、释放或过期，或 Worker 丢失/替换时调用此操作，防止 Endpoint/Grant 状态超出其依赖的 authority 生命周期。返回已移除的 Endpoint 身份，供调用方发出对应的撤销协调 action。
    pub(crate) fn purge_endpoint_authority(
        &self,
        worker_identity: &semantic::Identity,
    ) -> Vec<semantic::Identity> {
        let endpoint_identities = {
            let mut endpoints = self
                .runtime
                .endpoints
                .lock()
                .expect("endpoint lock poisoned");
            let endpoint_keys = endpoints
                .iter()
                .filter(|(_, endpoint)| endpoint.owner == *worker_identity)
                .map(|(key, endpoint)| (key.clone(), endpoint.identity.clone()))
                .collect::<Vec<_>>();
            for (key, _) in &endpoint_keys {
                endpoints.remove(key);
            }
            endpoint_keys
                .iter()
                .map(|(_, identity)| identity.clone())
                .collect::<Vec<_>>()
        };
        self.runtime
            .endpoint_grants
            .lock()
            .expect("endpoint grant lock poisoned")
            .retain(|_, grant| {
                !endpoint_identities.contains(&grant.endpoint) && grant.grantee != *worker_identity
            });
        endpoint_identities
    }

    /// Commits the authority side of an abnormal Worker disappearance. The
    /// caller must already have classified its evidence (heartbeat timeout,
    /// Provider reality, or runtime absence); a transport disconnect alone is
    /// intentionally insufficient to reach this transition.
    /// 提交 Worker 异常消失时 authority 侧的状态变化。调用方必须先对证据分类（心跳超时、Provider 实际状态或 runtime 缺失）；仅传输断开有意不足以触发此转换。
    pub(crate) fn mark_worker_lost(
        &self,
        context: &AuthorityCallContext,
        worker_identity: &semantic::Identity,
        reason_code: &str,
    ) -> Result<Vec<ProviderReconcileAction>, semantic::Rejection> {
        self.validate_context(context)?;
        let worker_name = self.worker_instance_name(context, worker_identity)?;
        let worker = self
            .runtime
            .instances
            .lock()
            .expect("instance lock poisoned")
            .get(&worker_name)
            .and_then(|process| process.semantic_worker.clone())
            .ok_or_else(|| {
                Self::rejection("WORKER_NOT_FOUND", "worker is not managed by this Kernel")
            })?;
        if worker.identity != *worker_identity {
            return Err(Self::rejection(
                "STALE_GENERATION",
                "worker identity generation is stale",
            ));
        }
        let lease = self.lease_for(context, &worker.lease)?;
        let is_allocated = self.runtime.daemon.is_allocated(&lease.name);
        if worker.state == semantic::WorkerState::Lost && !is_allocated {
            return Ok(vec![ProviderReconcileAction::Noop]);
        }
        if !worker.state.can_transition_to(semantic::WorkerState::Lost)
            && worker.state != semantic::WorkerState::Lost
        {
            return Ok(vec![ProviderReconcileAction::Noop]);
        }
        if lease.holder != worker.identity {
            return Err(Self::rejection(
                "FENCE_MISMATCH",
                "worker no longer owns the referenced lease",
            ));
        }
        // This record is the durable intent boundary. Once it succeeds, every
        // following error still leaves authority fail-closed rather than active.
        // 此记录是耐久意图边界。一旦写入成功，后续任何错误都会让 authority 保持失败关闭，而不是恢复为 active。
        self.record_runtime(
            RuntimeJournalEvent::WorkerLost,
            Some(&worker_name),
            Some(&lease),
            reason_code,
        )?;
        let revocation_required = matches!(
            lease.state,
            LeaseState::Active | LeaseState::Releasing | LeaseState::Expired
        );
        let revoked = if revocation_required {
            self.runtime
                .daemon
                .revoke(&lease.name, lease.fence_token)
                .map_err(Self::provider_rejection)?
        } else {
            lease.clone()
        };

        let cleanup = {
            let mut instances = self
                .runtime
                .instances
                .lock()
                .expect("instance lock poisoned");
            let process = instances.get_mut(&worker_name).ok_or_else(|| {
                Self::rejection("WORKER_NOT_FOUND", "worker is not managed by this Kernel")
            })?;
            match process.semantic_worker.as_mut() {
                Some(semantic_worker) => semantic_worker.state = semantic::WorkerState::Lost,
                None => {
                    return Err(Self::rejection(
                        "WORKER_COMPATIBILITY_ONLY",
                        "legacy plugin process is not a semantic Worker",
                    ));
                }
            }
            process.watchdog_triggered = true;
            process.control = None;
            process.semantic_control = None;
            process.pending_shutdown = None;
            process
                .actor
                .stop(&cy_kernel_api::StopRequest {
                    grace_period: std::time::Duration::ZERO,
                    immediate: true,
                })
                .cloned()
                .ok()
        };

        let mut actions = vec![ProviderReconcileAction::MarkWorkerLost(
            worker.identity.clone(),
        )];
        if revocation_required || lease.state == LeaseState::Revoked {
            actions.push(ProviderReconcileAction::RevokeLease(worker.lease.clone()));
        }
        let lost_operations = {
            let mut operations = self
                .runtime
                .semantic_operations
                .lock()
                .expect("semantic operation lock poisoned");
            operations
                .values_mut()
                .filter_map(|operation| {
                    let targets_worker = operation.executor == worker.identity
                        || operation
                            .metadata
                            .get("worker.id")
                            .is_some_and(|id| id == &worker.identity.id);
                    if targets_worker
                        && operation
                            .state
                            .can_transition_to(semantic::OperationState::Lost)
                    {
                        operation.state = semantic::OperationState::Lost;
                        Some(operation.identity.clone())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
        };
        actions.extend(
            lost_operations
                .iter()
                .cloned()
                .map(ProviderReconcileAction::MarkOperationLost),
        );

        let revoked_endpoints = self.purge_endpoint_authority(&worker.identity);
        actions.extend(
            revoked_endpoints
                .iter()
                .cloned()
                .map(ProviderReconcileAction::RevokeEndpoint),
        );

        // No visible state is published before both the revoke and its new
        // fence are journaled. If either write fails, the in-memory state is
        // already fail-closed and physical resources remain allocated.
        // 只有 revoke 和新的 fence 都写入日志后，才会发布可见状态。任一写入失败时，内存状态已经失败关闭，物理资源仍保持分配占用。
        if revocation_required || lease.state == LeaseState::Revoked {
            self.record_runtime(
                RuntimeJournalEvent::LeaseRevoked,
                Some(&worker_name),
                Some(&revoked),
                reason_code,
            )?;
            self.record_runtime(
                RuntimeJournalEvent::FenceAdvanced,
                Some(&worker_name),
                Some(&revoked),
                reason_code,
            )?;
            if cleanup.as_ref().is_some_and(|report| report.complete) {
                self.record_runtime(
                    RuntimeJournalEvent::InstanceTerminated,
                    Some(&worker.identity.id),
                    Some(&revoked),
                    &cleanup
                        .as_ref()
                        .expect("cleanup completion was checked")
                        .reason_code,
                )?;
                self.runtime
                    .daemon
                    .complete_revocation(&revoked.name, revoked.fence_token)
                    .map_err(Self::provider_rejection)?;
            }
        }
        for operation in &lost_operations {
            self.record_runtime(
                RuntimeJournalEvent::OperationLost,
                Some(&worker_name),
                Some(&revoked),
                reason_code,
            )?;
            self.publish_semantic_event_in(
                &context.namespace,
                operation.clone(),
                "operation.lost",
                "cyrene.operation.v1",
                Vec::new(),
            );
        }
        for endpoint in &revoked_endpoints {
            self.record_runtime(
                RuntimeJournalEvent::EndpointRevoked,
                Some(&worker_name),
                Some(&revoked),
                reason_code,
            )?;
            self.publish_semantic_event_in(
                &context.namespace,
                endpoint.clone(),
                "endpoint.revoked",
                "cyrene.endpoint.v1",
                Vec::new(),
            );
        }
        self.publish_semantic_event_in(
            &context.namespace,
            worker.identity.clone(),
            "worker.lost",
            "cyrene.worker.v1",
            Vec::new(),
        );
        if revocation_required || lease.state == LeaseState::Revoked {
            self.publish_semantic_event_in(
                &context.namespace,
                worker.lease.clone(),
                "lease.revoked",
                "cyrene.lease.v1",
                Vec::new(),
            );
        }
        Ok(actions)
    }

    /// Actively scans for expired leases and triggers authority revocation,
    /// worker termination, endpoint invalidation, fence advancement, and
    /// confirmed cleanup.
    /// 主动扫描过期租约，并触发 authority 撤销、worker 终止、endpoint 失效、fence 前进以及已确认的清理。
    pub(crate) fn enforce_lease_expiry(
        &self,
    ) -> Result<Vec<ProviderReconcileAction>, semantic::Rejection> {
        let now = Self::now_unix_ms();
        let leases = self.runtime.daemon.leases();
        let mut all_actions = Vec::new();
        for lease in leases {
            let is_expired = lease.expires_at_unix_ms.is_some_and(|exp| exp <= now);
            let is_allocated = self.runtime.daemon.is_allocated(&lease.name);
            let needs_active_expiry = (is_expired
                && matches!(
                    lease.state,
                    LeaseState::Active | LeaseState::Releasing | LeaseState::Expired
                ))
                || (matches!(lease.state, LeaseState::Revoked) && is_allocated);
            if !needs_active_expiry {
                continue;
            }

            // Find semantic namespace and identity if tracked in self.runtime.leases
            // 如果 self.runtime.leases 中跟踪了该租约，则查找其语义 namespace 和 identity。
            let lease_object = self
                .runtime
                .leases
                .lock()
                .expect("lease scope lock poisoned")
                .iter()
                .find(|(_, name)| *name == &lease.name)
                .map(|(object, _)| object.clone());

            let namespace = lease_object
                .as_ref()
                .map(|obj| obj.namespace.clone())
                .unwrap_or_default();
            let lease_semantic_id = lease_object
                .as_ref()
                .map(|obj| obj.identity.clone())
                .unwrap_or_else(|| semantic::Identity {
                    id: lease.name.clone(),
                    generation: lease.generation,
                });

            // Check if there is an active semantic worker bound to this lease
            // 检查是否有绑定到此租约的 active semantic worker。
            let bound_worker = {
                let instances = self
                    .runtime
                    .instances
                    .lock()
                    .expect("instance lock poisoned");
                instances.values().find_map(|process| {
                    if let Some(worker) = process.semantic_worker.as_ref() {
                        if worker.lease == lease_semantic_id
                            || worker.lease.id == lease_semantic_id.id
                            || process
                                .lease
                                .as_ref()
                                .is_some_and(|l| l.lease_name == lease.name)
                        {
                            return Some(worker.identity.clone());
                        }
                    }
                    None
                })
            };

            if let Some(worker_identity) = bound_worker {
                let context = AuthorityCallContext {
                    contract: semantic::ContractRevision::current(),
                    namespace: namespace.clone(),
                    request_id: format!("expiry-lost-{}", worker_identity.id),
                    idempotency_key: format!("expiry-lost-{}", worker_identity.id),
                };
                match self.mark_worker_lost(&context, &worker_identity, "LEASE_EXPIRED") {
                    Ok(actions) => all_actions.extend(actions),
                    Err(error) => {
                        eprintln!(
                            "enforce_lease_expiry: mark_worker_lost failed for {worker_identity:?}: {error:?}"
                        );
                    }
                }
                continue;
            }

            // If no semantic worker is bound, check if it's a legacy plugin instance
            // 如果没有绑定的 semantic worker，则检查是否为旧版 plugin 实例。
            let is_legacy_instance = {
                let instances = self
                    .runtime
                    .instances
                    .lock()
                    .expect("instance lock poisoned");
                instances
                    .iter()
                    .find(|(_, process)| {
                        process
                            .lease
                            .as_ref()
                            .is_some_and(|l| l.lease_name == lease.name)
                    })
                    .map(|(name, _)| name.clone())
            };

            if let Some(instance_name) = is_legacy_instance {
                let mut instances = self
                    .runtime
                    .instances
                    .lock()
                    .expect("instance lock poisoned");
                if let Some(process) = instances.get_mut(&instance_name) {
                    process.watchdog_triggered = true;
                    let report = process
                        .actor
                        .stop(&cy_kernel_api::StopRequest {
                            grace_period: std::time::Duration::ZERO,
                            immediate: true,
                        })
                        .ok();
                    let cleanup_complete = report.as_ref().is_some_and(|r| r.complete);
                    drop(instances);
                    let _ = self.record_runtime(
                        RuntimeJournalEvent::LeaseRevoked,
                        Some(&instance_name),
                        Some(&lease),
                        "LEASE_EXPIRED",
                    );
                    let revoked = if lease.state == LeaseState::Revoked {
                        Ok(lease.clone())
                    } else {
                        self.runtime.daemon.revoke(&lease.name, lease.fence_token)
                    };
                    if let Ok(revoked) = revoked {
                        let _ = self.record_runtime(
                            RuntimeJournalEvent::FenceAdvanced,
                            Some(&instance_name),
                            Some(&revoked),
                            "LEASE_EXPIRED",
                        );
                        if cleanup_complete {
                            let _ = self.record_runtime(
                                RuntimeJournalEvent::InstanceTerminated,
                                Some(&instance_name),
                                Some(&revoked),
                                "LEASE_EXPIRED",
                            );
                            let _ = self
                                .runtime
                                .daemon
                                .complete_revocation(&revoked.name, revoked.fence_token);
                            self.runtime
                                .instances
                                .lock()
                                .expect("instance lock poisoned")
                                .remove(&instance_name);
                        }
                    }
                }
            } else {
                // Standalone lease with no running process
                // 没有运行中进程的独立租约。
                let revoked = if lease.state == LeaseState::Revoked {
                    Ok(lease.clone())
                } else {
                    self.runtime.daemon.revoke(&lease.name, lease.fence_token)
                };
                if let Ok(revoked) = revoked {
                    let _ = self.record_runtime(
                        RuntimeJournalEvent::LeaseRevoked,
                        None,
                        Some(&revoked),
                        "LEASE_EXPIRED",
                    );
                    let _ = self.record_runtime(
                        RuntimeJournalEvent::FenceAdvanced,
                        None,
                        Some(&revoked),
                        "LEASE_EXPIRED",
                    );
                    let _ = self
                        .runtime
                        .daemon
                        .complete_revocation(&revoked.name, revoked.fence_token);
                    self.publish_semantic_event_in(
                        &namespace,
                        lease_semantic_id.clone(),
                        "lease.revoked",
                        "cyrene.lease.v1",
                        Vec::new(),
                    );
                    all_actions.push(ProviderReconcileAction::RevokeLease(lease_semantic_id));
                }
            }
        }
        Ok(all_actions)
    }
}

impl LocalKernelAuthority {
    pub(crate) fn register_resource_facts_provider(
        &self,
        context: &AuthorityCallContext,
        principal: &semantic::Principal,
        provider: semantic::Provider,
    ) -> Result<semantic::Provider, semantic::Rejection> {
        let registered =
            KernelProviderAuthority::register_provider(self, context, principal, provider)?;
        let key = (context.namespace.clone(), registered.identity.id.clone());
        let mut providers = self
            .runtime
            .providers
            .lock()
            .expect("provider state lock poisoned");
        let record = providers
            .get_mut(&key)
            .expect("registered provider must remain present");
        record.inventory_scope = ProviderInventoryScope::ResourceFactsOnly;
        Ok(registered)
    }
}

impl LocalKernelAuthority {
    fn validate_context(&self, context: &AuthorityCallContext) -> Result<(), semantic::Rejection> {
        context.validate_for(&semantic::ContractRevision::current())
    }

    fn rejection(
        reason_code: impl Into<String>,
        message: impl Into<String>,
    ) -> semantic::Rejection {
        semantic::Rejection::new(reason_code, message)
    }

    fn provider_rejection(error: ProviderError) -> semantic::Rejection {
        Self::rejection(error.reason_code, error.message)
    }

    fn bind_namespace(
        &self,
        context: &AuthorityCallContext,
        principal: &semantic::Principal,
    ) -> Result<(), semantic::Rejection> {
        let mut owners = self
            .runtime
            .namespace_owners
            .lock()
            .expect("namespace owner lock poisoned");
        match owners.entry(context.namespace.clone()) {
            Entry::Vacant(entry) => {
                entry.insert(principal.identity.clone());
                Ok(())
            }
            Entry::Occupied(entry) if entry.get() == &principal.identity => Ok(()),
            Entry::Occupied(_) => Err(Self::rejection(
                "NAMESPACE_AUTHORITY_DENIED",
                "authenticated Principal does not own this namespace",
            )),
        }
    }

    fn require_namespace_owner(
        &self,
        context: &AuthorityCallContext,
        principal: &semantic::Principal,
    ) -> Result<(), semantic::Rejection> {
        let owners = self
            .runtime
            .namespace_owners
            .lock()
            .expect("namespace owner lock poisoned");
        if owners
            .get(&context.namespace)
            .is_some_and(|owner| owner != &principal.identity)
        {
            return Err(Self::rejection(
                "NAMESPACE_AUTHORITY_DENIED",
                "authenticated Principal does not own this namespace",
            ));
        }
        Ok(())
    }

    fn lease_name(context: &AuthorityCallContext) -> semantic::Identity {
        semantic::Identity {
            id: Self::release_name(context),
            generation: 1,
        }
    }

    fn scoped_runtime_name(prefix: &str, object: &ObjectRef) -> String {
        if object.namespace == NamespaceId::default() {
            return object.identity.id.clone();
        }
        format!(
            "{prefix}-n{}-{}-i{}-{}-g{}",
            object.namespace.as_str().len(),
            object.namespace.as_str(),
            object.identity.id.len(),
            object.identity.id,
            object.identity.generation,
        )
    }

    fn lease_internal_name(
        context: &AuthorityCallContext,
        identity: &semantic::Identity,
    ) -> String {
        let object = context.object_ref(identity.clone());
        Self::scoped_runtime_name("lease", &object)
    }

    fn lease_for(
        &self,
        context: &AuthorityCallContext,
        identity: &semantic::Identity,
    ) -> Result<ResourceLease, semantic::Rejection> {
        let object = context.object_ref(identity.clone());
        let name = self
            .runtime
            .leases
            .lock()
            .expect("lease scope lock poisoned")
            .get(&object)
            .cloned()
            .ok_or_else(|| {
                Self::rejection("LEASE_NOT_FOUND", "lease is unknown in this namespace")
            })?;
        self.runtime
            .daemon
            .lease(&name)
            .map_err(Self::provider_rejection)
    }

    fn worker_instance_name(
        &self,
        context: &AuthorityCallContext,
        identity: &semantic::Identity,
    ) -> Result<String, semantic::Rejection> {
        let object = context.object_ref(identity.clone());
        self.runtime
            .workers
            .lock()
            .expect("worker scope lock poisoned")
            .get(&object)
            .cloned()
            .or_else(|| (context.namespace == NamespaceId::default()).then(|| identity.id.clone()))
            .ok_or_else(|| {
                Self::rejection("WORKER_NOT_FOUND", "worker is not managed by this Kernel")
            })
    }

    fn semantic_lease(object: &ObjectRef, lease: &ResourceLease) -> semantic::Lease {
        semantic::Lease {
            identity: object.identity.clone(),
            holder: lease.holder.clone(),
            resources: lease
                .allocations
                .iter()
                .map(|allocation| allocation.resource.clone())
                .collect(),
            state: match lease.state {
                LeaseState::Active => semantic::LeaseState::Active,
                LeaseState::Releasing => semantic::LeaseState::Releasing,
                LeaseState::Released => semantic::LeaseState::Released,
                LeaseState::Expired => semantic::LeaseState::Expired,
                LeaseState::Revoked => semantic::LeaseState::Revoked,
                LeaseState::Failed | LeaseState::Quarantined => semantic::LeaseState::Failed,
            },
            fence_token: lease.fence_token,
            expires_at_unix_ms: lease.expires_at_unix_ms,
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
    pub(crate) fn semantic_event_source(&self) -> semantic::Identity {
        self.semantic_event_source_for(&NamespaceId::default())
    }

    pub(crate) fn semantic_event_source_for(&self, namespace: &NamespaceId) -> semantic::Identity {
        let id = if namespace == &NamespaceId::default() {
            format!("kernel/{}", self.runtime.daemon.node_id)
        } else {
            format!(
                "kernel/{}/namespace/{}",
                self.runtime.daemon.node_id,
                namespace.as_str()
            )
        };
        semantic::Identity {
            id,
            generation: self.runtime.daemon.node_epoch,
        }
    }

    #[cfg(test)]
    pub(crate) fn publish_semantic_event(
        &self,
        subject: semantic::Identity,
        kind: impl Into<String>,
        schema_id: impl Into<String>,
        body: impl Into<Vec<u8>>,
    ) {
        self.publish_semantic_event_in(&NamespaceId::default(), subject, kind, schema_id, body);
    }

    pub(crate) fn publish_semantic_event_in(
        &self,
        namespace: &NamespaceId,
        subject: semantic::Identity,
        kind: impl Into<String>,
        schema_id: impl Into<String>,
        body: impl Into<Vec<u8>>,
    ) {
        let source = self.semantic_event_source_for(namespace);
        let history_exists = self
            .runtime
            .semantic_events
            .lock()
            .expect("semantic event history lock poisoned")
            .contains_key(namespace);
        let seeded_next_sequence = if history_exists {
            None
        } else {
            match self.event_history_for(namespace, &source) {
                Ok(events) => Some(
                    events
                        .last()
                        .map_or(Some(1), |event| event.sequence.checked_add(1)),
                ),
                // A source cannot safely publish a new sequence when its
                // durable history is unavailable or malformed.
                // 当耐久历史不可用或格式错误时，source 无法安全地发布新的序列号。
                Err(_) => return,
            }
        };
        let mut histories = self
            .runtime
            .semantic_events
            .lock()
            .expect("semantic event history lock poisoned");
        let history = histories
            .entry(namespace.clone())
            .or_insert_with(|| NamespaceEventHistory {
                next_sequence: seeded_next_sequence.unwrap_or(Some(1)),
                events: VecDeque::with_capacity(OPERATION_EVENT_HISTORY_CAPACITY),
            });
        let Some(sequence) = history.next_sequence else {
            return;
        };
        let event = semantic::Event {
            sequence,
            source,
            subject,
            kind: kind.into(),
            observed_at_unix_ms: Self::now_unix_ms(),
            schema_id: schema_id.into(),
            body: body.into(),
        };
        if event.validate().is_err() {
            return;
        }
        // Events are externally visible only after the composition-selected
        // durable store accepts them. State transitions carry their own journal
        // boundary; this prevents an in-memory replay cursor from claiming an
        // event that disappears across a restart.
        // Normative lifecycle facts (worker/lease/endpoint/operation) are
        // ordered, replayable, at-least-once semantic events per the Contract.
        // A failed durable append must NOT be silently dropped while later
        // events continue as if contiguous: that would make an already
        // subscribed client miss the fact with no way to detect the gap.
        // Degrade the stream instead: no further events are emitted for this
        // namespace until the durable store recovers and clients resnapshot.
        // 只有组合所选的耐久存储接受事件后，事件才对外可见。状态转换有各自的日志边界；这可防止内存 replay cursor 声称某个重启后会消失的事件。
        // 按照 Contract，规范生命周期事实（worker/lease/endpoint/operation）是有序、可 replay、至少投递一次的语义事件。
        // 耐久追加失败时，不能悄悄丢弃该事件后继续发送看似连续的后续事件，否则已订阅客户端会漏掉事实且无法发现缺口。
        // 应降级事件流：耐久存储恢复且客户端重新获取快照之前，该 namespace 不再发出事件。
        if self
            .runtime
            .event_store
            .append_event(DurableEventRecord {
                namespace: namespace.as_str().to_string(),
                event: event.clone(),
            })
            .is_err()
        {
            history.next_sequence = None;
            self.runtime.event_notifier.notify_waiters();
            return;
        }
        history.next_sequence = sequence.checked_add(1);
        if history.events.len() == OPERATION_EVENT_HISTORY_CAPACITY {
            history.events.pop_front();
        }
        history.events.push_back(event);
        self.runtime.event_notifier.notify_waiters();
    }

    fn read_events_page(
        &self,
        namespace: &NamespaceId,
        cursor: &semantic::EventCursor,
        limit: usize,
    ) -> Result<semantic::EventPage, semantic::Rejection> {
        let source = self.semantic_event_source_for(namespace);
        if cursor.source != source {
            let (oldest, latest) = self.in_memory_event_range(namespace);
            return Ok(semantic::EventPage {
                source,
                status: semantic::ReplayStatus::SourceChanged,
                events: Vec::new(),
                oldest_available_sequence: oldest,
                latest_available_sequence: latest,
                next_sequence: cursor.sequence,
            });
        }
        // A degraded stream is externally observable: once a durable append was
        // lost, an already-subscribed client must be told to resnapshot (Gap)
        // rather than silently receive an empty, apparently-contiguous stream
        // while its cursor goes stale.
        // 降级的事件流必须能被外部观察到：一旦耐久追加丢失，必须通知已订阅客户端重新获取快照（Gap），不能在 cursor 已过期时静默发送空的、看似连续的事件流。
        if self
            .runtime
            .semantic_events
            .lock()
            .expect("semantic event history lock poisoned")
            .get(namespace)
            .is_some_and(|history| history.next_sequence.is_none())
        {
            let (oldest, latest) = self.in_memory_event_range(namespace);
            return Ok(semantic::EventPage {
                source,
                status: semantic::ReplayStatus::Gap,
                events: Vec::new(),
                oldest_available_sequence: oldest,
                latest_available_sequence: latest,
                next_sequence: cursor.sequence,
            });
        }
        let history = self.event_history_for(namespace, &source)?;
        let oldest = history.first().map_or(0, |event| event.sequence);
        let latest = history.last().map_or(0, |event| event.sequence);
        let status = cursor.status_against(&source, oldest);
        if status != semantic::ReplayStatus::Current {
            return Ok(semantic::EventPage {
                source,
                status,
                events: Vec::new(),
                oldest_available_sequence: oldest,
                latest_available_sequence: latest,
                next_sequence: cursor.sequence,
            });
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
        Ok(semantic::EventPage {
            source,
            status,
            events,
            oldest_available_sequence: oldest,
            latest_available_sequence: latest,
            next_sequence,
        })
    }

    fn event_history_for(
        &self,
        namespace: &NamespaceId,
        source: &semantic::Identity,
    ) -> Result<Vec<semantic::Event>, semantic::Rejection> {
        let durable = self
            .runtime
            .event_store
            .events_for_source(source, namespace.as_str())
            .map_err(Self::provider_rejection)?;
        if let Some(records) = durable {
            let mut previous_sequence = None;
            let mut events = Vec::with_capacity(records.len());
            for record in records {
                if record.namespace != namespace.as_str()
                    || record.event.source != *source
                    || record.event.validate().is_err()
                    || previous_sequence.is_some_and(|previous| record.event.sequence <= previous)
                {
                    return Err(Self::rejection(
                        "EVENT_STORE_INVALID",
                        "durable event history did not match the requested ordered source",
                    ));
                }
                previous_sequence = Some(record.event.sequence);
                events.push(record.event);
            }
            return Ok(events);
        }
        let histories = self
            .runtime
            .semantic_events
            .lock()
            .expect("semantic event history lock poisoned");
        Ok(histories
            .get(namespace)
            .into_iter()
            .flat_map(|history| history.events.iter())
            .cloned()
            .collect())
    }

    fn in_memory_event_range(&self, namespace: &NamespaceId) -> (u64, u64) {
        let histories = self
            .runtime
            .semantic_events
            .lock()
            .expect("semantic event history lock poisoned");
        let history = histories.get(namespace);
        (
            history
                .and_then(|history| history.events.front())
                .map_or(0, |event| event.sequence),
            history
                .and_then(|history| history.events.back())
                .map_or(0, |event| event.sequence),
        )
    }

    fn record_runtime(
        &self,
        event: RuntimeJournalEvent,
        instance_name: Option<&str>,
        lease: Option<&ResourceLease>,
        reason_code: &str,
    ) -> Result<(), semantic::Rejection> {
        self.runtime
            .runtime_journal
            .append(RuntimeJournalRecord {
                event,
                node_id: self.runtime.daemon.node_id.clone(),
                node_epoch: self.runtime.daemon.node_epoch,
                instance_name: instance_name.map(str::to_owned),
                lease_name: lease.map(|lease| lease.name.clone()),
                fence_token: lease.map(|lease| lease.fence_token),
                reason_code: reason_code.to_string(),
                runtime_evidence: None,
            })
            .map_err(Self::provider_rejection)
    }

    fn record_runtime_launch(
        &self,
        instance_name: &str,
        lease: &ResourceLease,
        evidence: RuntimeProcessEvidence,
    ) -> Result<(), semantic::Rejection> {
        self.runtime
            .runtime_journal
            .append(RuntimeJournalRecord {
                event: RuntimeJournalEvent::InstanceLaunched,
                node_id: self.runtime.daemon.node_id.clone(),
                node_epoch: self.runtime.daemon.node_epoch,
                instance_name: Some(instance_name.to_string()),
                lease_name: Some(lease.name.clone()),
                fence_token: Some(lease.fence_token),
                reason_code: "WORKER_LAUNCHED".to_string(),
                runtime_evidence: Some(evidence),
            })
            .map_err(Self::provider_rejection)
    }

    fn remember_operation(
        &self,
        context: &AuthorityCallContext,
        operation: semantic::Operation,
    ) -> semantic::Operation {
        self.runtime
            .semantic_operations
            .lock()
            .expect("semantic operation lock poisoned")
            .insert(
                context.object_ref(operation.identity.clone()),
                operation.clone(),
            );
        self.publish_semantic_event_in(
            &context.namespace,
            operation.identity.clone(),
            semantic_operation_event_kind(operation.state),
            "cyrene.operation.v1",
            Vec::new(),
        );
        operation
    }

    fn release_name(context: &AuthorityCallContext) -> String {
        format!("lease-{}", context.effective_idempotency_key())
    }

    fn release_with_cleanup(
        &self,
        context: &AuthorityCallContext,
        lease_identity: &semantic::Identity,
        fence_token: u64,
    ) -> Result<semantic::Lease, semantic::Rejection> {
        let object = context.object_ref(lease_identity.clone());
        let current = self.lease_for(context, lease_identity)?;
        if current.generation != lease_identity.generation {
            return Err(Self::rejection(
                "STALE_GENERATION",
                "lease generation no longer has authority",
            ));
        }
        let retrying_failed_cleanup = current.state == LeaseState::Failed;
        self.record_runtime(
            RuntimeJournalEvent::LeaseReleaseStarted,
            None,
            Some(&current),
            "LEASE_RELEASE_STARTED",
        )?;
        let releasing = self
            .runtime
            .daemon
            .begin_release(&current.name, fence_token)
            .map_err(Self::provider_rejection)?;
        let (worker_id, journal_instance_name) = {
            let mut instances = self
                .runtime
                .instances
                .lock()
                .expect("instance lock poisoned");
            let process = instances.iter_mut().find(|(_, process)| {
                process.lease.as_ref().is_some_and(|lease| {
                    lease.lease_name == current.name.as_str() && lease.fence_token == fence_token
                })
            });
            if let Some((worker_id, process)) = process {
                if let Some(worker) = process.semantic_worker.as_mut() {
                    worker.state = semantic::WorkerState::Draining;
                }
                let report = match process.actor.stop(&cy_kernel_api::StopRequest {
                    grace_period: self.runtime.heartbeat.graceful_stop,
                    immediate: false,
                }) {
                    Ok(report) => report.clone(),
                    Err(error) => {
                        let _ = self
                            .runtime
                            .daemon
                            .fail_release(&releasing.name, fence_token);
                        return Err(Self::provider_rejection(error));
                    }
                };
                if !report.complete {
                    if let Some(worker) = process.semantic_worker.as_mut() {
                        worker.state = semantic::WorkerState::Failed;
                    }
                    let _ = self
                        .runtime
                        .daemon
                        .fail_release(&releasing.name, fence_token);
                    // Class C: the Lease was already fail_released (FAILED)
                    // with the allocation held; the cleanup-failed record is
                    // telemetry. The rejection below is the fail-closed gate.
                    let _ = self.record_runtime(
                        RuntimeJournalEvent::InstanceCleanupFailed,
                        Some(worker_id),
                        Some(&releasing),
                        &report.reason_code,
                    );
                    return Err(Self::rejection("CLEANUP_INCOMPLETE", report.reason_code));
                }
                if let Some(worker) = process.semantic_worker.as_mut() {
                    worker.state = semantic::WorkerState::Stopped;
                }
                (
                    Some(worker_id.clone()),
                    process
                        .semantic_worker
                        .as_ref()
                        .map(|worker| worker.identity.id.clone())
                        .unwrap_or_else(|| worker_id.clone()),
                )
            } else {
                if retrying_failed_cleanup {
                    let _ = self
                        .runtime
                        .daemon
                        .fail_release(&releasing.name, fence_token);
                    return Err(Self::rejection(
                        "CLEANUP_INCOMPLETE",
                        "a failed lease has no managed actor to prove physical cleanup",
                    ));
                }
                (None, String::new())
            }
        };
        if worker_id.is_some() {
            self.record_runtime(
                RuntimeJournalEvent::InstanceTerminated,
                Some(&journal_instance_name),
                Some(&releasing),
                "CLEANUP_COMPLETE",
            )?;
        }
        self.record_runtime(
            RuntimeJournalEvent::LeaseReleased,
            worker_id.as_deref(),
            Some(&releasing),
            "LEASE_RELEASED",
        )?;
        let released = self
            .runtime
            .daemon
            .complete_release(&current.name, fence_token)
            .map_err(Self::provider_rejection)?;
        // The Worker that held the released Lease no longer has authority: its
        // published Endpoints and dependent Grants must not outlive the Lease.
        // 持有已释放 Lease 的 Worker 已不再具有 authority：其已发布 Endpoint 和依赖它的 Grant 不能在 Lease 生命周期结束后继续存在。
        self.purge_endpoint_authority(&current.holder);
        Ok(Self::semantic_lease(&object, &released))
    }
}
