//! PostgreSQL persistence for Workspace device authorization records.
//!
//! The public state-machine port is synchronous, so this adapter confines SQLx
//! to one Tokio runtime on a dedicated thread. Requests cross a bounded queue;
//! queue pressure and all storage failures fail closed as `Unavailable`.

use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use cy_proto::workspace_v1::UserIdentityRef;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::migrate::Migrator;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgRow, PgSslMode};
use sqlx::query_builder::QueryBuilder;
use sqlx::{PgPool, Postgres, Row};
use thiserror::Error;

use crate::device_authorization::{
    DeviceAuthorizationCodeHash, DeviceAuthorizationId, DeviceAuthorizationRecord,
    DeviceAuthorizationScope, DeviceAuthorizationState, DeviceAuthorizationStore,
    DeviceAuthorizationStoreError, IssuedDeviceCertificate,
};
use crate::VersionedUserCodeDigest;

const DATABASE_URL_ENV: &str = "CYRENE_WORKSPACE_DEVICE_AUTHORIZATION_DATABASE_URL";
const MIGRATION_DATABASE_URL_ENV: &str =
    "CYRENE_WORKSPACE_DEVICE_AUTHORIZATION_MIGRATION_DATABASE_URL";
const TABLE: &str = "cyrene_workspace_device_authorization.authorizations";
const QUEUE_CAPACITY: usize = 32;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);
const DATABASE_TIMEOUT: Duration = Duration::from_secs(5);
const STARTUP_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_SCAN_LIMIT: usize = 1_000;
const MAX_CSR_BYTES: usize = 16 * 1024;
const MAX_WEBAUTHN_STATE_BYTES: usize = 64 * 1024;
const MAX_REQUEST_OPTIONS_BYTES: usize = 1024 * 1024;
const MAX_CERTIFICATE_BYTES: usize = 4 * 1024 * 1024;
const MAX_SERIAL_NUMBER_BYTES: usize = 256;
const MAX_STATE_PAYLOAD_BYTES: usize = 32 * 1024 * 1024;

static MIGRATOR: Migrator = sqlx::migrate!("./migrations/device_authorization");

const SELECT_COLUMNS: &str =
    "id, device_code_hash, user_code_key_version, user_code_mac, organization_id, workspace_id, \
     csr_der, csr_sha256, spki_sha256, created_at_unix_ms, expires_at_unix_ms, \
     poll_interval_ms, last_poll_at_unix_ms, revision, state_kind, approval_id, state_payload";

type StoreResult<T> = Result<T, DeviceAuthorizationStoreError>;
type StoreReply<T> = SyncSender<StoreResult<T>>;

/// Configuration/startup errors from the PostgreSQL adapter.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum DeviceAuthorizationPostgresError {
    /// The connection URL is missing or malformed.
    #[error("device authorization database configuration is invalid")]
    Configuration,
    /// The database could not be reached, migrated, or validated.
    #[error("device authorization database is unavailable")]
    Unavailable,
}

/// PostgreSQL implementation of [`DeviceAuthorizationStore`].
///
/// The URL must come from trusted server configuration. TLS certificate and
/// hostname verification are mandatory. The adapter persists only the
/// versioned user-code HMAC and never accepts or stores a raw device/user code.
pub struct PostgresDeviceAuthorizationStore {
    sender: Option<SyncSender<Command>>,
    shutdown: Arc<AtomicBool>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl PostgresDeviceAuthorizationStore {
    /// Connect using a trusted database URL and verify the installed schema.
    ///
    /// Startup errors contain no URL, password, or database-provided text.
    pub fn connect(database_url: &str) -> Result<Self, DeviceAuthorizationPostgresError> {
        Self::start(database_url, false)
    }

    /// Read the application database URL from server environment configuration.
    pub fn connect_from_environment() -> Result<Self, DeviceAuthorizationPostgresError> {
        let database_url = std::env::var(DATABASE_URL_ENV)
            .map_err(|_| DeviceAuthorizationPostgresError::Configuration)?;
        Self::connect(&database_url)
    }

    /// Apply this adapter's migrations using a separately provisioned migrator URL.
    ///
    /// This must be run as a deployment/operator action, not with the runtime
    /// application credential. Migration failures return a stable error.
    pub fn migrate(database_url: &str) -> Result<(), DeviceAuthorizationPostgresError> {
        let store = Self::start(database_url, true)?;
        drop(store);
        Ok(())
    }

    /// Apply migrations using the dedicated operator environment variable.
    pub fn migrate_from_environment() -> Result<(), DeviceAuthorizationPostgresError> {
        let database_url = std::env::var(MIGRATION_DATABASE_URL_ENV)
            .map_err(|_| DeviceAuthorizationPostgresError::Configuration)?;
        Self::migrate(&database_url)
    }

    /// Return a bounded page of records whose current state must expire.
    ///
    /// Issuing records are intentionally excluded: the CA may already have
    /// committed, so they must be recovered with the enrollment ID as the CA's
    /// idempotency key. This extension supports a sweeper without changing the
    /// current state-machine port.
    pub fn expired_records(
        &self,
        now_unix_ms: u64,
        limit: usize,
    ) -> StoreResult<Vec<DeviceAuthorizationRecord>> {
        let (reply, result) = mpsc::sync_channel(1);
        self.call(Command::ExpiredRecords(now_unix_ms, limit, reply), result)
    }

    /// Return a bounded page of durable issuance reservations for recovery.
    pub fn recoverable_issuances(
        &self,
        limit: usize,
    ) -> StoreResult<Vec<DeviceAuthorizationRecord>> {
        let (reply, result) = mpsc::sync_channel(1);
        self.call(Command::RecoverableIssuances(limit, reply), result)
    }

    fn start(
        database_url: &str,
        apply_migrations: bool,
    ) -> Result<Self, DeviceAuthorizationPostgresError> {
        let options = PgConnectOptions::from_str(database_url)
            .map_err(|_| DeviceAuthorizationPostgresError::Configuration)?
            .ssl_mode(PgSslMode::VerifyFull)
            .application_name("cyrene-workspace-device-authorization")
            .options([("statement_timeout", "5000"), ("lock_timeout", "3000")]);

        let (sender, commands) = mpsc::sync_channel(QUEUE_CAPACITY);
        let (ready_sender, ready) = mpsc::sync_channel(1);
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_shutdown = Arc::clone(&shutdown);
        let worker = thread::Builder::new()
            .name("workspace-device-auth-postgres".to_owned())
            .spawn(move || {
                worker_main(
                    options,
                    commands,
                    ready_sender,
                    apply_migrations,
                    worker_shutdown,
                )
            })
            .map_err(|_| DeviceAuthorizationPostgresError::Unavailable)?;

        match ready.recv_timeout(STARTUP_TIMEOUT) {
            Ok(Ok(())) => Ok(Self {
                sender: Some(sender),
                shutdown,
                worker: Mutex::new(Some(worker)),
            }),
            Ok(Err(error)) => {
                drop(sender);
                let _ = worker.join();
                Err(error)
            }
            Err(_) => {
                drop(sender);
                let _ = worker.join();
                Err(DeviceAuthorizationPostgresError::Unavailable)
            }
        }
    }

    fn call<T>(&self, command: Command, reply: Receiver<StoreResult<T>>) -> StoreResult<T> {
        let sender = self
            .sender
            .as_ref()
            .ok_or(DeviceAuthorizationStoreError::Unavailable)?;
        match sender.try_send(command) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                return Err(DeviceAuthorizationStoreError::Unavailable)
            }
        }
        reply
            .recv_timeout(REQUEST_TIMEOUT)
            .map_err(|_| DeviceAuthorizationStoreError::Unavailable)?
    }
}

impl Drop for PostgresDeviceAuthorizationStore {
    fn drop(&mut self) {
        // Closing the bounded sender lets the dedicated worker finish its
        // current statement and exit. SQL statements have a server timeout.
        self.shutdown.store(true, Ordering::Release);
        self.sender.take();
        if let Ok(mut worker) = self.worker.lock() {
            if let Some(worker) = worker.take() {
                let _ = worker.join();
            }
        }
    }
}

impl DeviceAuthorizationStore for PostgresDeviceAuthorizationStore {
    fn insert(&self, record: DeviceAuthorizationRecord) -> StoreResult<()> {
        let (reply, result) = mpsc::sync_channel(1);
        self.call(Command::Insert(record, reply), result)
    }

    fn by_device_code_hash(
        &self,
        code_hash: &DeviceAuthorizationCodeHash,
    ) -> StoreResult<Option<DeviceAuthorizationRecord>> {
        let (reply, result) = mpsc::sync_channel(1);
        self.call(Command::ByDeviceCodeHash(*code_hash, reply), result)
    }

    fn by_user_code_candidates(
        &self,
        candidates: &[VersionedUserCodeDigest],
    ) -> StoreResult<Option<DeviceAuthorizationRecord>> {
        let (reply, result) = mpsc::sync_channel(1);
        self.call(
            Command::ByUserCodeCandidates(candidates.to_vec(), reply),
            result,
        )
    }

    fn user_code_key_versions(&self) -> StoreResult<Vec<u32>> {
        let (reply, result) = mpsc::sync_channel(1);
        self.call(Command::UserCodeKeyVersions(reply), result)
    }

    fn by_approval_id(
        &self,
        approval_id: &DeviceAuthorizationId,
    ) -> StoreResult<Option<DeviceAuthorizationRecord>> {
        let (reply, result) = mpsc::sync_channel(1);
        self.call(Command::ByApprovalId(*approval_id, reply), result)
    }

    fn compare_and_swap(
        &self,
        expected_revision: u64,
        replacement: DeviceAuthorizationRecord,
    ) -> StoreResult<()> {
        let (reply, result) = mpsc::sync_channel(1);
        self.call(
            Command::CompareAndSwap(expected_revision, replacement, reply),
            result,
        )
    }
}

enum Command {
    Insert(DeviceAuthorizationRecord, StoreReply<()>),
    ByDeviceCodeHash(
        DeviceAuthorizationCodeHash,
        StoreReply<Option<DeviceAuthorizationRecord>>,
    ),
    ByUserCodeCandidates(
        Vec<VersionedUserCodeDigest>,
        StoreReply<Option<DeviceAuthorizationRecord>>,
    ),
    UserCodeKeyVersions(StoreReply<Vec<u32>>),
    ByApprovalId(
        DeviceAuthorizationId,
        StoreReply<Option<DeviceAuthorizationRecord>>,
    ),
    CompareAndSwap(u64, DeviceAuthorizationRecord, StoreReply<()>),
    ExpiredRecords(u64, usize, StoreReply<Vec<DeviceAuthorizationRecord>>),
    RecoverableIssuances(usize, StoreReply<Vec<DeviceAuthorizationRecord>>),
}

fn worker_main(
    options: PgConnectOptions,
    commands: Receiver<Command>,
    ready: SyncSender<Result<(), DeviceAuthorizationPostgresError>>,
    apply_migrations: bool,
    shutdown: Arc<AtomicBool>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => {
            let _ = ready.send(Err(DeviceAuthorizationPostgresError::Unavailable));
            return;
        }
    };
    let pool = match runtime.block_on(initialize_pool(options, apply_migrations)) {
        Ok(pool) => pool,
        Err(_) => {
            let _ = ready.send(Err(DeviceAuthorizationPostgresError::Unavailable));
            return;
        }
    };
    if ready.send(Ok(())).is_err() {
        return;
    }

    while !shutdown.load(Ordering::Acquire) {
        let command = match commands.recv_timeout(Duration::from_millis(100)) {
            Ok(command) => command,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        if shutdown.load(Ordering::Acquire) {
            break;
        }
        match command {
            Command::Insert(record, reply) => {
                let _ = reply.send(runtime.block_on(insert_record(&pool, &record)));
            }
            Command::ByDeviceCodeHash(hash, reply) => {
                let _ = reply.send(runtime.block_on(by_device_code_hash(&pool, &hash)));
            }
            Command::ByUserCodeCandidates(candidates, reply) => {
                let _ = reply.send(runtime.block_on(by_user_code_candidates(&pool, &candidates)));
            }
            Command::UserCodeKeyVersions(reply) => {
                let _ = reply.send(runtime.block_on(user_code_key_versions(&pool)));
            }
            Command::ByApprovalId(approval_id, reply) => {
                let _ = reply.send(runtime.block_on(by_approval_id(&pool, &approval_id)));
            }
            Command::CompareAndSwap(expected, replacement, reply) => {
                let _ =
                    reply.send(runtime.block_on(compare_and_swap(&pool, expected, &replacement)));
            }
            Command::ExpiredRecords(now, limit, reply) => {
                let _ = reply.send(runtime.block_on(expired_records(&pool, now, limit)));
            }
            Command::RecoverableIssuances(limit, reply) => {
                let _ = reply.send(runtime.block_on(recoverable_issuances(&pool, limit)));
            }
        }
    }
}

async fn initialize_pool(options: PgConnectOptions, apply_migrations: bool) -> StoreResult<PgPool> {
    let connect = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(DATABASE_TIMEOUT)
        .after_connect(|connection, _| {
            Box::pin(async move {
                sqlx::query("SET search_path TO cyrene_workspace_device_authorization, pg_catalog")
                    .execute(connection)
                    .await?;
                Ok(())
            })
        })
        .connect_with(options);
    let pool = tokio::time::timeout(DATABASE_TIMEOUT, connect)
        .await
        .map_err(|_| DeviceAuthorizationStoreError::Unavailable)?
        .map_err(|_| DeviceAuthorizationStoreError::Unavailable)?;

    if apply_migrations {
        sqlx::query("CREATE SCHEMA IF NOT EXISTS cyrene_workspace_device_authorization")
            .execute(&pool)
            .await
            .map_err(|_| DeviceAuthorizationStoreError::Unavailable)?;
        tokio::time::timeout(DATABASE_TIMEOUT, MIGRATOR.run(&pool))
            .await
            .map_err(|_| DeviceAuthorizationStoreError::Unavailable)?
            .map_err(|_| DeviceAuthorizationStoreError::Unavailable)?;
    } else {
        sqlx::query("SELECT id FROM cyrene_workspace_device_authorization.authorizations LIMIT 0")
            .fetch_all(&pool)
            .await
            .map_err(|_| DeviceAuthorizationStoreError::Unavailable)?;
    }
    Ok(pool)
}

async fn insert_record(pool: &PgPool, record: &DeviceAuthorizationRecord) -> StoreResult<()> {
    let encoded = EncodedRecord::new(record)?;
    let result = sqlx::query(&format!(
        "INSERT INTO {TABLE} ({SELECT_COLUMNS}) VALUES \
         ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)"
    ))
    .bind(encoded.id)
    .bind(encoded.device_code_hash)
    .bind(encoded.user_code_key_version)
    .bind(encoded.user_code_mac)
    .bind(encoded.organization_id)
    .bind(encoded.workspace_id)
    .bind(encoded.csr_der)
    .bind(encoded.csr_sha256)
    .bind(encoded.spki_sha256)
    .bind(encoded.created_at_unix_ms)
    .bind(encoded.expires_at_unix_ms)
    .bind(encoded.poll_interval_ms)
    .bind(encoded.last_poll_at_unix_ms)
    .bind(encoded.revision)
    .bind(encoded.state_kind)
    .bind(encoded.approval_id)
    .bind(encoded.state_payload)
    .execute(pool)
    .await;
    result.map(|_| ()).map_err(map_database_error)
}

async fn by_device_code_hash(
    pool: &PgPool,
    hash: &DeviceAuthorizationCodeHash,
) -> StoreResult<Option<DeviceAuthorizationRecord>> {
    let row = sqlx::query(&format!(
        "SELECT {SELECT_COLUMNS} FROM {TABLE} WHERE device_code_hash = $1"
    ))
    .bind(hash.to_vec())
    .fetch_optional(pool)
    .await
    .map_err(map_database_error)?;
    row.map(decode_record).transpose()
}

async fn by_user_code_candidates(
    pool: &PgPool,
    candidates: &[VersionedUserCodeDigest],
) -> StoreResult<Option<DeviceAuthorizationRecord>> {
    if candidates.is_empty() {
        return Ok(None);
    }
    if candidates.len() > crate::MAX_USER_CODE_KEY_VERSIONS {
        return Err(DeviceAuthorizationStoreError::Unavailable);
    }

    let mut query =
        QueryBuilder::<Postgres>::new(format!("SELECT {SELECT_COLUMNS} FROM {TABLE} WHERE "));
    {
        let mut clauses = query.separated(" OR ");
        for candidate in candidates {
            let (version, mac) = candidate.storage_parts();
            clauses
                .push("(user_code_key_version = ")
                .push_bind(i64::from(version))
                .push(" AND user_code_mac = ")
                .push_bind(mac.to_vec())
                .push(")");
        }
    }
    query.push(" LIMIT 2");
    let rows = query
        .build()
        .fetch_all(pool)
        .await
        .map_err(map_database_error)?;
    if rows.len() > 1 {
        return Err(DeviceAuthorizationStoreError::Unavailable);
    }
    rows.into_iter().next().map(decode_record).transpose()
}

async fn user_code_key_versions(pool: &PgPool) -> StoreResult<Vec<u32>> {
    let rows = sqlx::query(&format!(
        "SELECT DISTINCT user_code_key_version FROM {TABLE} ORDER BY user_code_key_version"
    ))
    .fetch_all(pool)
    .await
    .map_err(map_database_error)?;
    rows.into_iter()
        .map(|row| {
            let version: i64 = row
                .try_get("user_code_key_version")
                .map_err(|_| DeviceAuthorizationStoreError::Unavailable)?;
            u32::try_from(version).map_err(|_| DeviceAuthorizationStoreError::Unavailable)
        })
        .collect()
}

async fn by_approval_id(
    pool: &PgPool,
    approval_id: &DeviceAuthorizationId,
) -> StoreResult<Option<DeviceAuthorizationRecord>> {
    let row = sqlx::query(&format!(
        "SELECT {SELECT_COLUMNS} FROM {TABLE} WHERE approval_id = $1"
    ))
    .bind(approval_id.to_vec())
    .fetch_optional(pool)
    .await
    .map_err(map_database_error)?;
    row.map(decode_record).transpose()
}

async fn expired_records(
    pool: &PgPool,
    now_unix_ms: u64,
    limit: usize,
) -> StoreResult<Vec<DeviceAuthorizationRecord>> {
    let now = i64::try_from(now_unix_ms).map_err(|_| DeviceAuthorizationStoreError::Unavailable)?;
    let limit = bounded_limit(limit)?;
    let rows = sqlx::query(&format!(
        "SELECT {SELECT_COLUMNS} FROM {TABLE} \
         WHERE expires_at_unix_ms <= $1 \
           AND state_kind IN ('pending', 'awaiting_webauthn', 'verifying_webauthn', 'approved') \
         ORDER BY expires_at_unix_ms, id LIMIT $2"
    ))
    .bind(now)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(map_database_error)?;
    rows.into_iter().map(decode_record).collect()
}

async fn recoverable_issuances(
    pool: &PgPool,
    limit: usize,
) -> StoreResult<Vec<DeviceAuthorizationRecord>> {
    let limit = bounded_limit(limit)?;
    let rows = sqlx::query(&format!(
        "SELECT {SELECT_COLUMNS} FROM {TABLE} \
         WHERE state_kind = 'issuing' \
         ORDER BY created_at_unix_ms, id LIMIT $1"
    ))
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(map_database_error)?;
    rows.into_iter().map(decode_record).collect()
}

fn bounded_limit(limit: usize) -> StoreResult<i64> {
    if !(1..=MAX_SCAN_LIMIT).contains(&limit) {
        return Err(DeviceAuthorizationStoreError::Unavailable);
    }
    i64::try_from(limit).map_err(|_| DeviceAuthorizationStoreError::Unavailable)
}

async fn compare_and_swap(
    pool: &PgPool,
    expected_revision: u64,
    replacement: &DeviceAuthorizationRecord,
) -> StoreResult<()> {
    let Some(next_revision) = expected_revision.checked_add(1) else {
        return Err(DeviceAuthorizationStoreError::Conflict);
    };
    if replacement.revision != next_revision {
        return Err(DeviceAuthorizationStoreError::Conflict);
    }

    let mut transaction = pool.begin().await.map_err(map_database_error)?;
    let row = sqlx::query(&format!(
        "SELECT {SELECT_COLUMNS} FROM {TABLE} WHERE id = $1 FOR UPDATE"
    ))
    .bind(replacement.id.to_vec())
    .fetch_optional(&mut *transaction)
    .await
    .map_err(map_database_error)?
    .ok_or(DeviceAuthorizationStoreError::Conflict)?;
    let current = decode_record(row)?;
    if current.revision != expected_revision || !same_immutable_fields(&current, replacement) {
        return Err(DeviceAuthorizationStoreError::Conflict);
    }

    let encoded = EncodedRecord::new(replacement)?;
    let update = sqlx::query(&format!(
        "UPDATE {TABLE} SET \
         poll_interval_ms = $3, last_poll_at_unix_ms = $4, revision = $5, \
         state_kind = $6, approval_id = $7, state_payload = $8 \
         WHERE id = $1 AND revision = $2"
    ))
    .bind(encoded.id)
    .bind(i64::try_from(expected_revision).map_err(|_| DeviceAuthorizationStoreError::Conflict)?)
    .bind(encoded.poll_interval_ms)
    .bind(encoded.last_poll_at_unix_ms)
    .bind(encoded.revision)
    .bind(encoded.state_kind)
    .bind(encoded.approval_id)
    .bind(encoded.state_payload)
    .execute(&mut *transaction)
    .await
    .map_err(map_database_error)?;
    if update.rows_affected() != 1 {
        return Err(DeviceAuthorizationStoreError::Conflict);
    }
    transaction.commit().await.map_err(map_database_error)
}

fn same_immutable_fields(
    current: &DeviceAuthorizationRecord,
    replacement: &DeviceAuthorizationRecord,
) -> bool {
    current.id == replacement.id
        && current.device_code_hash == replacement.device_code_hash
        && current.user_code_digest == replacement.user_code_digest
        && current.scope == replacement.scope
        && current.csr_der == replacement.csr_der
        && current.csr_sha256 == replacement.csr_sha256
        && current.spki_sha256 == replacement.spki_sha256
        && current.created_at_unix_ms == replacement.created_at_unix_ms
        && current.expires_at_unix_ms == replacement.expires_at_unix_ms
}

fn map_database_error(error: sqlx::Error) -> DeviceAuthorizationStoreError {
    if let Some(database_error) = error.as_database_error() {
        if database_error.code().as_deref() == Some("23505") {
            return match database_error.constraint() {
                Some("authorizations_approval_id_unique") => {
                    DeviceAuthorizationStoreError::Conflict
                }
                Some(
                    "authorizations_pk"
                    | "authorizations_device_code_hash_unique"
                    | "authorizations_user_code_digest_unique",
                ) => DeviceAuthorizationStoreError::CodeCollision,
                _ => DeviceAuthorizationStoreError::Unavailable,
            };
        }
    }
    DeviceAuthorizationStoreError::Unavailable
}

struct EncodedRecord {
    id: Vec<u8>,
    device_code_hash: Vec<u8>,
    user_code_key_version: i64,
    user_code_mac: Vec<u8>,
    organization_id: String,
    workspace_id: String,
    csr_der: Vec<u8>,
    csr_sha256: Vec<u8>,
    spki_sha256: Vec<u8>,
    created_at_unix_ms: i64,
    expires_at_unix_ms: i64,
    poll_interval_ms: i64,
    last_poll_at_unix_ms: Option<i64>,
    revision: i64,
    state_kind: &'static str,
    approval_id: Option<Vec<u8>>,
    state_payload: Vec<u8>,
}

impl EncodedRecord {
    fn new(record: &DeviceAuthorizationRecord) -> StoreResult<Self> {
        if record.csr_der.is_empty()
            || record.csr_der.len() > MAX_CSR_BYTES
            || sha256(&record.csr_der) != record.csr_sha256
            || record.created_at_unix_ms > record.expires_at_unix_ms
            || record.poll_interval_ms == 0
        {
            return Err(DeviceAuthorizationStoreError::Unavailable);
        }
        let (version, mac) = record.user_code_digest.storage_parts();
        let stored_state = StoredAuthorizationState::from_domain(&record.state);
        if !stored_state.is_valid(&record.scope, &record.spki_sha256) {
            return Err(DeviceAuthorizationStoreError::Unavailable);
        }
        let state_kind = stored_state.kind();
        let approval_id = stored_state.approval_id().map(|id| id.to_vec());
        let state_payload = serde_json::to_vec(&StoredState {
            format_version: 1,
            state: stored_state,
        })
        .map_err(|_| DeviceAuthorizationStoreError::Unavailable)?;
        if state_payload.len() > MAX_STATE_PAYLOAD_BYTES {
            return Err(DeviceAuthorizationStoreError::Unavailable);
        }

        Ok(Self {
            id: record.id.to_vec(),
            device_code_hash: record.device_code_hash.to_vec(),
            user_code_key_version: i64::from(version),
            user_code_mac: mac.to_vec(),
            organization_id: record.scope.organization_id.clone(),
            workspace_id: record.scope.workspace_id.clone(),
            csr_der: record.csr_der.clone(),
            csr_sha256: record.csr_sha256.to_vec(),
            spki_sha256: record.spki_sha256.to_vec(),
            created_at_unix_ms: to_i64(record.created_at_unix_ms)?,
            expires_at_unix_ms: to_i64(record.expires_at_unix_ms)?,
            poll_interval_ms: to_i64(record.poll_interval_ms)?,
            last_poll_at_unix_ms: record.last_poll_at_unix_ms.map(to_i64).transpose()?,
            revision: to_i64(record.revision)?,
            state_kind,
            approval_id,
            state_payload,
        })
    }
}

fn to_i64(value: u64) -> StoreResult<i64> {
    i64::try_from(value).map_err(|_| DeviceAuthorizationStoreError::Unavailable)
}

fn decode_record(row: PgRow) -> StoreResult<DeviceAuthorizationRecord> {
    macro_rules! get {
        ($name:literal, $type:ty) => {
            row.try_get::<$type, _>($name)
                .map_err(|_| DeviceAuthorizationStoreError::Unavailable)?
        };
    }
    let id = fixed::<16>(get!("id", Vec<u8>))?;
    let device_code_hash = fixed::<32>(get!("device_code_hash", Vec<u8>))?;
    let key_version: i64 = get!("user_code_key_version", i64);
    let key_version =
        u32::try_from(key_version).map_err(|_| DeviceAuthorizationStoreError::Unavailable)?;
    let user_code_mac = fixed::<32>(get!("user_code_mac", Vec<u8>))?;
    let user_code_digest = VersionedUserCodeDigest::from_storage_parts(key_version, user_code_mac)
        .map_err(|_| DeviceAuthorizationStoreError::Unavailable)?;
    let scope = DeviceAuthorizationScope {
        organization_id: get!("organization_id", String),
        workspace_id: get!("workspace_id", String),
    };
    let csr_der: Vec<u8> = get!("csr_der", Vec<u8>);
    let csr_sha256 = fixed::<32>(get!("csr_sha256", Vec<u8>))?;
    let spki_sha256 = fixed::<32>(get!("spki_sha256", Vec<u8>))?;
    let created_at_unix_ms = from_i64(get!("created_at_unix_ms", i64))?;
    let expires_at_unix_ms = from_i64(get!("expires_at_unix_ms", i64))?;
    let poll_interval_ms = from_i64(get!("poll_interval_ms", i64))?;
    let last_poll_at_unix_ms = get!("last_poll_at_unix_ms", Option<i64>)
        .map(from_i64)
        .transpose()?;
    let revision = from_i64(get!("revision", i64))?;
    let state_kind: String = get!("state_kind", String);
    let approval_id = get!("approval_id", Option<Vec<u8>>)
        .map(fixed::<16>)
        .transpose()?;
    let state_payload: Vec<u8> = get!("state_payload", Vec<u8>);
    if state_payload.is_empty()
        || state_payload.len() > MAX_STATE_PAYLOAD_BYTES
        || csr_der.is_empty()
        || csr_der.len() > MAX_CSR_BYTES
        || sha256(&csr_der) != csr_sha256
        || created_at_unix_ms > expires_at_unix_ms
        || poll_interval_ms == 0
    {
        return Err(DeviceAuthorizationStoreError::Unavailable);
    }
    let wrapper: StoredState = serde_json::from_slice(&state_payload)
        .map_err(|_| DeviceAuthorizationStoreError::Unavailable)?;
    if wrapper.format_version != 1
        || wrapper.state.kind() != state_kind
        || wrapper.state.approval_id() != approval_id
        || !wrapper.state.is_valid(&scope, &spki_sha256)
    {
        return Err(DeviceAuthorizationStoreError::Unavailable);
    }

    Ok(DeviceAuthorizationRecord {
        id,
        device_code_hash,
        user_code_digest,
        scope,
        csr_der,
        csr_sha256,
        spki_sha256,
        created_at_unix_ms,
        expires_at_unix_ms,
        poll_interval_ms,
        last_poll_at_unix_ms,
        revision,
        state: wrapper.state.into_domain()?,
    })
}

fn fixed<const N: usize>(bytes: Vec<u8>) -> StoreResult<[u8; N]> {
    bytes
        .as_slice()
        .try_into()
        .map_err(|_| DeviceAuthorizationStoreError::Unavailable)
}

fn from_i64(value: i64) -> StoreResult<u64> {
    u64::try_from(value).map_err(|_| DeviceAuthorizationStoreError::Unavailable)
}

fn sha256(value: &[u8]) -> [u8; 32] {
    Sha256::digest(value).into()
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredState {
    format_version: u8,
    state: StoredAuthorizationState,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum StoredAuthorizationState {
    Pending,
    AwaitingWebauthn {
        approval_id: DeviceAuthorizationId,
        approver: StoredIdentity,
        credential_request_options_json: Vec<u8>,
        opaque_state: Vec<u8>,
    },
    VerifyingWebauthn {
        approval_id: DeviceAuthorizationId,
        approver: StoredIdentity,
        assertion_sha256: [u8; 32],
        opaque_state: Vec<u8>,
    },
    Issuing {
        approval_id: DeviceAuthorizationId,
        approver: StoredIdentity,
        issued_at_unix_ms: u64,
    },
    Approved {
        approval_id: DeviceAuthorizationId,
        approver: StoredIdentity,
        decided_at_unix_ms: u64,
        certificate: StoredCertificate,
    },
    Denied {
        approver: StoredIdentity,
        decided_at_unix_ms: u64,
    },
    Consumed {
        approver: StoredIdentity,
        decided_at_unix_ms: u64,
        consumed_at_unix_ms: u64,
    },
    Expired,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredIdentity {
    issuer: String,
    subject: String,
}

impl StoredIdentity {
    fn from_domain(identity: &UserIdentityRef) -> Self {
        Self {
            issuer: identity.issuer.clone(),
            subject: identity.subject.clone(),
        }
    }

    fn into_domain(self) -> StoreResult<UserIdentityRef> {
        if self.issuer.trim().is_empty()
            || self.subject.trim().is_empty()
            || self.issuer.len() > 2_048
            || self.subject.len() > 2_048
        {
            return Err(DeviceAuthorizationStoreError::Unavailable);
        }
        Ok(UserIdentityRef {
            issuer: self.issuer,
            subject: self.subject,
        })
    }

    fn is_valid(&self) -> bool {
        !self.issuer.trim().is_empty()
            && !self.subject.trim().is_empty()
            && self.issuer.len() <= 2_048
            && self.subject.len() <= 2_048
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredScope {
    organization_id: String,
    workspace_id: String,
}

impl From<&DeviceAuthorizationScope> for StoredScope {
    fn from(scope: &DeviceAuthorizationScope) -> Self {
        Self {
            organization_id: scope.organization_id.clone(),
            workspace_id: scope.workspace_id.clone(),
        }
    }
}

impl StoredScope {
    fn into_domain(self) -> DeviceAuthorizationScope {
        DeviceAuthorizationScope {
            organization_id: self.organization_id,
            workspace_id: self.workspace_id,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredCertificate {
    certificate_der: Vec<u8>,
    serial_number: Vec<u8>,
    scope: StoredScope,
    spki_sha256: [u8; 32],
    not_after_unix_ms: u64,
}

impl StoredCertificate {
    fn from_domain(certificate: &IssuedDeviceCertificate) -> Self {
        Self {
            certificate_der: certificate.certificate_der.clone(),
            serial_number: certificate.serial_number.clone(),
            scope: (&certificate.scope).into(),
            spki_sha256: certificate.spki_sha256,
            not_after_unix_ms: certificate.not_after_unix_ms,
        }
    }

    fn into_domain(self) -> IssuedDeviceCertificate {
        IssuedDeviceCertificate {
            certificate_der: self.certificate_der,
            serial_number: self.serial_number,
            scope: self.scope.into_domain(),
            spki_sha256: self.spki_sha256,
            not_after_unix_ms: self.not_after_unix_ms,
        }
    }

    fn is_valid(&self, scope: &DeviceAuthorizationScope, spki_sha256: &[u8; 32]) -> bool {
        !self.certificate_der.is_empty()
            && self.certificate_der.len() <= MAX_CERTIFICATE_BYTES
            && !self.serial_number.is_empty()
            && self.serial_number.len() <= MAX_SERIAL_NUMBER_BYTES
            && self.scope.organization_id == scope.organization_id
            && self.scope.workspace_id == scope.workspace_id
            && &self.spki_sha256 == spki_sha256
    }
}

impl StoredAuthorizationState {
    fn from_domain(state: &DeviceAuthorizationState) -> Self {
        match state {
            DeviceAuthorizationState::Pending => Self::Pending,
            DeviceAuthorizationState::AwaitingWebAuthn {
                approval_id,
                approver,
                credential_request_options_json,
                opaque_state,
            } => Self::AwaitingWebauthn {
                approval_id: *approval_id,
                approver: StoredIdentity::from_domain(approver),
                credential_request_options_json: credential_request_options_json.clone(),
                opaque_state: opaque_state.clone(),
            },
            DeviceAuthorizationState::VerifyingWebAuthn {
                approval_id,
                approver,
                assertion_sha256,
                opaque_state,
            } => Self::VerifyingWebauthn {
                approval_id: *approval_id,
                approver: StoredIdentity::from_domain(approver),
                assertion_sha256: *assertion_sha256,
                opaque_state: opaque_state.clone(),
            },
            DeviceAuthorizationState::Issuing {
                approval_id,
                approver,
                issued_at_unix_ms,
            } => Self::Issuing {
                approval_id: *approval_id,
                approver: StoredIdentity::from_domain(approver),
                issued_at_unix_ms: *issued_at_unix_ms,
            },
            DeviceAuthorizationState::Approved {
                approval_id,
                approver,
                decided_at_unix_ms,
                certificate,
            } => Self::Approved {
                approval_id: *approval_id,
                approver: StoredIdentity::from_domain(approver),
                decided_at_unix_ms: *decided_at_unix_ms,
                certificate: StoredCertificate::from_domain(certificate),
            },
            DeviceAuthorizationState::Denied {
                approver,
                decided_at_unix_ms,
            } => Self::Denied {
                approver: StoredIdentity::from_domain(approver),
                decided_at_unix_ms: *decided_at_unix_ms,
            },
            DeviceAuthorizationState::Consumed {
                approver,
                decided_at_unix_ms,
                consumed_at_unix_ms,
            } => Self::Consumed {
                approver: StoredIdentity::from_domain(approver),
                decided_at_unix_ms: *decided_at_unix_ms,
                consumed_at_unix_ms: *consumed_at_unix_ms,
            },
            DeviceAuthorizationState::Expired => Self::Expired,
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::AwaitingWebauthn { .. } => "awaiting_webauthn",
            Self::VerifyingWebauthn { .. } => "verifying_webauthn",
            Self::Issuing { .. } => "issuing",
            Self::Approved { .. } => "approved",
            Self::Denied { .. } => "denied",
            Self::Consumed { .. } => "consumed",
            Self::Expired => "expired",
        }
    }

    fn approval_id(&self) -> Option<DeviceAuthorizationId> {
        match self {
            Self::AwaitingWebauthn { approval_id, .. }
            | Self::VerifyingWebauthn { approval_id, .. }
            | Self::Issuing { approval_id, .. }
            | Self::Approved { approval_id, .. } => Some(*approval_id),
            _ => None,
        }
    }

    fn is_valid(&self, scope: &DeviceAuthorizationScope, spki_sha256: &[u8; 32]) -> bool {
        match self {
            Self::Pending | Self::Expired => true,
            Self::AwaitingWebauthn {
                approver,
                credential_request_options_json,
                opaque_state,
                ..
            } => {
                approver.is_valid()
                    && !credential_request_options_json.is_empty()
                    && credential_request_options_json.len() <= MAX_REQUEST_OPTIONS_BYTES
                    && !opaque_state.is_empty()
                    && opaque_state.len() <= MAX_WEBAUTHN_STATE_BYTES
            }
            Self::VerifyingWebauthn {
                approver,
                opaque_state,
                ..
            } => {
                approver.is_valid()
                    && !opaque_state.is_empty()
                    && opaque_state.len() <= MAX_WEBAUTHN_STATE_BYTES
            }
            Self::Issuing { approver, .. } | Self::Denied { approver, .. } => approver.is_valid(),
            Self::Approved {
                approver,
                certificate,
                ..
            } => approver.is_valid() && certificate.is_valid(scope, spki_sha256),
            Self::Consumed {
                approver,
                consumed_at_unix_ms,
                decided_at_unix_ms,
            } => approver.is_valid() && consumed_at_unix_ms >= decided_at_unix_ms,
        }
    }

    fn into_domain(self) -> StoreResult<DeviceAuthorizationState> {
        Ok(match self {
            Self::Pending => DeviceAuthorizationState::Pending,
            Self::AwaitingWebauthn {
                approval_id,
                approver,
                credential_request_options_json,
                opaque_state,
            } => DeviceAuthorizationState::AwaitingWebAuthn {
                approval_id,
                approver: approver.into_domain()?,
                credential_request_options_json,
                opaque_state,
            },
            Self::VerifyingWebauthn {
                approval_id,
                approver,
                assertion_sha256,
                opaque_state,
            } => DeviceAuthorizationState::VerifyingWebAuthn {
                approval_id,
                approver: approver.into_domain()?,
                assertion_sha256,
                opaque_state,
            },
            Self::Issuing {
                approval_id,
                approver,
                issued_at_unix_ms,
            } => DeviceAuthorizationState::Issuing {
                approval_id,
                approver: approver.into_domain()?,
                issued_at_unix_ms,
            },
            Self::Approved {
                approval_id,
                approver,
                decided_at_unix_ms,
                certificate,
            } => DeviceAuthorizationState::Approved {
                approval_id,
                approver: approver.into_domain()?,
                decided_at_unix_ms,
                certificate: certificate.into_domain(),
            },
            Self::Denied {
                approver,
                decided_at_unix_ms,
            } => DeviceAuthorizationState::Denied {
                approver: approver.into_domain()?,
                decided_at_unix_ms,
            },
            Self::Consumed {
                approver,
                decided_at_unix_ms,
                consumed_at_unix_ms,
            } => DeviceAuthorizationState::Consumed {
                approver: approver.into_domain()?,
                decided_at_unix_ms,
                consumed_at_unix_ms,
            },
            Self::Expired => DeviceAuthorizationState::Expired,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> UserIdentityRef {
        UserIdentityRef {
            issuer: "https://login.example/tenant/v2.0".to_owned(),
            subject: "subject-123".to_owned(),
        }
    }

    fn scope() -> DeviceAuthorizationScope {
        DeviceAuthorizationScope {
            organization_id: "org-1".to_owned(),
            workspace_id: "workspace-1".to_owned(),
        }
    }

    fn certificate() -> IssuedDeviceCertificate {
        IssuedDeviceCertificate {
            certificate_der: vec![0x30, 0x03, 0x01, 0x02, 0x03],
            serial_number: vec![1, 2, 3],
            scope: scope(),
            spki_sha256: [4; 32],
            not_after_unix_ms: 9_000,
        }
    }

    #[test]
    fn every_state_round_trips() {
        let approval_id = [7; 16];
        let states = vec![
            DeviceAuthorizationState::Pending,
            DeviceAuthorizationState::AwaitingWebAuthn {
                approval_id,
                approver: identity(),
                credential_request_options_json: br#"{"challenge":"visible-options"}"#.to_vec(),
                opaque_state: vec![8, 9, 10],
            },
            DeviceAuthorizationState::VerifyingWebAuthn {
                approval_id,
                approver: identity(),
                assertion_sha256: [3; 32],
                opaque_state: vec![11, 12],
            },
            DeviceAuthorizationState::Issuing {
                approval_id,
                approver: identity(),
                issued_at_unix_ms: 3_000,
            },
            DeviceAuthorizationState::Approved {
                approval_id,
                approver: identity(),
                decided_at_unix_ms: 4_000,
                certificate: certificate(),
            },
            DeviceAuthorizationState::Denied {
                approver: identity(),
                decided_at_unix_ms: 4_000,
            },
            DeviceAuthorizationState::Consumed {
                approver: identity(),
                decided_at_unix_ms: 4_000,
                consumed_at_unix_ms: 5_000,
            },
            DeviceAuthorizationState::Expired,
        ];

        for state in states {
            let stored = StoredAuthorizationState::from_domain(&state);
            let encoded = serde_json::to_vec(&StoredState {
                format_version: 1,
                state: stored,
            })
            .expect("state serialization");
            let decoded: StoredState = serde_json::from_slice(&encoded).expect("state parsing");
            assert_eq!(
                decoded.state.kind(),
                StoredAuthorizationState::from_domain(&state).kind()
            );
            assert!(decoded.state.is_valid(&scope(), &[4; 32]));
            assert_eq!(
                decoded.state.into_domain().expect("domain conversion"),
                state
            );
        }
    }

    #[test]
    fn state_lookup_columns_follow_state_payload() {
        let awaiting =
            StoredAuthorizationState::from_domain(&DeviceAuthorizationState::AwaitingWebAuthn {
                approval_id: [12; 16],
                approver: identity(),
                credential_request_options_json: b"{}".to_vec(),
                opaque_state: vec![1],
            });
        assert_eq!(awaiting.kind(), "awaiting_webauthn");
        assert_eq!(awaiting.approval_id(), Some([12; 16]));

        let expired = StoredAuthorizationState::Expired;
        assert_eq!(expired.kind(), "expired");
        assert_eq!(expired.approval_id(), None);
    }

    #[test]
    fn malformed_or_oversized_opaque_webauthn_state_is_rejected() {
        let state =
            StoredAuthorizationState::from_domain(&DeviceAuthorizationState::AwaitingWebAuthn {
                approval_id: [2; 16],
                approver: identity(),
                credential_request_options_json: b"{}".to_vec(),
                opaque_state: vec![1; MAX_WEBAUTHN_STATE_BYTES + 1],
            });
        assert!(!state.is_valid(&scope(), &[4; 32]));
    }

    #[test]
    fn schema_stores_only_versioned_user_code_macs() {
        let migration =
            include_str!("../migrations/device_authorization/0001_authorizations.up.sql");
        assert!(migration.contains("user_code_key_version BIGINT"));
        assert!(migration.contains("user_code_mac BYTEA"));
        assert!(migration.contains("device_code_hash BYTEA"));
        assert!(!migration.contains("user_code TEXT"));
        assert!(!migration.contains("device_code TEXT"));
    }
}
