//! 契约约束常量、ContractError 及通用校验器。

use std::collections::{BTreeMap, BTreeSet};

use crate::resource::Capability;

pub const CONTRACT_ID: &str = "cyrene.kernel.semantic";
pub const CONTRACT_VERSION: &str = "cyrene.kernel.semantic/v1";
pub const CONTRACT_MAJOR: u32 = 1;
pub const CONTRACT_MINOR: u32 = 0;
pub const MAX_ID_BYTES: usize = 256;
pub const MAX_NAMESPACED_ID_BYTES: usize = 128;
pub const MAX_CAPABILITIES: usize = 64;
pub const MAX_PROPERTIES: usize = 64;
pub const MAX_RESOURCES_PER_SNAPSHOT: usize = 1_024;
pub const MAX_RESOURCES_PER_LEASE: usize = 256;
pub const MAX_WORKERS_PER_SNAPSHOT: usize = 4_096;
pub const MAX_ENDPOINTS_PER_SNAPSHOT: usize = 4_096;
pub const MAX_EXECUTION_REF_BYTES: usize = 512;
pub const MAX_ERROR_MESSAGE_BYTES: usize = 1_024;
pub const MAX_EVENT_BODY_BYTES: usize = 64 * 1_024;
pub const MAX_EVENTS_PER_PAGE: usize = 256;
pub const MAX_TIMESTAMP_UNIX_MS: u64 = 253_402_300_799_999;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractError {
    pub reason_code: &'static str,
    pub message: String,
}

impl ContractError {
    pub(crate) fn new(reason_code: &'static str, message: impl Into<String>) -> Self {
        Self {
            reason_code,
            message: message.into(),
        }
    }
}

pub(crate) fn validate_capabilities(capabilities: &[Capability]) -> Result<(), ContractError> {
    if capabilities.len() > MAX_CAPABILITIES {
        return Err(ContractError::new(
            "CAPABILITY_LIMIT_EXCEEDED",
            "object has too many capabilities",
        ));
    }
    let mut ids = BTreeSet::new();
    for capability in capabilities {
        capability.validate()?;
        if !ids.insert(capability.id.as_str()) {
            return Err(ContractError::new(
                "CAPABILITY_DUPLICATE",
                "object contains a duplicate capability id",
            ));
        }
    }
    Ok(())
}

pub(crate) fn validate_properties(
    properties: &BTreeMap<String, String>,
) -> Result<(), ContractError> {
    if properties.len() > MAX_PROPERTIES {
        return Err(ContractError::new(
            "PROPERTY_LIMIT_EXCEEDED",
            "object has too many properties",
        ));
    }
    for (key, value) in properties {
        validate_namespaced_id("property key", key)?;
        validate_text("property value", value, MAX_ID_BYTES)?;
    }
    Ok(())
}

pub(crate) fn validate_namespaced_id(field: &str, value: &str) -> Result<(), ContractError> {
    validate_text(field, value, MAX_NAMESPACED_ID_BYTES)?;
    let mut expect_segment_start = true;
    for byte in value.bytes() {
        if matches!(byte, b'.' | b'-' | b'_') {
            if expect_segment_start {
                return Err(ContractError::new(
                    "NAMESPACED_ID_INVALID",
                    format!("{field} contains an empty identifier segment"),
                ));
            }
            expect_segment_start = true;
        } else if expect_segment_start {
            if !byte.is_ascii_lowercase() {
                return Err(ContractError::new(
                    "NAMESPACED_ID_INVALID",
                    format!("{field} segments must start with a lower-case letter"),
                ));
            }
            expect_segment_start = false;
        } else if !(byte.is_ascii_lowercase() || byte.is_ascii_digit()) {
            return Err(ContractError::new(
                "NAMESPACED_ID_INVALID",
                format!("{field} contains an unsupported character"),
            ));
        }
    }
    if expect_segment_start {
        return Err(ContractError::new(
            "NAMESPACED_ID_INVALID",
            format!("{field} cannot end with a separator"),
        ));
    }
    Ok(())
}

pub(crate) fn validate_timestamp(field: &str, unix_ms: u64) -> Result<(), ContractError> {
    if unix_ms == 0 || unix_ms > MAX_TIMESTAMP_UNIX_MS {
        return Err(ContractError::new(
            "TIMESTAMP_INVALID",
            format!("{field} must be a positive Protobuf-compatible Unix millisecond value"),
        ));
    }
    Ok(())
}

pub(crate) fn validate_reason_code(value: &str) -> Result<(), ContractError> {
    validate_text("reason code", value, MAX_NAMESPACED_ID_BYTES)?;
    if !value.bytes().enumerate().all(|(index, byte)| {
        if index == 0 {
            byte.is_ascii_uppercase()
        } else {
            byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_'
        }
    }) {
        return Err(ContractError::new(
            "REASON_CODE_INVALID",
            "reason code must use upper snake case",
        ));
    }
    Ok(())
}

pub(crate) fn validate_text(
    field: &str,
    value: &str,
    max_bytes: usize,
) -> Result<(), ContractError> {
    if value.is_empty() || value.len() > max_bytes || value.chars().any(char::is_control) {
        return Err(ContractError::new(
            "TEXT_INVALID",
            format!("{field} must be non-empty, bounded UTF-8 without control characters"),
        ));
    }
    Ok(())
}
