//! PostgreSQL-backed shared reservation for low-entropy user-code attempts.
//!
//! The limiter stores only a caller-derived 32-byte abuse digest. A short
//! transaction uses PostgreSQL time and a transaction advisory lock so all
//! service instances observe one fixed-window counter and the same bounded
//! number of active digest rows. SQLx remains asynchronous; callers must not
//! bridge this API with `block_on` from a Tokio worker.
//!
//! 模块只持久化服务端派生的摘要，并对并发数据库请求设置有界准入。

use std::str::FromStr;
use std::time::Duration;

use sqlx::migrate::Migrator;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};
use sqlx::PgPool;
use thiserror::Error;
use tokio::sync::Semaphore;

use crate::device_authorization::DeviceAuthorizationRateLimitError;

const DATABASE_URL_ENV: &str = "CYRENE_WORKSPACE_DEVICE_AUTHORIZATION_DATABASE_URL";
const MIGRATION_DATABASE_URL_ENV: &str =
    "CYRENE_WORKSPACE_DEVICE_AUTHORIZATION_MIGRATION_DATABASE_URL";
const DATABASE_TIMEOUT: Duration = Duration::from_secs(5);
const STARTUP_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_CONNECTIONS: u32 = 4;
const MAX_IN_FLIGHT_RESERVATIONS: usize = 32;
const MAX_ACTIVE_KEYS: i64 = 4_096;
const ADVISORY_LOCK_NAMESPACE: i32 = 0x4359;
const ADVISORY_LOCK_ID: i32 = 0x5543;
const DELETE_EXPIRED_SQL: &str = "DELETE FROM \
    cyrene_workspace_device_authorization.user_code_attempt_windows \
    WHERE abuse_key <> $2 \
      AND window_started_at_unix_ms <= $1 \
      AND window_duration_ms <= $1 - window_started_at_unix_ms";
const COUNT_ACTIVE_SQL: &str = "SELECT COUNT(*)::BIGINT FROM \
    cyrene_workspace_device_authorization.user_code_attempt_windows";

static MIGRATOR: Migrator = sqlx::migrate!("./migrations/device_authorization");

/// Stable startup/configuration errors for the shared attempt reservation.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum PostgresUserCodeAttemptReservationError {
    /// The trusted database URL is missing or malformed.
    #[error("user-code attempt limiter database configuration is invalid")]
    Configuration,
    /// PostgreSQL, schema verification, or migration is unavailable.
    #[error("user-code attempt limiter database is unavailable")]
    Unavailable,
}

/// Shared fixed-window attempt reservation backed by the device-authorization database.
///
/// This is an asynchronous adapter intended for the enrollment composite to
/// reserve an attempt before invoking the synchronous state-machine port with
/// a request-scoped one-shot limiter. It is not itself a
/// [`crate::device_authorization::UserCodeAttemptLimiter`] implementation.
pub struct PostgresUserCodeAttemptReservation {
    pool: PgPool,
    reservation_slots: Semaphore,
}

impl PostgresUserCodeAttemptReservation {
    /// Connect using a trusted runtime URL and validate the installed schema.
    pub async fn connect(
        database_url: &str,
    ) -> Result<Self, PostgresUserCodeAttemptReservationError> {
        Self::start(database_url, false).await
    }

    /// Connect using trusted server-side runtime configuration.
    pub async fn connect_from_environment() -> Result<Self, PostgresUserCodeAttemptReservationError>
    {
        let database_url = std::env::var(DATABASE_URL_ENV)
            .map_err(|_| PostgresUserCodeAttemptReservationError::Configuration)?;
        Self::connect(&database_url).await
    }

    /// Apply the authorization migrations with a separately provisioned operator URL.
    pub async fn migrate(
        database_url: &str,
    ) -> Result<(), PostgresUserCodeAttemptReservationError> {
        let store = Self::start(database_url, true).await?;
        store.pool.close().await;
        Ok(())
    }

    /// Apply migrations using the dedicated operator environment variable.
    pub async fn migrate_from_environment() -> Result<(), PostgresUserCodeAttemptReservationError> {
        let database_url = std::env::var(MIGRATION_DATABASE_URL_ENV)
            .map_err(|_| PostgresUserCodeAttemptReservationError::Configuration)?;
        Self::migrate(&database_url).await
    }

    /// Reserve one attempt for the caller-derived abuse digest.
    ///
    /// PostgreSQL samples `clock_timestamp()` after acquiring the transaction
    /// lock. `Ok(true)` means the attempt was recorded and is allowed;
    /// `Ok(false)` means the window has reached its limit. Storage errors,
    /// invalid bounds, and timeout fail closed as `Unavailable`.
    pub async fn reserve_attempt(
        &self,
        abuse_key: &[u8; 32],
        window_ms: u64,
        maximum_attempts: u32,
    ) -> Result<bool, DeviceAuthorizationRateLimitError> {
        if window_ms == 0 || maximum_attempts == 0 {
            return Err(DeviceAuthorizationRateLimitError::Unavailable);
        }
        let window_duration_ms =
            i64::try_from(window_ms).map_err(|_| DeviceAuthorizationRateLimitError::Unavailable)?;

        let _reservation_slot = self
            .reservation_slots
            .try_acquire()
            .map_err(|_| DeviceAuthorizationRateLimitError::Unavailable)?;
        tokio::time::timeout(
            DATABASE_TIMEOUT,
            self.reserve_attempt_inner(abuse_key, window_ms, window_duration_ms, maximum_attempts),
        )
        .await
        .map_err(|_| DeviceAuthorizationRateLimitError::Unavailable)?
    }

    async fn start(
        database_url: &str,
        apply_migrations: bool,
    ) -> Result<Self, PostgresUserCodeAttemptReservationError> {
        let application_name = if apply_migrations {
            "cyrene-workspace-user-code-attempt-limiter-migration"
        } else {
            "cyrene-workspace-user-code-attempt-limiter-runtime"
        };
        let options = PgConnectOptions::from_str(database_url)
            .map_err(|_| PostgresUserCodeAttemptReservationError::Configuration)?
            .ssl_mode(PgSslMode::VerifyFull)
            .application_name(application_name)
            .options([("statement_timeout", "5000"), ("lock_timeout", "3000")]);

        let connect = PgPoolOptions::new()
            .max_connections(MAX_CONNECTIONS)
            .acquire_timeout(DATABASE_TIMEOUT)
            .after_connect(|connection, _| {
                Box::pin(async move {
                    sqlx::query(
                        "SET search_path TO cyrene_workspace_device_authorization, pg_catalog",
                    )
                    .execute(connection)
                    .await?;
                    Ok(())
                })
            })
            .connect_with(options);
        let pool = tokio::time::timeout(STARTUP_TIMEOUT, connect)
            .await
            .map_err(|_| PostgresUserCodeAttemptReservationError::Unavailable)?
            .map_err(|_| PostgresUserCodeAttemptReservationError::Unavailable)?;

        if apply_migrations {
            sqlx::query("CREATE SCHEMA IF NOT EXISTS cyrene_workspace_device_authorization")
                .execute(&pool)
                .await
                .map_err(|_| PostgresUserCodeAttemptReservationError::Unavailable)?;
            tokio::time::timeout(DATABASE_TIMEOUT, MIGRATOR.run(&pool))
                .await
                .map_err(|_| PostgresUserCodeAttemptReservationError::Unavailable)?
                .map_err(|_| PostgresUserCodeAttemptReservationError::Unavailable)?;
        } else {
            sqlx::query(
                "SELECT abuse_key \
                 FROM cyrene_workspace_device_authorization.user_code_attempt_windows LIMIT 0",
            )
            .fetch_all(&pool)
            .await
            .map_err(|_| PostgresUserCodeAttemptReservationError::Unavailable)?;
        }

        Ok(Self {
            pool,
            reservation_slots: Semaphore::new(MAX_IN_FLIGHT_RESERVATIONS),
        })
    }

    async fn reserve_attempt_inner(
        &self,
        abuse_key: &[u8; 32],
        window_ms: u64,
        window_duration_ms: i64,
        maximum_attempts: u32,
    ) -> Result<bool, DeviceAuthorizationRateLimitError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| DeviceAuthorizationRateLimitError::Unavailable)?;

        // Serialize cleanup and capacity checks as well as same-key updates.
        // This mirrors the in-process limiter's bounded critical section and
        // makes the global 4,096-key ceiling race-free across replicas.
        sqlx::query("SELECT pg_advisory_xact_lock($1, $2)")
            .bind(ADVISORY_LOCK_NAMESPACE)
            .bind(ADVISORY_LOCK_ID)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|_| DeviceAuthorizationRateLimitError::Unavailable)?;

        let database_now_unix_ms_i64: i64 = sqlx::query_scalar(
            "SELECT FLOOR(EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT",
        )
        .fetch_one(&mut *transaction)
        .await
        .map_err(|_| DeviceAuthorizationRateLimitError::Unavailable)?;
        let database_now_unix_ms = u64::try_from(database_now_unix_ms_i64)
            .map_err(|_| DeviceAuthorizationRateLimitError::Unavailable)?;

        // Check the target row before cleanup so a live digest cannot be reset
        // or relaxed under a different service policy.
        let existing = sqlx::query_as::<_, (i64, i64, i64, i64)>(
            "SELECT window_started_at_unix_ms, attempts, window_duration_ms, maximum_attempts \
             FROM cyrene_workspace_device_authorization.user_code_attempt_windows \
             WHERE abuse_key = $1 FOR UPDATE",
        )
        .bind(abuse_key.as_slice())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| DeviceAuthorizationRateLimitError::Unavailable)?;

        let target_policy_matches =
            if let Some((window_started_at, _, stored_window_ms, stored_maximum_attempts)) =
                &existing
            {
                if database_now_unix_ms_i64 < *window_started_at {
                    return Err(DeviceAuthorizationRateLimitError::ClockMovedBackwards);
                }
                let stored_elapsed_ms = database_now_unix_ms_i64 - *window_started_at;
                let policy_matches = *stored_window_ms == window_duration_ms
                    && *stored_maximum_attempts == i64::from(maximum_attempts);
                if !policy_matches && stored_elapsed_ms < *stored_window_ms {
                    return Err(DeviceAuthorizationRateLimitError::Unavailable);
                }
                policy_matches
            } else {
                true
            };

        // Prune other keys using each row's own persisted window. Keep the
        // current row so an exact-policy expired window can reset in place.
        sqlx::query(DELETE_EXPIRED_SQL)
            .bind(database_now_unix_ms_i64)
            .bind(abuse_key.as_slice())
            .execute(&mut *transaction)
            .await
            .map_err(|_| DeviceAuthorizationRateLimitError::Unavailable)?;

        let allowed = match existing {
            Some((window_started_at, attempts, _, _)) => {
                let window_started_at = u64::try_from(window_started_at)
                    .map_err(|_| DeviceAuthorizationRateLimitError::Unavailable)?;
                let attempts = u32::try_from(attempts)
                    .map_err(|_| DeviceAuthorizationRateLimitError::Unavailable)?;
                let elapsed = database_now_unix_ms
                    .checked_sub(window_started_at)
                    .ok_or(DeviceAuthorizationRateLimitError::ClockMovedBackwards)?;

                if !target_policy_matches || elapsed >= window_ms {
                    sqlx::query(
                        "UPDATE cyrene_workspace_device_authorization.user_code_attempt_windows \
                         SET window_started_at_unix_ms = $2, attempts = 1, \
                             window_duration_ms = $3, maximum_attempts = $4 \
                         WHERE abuse_key = $1",
                    )
                    .bind(abuse_key.as_slice())
                    .bind(database_now_unix_ms_i64)
                    .bind(window_duration_ms)
                    .bind(i64::from(maximum_attempts))
                    .execute(&mut *transaction)
                    .await
                    .map_err(|_| DeviceAuthorizationRateLimitError::Unavailable)?;
                    true
                } else {
                    if attempts >= maximum_attempts {
                        false
                    } else {
                        let next_attempts = i64::from(attempts.saturating_add(1));
                        sqlx::query(
                            "UPDATE cyrene_workspace_device_authorization.user_code_attempt_windows \
                             SET attempts = $2 \
                             WHERE abuse_key = $1",
                        )
                        .bind(abuse_key.as_slice())
                        .bind(next_attempts)
                        .execute(&mut *transaction)
                        .await
                        .map_err(|_| DeviceAuthorizationRateLimitError::Unavailable)?;
                        true
                    }
                }
            }
            None => {
                let active_keys: i64 = sqlx::query_scalar(COUNT_ACTIVE_SQL)
                    .fetch_one(&mut *transaction)
                    .await
                    .map_err(|_| DeviceAuthorizationRateLimitError::Unavailable)?;
                if active_keys >= MAX_ACTIVE_KEYS {
                    return Err(DeviceAuthorizationRateLimitError::Unavailable);
                }
                sqlx::query(
                    "INSERT INTO cyrene_workspace_device_authorization.user_code_attempt_windows \
                     (abuse_key, window_started_at_unix_ms, attempts, \
                      window_duration_ms, maximum_attempts) \
                     VALUES ($1, $2, 1, $3, $4)",
                )
                .bind(abuse_key.as_slice())
                .bind(database_now_unix_ms_i64)
                .bind(window_duration_ms)
                .bind(i64::from(maximum_attempts))
                .execute(&mut *transaction)
                .await
                .map_err(|_| DeviceAuthorizationRateLimitError::Unavailable)?;
                true
            }
        };

        transaction
            .commit()
            .await
            .map_err(|_| DeviceAuthorizationRateLimitError::Unavailable)?;
        Ok(allowed)
    }
}
