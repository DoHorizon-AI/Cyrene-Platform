//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 authority_execution_target.rs                                  │
//! │  Module: cy_workspace_postgres_storage                             │
//! │  Role: Resolve trusted Product operation execution bindings.       │
//! │                                                                     │
//! │  模块职责：解析受信 Product operation 到执行目标的持久绑定。            │
//! └─────────────────────────────────────────────────────────────────────┘

use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
use thiserror::Error;
use uuid::Uuid;

const TARGET_TABLE: &str = "cyrene_workspace_device_registry.authority_execution_target_bindings";

/// A trusted operation binding to one exact, currently active execution device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorityExecutionTargetBinding {
    pub organization_id: String,
    pub workspace_id: String,
    pub operation_owner_id: String,
    pub operation_id: String,
    pub target_component: String,
    pub execution_device_id: String,
    pub execution_device_generation: u64,
    pub execution_authorization_id: Uuid,
    pub certificate_fingerprint_sha256: [u8; 32],
    pub target_binding_manifest_sha256: [u8; 32],
    pub bundle_manifest_sha256: [u8; 32],
    pub contract_activation_generation: u64,
    pub owner_source_commit: String,
    pub endpoint_base: String,
}

/// Closed storage error categories for trusted target resolution.
#[derive(Debug, Error)]
pub enum AuthorityExecutionTargetError {
    #[error("trusted execution target is invalid")]
    InvalidBinding,
    #[error("trusted execution target storage is unavailable")]
    Unavailable,
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
}

/// Runtime read-only resolver for operator-provisioned execution targets.
#[derive(Clone)]
pub struct PostgresAuthorityExecutionTargetStore {
    pool: PgPool,
}

impl PostgresAuthorityExecutionTargetStore {
    /// Creates a resolver using the application database pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Connects with the runtime registry/directory database configuration.
    pub async fn connect_from_environment() -> Result<Self, AuthorityExecutionTargetError> {
        let database_url = std::env::var("CYRENE_WORKSPACE_DEVICE_REGISTRY_DATABASE_URL")
            .or_else(|_| std::env::var("CYRENE_WORKSPACE_DIRECTORY_DATABASE_URL"))
            .map_err(|_| AuthorityExecutionTargetError::Unavailable)?;
        let pool = PgPool::connect(&database_url)
            .await
            .map_err(|_| AuthorityExecutionTargetError::Unavailable)?;
        Ok(Self::new(pool))
    }

    /// Resolves one exact Product operation and verifies its pinned device is still current.
    ///
    /// This read is advisory; enqueue repeats the registry/session checks while holding locks.
    pub async fn resolve(
        &self,
        organization_id: &str,
        workspace_id: &str,
        operation_owner_id: &str,
        operation_id: &str,
    ) -> Result<Option<AuthorityExecutionTargetBinding>, AuthorityExecutionTargetError> {
        let row = sqlx::query(&format!(
            r#"SELECT
                target.organization_id,
                target.workspace_id,
                target.operation_owner_id,
                target.operation_id,
                target.target_component,
                target.execution_device_id,
                target.execution_device_generation,
                target.execution_authorization_id,
                target.certificate_fingerprint_sha256,
                target.target_binding_manifest_sha256,
                target.bundle_manifest_sha256,
                target.contract_activation_generation,
                target.owner_source_commit,
                target.endpoint_base
            FROM {TARGET_TABLE} AS target
            JOIN cyrene_workspace_device_registry.certificate_records AS certificate
              ON certificate.authorization_id = target.execution_authorization_id
             AND certificate.organization_id = target.organization_id
             AND certificate.workspace_id = target.workspace_id
             AND certificate.device_id = target.execution_device_id
             AND certificate.authorization_generation = target.execution_device_generation
             AND certificate.certificate_sha256 = target.certificate_fingerprint_sha256
             AND certificate.state = 'active'
            JOIN cyrene_workspace_directory.workspace_device_identities AS identity
              ON identity.organization_id = target.organization_id
             AND identity.workspace_id = target.workspace_id
             AND identity.device_id = target.execution_device_id
             AND identity.current_authorization_generation = target.execution_device_generation
            WHERE target.organization_id = $1
              AND target.workspace_id = $2
              AND target.operation_owner_id = $3
              AND target.operation_id = $4
              AND target.active = TRUE"#
        ))
        .bind(organization_id)
        .bind(workspace_id)
        .bind(operation_owner_id)
        .bind(operation_id)
        .fetch_optional(&self.pool)
        .await?;

        let Some(row) = row else {
            return Ok(None);
        };
        let execution_device_generation: i64 = row.try_get("execution_device_generation")?;
        let authorization_id: Vec<u8> = row.try_get("execution_authorization_id")?;
        let fingerprint: Vec<u8> = row.try_get("certificate_fingerprint_sha256")?;
        let target_manifest: Vec<u8> = row.try_get("target_binding_manifest_sha256")?;
        let bundle_manifest: Vec<u8> = row.try_get("bundle_manifest_sha256")?;
        let contract_activation_generation: i64 = row.try_get("contract_activation_generation")?;
        let endpoint_base: String = row.try_get("endpoint_base")?;
        let binding = AuthorityExecutionTargetBinding {
            organization_id: row.try_get("organization_id")?,
            workspace_id: row.try_get("workspace_id")?,
            operation_owner_id: row.try_get("operation_owner_id")?,
            operation_id: row.try_get("operation_id")?,
            target_component: row.try_get("target_component")?,
            execution_device_id: row.try_get("execution_device_id")?,
            execution_device_generation: u64::try_from(execution_device_generation)
                .map_err(|_| AuthorityExecutionTargetError::Unavailable)?,
            execution_authorization_id: Uuid::from_slice(&authorization_id)
                .map_err(|_| AuthorityExecutionTargetError::Unavailable)?,
            certificate_fingerprint_sha256: fingerprint
                .try_into()
                .map_err(|_| AuthorityExecutionTargetError::Unavailable)?,
            target_binding_manifest_sha256: target_manifest
                .try_into()
                .map_err(|_| AuthorityExecutionTargetError::Unavailable)?,
            bundle_manifest_sha256: bundle_manifest
                .try_into()
                .map_err(|_| AuthorityExecutionTargetError::Unavailable)?,
            contract_activation_generation: u64::try_from(contract_activation_generation)
                .map_err(|_| AuthorityExecutionTargetError::Unavailable)?,
            owner_source_commit: row.try_get("owner_source_commit")?,
            endpoint_base,
        };
        validate_binding(&binding)?;
        Ok(Some(binding))
    }
}

/// Restricted writer for the signed-manifest administration path.
///
/// Construct this adapter with a pool whose database role has only the dedicated
/// `cyrene_workspace_execution_target_admin` function grant. Never use the runtime pool.
#[derive(Clone)]
pub struct PostgresAuthorityExecutionTargetAdmin {
    pool: PgPool,
}

impl PostgresAuthorityExecutionTargetAdmin {
    /// Creates an importer adapter for the restricted administration database role.
    pub fn new(restricted_admin_pool: PgPool) -> Self {
        Self {
            pool: restricted_admin_pool,
        }
    }

    /// Connects through the dedicated admin login and assumes only its NOLOGIN role.
    ///
    /// The connection URL must use `CYRENE_AUTHORITY_ADMIN_DATABASE_URL`; it never falls back
    /// to the runtime application credentials. The database login needs membership in the
    /// restricted admin role so this per-connection `SET ROLE` can succeed.
    pub async fn connect_from_environment() -> Result<Self, AuthorityExecutionTargetError> {
        let database_url = std::env::var("CYRENE_AUTHORITY_ADMIN_DATABASE_URL")
            .map_err(|_| AuthorityExecutionTargetError::Unavailable)?;
        let pool = PgPoolOptions::new()
            .after_connect(|connection, _| {
                Box::pin(async move {
                    sqlx::query("SET ROLE cyrene_workspace_execution_target_admin")
                        .execute(connection)
                        .await?;
                    Ok(())
                })
            })
            .connect(&database_url)
            .await
            .map_err(|_| AuthorityExecutionTargetError::Unavailable)?;
        Ok(Self::new(pool))
    }

    /// Imports one binding after the caller verifies the pinned manifest and operator identity.
    ///
    /// PostgreSQL independently checks that the selected authorization, certificate, and
    /// current Directory generation still match while holding row locks, then appends an audit
    /// event in the same transaction as the binding update.
    pub async fn upsert(
        &self,
        import: AuthorityExecutionTargetImport,
    ) -> Result<(), AuthorityExecutionTargetError> {
        validate_binding(&AuthorityExecutionTargetBinding::from(import.clone()))?;
        if import.configured_by.trim().is_empty()
            || import.configuration_reason.trim().is_empty()
            || import.configured_at_unix_ms == 0
        {
            return Err(AuthorityExecutionTargetError::InvalidBinding);
        }

        sqlx::query(
            r#"SELECT cyrene_workspace_device_registry.upsert_authority_execution_target_binding(
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13,
                $14, $15, $16, $17
            )"#,
        )
        .bind(&import.organization_id)
        .bind(&import.workspace_id)
        .bind(&import.operation_owner_id)
        .bind(&import.operation_id)
        .bind(&import.target_component)
        .bind(&import.execution_device_id)
        .bind(
            i64::try_from(import.execution_device_generation)
                .map_err(|_| AuthorityExecutionTargetError::InvalidBinding)?,
        )
        .bind(import.execution_authorization_id.as_bytes().as_slice())
        .bind(import.certificate_fingerprint_sha256.as_slice())
        .bind(import.target_binding_manifest_sha256.as_slice())
        .bind(import.bundle_manifest_sha256.as_slice())
        .bind(
            i64::try_from(import.contract_activation_generation)
                .map_err(|_| AuthorityExecutionTargetError::InvalidBinding)?,
        )
        .bind(&import.owner_source_commit)
        .bind(&import.endpoint_base)
        .bind(&import.configured_by)
        .bind(&import.configuration_reason)
        .bind(
            i64::try_from(import.configured_at_unix_ms)
                .map_err(|_| AuthorityExecutionTargetError::InvalidBinding)?,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

fn validate_binding(
    binding: &AuthorityExecutionTargetBinding,
) -> Result<(), AuthorityExecutionTargetError> {
    let endpoint = url::Url::parse(&binding.endpoint_base)
        .map_err(|_| AuthorityExecutionTargetError::InvalidBinding)?;
    if binding.organization_id.trim().is_empty()
        || binding.workspace_id.trim().is_empty()
        || binding.operation_owner_id.trim().is_empty()
        || binding.operation_id.trim().is_empty()
        || binding.target_component.trim().is_empty()
        || binding.execution_device_id.trim().is_empty()
        || binding.execution_device_generation == 0
        || binding.execution_authorization_id.is_nil()
        || binding.certificate_fingerprint_sha256 == [0; 32]
        || binding.target_binding_manifest_sha256 == [0; 32]
        || binding.bundle_manifest_sha256 == [0; 32]
        || binding.contract_activation_generation == 0
        || !matches!(binding.owner_source_commit.len(), 40 | 64)
        || !binding
            .owner_source_commit
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        || endpoint.scheme() != "https"
        || endpoint.host_str().is_none()
        || endpoint.username() != ""
        || endpoint.password().is_some()
        || endpoint.fragment().is_some()
    {
        return Err(AuthorityExecutionTargetError::InvalidBinding);
    }
    Ok(())
}

/// Trusted operator input to be passed to the restricted target import API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorityExecutionTargetImport {
    pub organization_id: String,
    pub workspace_id: String,
    pub operation_owner_id: String,
    pub operation_id: String,
    pub target_component: String,
    pub execution_device_id: String,
    pub execution_device_generation: u64,
    pub execution_authorization_id: Uuid,
    pub certificate_fingerprint_sha256: [u8; 32],
    pub target_binding_manifest_sha256: [u8; 32],
    pub bundle_manifest_sha256: [u8; 32],
    pub contract_activation_generation: u64,
    pub owner_source_commit: String,
    pub endpoint_base: String,
    pub configured_by: String,
    pub configuration_reason: String,
    pub configured_at_unix_ms: u64,
}

impl From<AuthorityExecutionTargetImport> for AuthorityExecutionTargetBinding {
    fn from(import: AuthorityExecutionTargetImport) -> Self {
        Self {
            organization_id: import.organization_id,
            workspace_id: import.workspace_id,
            operation_owner_id: import.operation_owner_id,
            operation_id: import.operation_id,
            target_component: import.target_component,
            execution_device_id: import.execution_device_id,
            execution_device_generation: import.execution_device_generation,
            execution_authorization_id: import.execution_authorization_id,
            certificate_fingerprint_sha256: import.certificate_fingerprint_sha256,
            target_binding_manifest_sha256: import.target_binding_manifest_sha256,
            bundle_manifest_sha256: import.bundle_manifest_sha256,
            contract_activation_generation: import.contract_activation_generation,
            owner_source_commit: import.owner_source_commit,
            endpoint_base: import.endpoint_base,
        }
    }
}
