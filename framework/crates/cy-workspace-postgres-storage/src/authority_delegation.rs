//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  Module: cy_workspace_postgres_storage::authority_delegation         │
//! │  Role: Persist revocable, user-owned workload delegations.           │
//! │                                                                     │
//! │  模块职责：持久化可撤销且归属于用户的 workload delegation。            │
//! └─────────────────────────────────────────────────────────────────────┘

use cy_workspace_control_plane::authority::{
    AuthorityDelegation, AuthorityDelegationError, AuthorityDelegationStore,
};
use sqlx::{PgPool, Row};
use uuid::Uuid;

const DELEGATION_TABLE: &str = "cyrene_workspace_device_registry.authority_delegations";

/// PostgreSQL-backed delegation store used by the production Authority host.
#[derive(Clone)]
pub struct PostgresAuthorityDelegationStore {
    pool: PgPool,
}

impl PostgresAuthorityDelegationStore {
    /// Construct a store from the Authority runtime pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl AuthorityDelegationStore for PostgresAuthorityDelegationStore {
    async fn create(
        &self,
        delegation: AuthorityDelegation,
    ) -> Result<AuthorityDelegation, AuthorityDelegationError> {
        let operations = serde_json::to_value(&delegation.operations)
            .map_err(|_| AuthorityDelegationError::Unavailable)?;
        let row = sqlx::query(&format!(
            r#"INSERT INTO {DELEGATION_TABLE} (
                delegation_id, generation, organization_id, workspace_id,
                principal_issuer, principal_subject, audience, managed_workload_id,
                operations, expires_at_unix_ms, created_at_unix_ms
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
            RETURNING delegation_id, generation, organization_id, workspace_id,
                principal_issuer, principal_subject, audience, managed_workload_id, operations,
                expires_at_unix_ms, revoked_at_unix_ms"#
        ))
        .bind(delegation.delegation_id)
        .bind(
            i64::try_from(delegation.generation)
                .map_err(|_| AuthorityDelegationError::Unavailable)?,
        )
        .bind(&delegation.organization_id)
        .bind(&delegation.workspace_id)
        .bind(&delegation.principal_issuer)
        .bind(&delegation.principal_subject)
        .bind(&delegation.audience)
        .bind(&delegation.managed_workload_id)
        .bind(operations)
        .bind(
            i64::try_from(delegation.expires_at_unix_ms)
                .map_err(|_| AuthorityDelegationError::Unavailable)?,
        )
        .bind(
            i64::try_from(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|_| AuthorityDelegationError::Unavailable)?
                    .as_millis(),
            )
            .map_err(|_| AuthorityDelegationError::Unavailable)?,
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|_| AuthorityDelegationError::Unavailable)?;
        row_to_delegation(row)
    }

    async fn get(
        &self,
        organization_id: &str,
        workspace_id: &str,
        delegation_id: Uuid,
    ) -> Result<AuthorityDelegation, AuthorityDelegationError> {
        let row = sqlx::query(&format!(
            "SELECT delegation_id, generation, organization_id, workspace_id, principal_issuer, principal_subject, audience, managed_workload_id, operations, expires_at_unix_ms, revoked_at_unix_ms FROM {DELEGATION_TABLE} WHERE delegation_id = $1 AND organization_id = $2 AND workspace_id = $3"
        ))
        .bind(delegation_id)
        .bind(organization_id)
        .bind(workspace_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| AuthorityDelegationError::Unavailable)?
        .ok_or(AuthorityDelegationError::NotFound)?;
        row_to_delegation(row)
    }

    async fn revoke(
        &self,
        organization_id: &str,
        workspace_id: &str,
        delegation_id: Uuid,
        now_unix_ms: u64,
    ) -> Result<AuthorityDelegation, AuthorityDelegationError> {
        let now = i64::try_from(now_unix_ms).map_err(|_| AuthorityDelegationError::Unavailable)?;
        let row = sqlx::query(&format!(
            r#"UPDATE {DELEGATION_TABLE}
            SET generation = generation + 1, revoked_at_unix_ms = $4
            WHERE delegation_id = $1 AND organization_id = $2 AND workspace_id = $3
              AND revoked_at_unix_ms IS NULL
            RETURNING delegation_id, generation, organization_id, workspace_id,
                principal_issuer, principal_subject, audience, managed_workload_id, operations,
                expires_at_unix_ms, revoked_at_unix_ms"#
        ))
        .bind(delegation_id)
        .bind(organization_id)
        .bind(workspace_id)
        .bind(now)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| AuthorityDelegationError::Unavailable)?;
        if let Some(row) = row {
            return row_to_delegation(row);
        }
        self.get(organization_id, workspace_id, delegation_id).await
    }
}

fn row_to_delegation(
    row: sqlx::postgres::PgRow,
) -> Result<AuthorityDelegation, AuthorityDelegationError> {
    let operations_value: serde_json::Value = row
        .try_get("operations")
        .map_err(|_| AuthorityDelegationError::Unavailable)?;
    let operations = serde_json::from_value(operations_value)
        .map_err(|_| AuthorityDelegationError::Unavailable)?;
    let generation: i64 = row
        .try_get("generation")
        .map_err(|_| AuthorityDelegationError::Unavailable)?;
    let expires_at: i64 = row
        .try_get("expires_at_unix_ms")
        .map_err(|_| AuthorityDelegationError::Unavailable)?;
    let revoked_at: Option<i64> = row
        .try_get("revoked_at_unix_ms")
        .map_err(|_| AuthorityDelegationError::Unavailable)?;
    Ok(AuthorityDelegation {
        delegation_id: row
            .try_get("delegation_id")
            .map_err(|_| AuthorityDelegationError::Unavailable)?,
        generation: u64::try_from(generation).map_err(|_| AuthorityDelegationError::Unavailable)?,
        organization_id: row
            .try_get("organization_id")
            .map_err(|_| AuthorityDelegationError::Unavailable)?,
        workspace_id: row
            .try_get("workspace_id")
            .map_err(|_| AuthorityDelegationError::Unavailable)?,
        principal_issuer: row
            .try_get("principal_issuer")
            .map_err(|_| AuthorityDelegationError::Unavailable)?,
        principal_subject: row
            .try_get("principal_subject")
            .map_err(|_| AuthorityDelegationError::Unavailable)?,
        audience: row
            .try_get("audience")
            .map_err(|_| AuthorityDelegationError::Unavailable)?,
        managed_workload_id: row
            .try_get("managed_workload_id")
            .map_err(|_| AuthorityDelegationError::Unavailable)?,
        operations,
        expires_at_unix_ms: u64::try_from(expires_at)
            .map_err(|_| AuthorityDelegationError::Unavailable)?,
        revoked_at_unix_ms: revoked_at
            .map(u64::try_from)
            .transpose()
            .map_err(|_| AuthorityDelegationError::Unavailable)?,
    })
}
