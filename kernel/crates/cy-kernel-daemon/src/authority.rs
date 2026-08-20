//! Transport-independent storage and coordination for canonical authority actions.

use std::{
    collections::{btree_map::Entry, BTreeMap, HashMap, VecDeque},
    sync::{atomic::AtomicU64, Arc, Mutex},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use cy_kernel_api::{
    semantic, AuthorityCallContext, CgroupLimits, InstalledPluginResolver, KernelAuthority,
    LeaseState, NamespaceId, ObjectRef, ProviderError, ResourceLease, ResourceRequest,
    RuntimeJournalEvent, RuntimeJournalRecord, RuntimeJournalSink,
};
use cy_proto::core_v1;

use crate::{
    adapter::OPERATION_EVENT_HISTORY_CAPACITY,
    convert::{inject_heartbeat_environment, now_timestamp, semantic_operation_event_kind},
    daemon::KernelDaemon,
    session::{ManagedProcess, WorkerHeartbeatConfig},
    watchdog::InstanceActor,
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
    /// First mutation binds a namespace to its authenticated Principal. This
    /// minimal local policy prevents an unrelated UDS peer from controlling it.
    pub(crate) namespace_owners: Arc<Mutex<BTreeMap<NamespaceId, semantic::Identity>>>,
    pub(crate) heartbeat: WorkerHeartbeatConfig,
    pub(crate) next_control_connection: Arc<AtomicU64>,
    pub(crate) runtime_journal: Arc<dyn RuntimeJournalSink>,
}

pub(crate) struct NamespaceEventHistory {
    next_sequence: u64,
    events: VecDeque<semantic::Event>,
}

/// Production local authority for one Kernel process. It has no tonic request
/// or response dependency; transports project requests onto this owner.
#[derive(Clone)]
pub struct LocalKernelAuthority {
    pub(crate) runtime: Arc<AuthorityRuntime>,
}

impl LocalKernelAuthority {
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
                namespace_owners: Arc::new(Mutex::new(BTreeMap::new())),
                heartbeat: WorkerHeartbeatConfig::default(),
                next_control_connection: Arc::new(AtomicU64::new(1)),
                runtime_journal,
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
        plan.limits = lease.limits.clone();
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
        actor.start().map_err(Self::provider_rejection)?;
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
        let _ = self.record_runtime(
            RuntimeJournalEvent::InstanceLaunched,
            Some(&worker.identity.id),
            Some(&lease),
            "WORKER_LAUNCHED",
        );
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
                RuntimeJournalEvent::LeaseReleased,
                Some(&worker_identity.id),
                Some(&releasing),
                "LEASE_RELEASED",
            )?;
            self.runtime
                .daemon
                .complete_release(&lease.name, fence_token)
                .map_err(Self::provider_rejection)?;
            self.runtime
                .instances
                .lock()
                .expect("instance lock poisoned")
                .remove(&worker_name);
            self.runtime
                .workers
                .lock()
                .expect("worker scope lock poisoned")
                .remove(&context.object_ref(worker_identity.clone()));
        } else {
            let _ = self
                .runtime
                .daemon
                .fail_release(&releasing.name, fence_token);
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
        let mut cancelled = current.clone();
        if !matches!(
            current.state,
            semantic::OperationState::Cancelling | semantic::OperationState::Cancelled
        ) {
            if !current
                .state
                .can_transition_to(semantic::OperationState::Cancelling)
            {
                return Err(Self::rejection(
                    "STATE_TRANSITION_INVALID",
                    "operation cannot be cancelled from its current state",
                ));
            }
            cancelled.state = semantic::OperationState::Cancelling;
            operations.insert(key, cancelled.clone());
        }
        drop(operations);
        self.publish_semantic_event_in(
            &context.namespace,
            cancelled.identity.clone(),
            semantic_operation_event_kind(cancelled.state),
            "cyrene.operation.v1",
            Vec::new(),
        );
        Ok(cancelled)
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
        if lease.state != LeaseState::Active || lease.holder != grant.grantee {
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
        Ok(self.events_after_inner(&context.namespace, cursor, limit))
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
        let mut histories = self
            .runtime
            .semantic_events
            .lock()
            .expect("semantic event history lock poisoned");
        let history = histories
            .entry(namespace.clone())
            .or_insert_with(|| NamespaceEventHistory {
                next_sequence: 1,
                events: VecDeque::with_capacity(OPERATION_EVENT_HISTORY_CAPACITY),
            });
        let event = semantic::Event {
            sequence: history.next_sequence,
            source: self.semantic_event_source_for(namespace),
            subject,
            kind: kind.into(),
            observed_at_unix_ms: Self::now_unix_ms(),
            schema_id: schema_id.into(),
            body: body.into(),
        };
        if event.validate().is_err() {
            return;
        }
        history.next_sequence = history.next_sequence.saturating_add(1);
        if history.events.len() == OPERATION_EVENT_HISTORY_CAPACITY {
            history.events.pop_front();
        }
        history.events.push_back(event);
    }

    fn events_after_inner(
        &self,
        namespace: &NamespaceId,
        cursor: &semantic::EventCursor,
        limit: usize,
    ) -> semantic::EventPage {
        let source = self.semantic_event_source_for(namespace);
        let histories = self
            .runtime
            .semantic_events
            .lock()
            .expect("semantic event history lock poisoned");
        let history = histories.get(namespace);
        let oldest = history
            .and_then(|history| history.events.front())
            .map_or(0, |event| event.sequence);
        let latest = history
            .and_then(|history| history.events.back())
            .map_or(0, |event| event.sequence);
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
            .into_iter()
            .flat_map(|history| history.events.iter())
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
        let worker_id = {
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
                Some(worker_id.clone())
            } else {
                None
            }
        };
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
        if let Some(worker_id) = worker_id {
            self.runtime
                .instances
                .lock()
                .expect("instance lock poisoned")
                .remove(&worker_id);
            self.runtime
                .workers
                .lock()
                .expect("worker scope lock poisoned")
                .retain(|_, instance_name| instance_name != &worker_id);
        }
        Ok(Self::semantic_lease(&object, &released))
    }
}
