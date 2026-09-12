// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: kernel/crates/cy-kernel-daemon/src/rpc/authority_service.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! Core v1 transport projection for the canonical Kernel authority.

use cy_kernel_api::{semantic, KernelAuthority};
use cy_proto::{core_v1, semantic_v1};
use tonic::{Request, Response, Status};

use crate::{
    adapter::{KernelServiceAdapter, OPERATION_EVENT_HISTORY_CAPACITY},
    convert::{
        authority_call_context_from_proto, expires_after, proto_duration,
        semantic_contract_revision_from_proto, semantic_endpoint_from_proto,
        semantic_endpoint_grant_from_proto, semantic_event_cursor_from_proto,
        semantic_identity_from_proto, semantic_operation_from_proto, semantic_query_from_proto,
        semantic_status, semantic_worker_from_proto, to_semantic_proto_contract_lease,
        to_semantic_proto_contract_revision, to_semantic_proto_endpoint,
        to_semantic_proto_endpoint_grant, to_semantic_proto_event_continuity,
        to_semantic_proto_event_page, to_semantic_proto_operation, to_semantic_proto_worker,
    },
    peer_cred::principal_from_request,
};

pub(crate) fn authority_status(rejection: semantic::Rejection) -> Status {
    let code = match rejection.reason_code.as_str() {
        "AUTHORITY_DENIED" | "NAMESPACE_AUTHORITY_DENIED" => tonic::Code::PermissionDenied,
        "WORKER_NOT_FOUND" | "OPERATION_NOT_FOUND" | "ENDPOINT_NOT_FOUND" => tonic::Code::NotFound,
        "WORKER_EXISTS" | "OPERATION_EXISTS" => tonic::Code::AlreadyExists,
        "REQUEST_ID_REQUIRED" | "EVENT_PAGE_LIMIT_INVALID" | "LEASE_EXPIRY_INVALID" => {
            tonic::Code::InvalidArgument
        }
        _ => tonic::Code::FailedPrecondition,
    };
    semantic_status(code, &rejection.reason_code, &rejection.message)
}

fn watch_continuity_response(
    continuity: semantic::EventContinuity,
) -> core_v1::WatchEventsResponse {
    core_v1::WatchEventsResponse {
        body: Some(core_v1::watch_events_response::Body::Continuity(
            to_semantic_proto_event_continuity(&continuity),
        )),
    }
}

#[tonic::async_trait]
impl core_v1::kernel_authority_service_server::KernelAuthorityService for KernelServiceAdapter {
    async fn negotiate(
        &self,
        request: Request<core_v1::NegotiateRequest>,
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
        request: Request<core_v1::AcquireLeaseRequest>,
    ) -> Result<Response<semantic_v1::Lease>, Status> {
        let principal = principal_from_request(&request)?;
        let request = request.into_inner();
        let context = authority_call_context_from_proto(request.context.as_ref())?;
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
            .authority()
            .acquire_lease(&context, &principal, holder, query, expires_after(ttl))
            .map_err(authority_status)?;
        Ok(Response::new(to_semantic_proto_contract_lease(&lease)))
    }

    async fn renew_lease(
        &self,
        request: Request<core_v1::RenewLeaseRequest>,
    ) -> Result<Response<semantic_v1::Lease>, Status> {
        let principal = principal_from_request(&request)?;
        let request = request.into_inner();
        let context = authority_call_context_from_proto(request.context.as_ref())?;
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
        request: Request<core_v1::ReleaseLeaseRequest>,
    ) -> Result<Response<semantic_v1::Lease>, Status> {
        let principal = principal_from_request(&request)?;
        let request = request.into_inner();
        let context = authority_call_context_from_proto(request.context.as_ref())?;
        let lease = semantic_identity_from_proto(request.lease, "lease")?;
        let lease = self
            .authority()
            .release_lease(&context, &principal, &lease, request.fence_token)
            .map_err(authority_status)?;
        Ok(Response::new(to_semantic_proto_contract_lease(&lease)))
    }

    async fn start_worker(
        &self,
        request: Request<core_v1::StartWorkerRequest>,
    ) -> Result<Response<semantic_v1::Operation>, Status> {
        let principal = principal_from_request(&request)?;
        let request = request.into_inner();
        let context = authority_call_context_from_proto(request.context.as_ref())?;
        let worker = semantic_worker_from_proto(
            request
                .worker
                .ok_or_else(|| Status::invalid_argument("worker is required"))?,
            principal.identity.clone(),
        )?;
        let operation = self
            .authority()
            .start_worker(&context, &principal, worker)
            .map_err(authority_status)?;
        Ok(Response::new(to_semantic_proto_operation(&operation)))
    }

    async fn report_heartbeat(
        &self,
        request: Request<core_v1::ReportHeartbeatRequest>,
    ) -> Result<Response<semantic_v1::Worker>, Status> {
        let principal = principal_from_request(&request)?;
        let request = request.into_inner();
        let context = authority_call_context_from_proto(request.context.as_ref())?;
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
        request: Request<core_v1::StopWorkerRequest>,
    ) -> Result<Response<semantic_v1::Operation>, Status> {
        let principal = principal_from_request(&request)?;
        let request = request.into_inner();
        let context = authority_call_context_from_proto(request.context.as_ref())?;
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
        request: Request<core_v1::CreateOperationRequest>,
    ) -> Result<Response<semantic_v1::Operation>, Status> {
        let principal = principal_from_request(&request)?;
        let request = request.into_inner();
        let context = authority_call_context_from_proto(request.context.as_ref())?;
        let operation = semantic_operation_from_proto(
            request
                .operation
                .ok_or_else(|| Status::invalid_argument("operation is required"))?,
        )?;
        let operation = self
            .authority()
            .create_operation(&context, &principal, operation)
            .map_err(authority_status)?;
        Ok(Response::new(to_semantic_proto_operation(&operation)))
    }

    async fn report_operation(
        &self,
        request: Request<core_v1::ReportOperationRequest>,
    ) -> Result<Response<semantic_v1::Operation>, Status> {
        let principal = principal_from_request(&request)?;
        let request = request.into_inner();
        let context = authority_call_context_from_proto(request.context.as_ref())?;
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
        request: Request<core_v1::CancelOperationRequest>,
    ) -> Result<Response<semantic_v1::Operation>, Status> {
        let principal = principal_from_request(&request)?;
        let request = request.into_inner();
        let context = authority_call_context_from_proto(request.context.as_ref())?;
        let operation = semantic_identity_from_proto(request.operation, "operation")?;
        let operation = self
            .authority()
            .cancel_operation(&context, &principal, &operation)
            .map_err(authority_status)?;
        Ok(Response::new(to_semantic_proto_operation(&operation)))
    }

    async fn publish_endpoint(
        &self,
        request: Request<core_v1::PublishEndpointRequest>,
    ) -> Result<Response<semantic_v1::Endpoint>, Status> {
        let principal = principal_from_request(&request)?;
        let request = request.into_inner();
        let context = authority_call_context_from_proto(request.context.as_ref())?;
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
        request: Request<core_v1::AuthorizeEndpointRequest>,
    ) -> Result<Response<semantic_v1::EndpointGrant>, Status> {
        let principal = principal_from_request(&request)?;
        let request = request.into_inner();
        let context = authority_call_context_from_proto(request.context.as_ref())?;
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
        request: Request<core_v1::RevokeEndpointRequest>,
    ) -> Result<Response<()>, Status> {
        let principal = principal_from_request(&request)?;
        let request = request.into_inner();
        let context = authority_call_context_from_proto(request.context.as_ref())?;
        let grant = semantic_identity_from_proto(request.grant, "grant")?;
        self.authority()
            .revoke_endpoint(&context, &principal, &grant)
            .map_err(authority_status)?;
        Ok(Response::new(()))
    }

    async fn read_events(
        &self,
        request: Request<core_v1::ReadEventsRequest>,
    ) -> Result<Response<core_v1::ReadEventsResponse>, Status> {
        let principal = principal_from_request(&request)?;
        let request = request.into_inner();
        let context = authority_call_context_from_proto(request.context.as_ref())?;
        let cursor = semantic_event_cursor_from_proto(request.cursor)?;
        let page = self
            .authority()
            .read_events(&context, &principal, &cursor, request.limit as usize)
            .map_err(authority_status)?;
        Ok(Response::new(core_v1::ReadEventsResponse {
            page: Some(to_semantic_proto_event_page(&page)),
        }))
    }

    type WatchEventsStream = std::pin::Pin<
        Box<
            dyn tokio_stream::Stream<Item = Result<core_v1::WatchEventsResponse, Status>>
                + Send
                + 'static,
        >,
    >;

    async fn watch_events(
        &self,
        request: Request<core_v1::WatchEventsRequest>,
    ) -> Result<Response<Self::WatchEventsStream>, Status> {
        let principal = principal_from_request(&request)?;
        let request = request.into_inner();
        let context = authority_call_context_from_proto(request.context.as_ref())?;
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
                let response = core_v1::WatchEventsResponse {
                    body: Some(core_v1::watch_events_response::Body::Event(
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
                                let response = core_v1::WatchEventsResponse {
                                    body: Some(core_v1::watch_events_response::Body::Event(
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
}
