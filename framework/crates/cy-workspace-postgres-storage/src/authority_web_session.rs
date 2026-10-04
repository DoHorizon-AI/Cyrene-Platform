//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 authority_web_session.rs                                       │
//! │  Module: cy_workspace_postgres_storage                             │
//! │  Role: Durable generations for independently verified web sessions.│
//! │                                                                     │
//! │  模块职责：为独立验证的 Web 会话分配并持久化可撤销代次。                 │
//! └─────────────────────────────────────────────────────────────────────┘

use cy_workspace_control_plane::web_identity::VerifiedWebPrincipal;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use thiserror::Error;
use uuid::Uuid;

const SESSION_TABLE: &str = "cyrene_workspace_device_registry.authority_web_sessions";

/// Database-backed identity and generation for one verified bearer session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorityWebSession {
    pub session_id: Uuid,
    pub organization_id: String,
    pub workspace_id: String,
    pub principal_issuer: String,
    pub principal_subject: String,
    pub session_generation: u64,
    pub issued_at_unix_ms: u64,
    pub expires_at_unix_ms: u64,
}

/// Fail-closed categories for durable web session resolution.
#[derive(Debug, Error)]
pub enum AuthorityWebSessionError {
    #[error("verified web session is invalid or expired")]
    InvalidOrExpired,
    #[error("verified web session has been revoked")]
    Revoked,
    #[error("web session storage is unavailable")]
    Unavailable,
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
}

/// Resolves only a principal produced by the Platform's verified web identity verifier.
///
/// The access token is hashed in process. Raw bearer bytes are never persisted or logged.
#[derive(Clone)]
pub struct PostgresAuthorityWebSessionStore {
    pool: PgPool,
}

impl PostgresAuthorityWebSessionStore {
    /// Creates a session resolver backed by the shared device registry database.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Connects using the runtime registry/directory database configuration.
    pub async fn connect_from_environment() -> Result<Self, AuthorityWebSessionError> {
        let database_url = std::env::var("CYRENE_WORKSPACE_DEVICE_REGISTRY_DATABASE_URL")
            .or_else(|_| std::env::var("CYRENE_WORKSPACE_DIRECTORY_DATABASE_URL"))
            .map_err(|_| AuthorityWebSessionError::Unavailable)?;
        let pool = PgPool::connect(&database_url)
            .await
            .map_err(|_| AuthorityWebSessionError::Unavailable)?;
        Ok(Self::new(pool))
    }

    /// Resolves or records a durable session for a server-verified bearer.
    ///
    /// The identity, organization, and expiry come from `VerifiedWebPrincipal`; workspace
    /// membership must already have been checked against the authoritative Directory.
    pub async fn resolve_verified_bearer(
        &self,
        principal: &VerifiedWebPrincipal,
        workspace_id: &str,
        bearer_token: &str,
        now_unix_ms: u64,
    ) -> Result<AuthorityWebSession, AuthorityWebSessionError> {
        let expires_at = u64::try_from(principal.expires_at_unix_ms())
            .map_err(|_| AuthorityWebSessionError::InvalidOrExpired)?;
        if workspace_id.trim().is_empty() || bearer_token.is_empty() || expires_at <= now_unix_ms {
            return Err(AuthorityWebSessionError::InvalidOrExpired);
        }

        let identity = principal.identity();
        if identity.issuer.trim().is_empty() || identity.subject.trim().is_empty() {
            return Err(AuthorityWebSessionError::InvalidOrExpired);
        }

        let bearer_token_sha256 = Sha256::digest(bearer_token.as_bytes()).to_vec();
        let session_id = Uuid::new_v4();
        let row = sqlx::query(&format!(
            r#"INSERT INTO {SESSION_TABLE} (
                session_id, organization_id, workspace_id, principal_issuer,
                principal_subject, bearer_token_sha256, issued_at_unix_ms,
                expires_at_unix_ms, created_at_unix_ms, last_seen_at_unix_ms
            ) VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $7, $7
            )
            ON CONFLICT (
                organization_id, workspace_id, principal_issuer,
                principal_subject, bearer_token_sha256
            ) DO UPDATE SET
                last_seen_at_unix_ms = GREATEST(
                    {SESSION_TABLE}.last_seen_at_unix_ms,
                    EXCLUDED.last_seen_at_unix_ms
                )
            WHERE {SESSION_TABLE}.revoked_at_unix_ms IS NULL
              AND {SESSION_TABLE}.expires_at_unix_ms > $7
            RETURNING session_id, organization_id, workspace_id,
                principal_issuer, principal_subject, session_generation,
                issued_at_unix_ms, expires_at_unix_ms, revoked_at_unix_ms"#
        ))
        .bind(session_id)
        .bind(principal.organization_id())
        .bind(workspace_id)
        .bind(&identity.issuer)
        .bind(&identity.subject)
        .bind(bearer_token_sha256)
        .bind(i64::try_from(now_unix_ms).map_err(|_| AuthorityWebSessionError::InvalidOrExpired)?)
        .bind(i64::try_from(expires_at).map_err(|_| AuthorityWebSessionError::InvalidOrExpired)?)
        .fetch_optional(&self.pool)
        .await?;

        let Some(row) = row else {
            return Err(AuthorityWebSessionError::Revoked);
        };
        if row
            .try_get::<Option<i64>, _>("revoked_at_unix_ms")?
            .is_some()
        {
            return Err(AuthorityWebSessionError::Revoked);
        }

        let session_generation: i64 = row.try_get("session_generation")?;
        let issued_at_unix_ms: i64 = row.try_get("issued_at_unix_ms")?;
        let expires_at_unix_ms: i64 = row.try_get("expires_at_unix_ms")?;
        Ok(AuthorityWebSession {
            session_id: row.try_get("session_id")?,
            organization_id: row.try_get("organization_id")?,
            workspace_id: row.try_get("workspace_id")?,
            principal_issuer: row.try_get("principal_issuer")?,
            principal_subject: row.try_get("principal_subject")?,
            session_generation: u64::try_from(session_generation)
                .map_err(|_| AuthorityWebSessionError::Unavailable)?,
            issued_at_unix_ms: u64::try_from(issued_at_unix_ms)
                .map_err(|_| AuthorityWebSessionError::Unavailable)?,
            expires_at_unix_ms: u64::try_from(expires_at_unix_ms)
                .map_err(|_| AuthorityWebSessionError::Unavailable)?,
        })
    }

    /// Revokes one exact durable session so future enqueue/credential checks fail closed.
    pub async fn revoke_session(
        &self,
        session_id: Uuid,
        now_unix_ms: u64,
    ) -> Result<bool, AuthorityWebSessionError> {
        let result = sqlx::query(&format!(
            r#"UPDATE {SESSION_TABLE}
            SET revoked_at_unix_ms = $2
            WHERE session_id = $1 AND revoked_at_unix_ms IS NULL"#
        ))
        .bind(session_id)
        .bind(i64::try_from(now_unix_ms).map_err(|_| AuthorityWebSessionError::Unavailable)?)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }
}
