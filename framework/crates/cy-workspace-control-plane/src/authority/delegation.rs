//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  Module: cy_workspace_control_plane::authority::delegation          │
//! │  Role: Model revocable, narrowly scoped workload delegations.       │
//! │                                                                     │
//! │  模块职责：建模可撤销、范围受限的 workload delegation。                │
//! └─────────────────────────────────────────────────────────────────────┘

use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::RwLock;
use uuid::Uuid;

/// One operation and its exact Product resource allowlist.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegatedOperationScope {
    /// Catalog owner that supplies the operation.
    pub owner_id: String,
    /// Exact operation ID from the active Product catalog.
    pub operation_id: String,
    /// Exact resource IDs permitted by this grant; empty is valid only for unscoped operations.
    pub resource_ids: Vec<String>,
}

/// A persisted workload grant anchored to the human who issued it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorityDelegation {
    /// Opaque stable grant ID.
    pub delegation_id: Uuid,
    /// Monotonic generation changed on revocation.
    pub generation: u64,
    /// Verified organization and workspace.
    pub organization_id: String,
    /// Exact workspace selected by the issuer.
    pub workspace_id: String,
    /// Verified user identity; workload callers cannot supply it.
    pub principal_issuer: String,
    /// Verified user identity; workload callers cannot supply it.
    pub principal_subject: String,
    /// Authenticated service audience allowed to use the grant.
    pub audience: String,
    /// Exact managed workload identity allowed to use the grant.
    pub managed_workload_id: String,
    /// Narrow owner, operation, and resource allowlist.
    pub operations: Vec<DelegatedOperationScope>,
    /// Absolute grant expiry in Unix milliseconds.
    pub expires_at_unix_ms: u64,
    /// Revocation time, if revoked.
    pub revoked_at_unix_ms: Option<u64>,
}

/// Fail-closed persistent delegation store failures.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum AuthorityDelegationError {
    /// The requested grant does not exist in the supplied owner scope.
    #[error("delegation was not found")]
    NotFound,
    /// The grant has expired or was revoked.
    #[error("delegation is no longer active")]
    Inactive,
    /// The store could not complete a durable operation.
    #[error("delegation store is unavailable")]
    Unavailable,
}

/// Durable grant storage used by Authority; implementations must persist revocation generations.
#[async_trait]
pub trait AuthorityDelegationStore: Send + Sync {
    /// Insert a grant and return the exact committed record.
    async fn create(
        &self,
        delegation: AuthorityDelegation,
    ) -> Result<AuthorityDelegation, AuthorityDelegationError>;

    /// Read a grant by exact organization and workspace.
    async fn get(
        &self,
        organization_id: &str,
        workspace_id: &str,
        delegation_id: Uuid,
    ) -> Result<AuthorityDelegation, AuthorityDelegationError>;

    /// Revoke a grant and increment its generation exactly once.
    async fn revoke(
        &self,
        organization_id: &str,
        workspace_id: &str,
        delegation_id: Uuid,
        now_unix_ms: u64,
    ) -> Result<AuthorityDelegation, AuthorityDelegationError>;
}

/// In-memory reference store for deterministic tests. Production composition uses PostgreSQL.
#[derive(Clone, Default)]
pub struct MemoryAuthorityDelegationStore {
    records: Arc<RwLock<BTreeMap<Uuid, AuthorityDelegation>>>,
}

#[async_trait]
impl AuthorityDelegationStore for MemoryAuthorityDelegationStore {
    async fn create(
        &self,
        delegation: AuthorityDelegation,
    ) -> Result<AuthorityDelegation, AuthorityDelegationError> {
        let mut records = self.records.write().await;
        if records.contains_key(&delegation.delegation_id) {
            return Err(AuthorityDelegationError::Unavailable);
        }
        records.insert(delegation.delegation_id, delegation.clone());
        Ok(delegation)
    }

    async fn get(
        &self,
        organization_id: &str,
        workspace_id: &str,
        delegation_id: Uuid,
    ) -> Result<AuthorityDelegation, AuthorityDelegationError> {
        let records = self.records.read().await;
        let record = records
            .get(&delegation_id)
            .filter(|record| {
                record.organization_id == organization_id && record.workspace_id == workspace_id
            })
            .cloned()
            .ok_or(AuthorityDelegationError::NotFound)?;
        Ok(record)
    }

    async fn revoke(
        &self,
        organization_id: &str,
        workspace_id: &str,
        delegation_id: Uuid,
        now_unix_ms: u64,
    ) -> Result<AuthorityDelegation, AuthorityDelegationError> {
        let mut records = self.records.write().await;
        let record = records
            .get_mut(&delegation_id)
            .filter(|record| {
                record.organization_id == organization_id && record.workspace_id == workspace_id
            })
            .ok_or(AuthorityDelegationError::NotFound)?;
        if record.revoked_at_unix_ms.is_none() {
            record.generation = record
                .generation
                .checked_add(1)
                .ok_or(AuthorityDelegationError::Unavailable)?;
            record.revoked_at_unix_ms = Some(now_unix_ms);
        }
        Ok(record.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant() -> AuthorityDelegation {
        AuthorityDelegation {
            delegation_id: Uuid::new_v4(),
            generation: 1,
            organization_id: "org-1".into(),
            workspace_id: "workspace-1".into(),
            principal_issuer: "https://issuer.example".into(),
            principal_subject: "user-1".into(),
            audience: "exchange".into(),
            managed_workload_id: "exchange-v1".into(),
            operations: vec![DelegatedOperationScope {
                owner_id: "github".into(),
                operation_id: "github.issue.comment.create".into(),
                resource_ids: vec!["repo-1#42".into()],
            }],
            expires_at_unix_ms: 2_000,
            revoked_at_unix_ms: None,
        }
    }

    #[tokio::test]
    async fn memory_store_scopes_reads_and_advances_revocation_generation_once() {
        let store = MemoryAuthorityDelegationStore::default();
        let original = grant();
        store.create(original.clone()).await.unwrap();
        assert_eq!(
            store
                .get("org-other", "workspace-1", original.delegation_id)
                .await,
            Err(AuthorityDelegationError::NotFound)
        );
        let revoked = store
            .revoke("org-1", "workspace-1", original.delegation_id, 1_000)
            .await
            .unwrap();
        assert_eq!(revoked.generation, 2);
        assert_eq!(revoked.revoked_at_unix_ms, Some(1_000));
        assert_eq!(revoked.managed_workload_id, "exchange-v1");
        let replay = store
            .revoke("org-1", "workspace-1", original.delegation_id, 1_500)
            .await
            .unwrap();
        assert_eq!(replay.generation, 2);
        assert_eq!(replay.revoked_at_unix_ms, Some(1_000));
    }
}
