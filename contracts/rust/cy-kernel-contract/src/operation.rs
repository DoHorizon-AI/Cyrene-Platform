// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: contracts/rust/cy-kernel-contract/src/operation.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! Operation 状态机与实体模型。

use std::collections::BTreeMap;

use crate::{
    identity::Identity,
    validation::{validate_namespaced_id, validate_properties, validate_timestamp, ContractError},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationState {
    Created,
    Pending,
    Running,
    Succeeded,
    Failed,
    Cancelling,
    Cancelled,
    Lost,
}

impl OperationState {
    pub fn can_transition_to(self, next: Self) -> bool {
        self == next
            || matches!(
                (self, next),
                (
                    Self::Created,
                    Self::Pending
                        | Self::Running
                        | Self::Cancelling
                        | Self::Cancelled
                        | Self::Failed
                ) | (
                    Self::Pending,
                    Self::Running | Self::Cancelling | Self::Cancelled | Self::Failed | Self::Lost
                ) | (
                    Self::Running,
                    Self::Succeeded | Self::Failed | Self::Cancelling | Self::Lost
                ) | (
                    Self::Cancelling,
                    Self::Cancelled | Self::Failed | Self::Lost
                )
            )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Operation {
    pub identity: Identity,
    pub owner: Identity,
    pub executor: Identity,
    pub kind: String,
    pub state: OperationState,
    pub deadline_unix_ms: Option<u64>,
    pub parent: Option<Identity>,
    pub metadata: BTreeMap<String, String>,
}

impl Operation {
    pub fn validate(&self) -> Result<(), ContractError> {
        self.identity.validate()?;
        self.owner.validate()?;
        self.executor.validate()?;
        validate_namespaced_id("operation kind", &self.kind)?;
        if let Some(deadline) = self.deadline_unix_ms {
            validate_timestamp("operation deadline", deadline)?;
        }
        if let Some(parent) = &self.parent {
            parent.validate()?;
            if parent == &self.identity {
                return Err(ContractError::new(
                    "OPERATION_PARENT_INVALID",
                    "operation cannot be its own parent",
                ));
            }
        }
        validate_properties(&self.metadata)
    }
}
