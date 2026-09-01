// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: contracts/rust/cy-kernel-contract/src/identity.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! 实体身份标识与主体模型。

use crate::validation::{validate_text, ContractError, MAX_ID_BYTES};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Identity {
    pub id: String,
    pub generation: u64,
}

impl Identity {
    pub fn validate(&self) -> Result<(), ContractError> {
        validate_text("identity id", &self.id, MAX_ID_BYTES)?;
        if self.generation == 0 {
            return Err(ContractError::new(
                "GENERATION_INVALID",
                "identity generation must be non-zero",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    pub identity: Identity,
}

impl Principal {
    pub fn validate(&self) -> Result<(), ContractError> {
        self.identity.validate()
    }
}
