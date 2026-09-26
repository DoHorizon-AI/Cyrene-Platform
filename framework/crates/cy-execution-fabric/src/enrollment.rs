//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 enrollment.rs                                                   │
//! │  Module: cy_execution_fabric::enrollment                            │
//! │  Role: Enrollment provider seam and development-only verifier.      │
//! │                                                                     │
//! │  模块职责：定义注册 Provider seam 与一次性开发 token 验证器。            │
//! └─────────────────────────────────────────────────────────────────────┘

use std::{collections::BTreeSet, sync::Mutex};

use cy_kernel_contract::Identity;

use crate::FabricContractError;

/// Account and Runtime scope bound to one workload identity.
/// 绑定到单个 workload identity 的 Account 与 Runtime scope。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeScope {
    pub organization_id: String,
    pub workspace_id: String,
    pub runtime: Identity,
}

/// Short-lived enrollment result. It contains no bootstrap credential.
/// 短期 enrollment result，不包含 bootstrap credential。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrollmentGrant {
    pub workload_identity: Identity,
    pub scope: RuntimeScope,
    pub expires_at_unix_ms: u64,
}

/// Replaceable OIDC/device/SSO/development enrollment boundary.
/// 可替换的 OIDC/device/SSO/development enrollment boundary。
pub trait EnrollmentProvider: Send + Sync {
    fn enroll(
        &self,
        proof: &str,
        scope: RuntimeScope,
        now_unix_ms: u64,
    ) -> Result<EnrollmentGrant, FabricContractError>;
}

/// Single-use in-memory development verifier. Never use in production.
/// 仅供 development 使用的一次性内存 verifier。严禁用于生产。
pub struct DevelopmentEnrollmentProvider {
    tokens: Mutex<BTreeSet<String>>,
    lifetime_ms: u64,
}

impl DevelopmentEnrollmentProvider {
    pub fn new(tokens: impl IntoIterator<Item = String>, lifetime_ms: u64) -> Self {
        Self {
            tokens: Mutex::new(tokens.into_iter().collect()),
            lifetime_ms,
        }
    }
}

impl EnrollmentProvider for DevelopmentEnrollmentProvider {
    fn enroll(
        &self,
        proof: &str,
        scope: RuntimeScope,
        now_unix_ms: u64,
    ) -> Result<EnrollmentGrant, FabricContractError> {
        // The enrollment proof is credential material and is never recorded.
        // Enrollment proof 属于 credential material，绝不记录。
        if proof.is_empty() {
            return Err(FabricContractError {
                reason_code: "AUTHENTICATION_REQUIRED",
                message: "development enrollment proof is required".to_string(),
            });
        }
        let mut tokens = self.tokens.lock().map_err(|_| FabricContractError {
            reason_code: "ENROLLMENT_UNAVAILABLE",
            message: "development enrollment verifier is unavailable".to_string(),
        })?;
        if !tokens.remove(proof) {
            return Err(FabricContractError {
                reason_code: "ENROLLMENT_PROOF_REJECTED",
                message: "development enrollment proof is invalid or already consumed".to_string(),
            });
        }
        let expires_at_unix_ms =
            now_unix_ms
                .checked_add(self.lifetime_ms)
                .ok_or(FabricContractError {
                    reason_code: "TIMESTAMP_INVALID",
                    message: "workload identity expiry overflowed".to_string(),
                })?;
        Ok(EnrollmentGrant {
            workload_identity: Identity {
                id: format!("workload-{}", scope.runtime.id),
                generation: scope.runtime.generation,
            },
            scope,
            expires_at_unix_ms,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn development_token_is_single_use_and_not_returned() {
        let provider = DevelopmentEnrollmentProvider::new(["once".to_string()], 1000);
        let scope = RuntimeScope {
            organization_id: "org-1".to_string(),
            workspace_id: "workspace-1".to_string(),
            runtime: Identity {
                id: "runtime-1".to_string(),
                generation: 1,
            },
        };
        let grant = provider.enroll("once", scope.clone(), 100).unwrap();
        assert_eq!(grant.expires_at_unix_ms, 1100);
        assert_eq!(grant.scope, scope);
        assert_eq!(
            provider
                .enroll("once", grant.scope, 101)
                .unwrap_err()
                .reason_code,
            "ENROLLMENT_PROOF_REJECTED"
        );
    }
}
