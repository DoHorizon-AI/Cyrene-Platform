//! Core v2 projection of the canonical authority with explicit namespace scope.

use cy_kernel_api::KernelAuthority;
use cy_proto::{core_v2, semantic_v1};
use tonic::{Request, Response, Status};

use crate::{
    adapter::{KernelServiceAdapter, OPERATION_EVENT_HISTORY_CAPACITY},
    convert::{
        authority_call_context_from_v2_proto, expires_after, proto_duration,
        semantic_contract_revision_from_proto, semantic_endpoint_from_proto,
        semantic_endpoint_grant_from_proto, semantic_event_cursor_from_proto,
        semantic_identity_from_proto, semantic_operation_from_proto, semantic_query_from_proto,
        semantic_worker_from_proto, to_semantic_proto_contract_lease,
        to_semantic_proto_contract_revision, to_semantic_proto_endpoint,
        to_semantic_proto_endpoint_grant, to_semantic_proto_event_page,
        to_semantic_proto_operation, to_semantic_proto_worker,
    },
    peer_cred::principal_from_request,
    rpc::authority_service::authority_status,
};

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
        request: Request<core_v2::AcquireSemanticLeaseRequest>,
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
        let lease = self
            .authority()
            .acquire_lease(&context, &principal, holder, query, expires_after(ttl))
            .map_err(authority_status)?;
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
        request: Request<core_v2::ReleaseSemanticLeaseRequest>,
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
        let operation = self
            .authority()
            .start_worker(&context, &principal, worker)
            .map_err(authority_status)?;
        Ok(Response::new(to_semantic_proto_operation(&operation)))
    }

    async fn heartbeat_worker(
        &self,
        request: Request<core_v2::HeartbeatWorkerRequest>,
    ) -> Result<Response<semantic_v1::Worker>, Status> {
        let principal = principal_from_request(&request)?;
        let request = request.into_inner();
        let context = authority_call_context_from_v2_proto(request.context.as_ref())?;
        let worker = semantic_identity_from_proto(request.worker, "worker")?;
        let lease = semantic_identity_from_proto(request.lease, "lease")?;
        let worker = self
            .authority()
            .heartbeat_worker(&context, &principal, &worker, &lease, request.fence_token)
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
        let operation = self
            .authority()
            .create_operation(&context, &principal, operation)
            .map_err(authority_status)?;
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
        request: Request<core_v2::CancelSemanticOperationRequest>,
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

    async fn subscribe_events(
        &self,
        request: Request<core_v2::SubscribeEventsRequest>,
    ) -> Result<Response<semantic_v1::EventPage>, Status> {
        let principal = principal_from_request(&request)?;
        let request = request.into_inner();
        let context = authority_call_context_from_v2_proto(request.context.as_ref())?;
        let cursor = semantic_event_cursor_from_proto(request.cursor)?;
        let limit = usize::try_from(request.page_size).unwrap_or(usize::MAX);
        if limit == 0 || limit > OPERATION_EVENT_HISTORY_CAPACITY {
            return Err(Status::invalid_argument(
                "event page_size must be in 1..=256",
            ));
        }
        let page = self
            .authority()
            .events_after(&context, &principal, &cursor, limit)
            .map_err(authority_status)?;
        debug_assert!(page.validate().is_ok());
        Ok(Response::new(to_semantic_proto_event_page(&page)))
    }
}
