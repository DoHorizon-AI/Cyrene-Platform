//! Canonical authority operations for leases, workers, endpoints, and events.
//!
//! The parent module owns the authority state; this projection implements the
//! transport-independent semantic action trait over that state.
//! 租约、worker、endpoint 和事件的规范 authority 操作。
//!
//! 父模块拥有 authority 状态；本投影在该状态上实现与传输无关的语义 action trait。

use super::*;

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
            .acquire_lease(ResourceRequest {
                lease_name: Self::lease_internal_name(context, &object.identity),
                expected_inventory_generation: self.runtime.daemon.resources.inventory().generation,
                holder,
                query,
                expires_at_unix_ms: Some(expires_at_unix_ms),
                limits: CgroupLimits::default(),
            })
            .map_err(Self::provider_rejection)?;
        if let Err(error) = self.record_runtime(
            RuntimeJournalEvent::LeaseAcquired,
            None,
            Some(&lease),
            "LEASE_ACQUIRED",
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
            .renew_lease(&current.name, fence_token, expires_at_unix_ms)
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
        // Validate plugin input here; the sandbox runtime injects Adapter-owned keys once.
        // 此处只校验插件输入,设备变量由 sandboxd 在实际启动边界注入。
        binding
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
        // B 类耐久意图：在执行物理 spawn 之前持久化启动意图。如果此记录无法持久化，就不能开始 spawn；这样即使 spawn 后的结果记录全部丢失（包括连续两次持久化失败后崩溃），重启恢复仍可将其归类为“意图存在，结果未知”。
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
                // 物理进程已经启动，必须同步回收。如果无法回收，则保留耐久清理证据，以便重启恢复能够识别这次未完成的启动，而不会丢失一个未跟踪的执行域。
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
            // B 类结果：InstanceLaunched 在物理 spawn 之后持久化，因此返回失败前必须同步回收已启动进程。若无法回收，耐久清理证据仍可让重启恢复识别未完成的启动。
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
            // 已停止的 Worker 不再持有 active Lease；其 Endpoint 和 Grant 失去 authority，不能继续留在 Kernel 状态中。
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

    fn report_heartbeat(
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

    fn read_events(
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
        self.read_events_page(&context.namespace, cursor, limit)
    }
}
