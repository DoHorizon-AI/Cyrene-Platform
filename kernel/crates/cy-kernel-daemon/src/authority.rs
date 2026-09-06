// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: kernel/crates/cy-kernel-daemon/src/authority.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! Transport-independent storage and coordination for canonical authority actions.

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
#[doc(hidden)]
pub struct AuthorityRuntime {
    pub(crate) daemon: Arc<KernelDaemon>,
    pub(crate) resolver: Arc<dyn InstalledPluginResolver>,
    pub(crate) instances: Arc<Mutex<HashMap<String, ManagedProcess>>>,
    /// The legacy process map is keyed by a private runtime name. Canonical
    /// authority lookup always enters through this typed namespace index.
    pub(crate) workers: Arc<Mutex<BTreeMap<ObjectRef, String>>>,
    pub(crate) leases: Arc<Mutex<BTreeMap<ObjectRef, String>>>,
    pub(crate) semantic_operations: Arc<Mutex<BTreeMap<ObjectRef, semantic::Operation>>>,
    pub(crate) semantic_events: Arc<Mutex<BTreeMap<NamespaceId, NamespaceEventHistory>>>,
    pub(crate) endpoints: Arc<Mutex<BTreeMap<ObjectRef, semantic::Endpoint>>>,
    pub(crate) endpoint_grants: Arc<Mutex<BTreeMap<ObjectRef, semantic::EndpointGrant>>>,
    /// Provider records are keyed by namespace and logical ID, never by the
    /// transport socket or session-generation-bearing Identity.
    pub(crate) providers: Arc<Mutex<BTreeMap<(NamespaceId, String), ProviderRecord>>>,
    /// First mutation binds a namespace to its authenticated Principal. This
    /// minimal local policy prevents an unrelated UDS peer from controlling it.
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
        // `Snapshot @ C + events_after(C)` reconstruct current state without
        // losing a transition that publishes between the state read and the
        // cursor read (the single-mutex hole that previously let a Worker or
        // Operation vanish from both the snapshot and the incremental replay).
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

impl KernelAuthority for LocalKernelAuthority {
    fn negotiate(
        &self,
        _principal: &semantic::Principal,
        offered: &[semantic::ContractRevision],
    ) -> Result<semantic::ContractRevision, semantic::Rejection> {
        let local = self.contract_revision();
        offered
            .iter()
            .filter_map(|revision| local.negotiate(revision))
            .max_by_key(|revision| revision.minor)
            .ok_or_else(|| {
                Self::rejection(
                    "CONTRACT_INCOMPATIBLE",
                    "no offered semantic contract revision is compatible with this Kernel",
                )
            })
    }

    fn acquire_lease(
        &self,
        context: &AuthorityCallContext,
        principal: &semantic::Principal,
        holder: semantic::Identity,
        query: semantic::ResourceQuery,
        expires_at_unix_ms: u64,
    ) -> Result<semantic::Lease, semantic::Rejection> {
        self.validate_context(context)?;
        self.bind_namespace(context, principal)?;
        if expires_at_unix_ms <= Self::now_unix_ms() {
            return Err(Self::rejection(
                "LEASE_EXPIRY_INVALID",
                "lease expiry must be in the future",
            ));
        }
        let identity = Self::lease_name(context);
        let object = context.object_ref(identity);
        let lease = self
            .runtime
            .daemon
            .reserve(ResourceRequest {
                lease_name: Self::lease_internal_name(context, &object.identity),
                expected_inventory_generation: self.runtime.daemon.resources.inventory().generation,
                holder,
                query,
                expires_at_unix_ms: Some(expires_at_unix_ms),
                limits: CgroupLimits::default(),
            })
            .map_err(Self::provider_rejection)?;
        if let Err(error) = self.record_runtime(
            RuntimeJournalEvent::LeaseReserved,
            None,
            Some(&lease),
            "LEASE_RESERVED",
        ) {
            let _ = self
                .runtime
                .daemon
                .begin_release(&lease.name, lease.fence_token);
            let _ = self
                .runtime
                .daemon
                .complete_release(&lease.name, lease.fence_token);
            return Err(error);
        }
        self.runtime
            .leases
            .lock()
            .expect("lease scope lock poisoned")
            .insert(object.clone(), lease.name.clone());
        Ok(Self::semantic_lease(&object, &lease))
    }

    fn renew_lease(
        &self,
        context: &AuthorityCallContext,
        principal: &semantic::Principal,
        lease: &semantic::Identity,
        fence_token: u64,
        expires_at_unix_ms: u64,
    ) -> Result<semantic::Lease, semantic::Rejection> {
        self.validate_context(context)?;
        self.require_namespace_owner(context, principal)?;
        let object = context.object_ref(lease.clone());
        let current = self.lease_for(context, lease)?;
        if current.generation != lease.generation {
            return Err(Self::rejection(
                "LEASE_GENERATION_STALE",
                "lease generation no longer has authority",
            ));
        }
        self.runtime
            .daemon
            .renew(&current.name, fence_token, expires_at_unix_ms)
            .map(|lease| Self::semantic_lease(&object, &lease))
            .map_err(Self::provider_rejection)
    }

    fn release_lease(
        &self,
        context: &AuthorityCallContext,
        principal: &semantic::Principal,
        lease: &semantic::Identity,
        fence_token: u64,
    ) -> Result<semantic::Lease, semantic::Rejection> {
        self.validate_context(context)?;
        self.require_namespace_owner(context, principal)?;
        self.release_with_cleanup(context, lease, fence_token)
    }

    fn start_worker(
        &self,
        context: &AuthorityCallContext,
        principal: &semantic::Principal,
        mut worker: semantic::Worker,
    ) -> Result<semantic::Operation, semantic::Rejection> {
        self.validate_context(context)?;
        self.require_namespace_owner(context, principal)?;
        worker.principal = principal.identity.clone();
        if !matches!(
            worker.state,
            semantic::WorkerState::Registered | semantic::WorkerState::Starting
        ) {
            return Err(Self::rejection(
                "STATE_TRANSITION_INVALID",
                "StartWorker requires REGISTERED or STARTING state",
            ));
        }
        let worker_object = context.object_ref(worker.identity.clone());
        if self
            .runtime
            .workers
            .lock()
            .expect("worker scope lock poisoned")
            .contains_key(&worker_object)
        {
            return Err(Self::rejection(
                "WORKER_EXISTS",
                "worker identity is already managed by this Kernel",
            ));
        }
        let lease = self.lease_for(context, &worker.lease)?;
        if lease.generation != worker.lease.generation
            || lease.holder != worker.identity
            || lease.state != LeaseState::Active
            || lease
                .expires_at_unix_ms
                .is_some_and(|exp| exp <= Self::now_unix_ms())
        {
            return Err(Self::rejection(
                "LEASE_NOT_ACTIVE",
                "worker does not hold the referenced active Lease incarnation",
            ));
        }
        let binding = self
            .runtime
            .daemon
            .binding_for_lease(&lease)
            .map_err(Self::provider_rejection)?;
        let resolved = self
            .runtime
            .resolver
            .resolve_worker_launch_plan(&worker)
            .map_err(Self::provider_rejection)?;
        let mut plan = resolved.plan;
        if plan.instance_name != worker.identity.id {
            return Err(Self::rejection(
                "EXECUTION_REFERENCE_MISMATCH",
                "resolver returned a plan for another Worker identity",
            ));
        }
        let instance_name = Self::scoped_runtime_name("worker", &worker_object);
        plan.instance_name = instance_name.clone();
        plan.limits =
            crate::convert::worker::enforced_worker_limits(&lease.limits, &worker.limits)?;
        plan.environment = inject_heartbeat_environment(
            plan.environment,
            &self.runtime.heartbeat,
            &instance_name,
            lease.fence_token,
        )
        .map_err(Self::provider_rejection)?;
        plan.environment = binding
            .merge_environment(&plan.environment)
            .map_err(Self::provider_rejection)?;
        let mut actor = InstanceActor::new(
            instance_name.clone(),
            lease.name.clone(),
            lease.fence_token,
            self.runtime.daemon.sandbox.clone(),
            plan,
            binding,
            self.runtime.heartbeat.timeout,
        );
        // Class B durable intent: persist the launch intent BEFORE the physical
        // spawn. If this record cannot be persisted, the spawn must not begin,
        // so a launch whose post-spawn outcome records are all lost (including
        // a crash after a double persistence failure) is still classifiable by
        // restart recovery as "intent present, outcome unknown".
        self.record_runtime(
            RuntimeJournalEvent::InstanceLaunching,
            Some(&worker.identity.id),
            Some(&lease),
            "WORKER_LAUNCHING",
        )?;
        actor.start().map_err(Self::provider_rejection)?;
        let evidence = match actor.recovery_evidence() {
            Ok(evidence) => evidence,
            Err(error) => {
                // The physical process was already spawned; it must be reaped
                // synchronously. If it cannot be reaped, leave durable cleanup
                // evidence so restart recovery can classify the incomplete
                // launch instead of losing an untracked execution domain.
                let stop_report = actor.stop(&cy_kernel_api::StopRequest {
                    grace_period: std::time::Duration::ZERO,
                    immediate: true,
                });
                if let Ok(report) = &stop_report {
                    if !report.complete {
                        let _ = self.record_runtime(
                            RuntimeJournalEvent::InstanceCleanupFailed,
                            Some(&worker.identity.id),
                            Some(&lease),
                            &report.reason_code,
                        );
                    }
                }
                return Err(Self::provider_rejection(error));
            }
        };
        if let Err(error) = self.record_runtime_launch(&worker.identity.id, &lease, evidence) {
            // Class B outcome: InstanceLaunched is persisted AFTER the physical
            // spawn, so the launched process must be reaped synchronously
            // before the failure is returned. If it cannot be reaped, durable
            // cleanup evidence keeps restart recovery able to classify the
            // incomplete launch.
            let stop_report = actor.stop(&cy_kernel_api::StopRequest {
                grace_period: std::time::Duration::ZERO,
                immediate: true,
            });
            if let Ok(report) = &stop_report {
                if !report.complete {
                    let _ = self.record_runtime(
                        RuntimeJournalEvent::InstanceCleanupFailed,
                        Some(&worker.identity.id),
                        Some(&lease),
                        &report.reason_code,
                    );
                }
            }
            return Err(error);
        }
        worker.state = semantic::WorkerState::Starting;
        let lease_ref = core_v1::ResourceLeaseRef {
            lease_name: lease.name.clone(),
            fence_token: lease.fence_token,
        };
        self.runtime
            .instances
            .lock()
            .expect("instance lock poisoned")
            .insert(
                instance_name.clone(),
                ManagedProcess {
                    actor,
                    lease: Some(lease_ref),
                    semantic_worker: Some(worker.clone()),
                    plugin: core_v1::InstalledPluginRef::default(),
                    generation: lease.fence_token,
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
                },
            );
        self.runtime
            .workers
            .lock()
            .expect("worker scope lock poisoned")
            .insert(worker_object, instance_name);
        self.publish_semantic_event_in(
            &context.namespace,
            worker.identity.clone(),
            "worker.starting",
            "cyrene.worker.v1",
            Vec::new(),
        );
        Ok(self.remember_operation(
            context,
            semantic::Operation {
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
                metadata: BTreeMap::from([("worker.id".to_string(), worker.identity.id)]),
            },
        ))
    }

    fn stop_worker(
        &self,
        context: &AuthorityCallContext,
        principal: &semantic::Principal,
        worker_identity: &semantic::Identity,
        lease_identity: &semantic::Identity,
        fence_token: u64,
        grace_period: std::time::Duration,
    ) -> Result<semantic::Operation, semantic::Rejection> {
        self.validate_context(context)?;
        self.require_namespace_owner(context, principal)?;
        let lease = self.lease_for(context, lease_identity)?;
        let worker_name = self.worker_instance_name(context, worker_identity)?;
        if lease.generation != lease_identity.generation
            || lease.fence_token != fence_token
            || lease.holder != *worker_identity
            || lease.state != LeaseState::Active
        {
            return Err(Self::rejection(
                "FENCE_MISMATCH",
                "worker stop no longer has active lease authority",
            ));
        }
        self.record_runtime(
            RuntimeJournalEvent::LeaseReleaseStarted,
            Some(&worker_identity.id),
            Some(&lease),
            "LEASE_RELEASE_STARTED",
        )?;
        let releasing = self
            .runtime
            .daemon
            .begin_release(&lease.name, fence_token)
            .map_err(Self::provider_rejection)?;
        let (worker, report) = {
            let mut instances = self
                .runtime
                .instances
                .lock()
                .expect("instance lock poisoned");
            let Some(process) = instances.get_mut(&worker_name) else {
                let _ = self
                    .runtime
                    .daemon
                    .fail_release(&releasing.name, fence_token);
                return Err(Self::rejection(
                    "WORKER_NOT_FOUND",
                    "worker is not managed by this Kernel",
                ));
            };
            let Some(worker) = process.semantic_worker.as_mut() else {
                let _ = self
                    .runtime
                    .daemon
                    .fail_release(&releasing.name, fence_token);
                return Err(Self::rejection(
                    "WORKER_COMPATIBILITY_ONLY",
                    "legacy plugin process is not a semantic Worker",
                ));
            };
            if worker.identity != *worker_identity || worker.lease != *lease_identity {
                let _ = self
                    .runtime
                    .daemon
                    .fail_release(&releasing.name, fence_token);
                return Err(Self::rejection(
                    "STALE_GENERATION",
                    "worker identity or lease generation is stale",
                ));
            }
            worker.state = semantic::WorkerState::Draining;
            let report = match process.actor.stop(&cy_kernel_api::StopRequest {
                grace_period,
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
            worker.state = if report.complete {
                semantic::WorkerState::Stopped
            } else {
                semantic::WorkerState::Failed
            };
            (worker.clone(), report)
        };
        if report.complete {
            self.record_runtime(
                RuntimeJournalEvent::InstanceTerminated,
                Some(&worker_identity.id),
                Some(&releasing),
                &report.reason_code,
            )?;
            self.record_runtime(
                RuntimeJournalEvent::LeaseReleased,
                Some(&worker_identity.id),
                Some(&releasing),
                "LEASE_RELEASED",
            )?;
            self.runtime
                .daemon
                .complete_release(&lease.name, fence_token)
                .map_err(Self::provider_rejection)?;
            // The stopped Worker no longer holds an active Lease; its Endpoints
            // and Grants lose authority and must not remain in Kernel state.
            self.purge_endpoint_authority(worker_identity);
        } else {
            let _ = self
                .runtime
                .daemon
                .fail_release(&releasing.name, fence_token);
            // Class C: the Lease was already fail_released (FAILED) with the
            // allocation held; the cleanup-failed record is telemetry.
            let _ = self.record_runtime(
                RuntimeJournalEvent::InstanceCleanupFailed,
                Some(&worker_identity.id),
                Some(&releasing),
                &report.reason_code,
            );
        }
        self.publish_semantic_event_in(
            &context.namespace,
            worker.identity.clone(),
            if report.complete {
                "worker.stopped"
            } else {
                "worker.failed"
            },
            "cyrene.worker.v1",
            Vec::new(),
        );
        Ok(self.remember_operation(
            context,
            semantic::Operation {
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
            },
        ))
    }

    fn heartbeat_worker(
        &self,
        context: &AuthorityCallContext,
        principal: &semantic::Principal,
        worker_identity: &semantic::Identity,
        lease_identity: &semantic::Identity,
        fence_token: u64,
    ) -> Result<semantic::Worker, semantic::Rejection> {
        self.validate_context(context)?;
        self.require_namespace_owner(context, principal)?;
        let lease = self.lease_for(context, lease_identity)?;
        if lease.generation != lease_identity.generation
            || lease.fence_token != fence_token
            || lease.holder != *worker_identity
            || lease.state != LeaseState::Active
            || lease
                .expires_at_unix_ms
                .is_some_and(|exp| exp <= Self::now_unix_ms())
        {
            return Err(Self::rejection(
                "FENCE_MISMATCH",
                "worker heartbeat no longer has active lease authority",
            ));
        }
        let worker_name = self.worker_instance_name(context, worker_identity)?;
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
                "legacy plugin process is not a semantic Worker",
            )
        })?;
        if worker.identity != *worker_identity || worker.lease != *lease_identity {
            return Err(Self::rejection(
                "STALE_GENERATION",
                "worker identity or lease generation is stale",
            ));
        }
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

    fn create_operation(
        &self,
        context: &AuthorityCallContext,
        principal: &semantic::Principal,
        operation: semantic::Operation,
    ) -> Result<semantic::Operation, semantic::Rejection> {
        self.validate_context(context)?;
        self.bind_namespace(context, principal)?;
        if operation.state != semantic::OperationState::Created {
            return Err(Self::rejection(
                "STATE_TRANSITION_INVALID",
                "CreateOperation requires CREATED state",
            ));
        }
        let key = context.object_ref(operation.identity.clone());
        let mut operations = self
            .runtime
            .semantic_operations
            .lock()
            .expect("semantic operation lock poisoned");
        if let Some(existing) = operations.get(&key) {
            if existing == &operation {
                return Ok(existing.clone());
            }
            return Err(Self::rejection(
                "OPERATION_EXISTS",
                "an operation with this identity already exists",
            ));
        }
        if operations.iter().any(|(object, existing)| {
            object.namespace == context.namespace
                && existing.identity.id == operation.identity.id
                && existing.identity.generation > operation.identity.generation
        }) {
            return Err(Self::rejection(
                "STALE_GENERATION",
                "operation identity generation is stale",
            ));
        }
        operations.insert(key, operation.clone());
        drop(operations);
        self.publish_semantic_event_in(
            &context.namespace,
            operation.identity.clone(),
            "operation.created",
            "cyrene.operation.v1",
            Vec::new(),
        );
        Ok(operation)
    }

    fn report_operation(
        &self,
        context: &AuthorityCallContext,
        principal: &semantic::Principal,
        reported: semantic::Operation,
    ) -> Result<semantic::Operation, semantic::Rejection> {
        self.validate_context(context)?;
        self.require_namespace_owner(context, principal)?;
        let key = context.object_ref(reported.identity.clone());
        let mut operations = self
            .runtime
            .semantic_operations
            .lock()
            .expect("semantic operation lock poisoned");
        let current = operations
            .get(&key)
            .cloned()
            .ok_or_else(|| Self::rejection("OPERATION_NOT_FOUND", "operation is unknown"))?;
        if current.owner != reported.owner
            || current.executor != reported.executor
            || current.kind != reported.kind
            || current.deadline_unix_ms != reported.deadline_unix_ms
            || current.parent != reported.parent
        {
            return Err(Self::rejection(
                "OPERATION_IMMUTABLE_FIELDS_CHANGED",
                "operation report attempted to change immutable authority fields",
            ));
        }
        if !current.state.can_transition_to(reported.state) {
            return Err(Self::rejection(
                "STATE_TRANSITION_INVALID",
                "operation report is not an idempotent or forward transition",
            ));
        }
        operations.insert(key, reported.clone());
        drop(operations);
        self.publish_semantic_event_in(
            &context.namespace,
            reported.identity.clone(),
            semantic_operation_event_kind(reported.state),
            "cyrene.operation.v1",
            Vec::new(),
        );
        Ok(reported)
    }

    fn cancel_operation(
        &self,
        context: &AuthorityCallContext,
        principal: &semantic::Principal,
        identity: &semantic::Identity,
    ) -> Result<semantic::Operation, semantic::Rejection> {
        self.validate_context(context)?;
        self.require_namespace_owner(context, principal)?;
        let key = context.object_ref(identity.clone());
        let mut operations = self
            .runtime
            .semantic_operations
            .lock()
            .expect("semantic operation lock poisoned");
        let current = operations
            .get(&key)
            .cloned()
            .ok_or_else(|| Self::rejection("OPERATION_NOT_FOUND", "operation is unknown"))?;
        if matches!(
            current.state,
            semantic::OperationState::Cancelling | semantic::OperationState::Cancelled
        ) {
            return Ok(current);
        }
        let mut cancelling = current.clone();
        {
            if !current
                .state
                .can_transition_to(semantic::OperationState::Cancelling)
            {
                return Err(Self::rejection(
                    "STATE_TRANSITION_INVALID",
                    "operation cannot be cancelled from its current state",
                ));
            }
            cancelling.state = semantic::OperationState::Cancelling;
            operations.insert(key, cancelling.clone());
        }
        drop(operations);
        self.publish_semantic_event_in(
            &context.namespace,
            cancelling.identity.clone(),
            semantic_operation_event_kind(cancelling.state),
            "cyrene.operation.v1",
            Vec::new(),
        );
        let executor = self
            .runtime
            .instances
            .lock()
            .expect("instance lock poisoned")
            .iter_mut()
            .find(|(_, process)| {
                process
                    .semantic_worker
                    .as_ref()
                    .is_some_and(|worker| cancelling.executor == worker.identity)
            })
            .map(|(_, process)| process.actor.request_cancel(cancelling.identity.id.clone()));
        match executor {
            Some(Ok(receipt)) => {
                self.schedule_cancellation_deadline(
                    context.clone(),
                    cancelling.identity.clone(),
                    receipt,
                );
                Ok(cancelling)
            }
            Some(Err(_)) | None => {
                Ok(self.mark_cancelling_operation_lost(context, &cancelling.identity))
            }
        }
    }

    fn publish_endpoint(
        &self,
        context: &AuthorityCallContext,
        principal: &semantic::Principal,
        endpoint: semantic::Endpoint,
    ) -> Result<semantic::Endpoint, semantic::Rejection> {
        self.validate_context(context)?;
        self.require_namespace_owner(context, principal)?;
        let owner_name = self.worker_instance_name(context, &endpoint.owner)?;
        let owner = self
            .runtime
            .instances
            .lock()
            .expect("instance lock poisoned")
            .get(&owner_name)
            .and_then(|process| process.semantic_worker.as_ref())
            .filter(|worker| worker.identity == endpoint.owner)
            .cloned()
            .ok_or_else(|| {
                Self::rejection(
                    "ENDPOINT_OWNER_UNKNOWN",
                    "endpoint owner is not an active managed Worker incarnation",
                )
            })?;
        if owner.principal != principal.identity {
            return Err(Self::rejection(
                "AUTHORITY_DENIED",
                "only the Worker owner Principal may publish an Endpoint",
            ));
        }
        let owner_lease = self.lease_for(context, &owner.lease)?;
        if owner_lease.generation != owner.lease.generation
            || owner_lease.holder != owner.identity
            || owner_lease.state != LeaseState::Active
            || owner_lease
                .expires_at_unix_ms
                .is_some_and(|exp| exp <= Self::now_unix_ms())
        {
            return Err(Self::rejection(
                "LEASE_NOT_ACTIVE",
                "endpoint owner no longer has an active Lease incarnation",
            ));
        }
        let endpoint_key = context.object_ref(endpoint.identity.clone());
        let mut endpoints = self
            .runtime
            .endpoints
            .lock()
            .expect("endpoint lock poisoned");
        if endpoints.iter().any(|(object, current)| {
            object.namespace == context.namespace
                && current.identity.id == endpoint.identity.id
                && current.identity.generation > endpoint.identity.generation
        }) {
            return Err(Self::rejection(
                "STALE_GENERATION",
                "endpoint identity generation is stale",
            ));
        }
        endpoints.retain(|object, current| {
            object.namespace != context.namespace
                || current.identity.id != endpoint.identity.id
                || current.identity.generation >= endpoint.identity.generation
        });
        endpoints.insert(endpoint_key, endpoint.clone());
        drop(endpoints);
        self.runtime
            .endpoint_grants
            .lock()
            .expect("endpoint grant lock poisoned")
            .retain(|object, grant| {
                object.namespace != context.namespace
                    || grant.endpoint.id != endpoint.identity.id
                    || grant.endpoint == endpoint.identity
            });
        Ok(endpoint)
    }

    fn authorize_endpoint(
        &self,
        context: &AuthorityCallContext,
        principal: &semantic::Principal,
        grant: semantic::EndpointGrant,
    ) -> Result<semantic::EndpointGrant, semantic::Rejection> {
        self.validate_context(context)?;
        self.require_namespace_owner(context, principal)?;
        let endpoint = self
            .runtime
            .endpoints
            .lock()
            .expect("endpoint lock poisoned")
            .get(&context.object_ref(grant.endpoint.clone()))
            .filter(|endpoint| endpoint.identity == grant.endpoint)
            .cloned()
            .ok_or_else(|| {
                Self::rejection(
                    "ENDPOINT_NOT_FOUND",
                    "endpoint grant targets an unpublished endpoint",
                )
            })?;
        let owner_name = self.worker_instance_name(context, &endpoint.owner)?;
        let owner = self
            .runtime
            .instances
            .lock()
            .expect("instance lock poisoned")
            .get(&owner_name)
            .and_then(|process| process.semantic_worker.as_ref())
            .filter(|worker| worker.identity == endpoint.owner)
            .cloned()
            .ok_or_else(|| {
                Self::rejection(
                    "ENDPOINT_OWNER_UNKNOWN",
                    "endpoint owner is not an active managed Worker incarnation",
                )
            })?;
        if owner.principal != principal.identity {
            return Err(Self::rejection(
                "AUTHORITY_DENIED",
                "only the Worker owner Principal may authorize an Endpoint grant",
            ));
        }
        let lease = self.lease_for(context, &grant.lease)?;
        if lease.generation != grant.lease.generation {
            return Err(Self::rejection(
                "LEASE_GENERATION_STALE",
                "endpoint grant lease generation is stale",
            ));
        }
        if lease.fence_token != grant.fence_token {
            return Err(Self::rejection(
                "STALE_FENCE_TOKEN",
                "endpoint grant lease fence token is stale",
            ));
        }
        if lease.state != LeaseState::Active
            || lease.holder != grant.grantee
            || lease
                .expires_at_unix_ms
                .is_some_and(|exp| exp <= Self::now_unix_ms())
        {
            return Err(Self::rejection(
                "LEASE_NOT_ACTIVE",
                "endpoint grant grantee does not hold an active lease",
            ));
        }
        if grant.expires_at_unix_ms <= Self::now_unix_ms()
            || lease
                .expires_at_unix_ms
                .is_some_and(|lease_expiry| grant.expires_at_unix_ms > lease_expiry)
        {
            return Err(Self::rejection(
                "ENDPOINT_GRANT_EXPIRY_INVALID",
                "endpoint grant expiry must be active and bounded by its lease",
            ));
        }
        self.runtime
            .endpoint_grants
            .lock()
            .expect("endpoint grant lock poisoned")
            .insert(context.object_ref(grant.identity.clone()), grant.clone());
        Ok(grant)
    }

    fn revoke_endpoint(
        &self,
        context: &AuthorityCallContext,
        principal: &semantic::Principal,
        grant: &semantic::Identity,
    ) -> Result<(), semantic::Rejection> {
        self.validate_context(context)?;
        self.require_namespace_owner(context, principal)?;
        let grant_key = context.object_ref(grant.clone());
        let stored_grant = self
            .runtime
            .endpoint_grants
            .lock()
            .expect("endpoint grant lock poisoned")
            .get(&grant_key)
            .cloned();
        if let Some(stored_grant) = stored_grant {
            let endpoint = self
                .runtime
                .endpoints
                .lock()
                .expect("endpoint lock poisoned")
                .get(&context.object_ref(stored_grant.endpoint.clone()))
                .filter(|endpoint| endpoint.identity == stored_grant.endpoint)
                .cloned()
                .ok_or_else(|| {
                    Self::rejection(
                        "ENDPOINT_OWNER_UNKNOWN",
                        "endpoint grant no longer has an active endpoint owner",
                    )
                })?;
            let owner_name = self.worker_instance_name(context, &endpoint.owner)?;
            let owner = self
                .runtime
                .instances
                .lock()
                .expect("instance lock poisoned")
                .get(&owner_name)
                .and_then(|process| process.semantic_worker.as_ref())
                .filter(|worker| worker.identity == endpoint.owner)
                .cloned()
                .ok_or_else(|| {
                    Self::rejection(
                        "ENDPOINT_OWNER_UNKNOWN",
                        "endpoint grant owner is not an active managed Worker incarnation",
                    )
                })?;
            if owner.principal != principal.identity {
                return Err(Self::rejection(
                    "AUTHORITY_DENIED",
                    "only the Worker owner Principal may revoke an Endpoint grant",
                ));
            }
        }
        self.runtime
            .endpoint_grants
            .lock()
            .expect("endpoint grant lock poisoned")
            .remove(&grant_key);
        Ok(())
    }

    fn events_after(
        &self,
        context: &AuthorityCallContext,
        principal: &semantic::Principal,
        cursor: &semantic::EventCursor,
        limit: usize,
    ) -> Result<semantic::EventPage, semantic::Rejection> {
        self.validate_context(context)?;
        self.require_namespace_owner(context, principal)?;
        if limit == 0 || limit > OPERATION_EVENT_HISTORY_CAPACITY {
            return Err(Self::rejection(
                "EVENT_PAGE_LIMIT_INVALID",
                "event page_size must be in 1..=256",
            ));
        }
        self.events_after_inner(&context.namespace, cursor, limit)
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

impl KernelProviderAuthority for LocalKernelAuthority {
    fn register_provider(
        &self,
        context: &AuthorityCallContext,
        principal: &semantic::Principal,
        provider: semantic::Provider,
    ) -> Result<semantic::Provider, semantic::Rejection> {
        self.validate_context(context)?;
        if provider.identity.generation == 0 {
            return Err(Self::rejection(
                "GENERATION_INVALID",
                "provider session generation must be non-zero",
            ));
        }
        provider
            .validate()
            .map_err(|error| Self::rejection(error.reason_code, error.message))?;

        let key = (context.namespace.clone(), provider.identity.id.clone());
        let registered = {
            let mut providers = self
                .runtime
                .providers
                .lock()
                .expect("provider state lock poisoned");
            match providers.entry(key) {
                Entry::Vacant(entry) => {
                    entry.insert(ProviderRecord {
                        provider: provider.clone(),
                        principal: principal.clone(),
                        inventory_scope: ProviderInventoryScope::Full,
                        inventory: None,
                        reconciled_snapshot_generation: None,
                        pending_stale_workers: Vec::new(),
                    });
                    provider.clone()
                }
                Entry::Occupied(mut entry) => {
                    let current = entry.get();
                    if current.principal != *principal {
                        return Err(Self::rejection(
                            "AUTHORITY_DENIED",
                            "Provider session belongs to another authenticated Principal",
                        ));
                    }
                    if provider.identity.generation < current.provider.identity.generation {
                        return Err(Self::rejection(
                            "STALE_GENERATION",
                            "provider session generation is stale",
                        ));
                    }
                    if provider.identity.generation == current.provider.identity.generation {
                        if current.provider == provider {
                            return Ok(current.provider.clone());
                        }
                        if current.provider.state != semantic::ProviderState::Unavailable
                            && provider.state == semantic::ProviderState::Unavailable
                            && current.provider.capabilities == provider.capabilities
                        {
                            entry.insert(ProviderRecord {
                                provider: provider.clone(),
                                principal: principal.clone(),
                                inventory_scope: ProviderInventoryScope::Full,
                                // A disconnected transport invalidates only
                                // its session-bound evidence. The logical
                                // Provider identity remains unchanged until a
                                // later registration establishes a new session.
                                inventory: None,
                                reconciled_snapshot_generation: None,
                                pending_stale_workers: Vec::new(),
                            });
                            provider.clone()
                        } else {
                            return Err(Self::rejection(
                                "STALE_GENERATION",
                                "provider session generation cannot change its facts or recover",
                            ));
                        }
                    } else {
                        entry.insert(ProviderRecord {
                            provider: provider.clone(),
                            principal: principal.clone(),
                            inventory_scope: ProviderInventoryScope::Full,
                            // A reconnect invalidates evidence tied to the old
                            // transport session, not resource or snapshot numbers.
                            inventory: None,
                            reconciled_snapshot_generation: None,
                            pending_stale_workers: Vec::new(),
                        });
                        provider.clone()
                    }
                }
            }
        };
        self.publish_semantic_event_in(
            &context.namespace,
            registered.identity.clone(),
            "provider.registered",
            "cyrene.provider.v1",
            Vec::new(),
        );
        Ok(registered)
    }

    fn publish_inventory(
        &self,
        context: &AuthorityCallContext,
        principal: &semantic::Principal,
        snapshot: semantic::ProviderSnapshot,
    ) -> Result<semantic::ProviderSnapshot, semantic::Rejection> {
        self.validate_context(context)?;
        snapshot
            .validate()
            .map_err(|error| Self::rejection(error.reason_code, error.message))?;

        let key = (context.namespace.clone(), snapshot.provider.id.clone());
        {
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
            if record.principal != *principal {
                return Err(Self::rejection(
                    "AUTHORITY_DENIED",
                    "Provider inventory caller does not own the registered session",
                ));
            }
            if record.provider.identity != snapshot.provider {
                return Err(Self::rejection(
                    "STALE_GENERATION",
                    "inventory belongs to a stale provider session",
                ));
            }
            if record
                .inventory
                .as_ref()
                .is_some_and(|current| snapshot.snapshot_generation <= current.snapshot_generation)
            {
                return Err(Self::rejection(
                    "STALE_GENERATION",
                    "inventory snapshot generation must advance",
                ));
            }
            record.inventory = Some(snapshot.clone());
            record.reconciled_snapshot_generation = None;
        }
        self.publish_semantic_event_in(
            &context.namespace,
            snapshot.provider.clone(),
            "provider.inventory.observed",
            "cyrene.provider.v1",
            Vec::new(),
        );
        Ok(snapshot)
    }

    fn reconcile_provider(
        &self,
        context: &AuthorityCallContext,
        principal: &semantic::Principal,
        provider: &semantic::Identity,
    ) -> Result<ProviderReconcileResult, semantic::Rejection> {
        self.validate_context(context)?;
        let key = (context.namespace.clone(), provider.id.clone());
        let (provider, snapshot_generation, snapshot, already_reconciled, resource_facts_only) = {
            let mut providers = self
                .runtime
                .providers
                .lock()
                .expect("provider state lock poisoned");
            let record = providers.get_mut(&key).ok_or_else(|| {
                Self::rejection(
                    "PROVIDER_NOT_FOUND",
                    "provider is unknown in this namespace",
                )
            })?;
            if record.principal != *principal {
                return Err(Self::rejection(
                    "AUTHORITY_DENIED",
                    "Provider reconciliation caller does not own the registered session",
                ));
            }
            if record.provider.identity != *provider {
                return Err(Self::rejection(
                    "STALE_GENERATION",
                    "reconciliation targets a stale provider session",
                ));
            }
            let inventory_expired = record
                .inventory
                .as_ref()
                .is_some_and(|snapshot| snapshot.expires_at_unix_ms <= Self::now_unix_ms());
            if inventory_expired {
                record.inventory = None;
                record.reconciled_snapshot_generation = None;
            }
            let snapshot_generation = record
                .inventory
                .as_ref()
                .map_or(0, |snapshot| snapshot.snapshot_generation);
            if record.inventory.is_none()
                && record.provider.state != semantic::ProviderState::Unavailable
                && !inventory_expired
            {
                return Err(Self::rejection(
                    "PROVIDER_INVENTORY_MISSING",
                    "provider has no current inventory",
                ));
            }
            (
                record.provider.identity.clone(),
                snapshot_generation,
                record.inventory.clone(),
                record.reconciled_snapshot_generation == Some(snapshot_generation)
                    && record.pending_stale_workers.is_empty(),
                record.inventory_scope == ProviderInventoryScope::ResourceFactsOnly,
            )
        };
        if already_reconciled {
            return Ok(ProviderReconcileResult {
                provider,
                snapshot_generation,
                actions: vec![ProviderReconcileAction::Noop],
            });
        }

        if resource_facts_only {
            let mut actions = snapshot
                .as_ref()
                .map(|snapshot| {
                    snapshot
                        .resources
                        .iter()
                        .map(|resource| {
                            ProviderReconcileAction::RefreshResource(resource.identity.clone())
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if actions.is_empty() {
                actions.push(ProviderReconcileAction::Noop);
            }
            self.runtime
                .providers
                .lock()
                .expect("provider state lock poisoned")
                .get_mut(&key)
                .expect("provider was checked above")
                .reconciled_snapshot_generation = Some(snapshot_generation);
            self.publish_semantic_event_in(
                &context.namespace,
                provider.clone(),
                "provider.reconciled",
                "cyrene.provider.v1",
                Vec::new(),
            );
            return Ok(ProviderReconcileResult {
                provider,
                snapshot_generation,
                actions,
            });
        }

        let mut actions = snapshot
            .as_ref()
            .map(|snapshot| {
                snapshot
                    .resources
                    .iter()
                    .map(|resource| {
                        ProviderReconcileAction::RefreshResource(resource.identity.clone())
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let observed_workers = snapshot
            .as_ref()
            .map(|snapshot| {
                snapshot
                    .workers
                    .iter()
                    .map(|worker| worker.identity.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let recorded_worker_names = self
            .runtime
            .workers
            .lock()
            .expect("worker scope lock poisoned")
            .iter()
            .filter(|(object, _)| object.namespace == context.namespace)
            .map(|(_, instance_name)| instance_name.clone())
            .collect::<Vec<_>>();
        let recorded_workers = self
            .runtime
            .instances
            .lock()
            .expect("instance lock poisoned")
            .iter()
            .filter(|(instance_name, _)| recorded_worker_names.contains(instance_name))
            .filter_map(|(_, process)| process.semantic_worker.clone())
            .filter(|worker| {
                worker.provider.id == provider.id
                    && !matches!(
                        worker.state,
                        semantic::WorkerState::Stopped
                            | semantic::WorkerState::Failed
                            | semantic::WorkerState::Lost
                    )
            })
            .collect::<Vec<_>>();
        for worker in recorded_workers {
            let missing_from_provider = !observed_workers.contains(&worker.identity);
            let lease_is_valid = self.lease_for(context, &worker.lease).is_ok_and(|lease| {
                lease.state == LeaseState::Active && lease.holder == worker.identity
            });
            if missing_from_provider || !lease_is_valid {
                actions.extend(self.mark_worker_lost(
                    context,
                    &worker.identity,
                    if missing_from_provider && snapshot.is_some() {
                        "WORKER_MISSING_FROM_PROVIDER"
                    } else if missing_from_provider {
                        "PROVIDER_UNAVAILABLE"
                    } else {
                        "WORKER_LEASE_INVALID"
                    },
                )?);
            }
        }
        let mut stale_workers = Vec::new();
        for observed in observed_workers {
            if !self
                .runtime
                .workers
                .lock()
                .expect("worker scope lock poisoned")
                .contains_key(&context.object_ref(observed.clone()))
            {
                stale_workers.push(observed);
            }
        }
        {
            let mut providers = self
                .runtime
                .providers
                .lock()
                .expect("provider state lock poisoned");
            let record = providers.get_mut(&key).expect("provider was checked above");
            for worker in stale_workers {
                if !record
                    .pending_stale_workers
                    .iter()
                    .any(|(candidate, _)| candidate == &worker)
                {
                    record
                        .pending_stale_workers
                        .push((worker, snapshot_generation));
                }
            }
            actions.extend(
                record.pending_stale_workers.iter().map(|(worker, _)| {
                    ProviderReconcileAction::TerminateStaleWorker(worker.clone())
                }),
            );
        }
        if actions.is_empty() {
            actions.push(ProviderReconcileAction::Noop);
        }
        let converged = !actions
            .iter()
            .any(|action| matches!(action, ProviderReconcileAction::TerminateStaleWorker(_)));
        if converged {
            self.runtime
                .providers
                .lock()
                .expect("provider state lock poisoned")
                .get_mut(&key)
                .expect("provider was checked above")
                .reconciled_snapshot_generation = Some(snapshot_generation);
        }
        self.publish_semantic_event_in(
            &context.namespace,
            provider.clone(),
            "provider.reconciled",
            "cyrene.provider.v1",
            Vec::new(),
        );
        Ok(ProviderReconcileResult {
            provider,
            snapshot_generation,
            actions,
        })
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

    fn events_after_inner(
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
        self.purge_endpoint_authority(&current.holder);
        Ok(Self::semantic_lease(&object, &released))
    }
}
