// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: kernel/crates/cy-kernel-daemon/src/rpc/authority_v2_service.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! Core v2 projection of the canonical authority with explicit namespace scope.

use cy_kernel_api::{semantic, KernelAuthority};
use cy_proto::{core_v2, semantic_v1};
use cy_runtime_maintenance::{
    MaintenanceOutcome, MaintenancePlan, ReadinessRequest, ReadinessStatus, TaskActivityRecord,
    TaskActivityState, UpdateTargetKind,
};
use tonic::{Request, Response, Status};

use crate::{
    adapter::{maintenance_status, KernelServiceAdapter, OPERATION_EVENT_HISTORY_CAPACITY},
    convert::{
        authority_call_context_from_v2_proto, expires_after, proto_duration,
        semantic_contract_revision_from_proto, semantic_endpoint_from_proto,
        semantic_endpoint_grant_from_proto, semantic_event_cursor_from_proto,
        semantic_identity_from_proto, semantic_operation_from_proto, semantic_query_from_proto,
        semantic_worker_from_proto, to_semantic_proto_contract_lease,
        to_semantic_proto_contract_revision, to_semantic_proto_endpoint,
        to_semantic_proto_endpoint_grant, to_semantic_proto_event_continuity,
        to_semantic_proto_event_page, to_semantic_proto_operation, to_semantic_proto_worker,
    },
    peer_cred::principal_from_request,
    rpc::authority_service::authority_status,
};

fn watch_continuity_response(
    continuity: semantic::EventContinuity,
) -> core_v2::WatchEventsResponse {
    core_v2::WatchEventsResponse {
        body: Some(core_v2::watch_events_response::Body::Continuity(
            to_semantic_proto_event_continuity(&continuity),
        )),
    }
}

fn require_kernel_operator(
    adapter: &KernelServiceAdapter,
    request: &Request<impl Sized>,
    token: &str,
) -> Result<(), Status> {
    require_root_broker_peer(request)?;
    if !adapter
        .runtime_maintenance()?
        .verify_operator_token(token)
        .map_err(maintenance_status)?
    {
        return Err(Status::permission_denied(
            "operator capability did not match the root-managed broker capability",
        ));
    }
    Ok(())
}

fn require_root_broker_peer(request: &Request<impl Sized>) -> Result<(), Status> {
    let principal = principal_from_request(request)?;
    if principal.identity.id.split('/').nth(1) != Some("uid-0") {
        return Err(Status::permission_denied(
            "maintenance authority calls require the root maintenance broker peer",
        ));
    }
    Ok(())
}

fn require_activity_source(
    adapter: &KernelServiceAdapter,
    request: &Request<impl Sized>,
    source_id: &str,
    source_token: &str,
) -> Result<(), Status> {
    let principal = principal_from_request(request)?;
    if principal.identity.id.split('/').nth(1) != Some("uid-0") {
        return Err(Status::permission_denied(
            "activity-source calls must be forwarded by the root maintenance broker",
        ));
    }
    let gate = adapter.runtime_maintenance()?;
    gate.refresh_catalog().map_err(maintenance_status)?;
    if !gate.verify_source_token(source_id, source_token) {
        return Err(Status::permission_denied(
            "activity-source token is not trusted by the installed catalog",
        ));
    }
    Ok(())
}

fn readiness_request_from_proto(
    request: &core_v2::UpdateReadinessRequest,
) -> Result<ReadinessRequest, Status> {
    let target_kind = match core_v2::MaintenanceTargetKind::try_from(request.target_kind) {
        Ok(core_v2::MaintenanceTargetKind::PackageOnly) => UpdateTargetKind::PackageOnly,
        Ok(core_v2::MaintenanceTargetKind::CoreRuntime) => UpdateTargetKind::CoreRuntime,
        _ => {
            return Err(Status::invalid_argument(
                "maintenance target_kind is required",
            ))
        }
    };
    Ok(ReadinessRequest {
        target_kind,
        requires_restart: request.requires_restart,
        expected_catalog_generation: request.expected_catalog_generation,
        expected_activity_sources: request.expected_activity_sources.clone(),
    })
}

fn readiness_status_to_proto(status: ReadinessStatus) -> i32 {
    let status = match status {
        ReadinessStatus::Unknown => core_v2::UpdateReadinessStatus::Unspecified,
        ReadinessStatus::Ready => core_v2::UpdateReadinessStatus::Ready,
        ReadinessStatus::ActiveTasks => core_v2::UpdateReadinessStatus::ActiveTasks,
        // Older readiness wire versions have no binding-operation status. Preserve the
        // independent blocker code and zero task count in the response projection.
        ReadinessStatus::ActiveBindingOperations => core_v2::UpdateReadinessStatus::Unspecified,
        ReadinessStatus::IdleRuntimeRequiresUnload => {
            core_v2::UpdateReadinessStatus::IdleRuntimeRequiresUnload
        }
        ReadinessStatus::MaintenanceActive => core_v2::UpdateReadinessStatus::MaintenanceActive,
        ReadinessStatus::StaleReadiness => core_v2::UpdateReadinessStatus::StaleReadiness,
        ReadinessStatus::UserConfirmationRequired => {
            core_v2::UpdateReadinessStatus::UserConfirmationRequired
        }
    };
    status as i32
}

fn activity_state_to_proto(state: TaskActivityState) -> i32 {
    let state = match state {
        TaskActivityState::Accepted => core_v2::WorkspaceTaskActivityState::Accepted,
        TaskActivityState::Queued => core_v2::WorkspaceTaskActivityState::Queued,
        TaskActivityState::Dispatching => core_v2::WorkspaceTaskActivityState::Dispatching,
        TaskActivityState::Running => core_v2::WorkspaceTaskActivityState::Running,
        TaskActivityState::Canceling => core_v2::WorkspaceTaskActivityState::Canceling,
        TaskActivityState::Inflight => core_v2::WorkspaceTaskActivityState::Inflight,
    };
    state as i32
}

fn task_activity_from_proto(state: i32) -> Result<TaskActivityState, Status> {
    match core_v2::WorkspaceTaskActivityState::try_from(state) {
        Ok(core_v2::WorkspaceTaskActivityState::Accepted) => Ok(TaskActivityState::Accepted),
        Ok(core_v2::WorkspaceTaskActivityState::Queued) => Ok(TaskActivityState::Queued),
        Ok(core_v2::WorkspaceTaskActivityState::Dispatching) => Ok(TaskActivityState::Dispatching),
        Ok(core_v2::WorkspaceTaskActivityState::Running) => Ok(TaskActivityState::Running),
        Ok(core_v2::WorkspaceTaskActivityState::Canceling) => Ok(TaskActivityState::Canceling),
        Ok(core_v2::WorkspaceTaskActivityState::Inflight) => Ok(TaskActivityState::Inflight),
        _ => Err(Status::invalid_argument("task activity state is required")),
    }
}

fn activity_records_to_proto(
    records: Vec<TaskActivityRecord>,
) -> Vec<core_v2::ActiveWorkspaceTask> {
    records
        .into_iter()
        .map(|record| core_v2::ActiveWorkspaceTask {
            source_id: record.source_id,
            task_id: record.task_id,
            state: activity_state_to_proto(record.state),
        })
        .collect()
}

#[tonic::async_trait]
impl core_v2::kernel_authority_service_server::KernelAuthorityService for KernelServiceAdapter {
    async fn negotiate(
        &self,
        request: Request<core_v2::NegotiateRequest>,
    ) -> Result<Response<semantic_v1::ContractRevision>, Status> {
        let principal = principal_from_request(&request)?;
        let offered = request
            .into_inner()
            .offered
            .into_iter()
            .filter_map(semantic_contract_revision_from_proto)
            .collect::<Vec<_>>();
        let selected = self
            .authority()
            .negotiate(&principal, &offered)
            .map_err(authority_status)?;
        Ok(Response::new(to_semantic_proto_contract_revision(
            &selected,
        )))
    }

    async fn acquire_lease(
        &self,
        request: Request<core_v2::AcquireLeaseRequest>,
    ) -> Result<Response<semantic_v1::Lease>, Status> {
        let principal = principal_from_request(&request)?;
        let request = request.into_inner();
        let context = authority_call_context_from_v2_proto(request.context.as_ref())?;
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
        let authority = self.authority();
        let lease = self.with_runtime_admission("core-v2-acquire-lease", || {
            authority
                .acquire_lease(&context, &principal, holder, query, expires_after(ttl))
                .map_err(authority_status)
        })?;
        Ok(Response::new(to_semantic_proto_contract_lease(&lease)))
    }

    async fn renew_lease(
        &self,
        request: Request<core_v2::RenewLeaseRequest>,
    ) -> Result<Response<semantic_v1::Lease>, Status> {
        let principal = principal_from_request(&request)?;
        let request = request.into_inner();
        let context = authority_call_context_from_v2_proto(request.context.as_ref())?;
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
        let lease = self
            .authority()
            .renew_lease(
                &context,
                &principal,
                &lease_identity,
                request.fence_token,
                expires_after(ttl),
            )
            .map_err(authority_status)?;
        Ok(Response::new(to_semantic_proto_contract_lease(&lease)))
    }

    async fn release_lease(
        &self,
        request: Request<core_v2::ReleaseLeaseRequest>,
    ) -> Result<Response<semantic_v1::Lease>, Status> {
        let principal = principal_from_request(&request)?;
        let request = request.into_inner();
        let context = authority_call_context_from_v2_proto(request.context.as_ref())?;
        let lease = semantic_identity_from_proto(request.lease, "lease")?;
        let lease = self
            .authority()
            .release_lease(&context, &principal, &lease, request.fence_token)
            .map_err(authority_status)?;
        Ok(Response::new(to_semantic_proto_contract_lease(&lease)))
    }

    async fn start_worker(
        &self,
        request: Request<core_v2::StartWorkerRequest>,
    ) -> Result<Response<semantic_v1::Operation>, Status> {
        let principal = principal_from_request(&request)?;
        let request = request.into_inner();
        let context = authority_call_context_from_v2_proto(request.context.as_ref())?;
        let worker = semantic_worker_from_proto(
            request
                .worker
                .ok_or_else(|| Status::invalid_argument("worker is required"))?,
            principal.identity.clone(),
        )?;
        let authority = self.authority();
        let operation = self.with_runtime_admission("core-v2-start-worker", || {
            authority
                .start_worker(&context, &principal, worker)
                .map_err(authority_status)
        })?;
        Ok(Response::new(to_semantic_proto_operation(&operation)))
    }

    async fn report_heartbeat(
        &self,
        request: Request<core_v2::ReportHeartbeatRequest>,
    ) -> Result<Response<semantic_v1::Worker>, Status> {
        let principal = principal_from_request(&request)?;
        let request = request.into_inner();
        let context = authority_call_context_from_v2_proto(request.context.as_ref())?;
        let worker = semantic_identity_from_proto(request.worker, "worker")?;
        let lease = semantic_identity_from_proto(request.lease, "lease")?;
        let worker = self
            .authority()
            .report_heartbeat(&context, &principal, &worker, &lease, request.fence_token)
            .map_err(authority_status)?;
        Ok(Response::new(to_semantic_proto_worker(&worker)))
    }

    async fn stop_worker(
        &self,
        request: Request<core_v2::StopWorkerRequest>,
    ) -> Result<Response<semantic_v1::Operation>, Status> {
        let principal = principal_from_request(&request)?;
        let request = request.into_inner();
        let context = authority_call_context_from_v2_proto(request.context.as_ref())?;
        let worker = semantic_identity_from_proto(request.worker, "worker")?;
        let lease = semantic_identity_from_proto(request.lease, "lease")?;
        let grace_period = request
            .grace_period
            .map(proto_duration)
            .transpose()?
            .unwrap_or(self.heartbeat.graceful_stop);
        let operation = self
            .authority()
            .stop_worker(
                &context,
                &principal,
                &worker,
                &lease,
                request.fence_token,
                grace_period,
            )
            .map_err(authority_status)?;
        self.request_semantic_worker_shutdown(&worker.id, "STOP_REQUESTED");
        Ok(Response::new(to_semantic_proto_operation(&operation)))
    }

    async fn create_operation(
        &self,
        request: Request<core_v2::CreateOperationRequest>,
    ) -> Result<Response<semantic_v1::Operation>, Status> {
        let principal = principal_from_request(&request)?;
        let request = request.into_inner();
        let context = authority_call_context_from_v2_proto(request.context.as_ref())?;
        let operation = semantic_operation_from_proto(
            request
                .operation
                .ok_or_else(|| Status::invalid_argument("operation is required"))?,
        )?;
        let authority = self.authority();
        let operation = self.with_runtime_admission("core-v2-create-operation", || {
            authority
                .create_operation(&context, &principal, operation)
                .map_err(authority_status)
        })?;
        Ok(Response::new(to_semantic_proto_operation(&operation)))
    }

    async fn report_operation(
        &self,
        request: Request<core_v2::ReportOperationRequest>,
    ) -> Result<Response<semantic_v1::Operation>, Status> {
        let principal = principal_from_request(&request)?;
        let request = request.into_inner();
        let context = authority_call_context_from_v2_proto(request.context.as_ref())?;
        let operation = semantic_operation_from_proto(
            request
                .operation
                .ok_or_else(|| Status::invalid_argument("operation is required"))?,
        )?;
        let operation = self
            .authority()
            .report_operation(&context, &principal, operation)
            .map_err(authority_status)?;
        Ok(Response::new(to_semantic_proto_operation(&operation)))
    }

    async fn cancel_operation(
        &self,
        request: Request<core_v2::CancelOperationRequest>,
    ) -> Result<Response<semantic_v1::Operation>, Status> {
        let principal = principal_from_request(&request)?;
        let request = request.into_inner();
        let context = authority_call_context_from_v2_proto(request.context.as_ref())?;
        let operation = semantic_identity_from_proto(request.operation, "operation")?;
        let operation = self
            .authority()
            .cancel_operation(&context, &principal, &operation)
            .map_err(authority_status)?;
        Ok(Response::new(to_semantic_proto_operation(&operation)))
    }

    async fn publish_endpoint(
        &self,
        request: Request<core_v2::PublishEndpointRequest>,
    ) -> Result<Response<semantic_v1::Endpoint>, Status> {
        let principal = principal_from_request(&request)?;
        let request = request.into_inner();
        let context = authority_call_context_from_v2_proto(request.context.as_ref())?;
        let endpoint = semantic_endpoint_from_proto(
            request
                .endpoint
                .ok_or_else(|| Status::invalid_argument("endpoint is required"))?,
        )?;
        let endpoint = self
            .authority()
            .publish_endpoint(&context, &principal, endpoint)
            .map_err(authority_status)?;
        Ok(Response::new(to_semantic_proto_endpoint(&endpoint)))
    }

    async fn authorize_endpoint(
        &self,
        request: Request<core_v2::AuthorizeEndpointRequest>,
    ) -> Result<Response<semantic_v1::EndpointGrant>, Status> {
        let principal = principal_from_request(&request)?;
        let request = request.into_inner();
        let context = authority_call_context_from_v2_proto(request.context.as_ref())?;
        let grant = semantic_endpoint_grant_from_proto(
            request
                .grant
                .ok_or_else(|| Status::invalid_argument("endpoint grant is required"))?,
        )?;
        let grant = self
            .authority()
            .authorize_endpoint(&context, &principal, grant)
            .map_err(authority_status)?;
        Ok(Response::new(to_semantic_proto_endpoint_grant(&grant)))
    }

    async fn revoke_endpoint(
        &self,
        request: Request<core_v2::RevokeEndpointRequest>,
    ) -> Result<Response<()>, Status> {
        let principal = principal_from_request(&request)?;
        let request = request.into_inner();
        let context = authority_call_context_from_v2_proto(request.context.as_ref())?;
        let grant = semantic_identity_from_proto(request.grant, "grant")?;
        self.authority()
            .revoke_endpoint(&context, &principal, &grant)
            .map_err(authority_status)?;
        Ok(Response::new(()))
    }

    async fn read_events(
        &self,
        request: Request<core_v2::ReadEventsRequest>,
    ) -> Result<Response<core_v2::ReadEventsResponse>, Status> {
        let principal = principal_from_request(&request)?;
        let request = request.into_inner();
        let context = authority_call_context_from_v2_proto(request.context.as_ref())?;
        let cursor = semantic_event_cursor_from_proto(request.cursor)?;
        let page = self
            .authority()
            .read_events(&context, &principal, &cursor, request.limit as usize)
            .map_err(authority_status)?;
        Ok(Response::new(core_v2::ReadEventsResponse {
            page: Some(to_semantic_proto_event_page(&page)),
        }))
    }

    type WatchEventsStream = std::pin::Pin<
        Box<
            dyn tokio_stream::Stream<Item = Result<core_v2::WatchEventsResponse, Status>>
                + Send
                + 'static,
        >,
    >;

    async fn watch_events(
        &self,
        request: Request<core_v2::WatchEventsRequest>,
    ) -> Result<Response<Self::WatchEventsStream>, Status> {
        let principal = principal_from_request(&request)?;
        let request = request.into_inner();
        let context = authority_call_context_from_v2_proto(request.context.as_ref())?;
        let initial_cursor = semantic_event_cursor_from_proto(request.cursor)?;
        let limit = match usize::try_from(request.page_size) {
            Ok(size) if (1..=OPERATION_EVENT_HISTORY_CAPACITY).contains(&size) => size,
            _ => OPERATION_EVENT_HISTORY_CAPACITY,
        };

        let initial_page = self
            .authority()
            .read_events(&context, &principal, &initial_cursor, limit)
            .map_err(authority_status)?;

        let authority = self.authority();
        let notifier = authority.runtime.event_notifier.clone();
        let (sender, receiver) =
            tokio::sync::mpsc::channel(crate::adapter::OPERATION_EVENT_SUBSCRIBER_CAPACITY);

        tokio::spawn(async move {
            if initial_page.status != semantic::ReplayStatus::Current {
                let _ = sender
                    .send(Ok(watch_continuity_response(initial_page.continuity())))
                    .await;
                return;
            }

            let mut cursor = initial_cursor;
            for event in initial_page.events {
                cursor.sequence = event.sequence;
                let response = core_v2::WatchEventsResponse {
                    body: Some(core_v2::watch_events_response::Body::Event(
                        crate::convert::to_semantic_proto_event(&event),
                    )),
                };
                if sender.try_send(Ok(response)).is_err() {
                    let _ = sender
                        .send(Err(Status::out_of_range(
                            "subscriber buffer full, slow consumer disconnected",
                        )))
                        .await;
                    return;
                }
            }

            loop {
                let notified = notifier.notified();
                match authority.read_events(&context, &principal, &cursor, limit) {
                    Ok(page) => match page.status {
                        semantic::ReplayStatus::SourceChanged => {
                            let _ = sender
                                .send(Ok(watch_continuity_response(page.continuity())))
                                .await;
                            return;
                        }
                        semantic::ReplayStatus::Gap => {
                            let _ = sender
                                .send(Ok(watch_continuity_response(page.continuity())))
                                .await;
                            return;
                        }
                        semantic::ReplayStatus::Current => {
                            let had_events = !page.events.is_empty();
                            for event in page.events {
                                cursor.sequence = event.sequence;
                                let response = core_v2::WatchEventsResponse {
                                    body: Some(core_v2::watch_events_response::Body::Event(
                                        crate::convert::to_semantic_proto_event(&event),
                                    )),
                                };
                                if sender.try_send(Ok(response)).is_err() {
                                    let _ = sender
                                        .send(Err(Status::out_of_range(
                                            "subscriber buffer full, slow consumer disconnected",
                                        )))
                                        .await;
                                    return;
                                }
                            }
                            if had_events {
                                continue;
                            }
                        }
                    },
                    Err(rejection) => {
                        let _ = sender.send(Err(authority_status(rejection))).await;
                        return;
                    }
                }

                tokio::select! {
                    _ = notified => {},
                    _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {},
                    _ = sender.closed() => {
                        return;
                    }
                }
            }
        });

        let stream = tokio_stream::wrappers::ReceiverStream::new(receiver);
        Ok(Response::new(Box::pin(stream)))
    }

    async fn get_update_readiness(
        &self,
        request: Request<core_v2::UpdateReadinessRequest>,
    ) -> Result<Response<core_v2::UpdateReadinessResponse>, Status> {
        require_root_broker_peer(&request)?;
        let readiness_request = readiness_request_from_proto(request.get_ref())?;
        let gate = self.runtime_maintenance()?;
        gate.refresh_catalog().map_err(maintenance_status)?;
        let snapshot = gate
            .get_update_readiness_with(&readiness_request, || self.runtime_usage())
            .map_err(maintenance_status)?;
        Ok(Response::new(core_v2::UpdateReadinessResponse {
            status: readiness_status_to_proto(snapshot.status),
            gate_generation: snapshot.gate_generation,
            active_task_count: snapshot.active_task_count,
            active_worker_count: snapshot.active_worker_count,
            active_lease_or_allocation_count: snapshot.active_allocation_count,
            unknown_activity_sources: snapshot.unknown_activity_sources,
            blocker_codes: snapshot.blocker_codes,
            active_tasks: activity_records_to_proto(snapshot.active_tasks),
            install_catalog_generation: snapshot.install_catalog_generation,
            inflight_runtime_admission_count: snapshot.inflight_runtime_admission_count,
            requires_restart_confirmation: snapshot.requires_restart_confirmation,
        }))
    }

    async fn begin_maintenance(
        &self,
        request: Request<core_v2::BeginMaintenanceRequest>,
    ) -> Result<Response<core_v2::BeginMaintenanceResponse>, Status> {
        require_kernel_operator(self, &request, &request.get_ref().operator_token)?;
        let request = request.into_inner();
        let readiness_request = ReadinessRequest {
            target_kind: match core_v2::MaintenanceTargetKind::try_from(request.target_kind) {
                Ok(core_v2::MaintenanceTargetKind::PackageOnly) => UpdateTargetKind::PackageOnly,
                Ok(core_v2::MaintenanceTargetKind::CoreRuntime) => UpdateTargetKind::CoreRuntime,
                _ => {
                    return Err(Status::invalid_argument(
                        "maintenance target_kind is required",
                    ))
                }
            },
            requires_restart: true,
            expected_catalog_generation: request.expected_catalog_generation,
            expected_activity_sources: request.expected_activity_sources,
        };
        let plan = MaintenancePlan {
            plan_id: request.plan_id,
            plan_digest: request.plan_digest,
            component_artifact_digests: request.component_artifact_digests.into_iter().collect(),
        };
        let gate = self.runtime_maintenance()?;
        let result = gate
            .begin_maintenance_with(
                &request.request_id,
                &plan,
                &readiness_request,
                request.expected_gate_generation,
                request.user_confirmed_restart,
                || self.runtime_usage(),
            )
            .map_err(maintenance_status)?;
        Ok(Response::new(core_v2::BeginMaintenanceResponse {
            status: readiness_status_to_proto(result.status),
            maintenance_token: result.maintenance_token.unwrap_or_default(),
            gate_generation: result.gate_generation,
            blocker_codes: result.blocker_codes,
        }))
    }

    async fn end_maintenance(
        &self,
        request: Request<core_v2::EndMaintenanceRequest>,
    ) -> Result<Response<core_v2::EndMaintenanceResponse>, Status> {
        require_kernel_operator(self, &request, &request.get_ref().operator_token)?;
        let request = request.into_inner();
        let outcome = match core_v2::MaintenanceOutcome::try_from(request.outcome) {
            Ok(core_v2::MaintenanceOutcome::Success) => MaintenanceOutcome::Success,
            Ok(core_v2::MaintenanceOutcome::RolledBack) => MaintenanceOutcome::RolledBack,
            Ok(core_v2::MaintenanceOutcome::Failed) => MaintenanceOutcome::Failed,
            _ => return Err(Status::invalid_argument("maintenance outcome is required")),
        };
        let result = self
            .runtime_maintenance()?
            .end_maintenance(
                &request.request_id,
                &request.maintenance_token,
                outcome,
                request.healthy,
            )
            .map_err(maintenance_status)?;
        Ok(Response::new(core_v2::EndMaintenanceResponse {
            unlocked: result.unlocked,
            status: readiness_status_to_proto(result.status),
            gate_generation: result.gate_generation,
            blocker_codes: if result.unlocked {
                Vec::new()
            } else {
                vec!["HEALTHY_SUCCESS_OR_ROLLBACK_REQUIRED".to_string()]
            },
        }))
    }

    async fn admit_task(
        &self,
        request: Request<core_v2::AdmitTaskRequest>,
    ) -> Result<Response<core_v2::AdmitTaskResponse>, Status> {
        require_activity_source(
            self,
            &request,
            &request.get_ref().source_id,
            &request.get_ref().source_token,
        )?;
        let request = request.into_inner();
        let state = task_activity_from_proto(request.state)?;
        let admission = self
            .runtime_maintenance()?
            .admit_task(&request.source_id, &request.task_id, state)
            .map_err(maintenance_status)?;
        Ok(Response::new(core_v2::AdmitTaskResponse {
            admitted: true,
            status: readiness_status_to_proto(ReadinessStatus::Ready),
            gate_generation: admission.gate_generation,
            blocker_codes: Vec::new(),
        }))
    }

    async fn update_task_activity(
        &self,
        request: Request<core_v2::UpdateTaskActivityRequest>,
    ) -> Result<Response<core_v2::UpdateTaskActivityResponse>, Status> {
        require_activity_source(
            self,
            &request,
            &request.get_ref().source_id,
            &request.get_ref().source_token,
        )?;
        let request = request.into_inner();
        let state = task_activity_from_proto(request.state)?;
        let admission = self
            .runtime_maintenance()?
            .update_task_activity(&request.source_id, &request.task_id, state)
            .map_err(maintenance_status)?;
        Ok(Response::new(core_v2::UpdateTaskActivityResponse {
            accepted: true,
            status: readiness_status_to_proto(ReadinessStatus::Ready),
            gate_generation: admission.gate_generation,
            blocker_codes: Vec::new(),
        }))
    }

    async fn complete_task(
        &self,
        request: Request<core_v2::CompleteTaskRequest>,
    ) -> Result<Response<core_v2::CompleteTaskResponse>, Status> {
        require_activity_source(
            self,
            &request,
            &request.get_ref().source_id,
            &request.get_ref().source_token,
        )?;
        let request = request.into_inner();
        let gate = self.runtime_maintenance()?;
        gate.complete_task(&request.source_id, &request.task_id)
            .map_err(maintenance_status)?;
        Ok(Response::new(core_v2::CompleteTaskResponse {
            completed: true,
            gate_generation: gate.current_gate_generation().map_err(maintenance_status)?,
            blocker_codes: Vec::new(),
        }))
    }

    async fn heartbeat_activity_source(
        &self,
        request: Request<core_v2::HeartbeatActivitySourceRequest>,
    ) -> Result<Response<core_v2::HeartbeatActivitySourceResponse>, Status> {
        require_activity_source(
            self,
            &request,
            &request.get_ref().source_id,
            &request.get_ref().source_token,
        )?;
        let request = request.into_inner();
        let gate = self.runtime_maintenance()?;
        if request.expected_catalog_generation > gate.catalog_generation() {
            return Err(Status::failed_precondition(
                "UPDATE_READINESS_UNKNOWN: client catalog generation is ahead of the installed catalog",
            ));
        }
        gate.heartbeat_activity_source(&request.source_id)
            .map_err(maintenance_status)?;
        Ok(Response::new(core_v2::HeartbeatActivitySourceResponse {
            last_heartbeat_unix_ms: crate::convert::now_unix_ms(),
            gate_generation: gate.current_gate_generation().map_err(maintenance_status)?,
        }))
    }

    async fn reconcile_activity_source(
        &self,
        request: Request<core_v2::ReconcileActivitySourceRequest>,
    ) -> Result<Response<core_v2::ReconcileActivitySourceResponse>, Status> {
        require_activity_source(
            self,
            &request,
            &request.get_ref().source_id,
            &request.get_ref().source_token,
        )?;
        let request = request.into_inner();
        let gate = self.runtime_maintenance()?;
        if request.expected_catalog_generation > gate.catalog_generation() {
            return Err(Status::failed_precondition(
                "UPDATE_READINESS_UNKNOWN: client catalog generation is ahead of the installed catalog",
            ));
        }
        let active_tasks = request
            .active_tasks
            .into_iter()
            .map(|task| {
                Ok(TaskActivityRecord {
                    source_id: request.source_id.clone(),
                    task_id: task.task_id,
                    state: task_activity_from_proto(task.state)?,
                })
            })
            .collect::<Result<Vec<_>, Status>>()?;
        gate.reconcile_activity_source(&request.source_id, active_tasks)
            .map_err(maintenance_status)?;
        Ok(Response::new(core_v2::ReconcileActivitySourceResponse {
            gate_generation: gate.current_gate_generation().map_err(maintenance_status)?,
            reconciled: true,
        }))
    }

    async fn list_active_tasks(
        &self,
        request: Request<core_v2::ListActiveTasksRequest>,
    ) -> Result<Response<core_v2::ListActiveTasksResponse>, Status> {
        require_activity_source(
            self,
            &request,
            &request.get_ref().source_id,
            &request.get_ref().source_token,
        )?;
        let request = request.into_inner();
        let tasks = self
            .runtime_maintenance()?
            .list_active_tasks(&request.source_id)
            .map_err(maintenance_status)?;
        Ok(Response::new(core_v2::ListActiveTasksResponse {
            active_tasks: tasks
                .into_iter()
                .map(|task| core_v2::TaskActivity {
                    task_id: task.task_id,
                    state: activity_state_to_proto(task.state),
                })
                .collect(),
        }))
    }
}

#[cfg(test)]
mod readiness_status_tests {
    use super::*;

    #[test]
    fn binding_operations_use_unknown_wire_status() {
        assert_eq!(
            readiness_status_to_proto(ReadinessStatus::ActiveBindingOperations),
            core_v2::UpdateReadinessStatus::Unspecified as i32
        );
    }
}
