//! Dedicated authenticated UDS projection for Provider lifecycle evidence.

use cy_kernel_api::{KernelProviderAuthority, ProviderReconcileAction};
use cy_proto::{provider_v1, semantic_v1};
use tonic::{Request, Response, Status};

use crate::{
    adapter::KernelServiceAdapter,
    convert::{
        provider_call_context_from_proto, semantic_identity_from_proto,
        semantic_provider_from_proto, semantic_provider_snapshot_from_proto, semantic_status,
        to_semantic_proto_identity, to_semantic_proto_provider,
        to_semantic_proto_provider_snapshot,
    },
    peer_cred::principal_from_request,
    rpc::authority_service::authority_status,
};

fn to_proto_action(action: ProviderReconcileAction) -> provider_v1::ProviderReconcileAction {
    use provider_v1::ProviderReconcileActionKind as Kind;

    let (kind, subject) = match action {
        ProviderReconcileAction::Noop => (Kind::Noop, None),
        ProviderReconcileAction::RefreshResource(subject) => (Kind::RefreshResource, Some(subject)),
        ProviderReconcileAction::MarkWorkerLost(subject) => (Kind::MarkWorkerLost, Some(subject)),
        ProviderReconcileAction::RevokeLease(subject) => (Kind::RevokeLease, Some(subject)),
        ProviderReconcileAction::TerminateStaleWorker(subject) => {
            (Kind::TerminateStaleWorker, Some(subject))
        }
        ProviderReconcileAction::RestartWorker(subject) => (Kind::RestartWorker, Some(subject)),
        ProviderReconcileAction::MarkOperationLost(subject) => {
            (Kind::MarkOperationLost, Some(subject))
        }
        ProviderReconcileAction::RevokeEndpoint(subject) => (Kind::RevokeEndpoint, Some(subject)),
    };
    provider_v1::ProviderReconcileAction {
        kind: kind as i32,
        subject: subject.as_ref().map(to_semantic_proto_identity),
    }
}

#[tonic::async_trait]
impl provider_v1::kernel_provider_service_server::KernelProviderService for KernelServiceAdapter {
    async fn register_provider(
        &self,
        request: Request<provider_v1::RegisterProviderRequest>,
    ) -> Result<Response<semantic_v1::Provider>, Status> {
        let principal = principal_from_request(&request)?;
        let request = request.into_inner();
        let context = provider_call_context_from_proto(request.context.as_ref())?;
        let provider = semantic_provider_from_proto(
            request
                .provider
                .ok_or_else(|| Status::invalid_argument("provider is required"))?,
        )?;
        let provider = self
            .authority()
            .register_provider(&context, &principal, provider)
            .map_err(authority_status)?;
        Ok(Response::new(to_semantic_proto_provider(&provider)))
    }

    async fn publish_inventory(
        &self,
        request: Request<provider_v1::PublishInventoryRequest>,
    ) -> Result<Response<semantic_v1::ProviderSnapshot>, Status> {
        let principal = principal_from_request(&request)?;
        let request = request.into_inner();
        let context = provider_call_context_from_proto(request.context.as_ref())?;
        let snapshot = semantic_provider_snapshot_from_proto(
            request
                .snapshot
                .ok_or_else(|| Status::invalid_argument("provider snapshot is required"))?,
        )?;
        let snapshot = self
            .authority()
            .publish_inventory(&context, &principal, snapshot)
            .map_err(authority_status)?;
        Ok(Response::new(to_semantic_proto_provider_snapshot(
            &snapshot,
        )))
    }

    async fn reconcile_provider(
        &self,
        request: Request<provider_v1::ReconcileProviderRequest>,
    ) -> Result<Response<provider_v1::ReconcileProviderResponse>, Status> {
        let principal = principal_from_request(&request)?;
        let request = request.into_inner();
        let context = provider_call_context_from_proto(request.context.as_ref())?;
        let provider = semantic_identity_from_proto(request.provider, "provider")?;
        for result in request.stale_worker_results {
            let worker = semantic_identity_from_proto(result.worker, "stale worker")?;
            let terminated = match result.outcome {
                Some(provider_v1::terminate_stale_worker_result::Outcome::Terminated(value)) => {
                    value
                }
                Some(provider_v1::terminate_stale_worker_result::Outcome::Failure(failure)) => {
                    if failure.reason_code.is_empty() {
                        return Err(semantic_status(
                            tonic::Code::InvalidArgument,
                            "RECONCILIATION_RESULT_INVALID",
                            "termination failure requires a reason code",
                        ));
                    }
                    false
                }
                None => {
                    return Err(semantic_status(
                        tonic::Code::InvalidArgument,
                        "RECONCILIATION_RESULT_INVALID",
                        "termination result requires an outcome",
                    ));
                }
            };
            self.authority()
                .confirm_stale_worker_termination(
                    &context,
                    &principal,
                    &provider,
                    result.snapshot_generation,
                    &worker,
                    terminated,
                )
                .map_err(authority_status)?;
        }
        let result = self
            .authority()
            .reconcile_provider(&context, &principal, &provider)
            .map_err(authority_status)?;
        Ok(Response::new(provider_v1::ReconcileProviderResponse {
            provider: Some(to_semantic_proto_identity(&result.provider)),
            snapshot_generation: result.snapshot_generation,
            actions: result.actions.into_iter().map(to_proto_action).collect(),
        }))
    }
}
