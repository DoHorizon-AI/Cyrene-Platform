//! 契约版本协商与错误拒绝类型。

use crate::validation::{
    validate_namespaced_id, validate_reason_code, validate_text, ContractError, CONTRACT_ID,
    CONTRACT_MAJOR, CONTRACT_MINOR, MAX_ERROR_MESSAGE_BYTES,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractRevision {
    pub contract_id: String,
    pub major: u32,
    pub minor: u32,
}

impl ContractRevision {
    pub fn current() -> Self {
        Self {
            contract_id: CONTRACT_ID.to_string(),
            major: CONTRACT_MAJOR,
            minor: CONTRACT_MINOR,
        }
    }

    pub fn validate(&self) -> Result<(), ContractError> {
        validate_namespaced_id("contract id", &self.contract_id)?;
        if self.major == 0 {
            return Err(ContractError::new(
                "CONTRACT_MAJOR_INVALID",
                "semantic contract major version must be non-zero",
            ));
        }
        Ok(())
    }

    /// Compatible revisions have the same contract ID and major version. The
    /// negotiated minor is the lower supported minor.
    pub fn negotiate(&self, offered: &Self) -> Option<Self> {
        if self.validate().is_err()
            || offered.validate().is_err()
            || self.contract_id != offered.contract_id
            || self.major != offered.major
        {
            return None;
        }
        Some(Self {
            contract_id: self.contract_id.clone(),
            major: self.major,
            minor: self.minor.min(offered.minor),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejection {
    pub reason_code: String,
    pub message: String,
}

impl Rejection {
    pub fn new(reason_code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            reason_code: reason_code.into(),
            message: message.into(),
        }
    }

    pub fn validate(&self) -> Result<(), ContractError> {
        validate_reason_code(&self.reason_code)?;
        validate_text("rejection message", &self.message, MAX_ERROR_MESSAGE_BYTES)
    }
}
