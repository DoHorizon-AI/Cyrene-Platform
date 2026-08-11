//! KernelServiceAdapter Operation 状态辅助函数与语义操作追踪。

use cy_kernel_api::{semantic, ProviderError};
use cy_proto::core_v1;
use tonic::Status;

use crate::{
    adapter::KernelServiceAdapter,
    convert::{now_timestamp, semantic_identity_key, semantic_operation_event_kind},
};

impl KernelServiceAdapter {
    pub(crate) fn operation_name(&self, prefix: &str, id: &str) -> String {
        format!("operations/{prefix}-{id}")
    }

    pub(crate) fn lease_name(
        &self,
        mutation: Option<&core_v1::MutationContext>,
    ) -> Result<String, Status> {
        mutation
            .and_then(|mutation| {
                if !mutation.idempotency_key.is_empty() {
                    Some(mutation.idempotency_key.clone())
                } else {
                    mutation
                        .request
                        .as_ref()
                        .filter(|request| !request.request_id.is_empty())
                        .map(|request| request.request_id.clone())
                }
            })
            .filter(|value| !value.is_empty())
            .map(|value| format!("lease-{value}"))
            .ok_or_else(|| {
                Status::invalid_argument("mutation idempotency_key or request_id is required")
            })
    }

    pub(crate) fn operation_success(&self, name: String, target: String) -> core_v1::Operation {
        let timestamp = now_timestamp();
        self.remember_operation(core_v1::Operation {
            name,
            state: core_v1::OperationState::Succeeded as i32,
            target_resource_name: target,
            cancellable: false,
            created_at: Some(timestamp.clone()),
            updated_at: Some(timestamp),
            outcome: None,
        })
    }

    pub(crate) fn operation_running(&self, name: String, target: String) -> core_v1::Operation {
        let timestamp = now_timestamp();
        self.remember_operation(core_v1::Operation {
            name,
            state: core_v1::OperationState::Running as i32,
            target_resource_name: target,
            cancellable: true,
            created_at: Some(timestamp.clone()),
            updated_at: Some(timestamp),
            outcome: None,
        })
    }

    pub(crate) fn operation_cancelled(&self, name: String, target: String) -> core_v1::Operation {
        let timestamp = now_timestamp();
        self.remember_operation(core_v1::Operation {
            name,
            state: core_v1::OperationState::Cancelled as i32,
            target_resource_name: target,
            cancellable: false,
            created_at: Some(timestamp.clone()),
            updated_at: Some(timestamp),
            outcome: None,
        })
    }

    pub(crate) fn remember_semantic_operation(
        &self,
        operation: semantic::Operation,
    ) -> semantic::Operation {
        self.semantic_operations
            .lock()
            .expect("semantic operation lock poisoned")
            .insert(
                semantic_identity_key(&operation.identity),
                operation.clone(),
            );
        self.publish_semantic_event(
            operation.identity.clone(),
            semantic_operation_event_kind(operation.state),
            "cyrene.operation.v1",
            Vec::new(),
        );
        operation
    }

    pub(crate) fn operation_failure(
        &self,
        name: String,
        target: String,
        error: &ProviderError,
    ) -> core_v1::Operation {
        let timestamp = now_timestamp();
        self.remember_operation(core_v1::Operation {
            name,
            state: core_v1::OperationState::Failed as i32,
            target_resource_name: target,
            cancellable: false,
            created_at: Some(timestamp.clone()),
            updated_at: Some(timestamp),
            outcome: Some(core_v1::operation::Outcome::Error(
                cy_proto::google::rpc::Status {
                    code: 9,
                    message: format!("{}: {}", error.reason_code, error.message),
                    details: Vec::new(),
                },
            )),
        })
    }
}
