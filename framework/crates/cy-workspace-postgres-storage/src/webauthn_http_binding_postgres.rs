//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 webauthn_http_binding_postgres.rs                                │
//! │  Module: cy_workspace_fabric::webauthn_http_binding_postgres         │
//! │  Role: Durable PostgreSQL bindings for WebAuthn HTTP sessions.       │
//! │                                                                     │
//! │  模块职责：持久化 WebAuthn HTTP ceremony 与已验证 bearer session 的绑定。 │
//! └─────────────────────────────────────────────────────────────────────┘
//!
//! This adapter stores no bearer or CSRF plaintext. Runtime connections use
//! a dedicated PostgreSQL role and schema, and all expiry/CAS decisions use
//! database time so multiple ACA replicas share one replay authority.

use std::str::FromStr;
use std::time::Duration;

use sqlx::migrate::Migrator;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};
use sqlx::{FromRow, PgPool, Postgres, Transaction};
use subtle::ConstantTimeEq;
use thiserror::Error;
use tonic::async_trait;

use crate::device_authorization::DeviceAuthorizationId;
use crate::webauthn_http::{
    VerifiedWebSessionContext, WebAuthnHttpCeremonyPurpose, WebAuthnHttpSessionBindingError,
    WebAuthnHttpSessionBindingStore, WebAuthnSessionCeremonyBinding,
    WebAuthnSessionFinishReservation,
};

const DATABASE_URL_ENV: &str = "CYRENE_WORKSPACE_WEBAUTHN_HTTP_SESSION_BINDING_DATABASE_URL";
const MIGRATION_DATABASE_URL_ENV: &str =
    "CYRENE_WORKSPACE_WEBAUTHN_HTTP_SESSION_BINDING_MIGRATION_DATABASE_URL";
const DATABASE_TIMEOUT: Duration = Duration::from_secs(5);
const STARTUP_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_CONNECTIONS: u32 = 8;

static MIGRATOR: Migrator = sqlx::migrate!("./migrations/webauthn_http_binding");

/// Configuration or startup failure from the HTTP session-binding adapter.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum WebAuthnHttpSessionBindingPostgresError {
    /// The trusted database URL is missing or malformed.
    #[error("WebAuthn HTTP session-binding database configuration is invalid")]
    Configuration,
    /// PostgreSQL, schema verification, or migration is unavailable.
    #[error("WebAuthn HTTP session-binding database is unavailable")]
    Unavailable,
}

/// PostgreSQL-backed implementation of the WebAuthn HTTP session-binding port.
///
/// Runtime connections must use a separately provisioned login principal that
/// is a member only of cyrene_workspace_webauthn_http_binding_app for this
/// schema. Connections require PostgreSQL TLS with hostname verification. This
/// adapter is separate from the credential/ceremony store and never falls back
/// to SQLite, process memory, or local disk.
pub struct PostgresWebAuthnHttpSessionBindingStore {
    pool: PgPool,
}

impl PostgresWebAuthnHttpSessionBindingStore {
    /// Connect with the trusted runtime database URL and validate the schema.
    pub async fn connect(
        database_url: &str,
    ) -> Result<Self, WebAuthnHttpSessionBindingPostgresError> {
        Self::start(database_url, false).await
    }

    /// Connect using server-side runtime configuration.
    pub async fn connect_from_environment() -> Result<Self, WebAuthnHttpSessionBindingPostgresError>
    {
        let database_url = std::env::var(DATABASE_URL_ENV)
            .map_err(|_| WebAuthnHttpSessionBindingPostgresError::Configuration)?;
        Self::connect(&database_url).await
    }

    /// Apply the schema migration with a separately provisioned operator URL.
    /// Runtime credentials must not own the schema or SQLx migration table.
    pub async fn migrate(
        database_url: &str,
    ) -> Result<(), WebAuthnHttpSessionBindingPostgresError> {
        let store = Self::start(database_url, true).await?;
        store.pool.close().await;
        Ok(())
    }

    /// Apply migrations using the dedicated operator environment variable.
    pub async fn migrate_from_environment() -> Result<(), WebAuthnHttpSessionBindingPostgresError> {
        let database_url = std::env::var(MIGRATION_DATABASE_URL_ENV)
            .map_err(|_| WebAuthnHttpSessionBindingPostgresError::Configuration)?;
        Self::migrate(&database_url).await
    }

    /// Check the live session-binding database and required table without writes.
    pub async fn health_check(&self) -> Result<(), WebAuthnHttpSessionBindingPostgresError> {
        sqlx::query(
            "SELECT ceremony_id FROM cyrene_workspace_webauthn_http_binding.session_bindings LIMIT 0",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|_| WebAuthnHttpSessionBindingPostgresError::Unavailable)?;
        Ok(())
    }

    async fn start(
        database_url: &str,
        apply_migrations: bool,
    ) -> Result<Self, WebAuthnHttpSessionBindingPostgresError> {
        let application_name = if apply_migrations {
            "cyrene-workspace-webauthn-http-binding-migration"
        } else {
            "cyrene-workspace-webauthn-http-binding-runtime"
        };
        let options = PgConnectOptions::from_str(database_url)
            .map_err(|_| WebAuthnHttpSessionBindingPostgresError::Configuration)?
            .ssl_mode(PgSslMode::VerifyFull)
            .application_name(application_name)
            .options([("statement_timeout", "5000"), ("lock_timeout", "3000")]);

        let connect = PgPoolOptions::new()
            .max_connections(MAX_CONNECTIONS)
            .acquire_timeout(DATABASE_TIMEOUT)
            .after_connect(|connection, _| {
                Box::pin(async move {
                    sqlx::query(
                        "SET search_path TO cyrene_workspace_webauthn_http_binding, pg_catalog",
                    )
                    .execute(connection)
                    .await?;
                    Ok(())
                })
            })
            .connect_with(options);
        let pool = tokio::time::timeout(STARTUP_TIMEOUT, connect)
            .await
            .map_err(|_| WebAuthnHttpSessionBindingPostgresError::Unavailable)?
            .map_err(|_| WebAuthnHttpSessionBindingPostgresError::Unavailable)?;

        if apply_migrations {
            sqlx::query("CREATE SCHEMA IF NOT EXISTS cyrene_workspace_webauthn_http_binding")
                .execute(&pool)
                .await
                .map_err(|_| WebAuthnHttpSessionBindingPostgresError::Unavailable)?;
            tokio::time::timeout(DATABASE_TIMEOUT, MIGRATOR.run(&pool))
                .await
                .map_err(|_| WebAuthnHttpSessionBindingPostgresError::Unavailable)?
                .map_err(|_| WebAuthnHttpSessionBindingPostgresError::Unavailable)?;
        } else {
            sqlx::query(
                "SELECT ceremony_id FROM cyrene_workspace_webauthn_http_binding.session_bindings LIMIT 0",
            )
            .fetch_all(&pool)
            .await
            .map_err(|_| WebAuthnHttpSessionBindingPostgresError::Unavailable)?;
        }

        Ok(Self { pool })
    }
}

#[derive(FromRow)]
struct SessionBindingRecord {
    purpose: i16,
    ceremony_id: Vec<u8>,
    owner_issuer: String,
    owner_subject: String,
    organization_id: String,
    workspace_id: String,
    session_binding_hmac: Vec<u8>,
    expires_at_unix_ms: i64,
    finish_status: i16,
    finish_response_sha256: Option<Vec<u8>>,
}

impl SessionBindingRecord {
    fn matches_session(
        &self,
        session: &VerifiedWebSessionContext,
    ) -> Result<bool, WebAuthnHttpSessionBindingError> {
        if self.session_binding_hmac.len() != 32 {
            return Err(WebAuthnHttpSessionBindingError::Unavailable);
        }
        let digest_matches = bool::from(
            self.session_binding_hmac
                .as_slice()
                .ct_eq(session.session_binding().as_bytes().as_slice()),
        );
        let principal = session.principal();
        Ok(digest_matches
            && self.owner_issuer == principal.identity().issuer
            && self.owner_subject == principal.identity().subject
            && self.organization_id == principal.organization_id())
    }

    fn validate_storage(&self) -> Result<(), WebAuthnHttpSessionBindingError> {
        if self.ceremony_id.len() != 16
            || !matches!(self.purpose, 0 | 1)
            || self.expires_at_unix_ms <= 0
            || !(0..=2).contains(&self.finish_status)
            || self
                .finish_response_sha256
                .as_ref()
                .is_some_and(|digest| digest.len() != 32)
            || !matches!(
                (self.finish_status, self.finish_response_sha256.is_some()),
                (0, false) | (1, true) | (2, true)
            )
            || self.owner_issuer.trim().is_empty()
            || self.owner_subject.trim().is_empty()
            || self.organization_id.trim().is_empty()
            || self.workspace_id.trim().is_empty()
        {
            return Err(WebAuthnHttpSessionBindingError::Unavailable);
        }
        Ok(())
    }

    fn into_binding(
        self,
        purpose: WebAuthnHttpCeremonyPurpose,
        ceremony_id: DeviceAuthorizationId,
        session: &VerifiedWebSessionContext,
    ) -> Result<WebAuthnSessionCeremonyBinding, WebAuthnHttpSessionBindingError> {
        self.validate_storage()?;
        if self.purpose != purpose_code(purpose)
            || self.ceremony_id.as_slice() != ceremony_id.as_slice()
            || !self.matches_session(session)?
        {
            return Err(WebAuthnHttpSessionBindingError::NotFoundOrExpired);
        }
        let expires_at_unix_ms = u64::try_from(self.expires_at_unix_ms)
            .map_err(|_| WebAuthnHttpSessionBindingError::Unavailable)?;
        WebAuthnSessionCeremonyBinding::from_verified_session(
            purpose,
            ceremony_id,
            session,
            self.workspace_id,
            expires_at_unix_ms,
        )
        .map_err(|_| WebAuthnHttpSessionBindingError::Unavailable)
    }
}

#[async_trait]
impl WebAuthnHttpSessionBindingStore for PostgresWebAuthnHttpSessionBindingStore {
    async fn bind_ceremony(
        &self,
        binding: WebAuthnSessionCeremonyBinding,
    ) -> Result<(), WebAuthnHttpSessionBindingError> {
        let owner = binding.owner();
        let expires_at_unix_ms = i64::try_from(binding.expires_at_unix_ms())
            .map_err(|_| WebAuthnHttpSessionBindingError::Conflict)?;
        if owner.issuer.trim().is_empty()
            || owner.issuer.len() > 4096
            || owner.subject.trim().is_empty()
            || owner.subject.len() > 4096
            || binding.organization_id().trim().is_empty()
            || binding.organization_id().len() > 4096
            || binding.workspace_id().trim().is_empty()
            || binding.workspace_id().len() > 128
            || binding.ceremony_id().iter().all(|byte| *byte == 0)
            || binding
                .session_binding()
                .as_bytes()
                .iter()
                .all(|byte| *byte == 0)
        {
            return Err(WebAuthnHttpSessionBindingError::Conflict);
        }

        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| WebAuthnHttpSessionBindingError::Unavailable)?;
        let inserted = sqlx::query(
            "INSERT INTO cyrene_workspace_webauthn_http_binding.session_bindings \
             (purpose, ceremony_id, owner_issuer, owner_subject, organization_id, workspace_id, \
              session_binding_hmac, expires_at_unix_ms) \
             SELECT $1, $2, $3, $4, $5, $6, $7, $8 \
             WHERE $8 > floor(extract(epoch FROM clock_timestamp()) * 1000)::bigint \
             ON CONFLICT (purpose, ceremony_id) DO NOTHING \
             RETURNING ceremony_id",
        )
        .bind(purpose_code(binding.purpose()))
        .bind(binding.ceremony_id().as_slice())
        .bind(&owner.issuer)
        .bind(&owner.subject)
        .bind(binding.organization_id())
        .bind(binding.workspace_id())
        .bind(binding.session_binding().as_bytes().as_slice())
        .bind(expires_at_unix_ms)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| WebAuthnHttpSessionBindingError::Unavailable)?;

        if inserted.is_none() {
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM cyrene_workspace_webauthn_http_binding.session_bindings \
                 WHERE purpose = $1 AND ceremony_id = $2)",
            )
            .bind(purpose_code(binding.purpose()))
            .bind(binding.ceremony_id().as_slice())
            .fetch_one(&mut *transaction)
            .await
            .map_err(|_| WebAuthnHttpSessionBindingError::Unavailable)?;
            transaction
                .rollback()
                .await
                .map_err(|_| WebAuthnHttpSessionBindingError::Unavailable)?;
            return if exists {
                Err(WebAuthnHttpSessionBindingError::Conflict)
            } else {
                Err(WebAuthnHttpSessionBindingError::NotFoundOrExpired)
            };
        }

        transaction
            .commit()
            .await
            .map_err(|_| WebAuthnHttpSessionBindingError::Unavailable)
    }

    async fn active_binding(
        &self,
        purpose: WebAuthnHttpCeremonyPurpose,
        ceremony_id: &DeviceAuthorizationId,
        session: &VerifiedWebSessionContext,
        _now_unix_ms: u64,
    ) -> Result<WebAuthnSessionCeremonyBinding, WebAuthnHttpSessionBindingError> {
        let row = sqlx::query_as::<_, SessionBindingRecord>(
            "SELECT purpose, ceremony_id, owner_issuer, owner_subject, organization_id, workspace_id, \
                    session_binding_hmac, expires_at_unix_ms, finish_status, finish_response_sha256 \
             FROM cyrene_workspace_webauthn_http_binding.session_bindings \
             WHERE purpose = $1 AND ceremony_id = $2 \
               AND expires_at_unix_ms > floor(extract(epoch FROM clock_timestamp()) * 1000)::bigint",
        )
        .bind(purpose_code(purpose))
        .bind(ceremony_id.as_slice())
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| WebAuthnHttpSessionBindingError::Unavailable)?
        .ok_or(WebAuthnHttpSessionBindingError::NotFoundOrExpired)?;
        row.into_binding(purpose, *ceremony_id, session)
    }

    async fn reserve_finish(
        &self,
        purpose: WebAuthnHttpCeremonyPurpose,
        ceremony_id: &DeviceAuthorizationId,
        session: &VerifiedWebSessionContext,
        response_sha256: [u8; 32],
        _now_unix_ms: u64,
    ) -> Result<WebAuthnSessionFinishReservation, WebAuthnHttpSessionBindingError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| WebAuthnHttpSessionBindingError::Unavailable)?;
        let row = load_active_for_update(&mut transaction, purpose, ceremony_id).await?;
        if !row.matches_session(session)? {
            transaction
                .rollback()
                .await
                .map_err(|_| WebAuthnHttpSessionBindingError::Unavailable)?;
            return Err(WebAuthnHttpSessionBindingError::NotFoundOrExpired);
        }

        let result = match row.finish_status {
            0 => {
                if row.finish_response_sha256.is_some() {
                    return Err(WebAuthnHttpSessionBindingError::Unavailable);
                }
                let rows = sqlx::query(
                    "UPDATE cyrene_workspace_webauthn_http_binding.session_bindings \
                     SET finish_status = 1, finish_response_sha256 = $3 \
                     WHERE purpose = $1 AND ceremony_id = $2 AND finish_status = 0 \
                       AND expires_at_unix_ms > floor(extract(epoch FROM clock_timestamp()) * 1000)::bigint",
                )
                .bind(purpose_code(purpose))
                .bind(ceremony_id.as_slice())
                .bind(response_sha256.as_slice())
                .execute(&mut *transaction)
                .await
                .map_err(|_| WebAuthnHttpSessionBindingError::Unavailable)?
                .rows_affected();
                if rows != 1 {
                    transaction
                        .rollback()
                        .await
                        .map_err(|_| WebAuthnHttpSessionBindingError::Unavailable)?;
                    return Err(WebAuthnHttpSessionBindingError::NotFoundOrExpired);
                }
                WebAuthnSessionFinishReservation::Continue
            }
            1 => {
                if digest_matches(row.finish_response_sha256.as_deref(), &response_sha256)? {
                    WebAuthnSessionFinishReservation::Continue
                } else {
                    transaction
                        .rollback()
                        .await
                        .map_err(|_| WebAuthnHttpSessionBindingError::Unavailable)?;
                    return Err(WebAuthnHttpSessionBindingError::Conflict);
                }
            }
            2 => {
                if digest_matches(row.finish_response_sha256.as_deref(), &response_sha256)? {
                    WebAuthnSessionFinishReservation::AlreadyComplete
                } else {
                    transaction
                        .rollback()
                        .await
                        .map_err(|_| WebAuthnHttpSessionBindingError::Unavailable)?;
                    return Err(WebAuthnHttpSessionBindingError::Conflict);
                }
            }
            _ => return Err(WebAuthnHttpSessionBindingError::Unavailable),
        };
        transaction
            .commit()
            .await
            .map_err(|_| WebAuthnHttpSessionBindingError::Unavailable)?;
        Ok(result)
    }

    async fn complete_finish(
        &self,
        purpose: WebAuthnHttpCeremonyPurpose,
        ceremony_id: &DeviceAuthorizationId,
        session: &VerifiedWebSessionContext,
        response_sha256: [u8; 32],
        _now_unix_ms: u64,
    ) -> Result<(), WebAuthnHttpSessionBindingError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| WebAuthnHttpSessionBindingError::Unavailable)?;
        let row = load_active_for_update(&mut transaction, purpose, ceremony_id).await?;
        if !row.matches_session(session)? {
            transaction
                .rollback()
                .await
                .map_err(|_| WebAuthnHttpSessionBindingError::Unavailable)?;
            return Err(WebAuthnHttpSessionBindingError::NotFoundOrExpired);
        }

        match row.finish_status {
            1 => {
                if !digest_matches(row.finish_response_sha256.as_deref(), &response_sha256)? {
                    transaction
                        .rollback()
                        .await
                        .map_err(|_| WebAuthnHttpSessionBindingError::Unavailable)?;
                    return Err(WebAuthnHttpSessionBindingError::Conflict);
                }
                let rows = sqlx::query(
                    "UPDATE cyrene_workspace_webauthn_http_binding.session_bindings \
                     SET finish_status = 2 \
                     WHERE purpose = $1 AND ceremony_id = $2 AND finish_status = 1 \
                       AND expires_at_unix_ms > floor(extract(epoch FROM clock_timestamp()) * 1000)::bigint",
                )
                .bind(purpose_code(purpose))
                .bind(ceremony_id.as_slice())
                .execute(&mut *transaction)
                .await
                .map_err(|_| WebAuthnHttpSessionBindingError::Unavailable)?
                .rows_affected();
                if rows != 1 {
                    transaction
                        .rollback()
                        .await
                        .map_err(|_| WebAuthnHttpSessionBindingError::Unavailable)?;
                    return Err(WebAuthnHttpSessionBindingError::NotFoundOrExpired);
                }
            }
            2 => {
                if !digest_matches(row.finish_response_sha256.as_deref(), &response_sha256)? {
                    transaction
                        .rollback()
                        .await
                        .map_err(|_| WebAuthnHttpSessionBindingError::Unavailable)?;
                    return Err(WebAuthnHttpSessionBindingError::Conflict);
                }
            }
            0 => {
                transaction
                    .rollback()
                    .await
                    .map_err(|_| WebAuthnHttpSessionBindingError::Unavailable)?;
                return Err(WebAuthnHttpSessionBindingError::Conflict);
            }
            _ => return Err(WebAuthnHttpSessionBindingError::Unavailable),
        }

        transaction
            .commit()
            .await
            .map_err(|_| WebAuthnHttpSessionBindingError::Unavailable)
    }
}

async fn load_active_for_update(
    transaction: &mut Transaction<'_, Postgres>,
    purpose: WebAuthnHttpCeremonyPurpose,
    ceremony_id: &DeviceAuthorizationId,
) -> Result<SessionBindingRecord, WebAuthnHttpSessionBindingError> {
    let row = sqlx::query_as::<_, SessionBindingRecord>(
        "SELECT purpose, ceremony_id, owner_issuer, owner_subject, organization_id, workspace_id, \
                session_binding_hmac, expires_at_unix_ms, finish_status, finish_response_sha256 \
         FROM cyrene_workspace_webauthn_http_binding.session_bindings \
         WHERE purpose = $1 AND ceremony_id = $2 \
           AND expires_at_unix_ms > floor(extract(epoch FROM clock_timestamp()) * 1000)::bigint \
         FOR UPDATE",
    )
    .bind(purpose_code(purpose))
    .bind(ceremony_id.as_slice())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|_| WebAuthnHttpSessionBindingError::Unavailable)?
    .ok_or(WebAuthnHttpSessionBindingError::NotFoundOrExpired)?;
    row.validate_storage()?;
    Ok(row)
}

fn digest_matches(
    stored: Option<&[u8]>,
    expected: &[u8; 32],
) -> Result<bool, WebAuthnHttpSessionBindingError> {
    let stored = stored.ok_or(WebAuthnHttpSessionBindingError::Unavailable)?;
    if stored.len() != 32 {
        return Err(WebAuthnHttpSessionBindingError::Unavailable);
    }
    Ok(bool::from(stored.ct_eq(expected.as_slice())))
}

const fn purpose_code(purpose: WebAuthnHttpCeremonyPurpose) -> i16 {
    match purpose {
        WebAuthnHttpCeremonyPurpose::CredentialRegistration => 0,
        WebAuthnHttpCeremonyPurpose::DeviceApproval => 1,
    }
}
