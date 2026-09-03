// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: contracts/rust/cy-kernel-contract/src/worker.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! Worker 运行状态机与实体模型。

use std::collections::BTreeMap;

use crate::{
    identity::Identity,
    resource::Quantity,
    validation::{
        validate_namespaced_id, validate_text, ContractError, MAX_EXECUTION_REF_BYTES,
        MAX_PROPERTIES,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerState {
    Registered,
    Starting,
    Running,
    Draining,
    Stopped,
    Failed,
    Lost,
}

impl WorkerState {
    pub fn can_transition_to(self, next: Self) -> bool {
        self == next
            || matches!(
                (self, next),
                (
                    Self::Registered,
                    Self::Starting | Self::Draining | Self::Stopped | Self::Failed | Self::Lost
                ) | (
                    Self::Starting,
                    Self::Running | Self::Draining | Self::Stopped | Self::Failed | Self::Lost
                ) | (
                    Self::Running,
                    Self::Draining | Self::Stopped | Self::Failed | Self::Lost
                ) | (Self::Draining, Self::Stopped | Self::Failed | Self::Lost)
            )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worker {
    pub identity: Identity,
    pub principal: Identity,
    pub provider: Identity,
    pub lease: Identity,
    pub state: WorkerState,
    pub execution_ref: String,
    pub limits: BTreeMap<String, Quantity>,
}

impl Worker {
    pub fn validate(&self) -> Result<(), ContractError> {
        self.identity.validate()?;
        self.principal.validate()?;
        self.provider.validate()?;
        self.lease.validate()?;
        validate_text(
            "worker execution reference",
            &self.execution_ref,
            MAX_EXECUTION_REF_BYTES,
        )?;
        if self.limits.len() > MAX_PROPERTIES {
            return Err(ContractError::new(
                "WORKER_LIMIT_COUNT_EXCEEDED",
                "worker has too many generic execution limits",
            ));
        }
        for (key, quantity) in &self.limits {
            validate_namespaced_id("worker limit key", key)?;
            quantity.validate()?;
        }
        Ok(())
    }
}
