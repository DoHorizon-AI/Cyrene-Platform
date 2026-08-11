//! 资源租约状态机、实体模型与权威/续约判定。

use std::collections::BTreeSet;

use crate::{
    identity::Identity,
    validation::{validate_timestamp, ContractError, MAX_RESOURCES_PER_LEASE},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseState {
    Active,
    Releasing,
    Released,
    Expired,
    Revoked,
    Failed,
}

impl LeaseState {
    /// Whether a state report is an idempotent replay or a legal forward-only
    /// transition for the same Lease generation.
    pub fn can_transition_to(self, next: Self) -> bool {
        self == next
            || matches!(
                (self, next),
                (
                    Self::Active,
                    Self::Releasing | Self::Expired | Self::Revoked | Self::Failed
                ) | (
                    Self::Releasing,
                    Self::Released | Self::Revoked | Self::Failed
                )
            )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    pub identity: Identity,
    pub holder: Identity,
    pub resources: Vec<Identity>,
    pub state: LeaseState,
    pub fence_token: u64,
    pub expires_at_unix_ms: Option<u64>,
}

impl Lease {
    pub fn validate(&self) -> Result<(), ContractError> {
        self.identity.validate()?;
        self.holder.validate()?;
        if self.resources.is_empty() || self.resources.len() > MAX_RESOURCES_PER_LEASE {
            return Err(ContractError::new(
                "LEASE_RESOURCE_LIMIT_INVALID",
                "lease must contain a bounded, non-empty resource set",
            ));
        }
        let mut resources = BTreeSet::new();
        for resource in &self.resources {
            resource.validate()?;
            if !resources.insert(resource) {
                return Err(ContractError::new(
                    "LEASE_RESOURCE_DUPLICATE",
                    "lease contains a duplicate resource identity",
                ));
            }
        }
        if self.fence_token == 0 {
            return Err(ContractError::new(
                "FENCE_TOKEN_INVALID",
                "lease fence token must be non-zero",
            ));
        }
        let expires_at = self.expires_at_unix_ms.ok_or_else(|| {
            ContractError::new(
                "LEASE_EXPIRY_REQUIRED",
                "v1 leases must have a finite expiry",
            )
        })?;
        validate_timestamp("lease expiry", expires_at)
    }

    pub fn authorizes(
        &self,
        holder: &Identity,
        resource: &Identity,
        fence_token: u64,
        now_unix_ms: u64,
    ) -> bool {
        self.validate().is_ok()
            && self.state == LeaseState::Active
            && &self.holder == holder
            && self.resources.contains(resource)
            && self.fence_token == fence_token
            && self
                .expires_at_unix_ms
                .is_some_and(|expires_at| now_unix_ms < expires_at)
    }

    pub fn renew(
        &self,
        fence_token: u64,
        expires_at_unix_ms: u64,
        now_unix_ms: u64,
    ) -> Result<Self, ContractError> {
        self.validate()?;
        if self.state != LeaseState::Active {
            return Err(ContractError::new(
                "LEASE_NOT_ACTIVE",
                "only an active lease can be renewed",
            ));
        }
        let current_expiry = self.expires_at_unix_ms.ok_or_else(|| {
            ContractError::new(
                "LEASE_EXPIRY_REQUIRED",
                "v1 leases must have a finite expiry",
            )
        })?;
        if now_unix_ms >= current_expiry {
            return Err(ContractError::new(
                "LEASE_EXPIRED",
                "an expired lease cannot be renewed",
            ));
        }
        if fence_token != self.fence_token {
            return Err(ContractError::new(
                "FENCE_MISMATCH",
                "lease renewal fence does not match current authority",
            ));
        }
        validate_timestamp("lease renewal expiry", expires_at_unix_ms)?;
        if expires_at_unix_ms <= current_expiry {
            return Err(ContractError::new(
                "LEASE_RENEWAL_INVALID",
                "lease renewal must strictly extend the finite expiry",
            ));
        }
        let mut renewed = self.clone();
        renewed.expires_at_unix_ms = Some(expires_at_unix_ms);
        Ok(renewed)
    }
}
