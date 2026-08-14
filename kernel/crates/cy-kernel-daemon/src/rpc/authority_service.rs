//! `core_v1::KernelAuthorityService` gRPC 语义权威服务实现。

use std::collections::BTreeMap;

use cy_kernel_api::{semantic, CgroupLimits, LeaseState, ResourceRequest, RuntimeJournalEvent};
use cy_proto::{core_v1, semantic_v1};
use tonic::{Request, Response, Status};

use crate::{
    adapter::{KernelServiceAdapter, OPERATION_EVENT_HISTORY_CAPACITY},
    convert::{
        authority_lease_name, expires_after, inject_heartbeat_environment, now_unix_ms,
        proto_duration, provider_status, semantic_contract_revision_from_proto,
        semantic_endpoint_from_proto, semantic_endpoint_grant_from_proto,
        semantic_event_cursor_from_proto, semantic_identity_from_proto, semantic_identity_key,
        semantic_operation_event_kind, semantic_operation_from_proto, semantic_query_from_proto,
        semantic_status, semantic_worker_from_proto, to_semantic_proto_contract_revision,
        to_semantic_proto_endpoint, to_semantic_proto_endpoint_grant, to_semantic_proto_event_page,
        to_semantic_proto_lease, to_semantic_proto_operation, to_semantic_proto_worker,
        validate_authority_context,
    },
    peer_cred::principal_from_request,
    watchdog::InstanceActor,
    session::ManagedProcess,
};

#[tonic::async_trait]
impl core_v1::kernel_authority_service_server::KernelAuthorityService for KernelServiceAdapter {
    async fn negotiate(
        &self,
        request: Request<core_v1::NegotiateRequest>,
    ) -> Result<Response<semantic_v1::ContractRevision>, Status> {
        // Authenticate the caller from the trusted transport even though the
        // negotiation result carries no Principal of its own.
        let _principal = principal_from_request(&request)?;
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
        let _principal = principal_from_request(&request)?;
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
        if let Err(error) = self.record_runtime(
            RuntimeJournalEvent::LeaseReserved,
            None,
            Some(&journal_lease),
            "LEASE_RESERVED",
        ) {
            // Durable fence record could not be persisted: roll back the
            // in-memory lease so it is never externally visible without the
            // durable evidence the contract requires.
            let _ = self.daemon.release(&lease.name, lease.fence_token);
            return Err(provider_status(error));
        }
        Ok(Response::new(to_semantic_proto_lease(&lease)))
    }

    async fn renew_lease(
        &self,
        request: Request<core_v1::RenewLeaseRequest>,
    ) -> Result<Response<semantic_v1::Lease>, Status> {
        let _principal = principal_from_request(&request)?;
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
        let _principal = principal_from_request(&request)?;
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
        let journal_lease = core_v1::ResourceLeaseRef {
            lease_name: lease_identity.id.clone(),
            fence_token: request.fence_token,
        };
        // Persist the durable release record BEFORE releasing in memory
        // (fail-closed): if the journal write fails we keep the lease rather
        // than lose the durable evidence of the reservation.
        if let Err(error) = self.record_runtime(
            RuntimeJournalEvent::LeaseReleased,
            None,
            Some(&journal_lease),
            "LEASE_RELEASED",
        ) {
            return Err(provider_status(error));
        }
        self.daemon
            .release(&lease_identity.id, request.fence_token)
            .map_err(provider_status)?;
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
        let principal = principal_from_request(&request)?;
        let request = request.into_inner();
        validate_authority_context(request.context.as_ref())?;
        let mut worker = semantic_worker_from_proto(
            request
                .worker
                .ok_or_else(|| Status::invalid_argument("worker is required"))?,
            // The authority Principal comes from the trusted transport, not
            // from the caller-asserted `worker.principal` body field.
            principal.identity.clone(),
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
        let mut actor = InstanceActor::new(
            worker.identity.id.clone(),
            lease.name.clone(),
            lease.fence_token,
            self.daemon.sandbox.clone(),
            plan,
            binding,
            self.heartbeat.timeout,
        );
        actor.start().map_err(provider_status)?;
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
                    actor,
                    lease: Some(lease_ref.clone()),
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
        if let Err(error) = self.record_runtime(
            RuntimeJournalEvent::InstanceLaunched,
            Some(&worker.identity.id),
            Some(&lease_ref),
            "WORKER_LAUNCHED",
        ) {
            eprintln!("runtime journal InstanceLaunched write failed: {error}");
        }
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
        let _principal = principal_from_request(&request)?;
        let request = request.into_inner();
        validate_authority_context(request.context.as_ref())?;
        let worker_identity = semantic_identity_from_proto(request.worker, "worker")?;
        let lease_identity = semantic_identity_from_proto(request.lease, "lease")?;
        let response = self.accept_semantic_worker_heartbeat(
            worker_identity,
            lease_identity,
            request.fence_token,
        )?;
        Ok(Response::new(to_semantic_proto_worker(&response)))
    }

    async fn stop_worker(
        &self,
        request: Request<core_v1::StopWorkerRequest>,
    ) -> Result<Response<semantic_v1::Operation>, Status> {
        let _principal = principal_from_request(&request)?;
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
            self.request_semantic_worker_shutdown(&worker_identity.id, "STOP_REQUESTED");
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
                .actor
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
        let _principal = principal_from_request(&request)?;
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
        let _principal = principal_from_request(&request)?;
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
        let _principal = principal_from_request(&request)?;
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
        let _principal = principal_from_request(&request)?;
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
        let _principal = principal_from_request(&request)?;
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
        let _principal = principal_from_request(&request)?;
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
        let _principal = principal_from_request(&request)?;
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
