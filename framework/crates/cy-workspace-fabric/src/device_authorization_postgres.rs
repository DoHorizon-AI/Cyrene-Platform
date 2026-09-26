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
    DeviceAuthorizationCodeHash, DeviceAuthorizationDeviceKey, DeviceAuthorizationId,
    DeviceAuthorizationRecord, DeviceAuthorizationRegistrationBinding, DeviceAuthorizationScope,
    DeviceAuthorizationState, DeviceAuthorizationStore, DeviceAuthorizationStoreError,
    DeviceCertificateDeliveryReceipt, DeviceCertificateIssuanceFailure,
    DeviceCertificateRetirementError, DeviceCertificateRetirementReason, IssuedDeviceCertificate,
    VerifiedDirectoryRegistrationBinding,
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
const MAX_CA_CHAIN_CERTIFICATES: usize = 8;
// Vec<u8> JSON encoding can expand each DER byte to four characters. Keep the
// bundle below the database payload limit with room for the typed state fields.
const MAX_CERTIFICATE_BUNDLE_BYTES: usize = 6 * 1024 * 1024;
const MAX_STATE_PAYLOAD_BYTES: usize = 32 * 1024 * 1024;

static MIGRATOR: Migrator = sqlx::migrate!("./migrations/device_authorization");

const SELECT_COLUMNS: &str =
    "id, device_code_hash, user_code_key_version, user_code_mac, organization_id, workspace_id, \
     registration_binding_id, device_id, authorization_generation, \
     csr_der, csr_sha256, spki_sha256, created_at_unix_ms, expires_at_unix_ms, \
     poll_interval_ms, last_poll_at_unix_ms, revision, state_kind, approval_id, \
     state_deadline_unix_ms, delivery_certificate_not_after_unix_ms, state_payload";

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

    /// Return a bounded page of certificate deliveries whose ACK deadline passed.
    ///
    /// This is an advisory recovery scan. A worker must still re-read the row
    /// and use trusted current time before attempting certificate retirement.
    pub fn due_certificate_deliveries(
        &self,
        now_unix_ms: u64,
        limit: usize,
    ) -> StoreResult<Vec<DeviceAuthorizationRecord>> {
        let (reply, result) = mpsc::sync_channel(1);
        self.call(
            Command::DueCertificateDeliveries(now_unix_ms, limit, reply),
            result,
        )
    }

    /// Return a bounded page of unresolved certificate retirement records.
    ///
    /// A worker must retry the idempotent retirement operation with the
    /// authorization ID and certificate fingerprint from each record.
    pub fn recoverable_retirements(
        &self,
        limit: usize,
    ) -> StoreResult<Vec<DeviceAuthorizationRecord>> {
        let (reply, result) = mpsc::sync_channel(1);
        self.call(Command::RecoverableRetirements(limit, reply), result)
    }

    /// Read the current clock from the same database that stores authorization state.
    pub fn database_time_unix_ms(&self) -> StoreResult<u64> {
        let (reply, result) = mpsc::sync_channel(1);
        self.call(Command::DatabaseTime(reply), result)
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

    fn insert_registered(&self, record: DeviceAuthorizationRecord) -> StoreResult<()> {
        let (reply, result) = mpsc::sync_channel(1);
        self.call(Command::InsertRegistered(record, reply), result)
    }

    fn require_current_registered_record(
        &self,
        record: &DeviceAuthorizationRecord,
    ) -> StoreResult<()> {
        let (reply, result) = mpsc::sync_channel(1);
        self.call(
            Command::RequireCurrentRegisteredRecord(record.clone(), reply),
            result,
        )
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

    fn by_authorization_id(
        &self,
        authorization_id: &DeviceAuthorizationId,
    ) -> StoreResult<Option<DeviceAuthorizationRecord>> {
        let (reply, result) = mpsc::sync_channel(1);
        self.call(Command::ByAuthorizationId(*authorization_id, reply), result)
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

    fn compare_and_swap_due_delivery_to_retirement(
        &self,
        expected_revision: u64,
        replacement: DeviceAuthorizationRecord,
    ) -> StoreResult<bool> {
        let (reply, result) = mpsc::sync_channel(1);
        self.call(
            Command::CompareAndSwapDueDeliveryToRetirement(expected_revision, replacement, reply),
            result,
        )
    }

    fn compare_and_swap_registered(
        &self,
        expected_revision: u64,
        replacement: DeviceAuthorizationRecord,
    ) -> StoreResult<()> {
        let (reply, result) = mpsc::sync_channel(1);
        self.call(
            Command::CompareAndSwapRegistered(expected_revision, replacement, reply),
            result,
        )
    }

    fn compare_and_swap_delivery_ack(
        &self,
        expected_revision: u64,
        replacement: DeviceAuthorizationRecord,
    ) -> StoreResult<()> {
        let (reply, result) = mpsc::sync_channel(1);
        self.call(
            Command::CompareAndSwapDeliveryAck(expected_revision, replacement, reply),
            result,
        )
    }

    fn compare_and_swap_registered_delivery_ack(
        &self,
        expected_revision: u64,
        replacement: DeviceAuthorizationRecord,
    ) -> StoreResult<()> {
        let (reply, result) = mpsc::sync_channel(1);
        self.call(
            Command::CompareAndSwapRegisteredDeliveryAck(expected_revision, replacement, reply),
            result,
        )
    }
}

enum Command {
    Insert(DeviceAuthorizationRecord, StoreReply<()>),
    InsertRegistered(DeviceAuthorizationRecord, StoreReply<()>),
    RequireCurrentRegisteredRecord(DeviceAuthorizationRecord, StoreReply<()>),
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
    ByAuthorizationId(
        DeviceAuthorizationId,
        StoreReply<Option<DeviceAuthorizationRecord>>,
    ),
    CompareAndSwap(u64, DeviceAuthorizationRecord, StoreReply<()>),
    CompareAndSwapDueDeliveryToRetirement(u64, DeviceAuthorizationRecord, StoreReply<bool>),
    CompareAndSwapRegistered(u64, DeviceAuthorizationRecord, StoreReply<()>),
    CompareAndSwapDeliveryAck(u64, DeviceAuthorizationRecord, StoreReply<()>),
    CompareAndSwapRegisteredDeliveryAck(u64, DeviceAuthorizationRecord, StoreReply<()>),
    ExpiredRecords(u64, usize, StoreReply<Vec<DeviceAuthorizationRecord>>),
    RecoverableIssuances(usize, StoreReply<Vec<DeviceAuthorizationRecord>>),
    DueCertificateDeliveries(u64, usize, StoreReply<Vec<DeviceAuthorizationRecord>>),
    RecoverableRetirements(usize, StoreReply<Vec<DeviceAuthorizationRecord>>),
    DatabaseTime(StoreReply<u64>),
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
            Command::InsertRegistered(record, reply) => {
                let _ = reply.send(runtime.block_on(insert_registered_record(&pool, &record)));
            }
            Command::RequireCurrentRegisteredRecord(record, reply) => {
                let _ =
                    reply.send(runtime.block_on(require_current_registered_record(&pool, &record)));
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
            Command::ByAuthorizationId(authorization_id, reply) => {
                let _ = reply.send(runtime.block_on(by_authorization_id(&pool, &authorization_id)));
            }
            Command::CompareAndSwap(expected, replacement, reply) => {
                let _ = reply.send(runtime.block_on(compare_and_swap(
                    &pool,
                    expected,
                    &replacement,
                    false,
                    false,
                )));
            }
            Command::CompareAndSwapRegistered(expected, replacement, reply) => {
                let _ = reply.send(runtime.block_on(compare_and_swap(
                    &pool,
                    expected,
                    &replacement,
                    false,
                    true,
                )));
            }
            Command::CompareAndSwapDeliveryAck(expected, replacement, reply) => {
                let _ = reply.send(runtime.block_on(compare_and_swap(
                    &pool,
                    expected,
                    &replacement,
                    true,
                    false,
                )));
            }
            Command::CompareAndSwapDueDeliveryToRetirement(expected, replacement, reply) => {
                let _ = reply.send(
                    runtime.block_on(compare_and_swap_due_delivery_to_retirement(
                        &pool,
                        expected,
                        &replacement,
                    )),
                );
            }
            Command::CompareAndSwapRegisteredDeliveryAck(expected, replacement, reply) => {
                let _ = reply.send(runtime.block_on(compare_and_swap(
                    &pool,
                    expected,
                    &replacement,
                    true,
                    true,
                )));
            }
            Command::ExpiredRecords(now, limit, reply) => {
                let _ = reply.send(runtime.block_on(expired_records(&pool, now, limit)));
            }
            Command::RecoverableIssuances(limit, reply) => {
                let _ = reply.send(runtime.block_on(recoverable_issuances(&pool, limit)));
            }
            Command::DueCertificateDeliveries(now, limit, reply) => {
                let _ = reply.send(runtime.block_on(due_certificate_deliveries(&pool, now, limit)));
            }
            Command::RecoverableRetirements(limit, reply) => {
                let _ = reply.send(runtime.block_on(recoverable_retirements(&pool, limit)));
            }
            Command::DatabaseTime(reply) => {
                let _ = reply.send(runtime.block_on(database_time_unix_ms(&pool)));
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
        sqlx::query(
            "SELECT state_deadline_unix_ms, delivery_certificate_not_after_unix_ms \
             FROM cyrene_workspace_device_authorization.authorizations LIMIT 0",
        )
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
         ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, \
          $18, $19, $20, $21, $22)"
    ))
    .bind(encoded.id)
    .bind(encoded.device_code_hash)
    .bind(encoded.user_code_key_version)
    .bind(encoded.user_code_mac)
    .bind(encoded.organization_id)
    .bind(encoded.workspace_id)
    .bind(encoded.registration_binding_id)
    .bind(encoded.device_id)
    .bind(encoded.authorization_generation)
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
    .bind(encoded.state_deadline_unix_ms)
    .bind(encoded.delivery_certificate_not_after_unix_ms)
    .bind(encoded.state_payload)
    .execute(pool)
    .await;
    result.map(|_| ()).map_err(map_database_error)
}

async fn insert_registered_record(
    pool: &PgPool,
    record: &DeviceAuthorizationRecord,
) -> StoreResult<()> {
    let encoded = EncodedRecord::new(record)?;
    let mut transaction = pool.begin().await.map_err(map_database_error)?;
    lock_and_validate_registration(&mut transaction, &record.registration_binding).await?;

    let key = record.registration_binding.key();
    let newest = sqlx::query(
        "SELECT authorization_generation, state_kind \
         FROM cyrene_workspace_device_authorization.authorizations \
         WHERE organization_id = $1 AND workspace_id = $2 AND device_id = $3 \
         ORDER BY authorization_generation DESC LIMIT 1 FOR UPDATE",
    )
    .bind(&key.organization_id)
    .bind(&key.workspace_id)
    .bind(&key.device_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(map_database_error)?;

    if let Some(newest) = newest {
        let newest_generation: i64 = newest
            .try_get("authorization_generation")
            .map_err(|_| DeviceAuthorizationStoreError::Unavailable)?;
        let newest_generation = from_i64(newest_generation)?;
        let newest_state: String = newest
            .try_get("state_kind")
            .map_err(|_| DeviceAuthorizationStoreError::Unavailable)?;
        let generation = record.registration_binding.authorization_generation();
        if generation < newest_generation
            || (generation > newest_generation
                && matches!(
                    newest_state.as_str(),
                    "issuing" | "delivery_pending" | "retirement_pending"
                ))
        {
            return Err(DeviceAuthorizationStoreError::Conflict);
        }
    }

    insert_encoded(&mut transaction, &encoded).await?;
    transaction.commit().await.map_err(map_database_error)
}

async fn require_current_registered_record(
    pool: &PgPool,
    record: &DeviceAuthorizationRecord,
) -> StoreResult<()> {
    if !registration_binding_matches_record(record) {
        return Err(DeviceAuthorizationStoreError::Conflict);
    }
    let mut transaction = pool.begin().await.map_err(map_database_error)?;
    lock_and_validate_registration(&mut transaction, &record.registration_binding).await?;
    let row = sqlx::query(&format!(
        "SELECT {SELECT_COLUMNS} FROM {TABLE} WHERE id = $1 FOR UPDATE"
    ))
    .bind(record.id.to_vec())
    .fetch_optional(&mut *transaction)
    .await
    .map_err(map_database_error)?
    .ok_or(DeviceAuthorizationStoreError::Conflict)?;
    let current = decode_record(row)?;
    if current.revision != record.revision || !same_immutable_fields(&current, record) {
        return Err(DeviceAuthorizationStoreError::Conflict);
    }
    transaction.commit().await.map_err(map_database_error)
}

/// Locks the stable Directory identity before the authorization row and verifies
/// its immutable registration binding while both stores share this transaction.
async fn lock_and_validate_registration(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    binding: &DeviceAuthorizationRegistrationBinding,
) -> StoreResult<()> {
    if binding.authorization_generation() == 0
        || binding.key().device_id.trim().is_empty()
        || binding.key().organization_id.trim().is_empty()
        || binding.key().workspace_id.trim().is_empty()
    {
        return Err(DeviceAuthorizationStoreError::Conflict);
    }
    let key = binding.key();
    let current_generation = sqlx::query_scalar::<_, i64>(
        "SELECT current_authorization_generation \
         FROM cyrene_workspace_directory.workspace_device_identities \
         WHERE organization_id = $1 AND workspace_id = $2 AND device_id = $3 FOR UPDATE",
    )
    .bind(&key.organization_id)
    .bind(&key.workspace_id)
    .bind(&key.device_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(map_database_error)?
    .ok_or(DeviceAuthorizationStoreError::Conflict)?;
    if from_i64(current_generation)? != binding.authorization_generation() {
        return Err(DeviceAuthorizationStoreError::Conflict);
    }

    let binding_matches = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (\
             SELECT 1 FROM cyrene_workspace_directory.device_registration_bindings \
             WHERE uuid_send(binding_id) = $1 \
               AND organization_id = $2 AND workspace_id = $3 AND device_id = $4 \
               AND authorization_generation = $5 AND csr_sha256 = $6 AND spki_sha256 = $7\
         )",
    )
    .bind(binding.binding_id().to_vec())
    .bind(&key.organization_id)
    .bind(&key.workspace_id)
    .bind(&key.device_id)
    .bind(to_i64(binding.authorization_generation())?)
    .bind(binding.csr_sha256().to_vec())
    .bind(binding.spki_sha256().to_vec())
    .fetch_one(&mut **transaction)
    .await
    .map_err(map_database_error)?;
    if !binding_matches {
        return Err(DeviceAuthorizationStoreError::Conflict);
    }
    Ok(())
}

async fn insert_encoded(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    encoded: &EncodedRecord,
) -> StoreResult<()> {
    sqlx::query(&format!(
        "INSERT INTO {TABLE} ({SELECT_COLUMNS}) VALUES \
         ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, \
          $18, $19, $20, $21, $22)"
    ))
    .bind(&encoded.id)
    .bind(&encoded.device_code_hash)
    .bind(encoded.user_code_key_version)
    .bind(&encoded.user_code_mac)
    .bind(&encoded.organization_id)
    .bind(&encoded.workspace_id)
    .bind(&encoded.registration_binding_id)
    .bind(&encoded.device_id)
    .bind(encoded.authorization_generation)
    .bind(&encoded.csr_der)
    .bind(&encoded.csr_sha256)
    .bind(&encoded.spki_sha256)
    .bind(encoded.created_at_unix_ms)
    .bind(encoded.expires_at_unix_ms)
    .bind(encoded.poll_interval_ms)
    .bind(encoded.last_poll_at_unix_ms)
    .bind(encoded.revision)
    .bind(encoded.state_kind)
    .bind(&encoded.approval_id)
    .bind(encoded.state_deadline_unix_ms)
    .bind(encoded.delivery_certificate_not_after_unix_ms)
    .bind(&encoded.state_payload)
    .execute(&mut **transaction)
    .await
    .map_err(map_database_error)?;
    Ok(())
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
    for (index, candidate) in candidates.iter().enumerate() {
        if index > 0 {
            query.push(" OR ");
        }
        let (version, mac) = candidate.storage_parts();
        query
            .push("(user_code_key_version = ")
            .push_bind(i64::from(version))
            .push(" AND user_code_mac = ")
            .push_bind(mac.to_vec())
            .push(")");
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

async fn by_authorization_id(
    pool: &PgPool,
    authorization_id: &DeviceAuthorizationId,
) -> StoreResult<Option<DeviceAuthorizationRecord>> {
    let row = sqlx::query(&format!(
        "SELECT {SELECT_COLUMNS} FROM {TABLE} WHERE id = $1"
    ))
    .bind(authorization_id.to_vec())
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
           AND state_kind IN ('pending', 'awaiting_webauthn', 'verifying_webauthn') \
         ORDER BY expires_at_unix_ms, id LIMIT $2"
    ))
    .bind(now)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(map_database_error)?;
    rows.into_iter().map(decode_record).collect()
}

async fn due_certificate_deliveries(
    pool: &PgPool,
    now_unix_ms: u64,
    limit: usize,
) -> StoreResult<Vec<DeviceAuthorizationRecord>> {
    let now = i64::try_from(now_unix_ms).map_err(|_| DeviceAuthorizationStoreError::Unavailable)?;
    let limit = bounded_limit(limit)?;
    let rows = sqlx::query(&format!(
        "SELECT {SELECT_COLUMNS} FROM {TABLE} \
         WHERE state_kind = 'delivery_pending' AND ( \
             state_deadline_unix_ms <= $1 OR \
             delivery_certificate_not_after_unix_ms <= $1 \
         ) \
         ORDER BY LEAST( \
             state_deadline_unix_ms, delivery_certificate_not_after_unix_ms \
         ), id LIMIT $2"
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

async fn recoverable_retirements(
    pool: &PgPool,
    limit: usize,
) -> StoreResult<Vec<DeviceAuthorizationRecord>> {
    let limit = bounded_limit(limit)?;
    let rows = sqlx::query(&format!(
        "SELECT {SELECT_COLUMNS} FROM {TABLE} \
         WHERE state_kind = 'retirement_pending' \
         ORDER BY created_at_unix_ms, id LIMIT $1"
    ))
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(map_database_error)?;
    rows.into_iter().map(decode_record).collect()
}

async fn database_time_unix_ms(pool: &PgPool) -> StoreResult<u64> {
    let now = sqlx::query_scalar::<_, i64>(
        "SELECT FLOOR(EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT",
    )
    .fetch_one(pool)
    .await
    .map_err(map_database_error)?;
    from_i64(now)
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
    delivery_ack: bool,
    registered: bool,
) -> StoreResult<()> {
    let Some(next_revision) = expected_revision.checked_add(1) else {
        return Err(DeviceAuthorizationStoreError::Conflict);
    };
    if replacement.revision != next_revision {
        return Err(DeviceAuthorizationStoreError::Conflict);
    }

    let mut transaction = pool.begin().await.map_err(map_database_error)?;
    if registered {
        if !registration_binding_matches_record(replacement) {
            return Err(DeviceAuthorizationStoreError::Conflict);
        }
        lock_and_validate_registration(&mut transaction, &replacement.registration_binding).await?;
    }
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
    if delivery_ack {
        if !valid_delivery_ack_transition(&current, replacement) {
            return Err(DeviceAuthorizationStoreError::Conflict);
        }
    } else if matches!(
        replacement.state,
        DeviceAuthorizationState::Delivered { .. }
    ) {
        // This transition has a database-time deadline guard and must use the
        // dedicated ACK method below.
        return Err(DeviceAuthorizationStoreError::Conflict);
    }

    let encoded = EncodedRecord::new(replacement)?;
    let deadline_guard = if delivery_ack {
        " AND state_kind = 'delivery_pending' \
         AND state_deadline_unix_ms > FLOOR(EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT"
    } else {
        ""
    };
    let update = sqlx::query(&format!(
        "UPDATE {TABLE} SET \
         poll_interval_ms = $3, last_poll_at_unix_ms = $4, revision = $5, \
         state_kind = $6, approval_id = $7, state_deadline_unix_ms = $8, \
         delivery_certificate_not_after_unix_ms = $9, state_payload = $10 \
         WHERE id = $1 AND revision = $2{deadline_guard}"
    ))
    .bind(encoded.id)
    .bind(i64::try_from(expected_revision).map_err(|_| DeviceAuthorizationStoreError::Conflict)?)
    .bind(encoded.poll_interval_ms)
    .bind(encoded.last_poll_at_unix_ms)
    .bind(encoded.revision)
    .bind(encoded.state_kind)
    .bind(encoded.approval_id)
    .bind(encoded.state_deadline_unix_ms)
    .bind(encoded.delivery_certificate_not_after_unix_ms)
    .bind(encoded.state_payload)
    .execute(&mut *transaction)
    .await
    .map_err(map_database_error)?;
    if update.rows_affected() != 1 {
        return Err(DeviceAuthorizationStoreError::Conflict);
    }
    transaction.commit().await.map_err(map_database_error)
}

async fn compare_and_swap_due_delivery_to_retirement(
    pool: &PgPool,
    expected_revision: u64,
    replacement: &DeviceAuthorizationRecord,
) -> StoreResult<bool> {
    let Some(next_revision) = expected_revision.checked_add(1) else {
        return Ok(false);
    };
    if replacement.revision != next_revision {
        return Ok(false);
    }

    let mut transaction = pool.begin().await.map_err(map_database_error)?;
    let row = sqlx::query(&format!(
        "SELECT {SELECT_COLUMNS} FROM {TABLE} WHERE id = $1 FOR UPDATE"
    ))
    .bind(replacement.id.to_vec())
    .fetch_optional(&mut *transaction)
    .await
    .map_err(map_database_error)?;
    let Some(row) = row else {
        return Ok(false);
    };
    let current = decode_record(row)?;
    let Some(entered_at_unix_ms) = due_retirement_entered_at(&current, replacement) else {
        return Ok(false);
    };
    if current.revision != expected_revision
        || !same_immutable_fields(&current, replacement)
        || current.poll_interval_ms != replacement.poll_interval_ms
        || current.last_poll_at_unix_ms != replacement.last_poll_at_unix_ms
    {
        return Ok(false);
    }

    let encoded = EncodedRecord::new(replacement)?;
    let update = sqlx::query(&format!(
        "UPDATE {TABLE} SET revision = $3, state_kind = $4, approval_id = $5, \
         state_deadline_unix_ms = NULL, delivery_certificate_not_after_unix_ms = NULL, \
         state_payload = $6 \
         WHERE id = $1 AND revision = $2 AND state_kind = 'delivery_pending' \
           AND registration_binding_id = $7 AND device_id = $8 \
           AND authorization_generation = $9 \
           AND $10 <= FLOOR(EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT \
           AND ( \
             state_deadline_unix_ms <= FLOOR(EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT \
             OR delivery_certificate_not_after_unix_ms \
                 <= FLOOR(EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT \
           )"
    ))
    .bind(&encoded.id)
    .bind(i64::try_from(expected_revision).map_err(|_| DeviceAuthorizationStoreError::Conflict)?)
    .bind(encoded.revision)
    .bind(encoded.state_kind)
    .bind(&encoded.approval_id)
    .bind(&encoded.state_payload)
    .bind(&encoded.registration_binding_id)
    .bind(&encoded.device_id)
    .bind(encoded.authorization_generation)
    .bind(to_i64(entered_at_unix_ms)?)
    .execute(&mut *transaction)
    .await
    .map_err(map_database_error)?;
    if update.rows_affected() == 0 {
        return Ok(false);
    }
    transaction.commit().await.map_err(map_database_error)?;
    Ok(true)
}

fn due_retirement_entered_at(
    current: &DeviceAuthorizationRecord,
    replacement: &DeviceAuthorizationRecord,
) -> Option<u64> {
    let (
        current_approval_id,
        current_approver,
        current_certificate,
        current_delivery_id,
        current_certificate_sha256,
    ) = match &current.state {
        DeviceAuthorizationState::DeliveryPending {
            approval_id,
            approver,
            certificate,
            delivery_id,
            certificate_sha256,
            ..
        } => (
            approval_id,
            approver,
            certificate,
            delivery_id,
            certificate_sha256,
        ),
        _ => return None,
    };
    let (
        next_approval_id,
        next_approver,
        next_certificate,
        next_delivery_id,
        next_certificate_sha256,
        entered_at_unix_ms,
    ) = match &replacement.state {
        DeviceAuthorizationState::RetirementPending {
            approval_id,
            approver,
            certificate,
            delivery_id: Some(delivery_id),
            certificate_sha256,
            reason: DeviceCertificateRetirementReason::DeliveryDeadlineReached,
            entered_at_unix_ms,
            last_failure: None,
        } => (
            approval_id,
            approver,
            certificate,
            delivery_id,
            certificate_sha256,
            entered_at_unix_ms,
        ),
        _ => return None,
    };
    if current_approval_id != next_approval_id
        || current_approver != next_approver
        || current_certificate != next_certificate
        || current_delivery_id != next_delivery_id
        || current_certificate_sha256 != next_certificate_sha256
        || to_i64(*entered_at_unix_ms).is_err()
    {
        return None;
    }
    Some(*entered_at_unix_ms)
}

fn valid_delivery_ack_transition(
    current: &DeviceAuthorizationRecord,
    replacement: &DeviceAuthorizationRecord,
) -> bool {
    match (&current.state, &replacement.state) {
        (
            DeviceAuthorizationState::DeliveryPending {
                approval_id: current_approval_id,
                delivery_id: current_delivery_id,
                certificate_sha256: current_certificate_sha256,
                delivery_deadline_unix_ms,
                ..
            },
            DeviceAuthorizationState::Delivered {
                approval_id: next_approval_id,
                receipt,
            },
        ) => {
            current_approval_id == next_approval_id
                && receipt.authorization_id == replacement.id
                && current_delivery_id == &receipt.delivery_id
                && current_certificate_sha256 == &receipt.certificate_sha256
                && receipt.csr_sha256 == current.csr_sha256
                && receipt.csr_spki_sha256 == current.spki_sha256
                && receipt.device_id == current.registration_binding.key().device_id
                && receipt.authorization_generation
                    == current.registration_binding.authorization_generation()
                && receipt.acknowledged_at_unix_ms < *delivery_deadline_unix_ms
                && to_i64(receipt.acknowledged_at_unix_ms).is_ok()
        }
        _ => false,
    }
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
        && current.registration_binding == replacement.registration_binding
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
                    | "authorizations_user_code_digest_unique"
                    | "authorizations_registration_binding_unique",
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
    registration_binding_id: Vec<u8>,
    device_id: String,
    authorization_generation: i64,
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
    state_deadline_unix_ms: Option<i64>,
    delivery_certificate_not_after_unix_ms: Option<i64>,
    state_payload: Vec<u8>,
}

impl EncodedRecord {
    fn new(record: &DeviceAuthorizationRecord) -> StoreResult<Self> {
        if !registration_binding_matches_record(record) {
            return Err(DeviceAuthorizationStoreError::Unavailable);
        }
        if record.csr_der.is_empty()
            || record.csr_der.len() > MAX_CSR_BYTES
            || sha256(&record.csr_der) != record.csr_sha256
            || record.created_at_unix_ms >= record.expires_at_unix_ms
            || record.poll_interval_ms == 0
        {
            return Err(DeviceAuthorizationStoreError::Unavailable);
        }
        let (version, mac) = record.user_code_digest.storage_parts();
        let stored_state = StoredAuthorizationState::from_domain(&record.state);
        if !stored_state.is_valid(
            &record.scope,
            &record.spki_sha256,
            &record.id,
            &record.csr_sha256,
        ) || !stored_state.matches_registration(&record.registration_binding)
        {
            return Err(DeviceAuthorizationStoreError::Unavailable);
        }
        let state_kind = stored_state.kind();
        let approval_id = stored_state.approval_id().map(|id| id.to_vec());
        let state_deadline_unix_ms = stored_state.deadline_unix_ms().map(to_i64).transpose()?;
        let delivery_certificate_not_after_unix_ms = stored_state
            .delivery_certificate_not_after_unix_ms()
            .map(to_i64)
            .transpose()?;
        let format_version = stored_state.format_version();
        let state_payload = serde_json::to_vec(&StoredState {
            format_version,
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
            registration_binding_id: record.registration_binding.binding_id().to_vec(),
            device_id: record.registration_binding.key().device_id.clone(),
            authorization_generation: to_i64(
                record.registration_binding.authorization_generation(),
            )?,
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
            state_deadline_unix_ms,
            delivery_certificate_not_after_unix_ms,
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
    let registration_binding = PersistedDirectoryBinding {
        binding_id: fixed::<16>(get!("registration_binding_id", Vec<u8>))?,
        organization_id: scope.organization_id.clone(),
        workspace_id: scope.workspace_id.clone(),
        device_id: get!("device_id", String),
        authorization_generation: from_i64(get!("authorization_generation", i64))?,
        csr_sha256: fixed::<32>(get!("csr_sha256", Vec<u8>))?,
        spki_sha256: fixed::<32>(get!("spki_sha256", Vec<u8>))?,
    };
    let registration_binding =
        DeviceAuthorizationRegistrationBinding::from_verified_directory_binding(
            &registration_binding,
        );
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
    let state_deadline_unix_ms = get!("state_deadline_unix_ms", Option<i64>)
        .map(from_i64)
        .transpose()?;
    let delivery_certificate_not_after_unix_ms =
        get!("delivery_certificate_not_after_unix_ms", Option<i64>)
            .map(from_i64)
            .transpose()?;
    let state_payload: Vec<u8> = get!("state_payload", Vec<u8>);
    if state_payload.is_empty()
        || state_payload.len() > MAX_STATE_PAYLOAD_BYTES
        || csr_der.is_empty()
        || csr_der.len() > MAX_CSR_BYTES
        || sha256(&csr_der) != csr_sha256
        || created_at_unix_ms >= expires_at_unix_ms
        || poll_interval_ms == 0
    {
        return Err(DeviceAuthorizationStoreError::Unavailable);
    }
    let wrapper: StoredState = serde_json::from_slice(&state_payload)
        .map_err(|_| DeviceAuthorizationStoreError::Unavailable)?;
    if !wrapper
        .state
        .supports_format_version(wrapper.format_version)
        || wrapper.state.kind() != state_kind
        || wrapper.state.approval_id() != approval_id
        || wrapper.state.deadline_unix_ms() != state_deadline_unix_ms
        || wrapper.state.delivery_certificate_not_after_unix_ms()
            != delivery_certificate_not_after_unix_ms
        || !wrapper
            .state
            .is_valid(&scope, &spki_sha256, &id, &csr_sha256)
        || !wrapper.state.matches_registration(&registration_binding)
    {
        return Err(DeviceAuthorizationStoreError::Unavailable);
    }

    let record = DeviceAuthorizationRecord {
        id,
        registration_binding,
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
    };
    if !registration_binding_matches_record(&record) {
        return Err(DeviceAuthorizationStoreError::Unavailable);
    }
    Ok(record)
}

/// Rehydrates the immutable binding snapshot retained with an authorization row.
/// Database reads remain untrusted until a registered operation validates them
/// against the current Directory rows under lock.
struct PersistedDirectoryBinding {
    binding_id: [u8; 16],
    organization_id: String,
    workspace_id: String,
    device_id: String,
    authorization_generation: u64,
    csr_sha256: [u8; 32],
    spki_sha256: [u8; 32],
}

impl VerifiedDirectoryRegistrationBinding for PersistedDirectoryBinding {
    fn binding_id(&self) -> &[u8; 16] {
        &self.binding_id
    }

    fn organization_id(&self) -> &str {
        &self.organization_id
    }

    fn workspace_id(&self) -> &str {
        &self.workspace_id
    }

    fn device_id(&self) -> &str {
        &self.device_id
    }

    fn authorization_generation(&self) -> u64 {
        self.authorization_generation
    }

    fn csr_sha256(&self) -> &[u8; 32] {
        &self.csr_sha256
    }

    fn spki_sha256(&self) -> &[u8; 32] {
        &self.spki_sha256
    }
}

fn registration_binding_matches_record(record: &DeviceAuthorizationRecord) -> bool {
    let binding = &record.registration_binding;
    binding.authorization_generation() > 0
        && !binding.key().device_id.trim().is_empty()
        && binding.key().organization_id == record.scope.organization_id
        && binding.key().workspace_id == record.scope.workspace_id
        && binding.csr_sha256() == &record.csr_sha256
        && binding.spki_sha256() == &record.spki_sha256
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
    DeliveryPending {
        approval_id: DeviceAuthorizationId,
        approver: StoredIdentity,
        decided_at_unix_ms: u64,
        certificate: StoredCertificate,
        delivery_id: DeviceAuthorizationId,
        certificate_sha256: [u8; 32],
        delivery_deadline_unix_ms: u64,
    },
    Delivered {
        approval_id: DeviceAuthorizationId,
        receipt: StoredDeliveryReceipt,
    },
    RetirementPending {
        approval_id: DeviceAuthorizationId,
        approver: StoredIdentity,
        certificate: StoredCertificate,
        certificate_sha256: [u8; 32],
        delivery_id: Option<DeviceAuthorizationId>,
        reason: StoredRetirementReason,
        entered_at_unix_ms: u64,
        last_failure: Option<StoredRetirementError>,
    },
    DeliveryExpired {
        approval_id: DeviceAuthorizationId,
        delivery_id: DeviceAuthorizationId,
        certificate_sha256: [u8; 32],
        expired_at_unix_ms: u64,
    },
    IssuanceFailed {
        approval_id: DeviceAuthorizationId,
        approver: StoredIdentity,
        failed_at_unix_ms: u64,
        failure: StoredIssuanceFailure,
        certificate_sha256: Option<[u8; 32]>,
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
struct StoredDeliveryReceipt {
    authorization_id: DeviceAuthorizationId,
    delivery_id: DeviceAuthorizationId,
    device_id: String,
    authorization_generation: u64,
    certificate_sha256: [u8; 32],
    csr_sha256: [u8; 32],
    csr_spki_sha256: [u8; 32],
    acknowledged_at_unix_ms: u64,
}

impl From<&DeviceCertificateDeliveryReceipt> for StoredDeliveryReceipt {
    fn from(receipt: &DeviceCertificateDeliveryReceipt) -> Self {
        Self {
            authorization_id: receipt.authorization_id,
            delivery_id: receipt.delivery_id,
            device_id: receipt.device_id.clone(),
            authorization_generation: receipt.authorization_generation,
            certificate_sha256: receipt.certificate_sha256,
            csr_sha256: receipt.csr_sha256,
            csr_spki_sha256: receipt.csr_spki_sha256,
            acknowledged_at_unix_ms: receipt.acknowledged_at_unix_ms,
        }
    }
}

impl From<StoredDeliveryReceipt> for DeviceCertificateDeliveryReceipt {
    fn from(receipt: StoredDeliveryReceipt) -> Self {
        Self {
            authorization_id: receipt.authorization_id,
            delivery_id: receipt.delivery_id,
            device_id: receipt.device_id,
            authorization_generation: receipt.authorization_generation,
            certificate_sha256: receipt.certificate_sha256,
            csr_sha256: receipt.csr_sha256,
            csr_spki_sha256: receipt.csr_spki_sha256,
            acknowledged_at_unix_ms: receipt.acknowledged_at_unix_ms,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StoredRetirementReason {
    MisboundCertificate,
    DeliveryDeadlineReached,
    CertificateExpiredBeforeDelivery,
}

impl From<DeviceCertificateRetirementReason> for StoredRetirementReason {
    fn from(reason: DeviceCertificateRetirementReason) -> Self {
        match reason {
            DeviceCertificateRetirementReason::MisboundCertificate => Self::MisboundCertificate,
            DeviceCertificateRetirementReason::DeliveryDeadlineReached => {
                Self::DeliveryDeadlineReached
            }
            DeviceCertificateRetirementReason::CertificateExpiredBeforeDelivery => {
                Self::CertificateExpiredBeforeDelivery
            }
        }
    }
}

impl From<StoredRetirementReason> for DeviceCertificateRetirementReason {
    fn from(reason: StoredRetirementReason) -> Self {
        match reason {
            StoredRetirementReason::MisboundCertificate => Self::MisboundCertificate,
            StoredRetirementReason::DeliveryDeadlineReached => Self::DeliveryDeadlineReached,
            StoredRetirementReason::CertificateExpiredBeforeDelivery => {
                Self::CertificateExpiredBeforeDelivery
            }
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StoredIssuanceFailure {
    SignerRejectedWithoutCommit,
    MisboundCertificateRetired,
    CertificateExpiredBeforeDeliveryRetired,
}

impl From<DeviceCertificateIssuanceFailure> for StoredIssuanceFailure {
    fn from(failure: DeviceCertificateIssuanceFailure) -> Self {
        match failure {
            DeviceCertificateIssuanceFailure::SignerRejectedWithoutCommit => {
                Self::SignerRejectedWithoutCommit
            }
            DeviceCertificateIssuanceFailure::MisboundCertificateRetired => {
                Self::MisboundCertificateRetired
            }
            DeviceCertificateIssuanceFailure::CertificateExpiredBeforeDeliveryRetired => {
                Self::CertificateExpiredBeforeDeliveryRetired
            }
        }
    }
}

impl From<StoredIssuanceFailure> for DeviceCertificateIssuanceFailure {
    fn from(failure: StoredIssuanceFailure) -> Self {
        match failure {
            StoredIssuanceFailure::SignerRejectedWithoutCommit => Self::SignerRejectedWithoutCommit,
            StoredIssuanceFailure::MisboundCertificateRetired => Self::MisboundCertificateRetired,
            StoredIssuanceFailure::CertificateExpiredBeforeDeliveryRetired => {
                Self::CertificateExpiredBeforeDeliveryRetired
            }
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StoredRetirementError {
    Rejected,
    OutcomeUnknown,
}

impl From<DeviceCertificateRetirementError> for StoredRetirementError {
    fn from(error: DeviceCertificateRetirementError) -> Self {
        match error {
            DeviceCertificateRetirementError::Rejected => Self::Rejected,
            DeviceCertificateRetirementError::OutcomeUnknown => Self::OutcomeUnknown,
        }
    }
}

impl From<StoredRetirementError> for DeviceCertificateRetirementError {
    fn from(error: StoredRetirementError) -> Self {
        match error {
            StoredRetirementError::Rejected => Self::Rejected,
            StoredRetirementError::OutcomeUnknown => Self::OutcomeUnknown,
        }
    }
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
struct StoredDeviceKey {
    organization_id: String,
    workspace_id: String,
    device_id: String,
}

impl From<&DeviceAuthorizationDeviceKey> for StoredDeviceKey {
    fn from(key: &DeviceAuthorizationDeviceKey) -> Self {
        Self {
            organization_id: key.organization_id.clone(),
            workspace_id: key.workspace_id.clone(),
            device_id: key.device_id.clone(),
        }
    }
}

impl StoredDeviceKey {
    fn into_domain(self) -> DeviceAuthorizationDeviceKey {
        DeviceAuthorizationDeviceKey {
            organization_id: self.organization_id,
            workspace_id: self.workspace_id,
            device_id: self.device_id,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredCertificate {
    certificate_der: Vec<u8>,
    ca_chain_der: Vec<Vec<u8>>,
    serial_number: Vec<u8>,
    registration_binding_id: [u8; 16],
    device_key: StoredDeviceKey,
    authorization_generation: u64,
    scope: StoredScope,
    spki_sha256: [u8; 32],
    not_after_unix_ms: u64,
}

impl StoredCertificate {
    fn from_domain(certificate: &IssuedDeviceCertificate) -> Self {
        Self {
            certificate_der: certificate.certificate_der.clone(),
            ca_chain_der: certificate.ca_chain_der.clone(),
            serial_number: certificate.serial_number.clone(),
            registration_binding_id: certificate.registration_binding_id,
            device_key: (&certificate.device_key).into(),
            authorization_generation: certificate.authorization_generation,
            scope: (&certificate.scope).into(),
            spki_sha256: certificate.spki_sha256,
            not_after_unix_ms: certificate.not_after_unix_ms,
        }
    }

    fn into_domain(self) -> IssuedDeviceCertificate {
        let scope = self.scope.into_domain();
        IssuedDeviceCertificate {
            certificate_der: self.certificate_der,
            ca_chain_der: self.ca_chain_der,
            serial_number: self.serial_number,
            registration_binding_id: self.registration_binding_id,
            device_key: self.device_key.into_domain(),
            authorization_generation: self.authorization_generation,
            scope,
            spki_sha256: self.spki_sha256,
            not_after_unix_ms: self.not_after_unix_ms,
        }
    }

    fn is_valid(&self, scope: &DeviceAuthorizationScope, spki_sha256: &[u8; 32]) -> bool {
        let chain_bytes = self
            .ca_chain_der
            .iter()
            .try_fold(self.certificate_der.len(), |total, certificate| {
                total.checked_add(certificate.len())
            });
        !self.certificate_der.is_empty()
            && self.certificate_der.len() <= MAX_CERTIFICATE_BYTES
            && self.ca_chain_der.len() <= MAX_CA_CHAIN_CERTIFICATES
            && self.ca_chain_der.iter().all(|certificate| {
                !certificate.is_empty() && certificate.len() <= MAX_CERTIFICATE_BYTES
            })
            && chain_bytes.is_some_and(|total| total <= MAX_CERTIFICATE_BUNDLE_BYTES)
            && !self.serial_number.is_empty()
            && self.serial_number.len() <= MAX_SERIAL_NUMBER_BYTES
            && !self.device_key.device_id.trim().is_empty()
            && self.device_key.organization_id == self.scope.organization_id
            && self.device_key.workspace_id == self.scope.workspace_id
            && self.authorization_generation > 0
            && self.scope.organization_id == scope.organization_id
            && self.scope.workspace_id == scope.workspace_id
            && &self.spki_sha256 == spki_sha256
    }

    fn matches_registration(&self, binding: &DeviceAuthorizationRegistrationBinding) -> bool {
        self.registration_binding_id == *binding.binding_id()
            && self.device_key.device_id == binding.key().device_id
            && self.authorization_generation == binding.authorization_generation()
            && self.scope.organization_id == binding.key().organization_id
            && self.scope.workspace_id == binding.key().workspace_id
            && self.device_key.organization_id == binding.key().organization_id
            && self.device_key.workspace_id == binding.key().workspace_id
            && self.spki_sha256 == *binding.spki_sha256()
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
            DeviceAuthorizationState::DeliveryPending {
                approval_id,
                approver,
                decided_at_unix_ms,
                certificate,
                delivery_id,
                certificate_sha256,
                delivery_deadline_unix_ms,
            } => Self::DeliveryPending {
                approval_id: *approval_id,
                approver: StoredIdentity::from_domain(approver),
                decided_at_unix_ms: *decided_at_unix_ms,
                certificate: StoredCertificate::from_domain(certificate),
                delivery_id: *delivery_id,
                certificate_sha256: *certificate_sha256,
                delivery_deadline_unix_ms: *delivery_deadline_unix_ms,
            },
            DeviceAuthorizationState::Delivered {
                approval_id,
                receipt,
            } => Self::Delivered {
                approval_id: *approval_id,
                receipt: receipt.into(),
            },
            DeviceAuthorizationState::RetirementPending {
                approval_id,
                approver,
                certificate,
                certificate_sha256,
                delivery_id,
                reason,
                entered_at_unix_ms,
                last_failure,
            } => Self::RetirementPending {
                approval_id: *approval_id,
                approver: StoredIdentity::from_domain(approver),
                certificate: StoredCertificate::from_domain(certificate),
                certificate_sha256: *certificate_sha256,
                delivery_id: *delivery_id,
                reason: (*reason).into(),
                entered_at_unix_ms: *entered_at_unix_ms,
                last_failure: last_failure.map(Into::into),
            },
            DeviceAuthorizationState::DeliveryExpired {
                approval_id,
                delivery_id,
                certificate_sha256,
                expired_at_unix_ms,
            } => Self::DeliveryExpired {
                approval_id: *approval_id,
                delivery_id: *delivery_id,
                certificate_sha256: *certificate_sha256,
                expired_at_unix_ms: *expired_at_unix_ms,
            },
            DeviceAuthorizationState::IssuanceFailed {
                approval_id,
                approver,
                failed_at_unix_ms,
                failure,
                certificate_sha256,
            } => Self::IssuanceFailed {
                approval_id: *approval_id,
                approver: StoredIdentity::from_domain(approver),
                failed_at_unix_ms: *failed_at_unix_ms,
                failure: (*failure).into(),
                certificate_sha256: *certificate_sha256,
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
            Self::DeliveryPending { .. } => "delivery_pending",
            Self::Delivered { .. } => "delivered",
            Self::RetirementPending { .. } => "retirement_pending",
            Self::DeliveryExpired { .. } => "delivery_expired",
            Self::IssuanceFailed { .. } => "issuance_failed",
            Self::Denied { .. } => "denied",
            Self::Consumed { .. } => "consumed",
            Self::Expired => "expired",
        }
    }

    fn format_version(&self) -> u8 {
        match self {
            Self::DeliveryPending { .. }
            | Self::Delivered { .. }
            | Self::RetirementPending { .. }
            | Self::DeliveryExpired { .. }
            | Self::IssuanceFailed { .. } => 2,
            _ => 1,
        }
    }

    fn supports_format_version(&self, format_version: u8) -> bool {
        match format_version {
            1 => self.format_version() == 1,
            2 => true,
            _ => false,
        }
    }

    fn approval_id(&self) -> Option<DeviceAuthorizationId> {
        match self {
            Self::AwaitingWebauthn { approval_id, .. }
            | Self::VerifyingWebauthn { approval_id, .. }
            | Self::Issuing { approval_id, .. }
            | Self::DeliveryPending { approval_id, .. }
            | Self::Delivered { approval_id, .. }
            | Self::RetirementPending { approval_id, .. }
            | Self::DeliveryExpired { approval_id, .. }
            | Self::IssuanceFailed { approval_id, .. } => Some(*approval_id),
            _ => None,
        }
    }

    fn deadline_unix_ms(&self) -> Option<u64> {
        match self {
            Self::DeliveryPending {
                delivery_deadline_unix_ms,
                ..
            } => Some(*delivery_deadline_unix_ms),
            _ => None,
        }
    }

    fn delivery_certificate_not_after_unix_ms(&self) -> Option<u64> {
        match self {
            Self::DeliveryPending { certificate, .. } => Some(certificate.not_after_unix_ms),
            _ => None,
        }
    }

    fn matches_registration(&self, binding: &DeviceAuthorizationRegistrationBinding) -> bool {
        match self {
            Self::DeliveryPending { certificate, .. }
            | Self::RetirementPending { certificate, .. } => {
                certificate.matches_registration(binding)
            }
            Self::Delivered { receipt, .. } => {
                receipt.device_id == binding.key().device_id
                    && receipt.authorization_generation == binding.authorization_generation()
            }
            Self::Pending
            | Self::AwaitingWebauthn { .. }
            | Self::VerifyingWebauthn { .. }
            | Self::Issuing { .. }
            | Self::DeliveryExpired { .. }
            | Self::IssuanceFailed { .. }
            | Self::Denied { .. }
            | Self::Consumed { .. }
            | Self::Expired => true,
        }
    }

    fn is_valid(
        &self,
        scope: &DeviceAuthorizationScope,
        spki_sha256: &[u8; 32],
        authorization_id: &DeviceAuthorizationId,
        csr_sha256: &[u8; 32],
    ) -> bool {
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
            Self::Issuing {
                approver,
                issued_at_unix_ms,
                ..
            }
            | Self::Denied {
                approver,
                decided_at_unix_ms: issued_at_unix_ms,
            } => approver.is_valid() && to_i64(*issued_at_unix_ms).is_ok(),
            Self::DeliveryPending {
                approver,
                certificate,
                decided_at_unix_ms,
                certificate_sha256,
                delivery_deadline_unix_ms,
                ..
            } => {
                approver.is_valid()
                    && certificate.is_valid(scope, spki_sha256)
                    && sha256(&certificate.certificate_der) == *certificate_sha256
                    && *delivery_deadline_unix_ms > *decided_at_unix_ms
                    && delivery_deadline_unix_ms.saturating_sub(*decided_at_unix_ms)
                        <= crate::device_authorization::MAX_CERTIFICATE_DELIVERY_TTL_MS
                    && *delivery_deadline_unix_ms <= certificate.not_after_unix_ms
                    && to_i64(*decided_at_unix_ms).is_ok()
                    && to_i64(*delivery_deadline_unix_ms).is_ok()
            }
            Self::Delivered { receipt, .. } => {
                receipt.authorization_id == *authorization_id
                    && !receipt.device_id.trim().is_empty()
                    && receipt.authorization_generation > 0
                    && receipt.csr_sha256 == *csr_sha256
                    && receipt.csr_spki_sha256 == *spki_sha256
                    && to_i64(receipt.acknowledged_at_unix_ms).is_ok()
            }
            Self::RetirementPending {
                approver,
                certificate,
                certificate_sha256,
                delivery_id,
                reason,
                entered_at_unix_ms,
                ..
            } => {
                approver.is_valid()
                    && certificate.is_valid(scope, spki_sha256)
                    && sha256(&certificate.certificate_der) == *certificate_sha256
                    && match reason {
                        StoredRetirementReason::DeliveryDeadlineReached => delivery_id.is_some(),
                        StoredRetirementReason::MisboundCertificate
                        | StoredRetirementReason::CertificateExpiredBeforeDelivery => {
                            delivery_id.is_none()
                        }
                    }
                    && to_i64(*entered_at_unix_ms).is_ok()
            }
            Self::DeliveryExpired {
                expired_at_unix_ms, ..
            } => to_i64(*expired_at_unix_ms).is_ok(),
            Self::IssuanceFailed {
                approver,
                failed_at_unix_ms,
                failure,
                certificate_sha256,
                ..
            } => {
                approver.is_valid()
                    && to_i64(*failed_at_unix_ms).is_ok()
                    && match failure {
                        StoredIssuanceFailure::SignerRejectedWithoutCommit => {
                            certificate_sha256.is_none()
                        }
                        StoredIssuanceFailure::MisboundCertificateRetired
                        | StoredIssuanceFailure::CertificateExpiredBeforeDeliveryRetired => {
                            certificate_sha256.is_some()
                        }
                    }
            }
            Self::Consumed {
                approver,
                consumed_at_unix_ms,
                decided_at_unix_ms,
            } => {
                approver.is_valid()
                    && consumed_at_unix_ms >= decided_at_unix_ms
                    && to_i64(*decided_at_unix_ms).is_ok()
                    && to_i64(*consumed_at_unix_ms).is_ok()
            }
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
            Self::DeliveryPending {
                approval_id,
                approver,
                decided_at_unix_ms,
                certificate,
                delivery_id,
                certificate_sha256,
                delivery_deadline_unix_ms,
            } => DeviceAuthorizationState::DeliveryPending {
                approval_id,
                approver: approver.into_domain()?,
                decided_at_unix_ms,
                certificate: certificate.into_domain(),
                delivery_id,
                certificate_sha256,
                delivery_deadline_unix_ms,
            },
            Self::Delivered {
                approval_id,
                receipt,
            } => DeviceAuthorizationState::Delivered {
                approval_id,
                receipt: receipt.into(),
            },
            Self::RetirementPending {
                approval_id,
                approver,
                certificate,
                certificate_sha256,
                delivery_id,
                reason,
                entered_at_unix_ms,
                last_failure,
            } => DeviceAuthorizationState::RetirementPending {
                approval_id,
                approver: approver.into_domain()?,
                certificate: certificate.into_domain(),
                certificate_sha256,
                delivery_id,
                reason: reason.into(),
                entered_at_unix_ms,
                last_failure: last_failure.map(Into::into),
            },
            Self::DeliveryExpired {
                approval_id,
                delivery_id,
                certificate_sha256,
                expired_at_unix_ms,
            } => DeviceAuthorizationState::DeliveryExpired {
                approval_id,
                delivery_id,
                certificate_sha256,
                expired_at_unix_ms,
            },
            Self::IssuanceFailed {
                approval_id,
                approver,
                failed_at_unix_ms,
                failure,
                certificate_sha256,
            } => DeviceAuthorizationState::IssuanceFailed {
                approval_id,
                approver: approver.into_domain()?,
                failed_at_unix_ms,
                failure: failure.into(),
                certificate_sha256,
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
    use std::time::{SystemTime, UNIX_EPOCH};

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

    fn registration_binding(
        binding_id: [u8; 16],
        device_id: String,
        authorization_generation: u64,
        csr_sha256: [u8; 32],
        spki_sha256: [u8; 32],
    ) -> DeviceAuthorizationRegistrationBinding {
        DeviceAuthorizationRegistrationBinding::test_fixture(
            binding_id,
            DeviceAuthorizationDeviceKey {
                organization_id: scope().organization_id,
                workspace_id: scope().workspace_id,
                device_id,
            },
            authorization_generation,
            csr_sha256,
            spki_sha256,
        )
    }

    fn device_id_for(id: &DeviceAuthorizationId) -> String {
        id.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn certificate() -> IssuedDeviceCertificate {
        IssuedDeviceCertificate {
            certificate_der: vec![0x30, 0x03, 0x01, 0x02, 0x03],
            ca_chain_der: vec![vec![0x30, 0x01, 0x00]],
            serial_number: vec![1, 2, 3],
            registration_binding_id: [15; 16],
            device_key: DeviceAuthorizationDeviceKey {
                organization_id: scope().organization_id,
                workspace_id: scope().workspace_id,
                device_id: "device-test".to_owned(),
            },
            authorization_generation: 1,
            scope: scope(),
            spki_sha256: [4; 32],
            not_after_unix_ms: 9_000,
        }
    }

    fn current_unix_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_millis()
            .try_into()
            .expect("current time fits in u64")
    }

    fn random_bytes<const N: usize>() -> [u8; N] {
        let mut value = [0; N];
        getrandom::getrandom(&mut value).expect("test randomness");
        value
    }

    fn pending_record(
        id: DeviceAuthorizationId,
        approval_id: DeviceAuthorizationId,
        delivery_id: DeviceAuthorizationId,
        user_code_mac: [u8; 32],
        decided_at_unix_ms: u64,
        delivery_deadline_unix_ms: u64,
    ) -> DeviceAuthorizationRecord {
        let csr_der = vec![1, 2, 3, 4];
        let csr_sha256 = sha256(&csr_der);
        let device_id = device_id_for(&id);
        let registration_binding =
            registration_binding(id, device_id.clone(), 1, csr_sha256, [4; 32]);
        let certificate = IssuedDeviceCertificate {
            certificate_der: vec![0x30, 0x03, 0x05, id[0], id[1]],
            ca_chain_der: vec![vec![0x30, 0x01, 0x00]],
            serial_number: vec![id[0], id[1]],
            registration_binding_id: id,
            device_key: registration_binding.key().clone(),
            authorization_generation: 1,
            scope: scope(),
            spki_sha256: [4; 32],
            not_after_unix_ms: delivery_deadline_unix_ms.saturating_add(1_000),
        };
        let certificate_sha256 = sha256(&certificate.certificate_der);
        let now = current_unix_ms();
        DeviceAuthorizationRecord {
            id,
            registration_binding,
            device_code_hash: sha256(&id),
            user_code_digest: VersionedUserCodeDigest::from_storage_parts(1, user_code_mac)
                .expect("versioned digest"),
            scope: scope(),
            csr_sha256,
            csr_der,
            spki_sha256: [4; 32],
            created_at_unix_ms: now.saturating_sub(10_000),
            expires_at_unix_ms: now.saturating_add(600_000),
            poll_interval_ms: 5_000,
            last_poll_at_unix_ms: None,
            revision: 0,
            state: DeviceAuthorizationState::DeliveryPending {
                approval_id,
                approver: identity(),
                decided_at_unix_ms,
                certificate,
                delivery_id,
                certificate_sha256,
                delivery_deadline_unix_ms,
            },
        }
    }

    fn set_application_role_login(database_url: &str, login: bool) {
        let options = PgConnectOptions::from_str(database_url).expect("valid admin URL");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("admin test runtime");
        runtime.block_on(async {
            let pool = PgPoolOptions::new()
                .max_connections(1)
                .connect_with(options)
                .await
                .expect("connect as migration role");
            let statement = if login {
                "ALTER ROLE cyrene_workspace_device_authorization_app LOGIN"
            } else {
                "ALTER ROLE cyrene_workspace_device_authorization_app NOLOGIN"
            };
            sqlx::query(statement)
                .execute(&pool)
                .await
                .expect("toggle application login for local trust test");
            pool.close().await;
        });
    }

    #[test]
    fn every_state_round_trips() {
        let approval_id = [7; 16];
        let authorization_id = [9; 16];
        let csr_sha256 = [6; 32];
        let certificate_sha256 = sha256(&certificate().certificate_der);
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
            DeviceAuthorizationState::DeliveryPending {
                approval_id,
                approver: identity(),
                decided_at_unix_ms: 4_000,
                certificate: certificate(),
                delivery_id: [8; 16],
                certificate_sha256,
                delivery_deadline_unix_ms: 6_000,
            },
            DeviceAuthorizationState::Delivered {
                approval_id,
                receipt: DeviceCertificateDeliveryReceipt {
                    authorization_id,
                    delivery_id: [8; 16],
                    device_id: "device-test".to_owned(),
                    authorization_generation: 1,
                    certificate_sha256,
                    csr_sha256,
                    csr_spki_sha256: [4; 32],
                    acknowledged_at_unix_ms: 5_000,
                },
            },
            DeviceAuthorizationState::RetirementPending {
                approval_id,
                approver: identity(),
                certificate: certificate(),
                certificate_sha256,
                delivery_id: Some([8; 16]),
                reason: DeviceCertificateRetirementReason::DeliveryDeadlineReached,
                entered_at_unix_ms: 6_000,
                last_failure: Some(DeviceCertificateRetirementError::OutcomeUnknown),
            },
            DeviceAuthorizationState::DeliveryExpired {
                approval_id,
                delivery_id: [8; 16],
                certificate_sha256,
                expired_at_unix_ms: 6_000,
            },
            DeviceAuthorizationState::IssuanceFailed {
                approval_id,
                approver: identity(),
                failed_at_unix_ms: 6_000,
                failure: DeviceCertificateIssuanceFailure::SignerRejectedWithoutCommit,
                certificate_sha256: None,
            },
            DeviceAuthorizationState::IssuanceFailed {
                approval_id,
                approver: identity(),
                failed_at_unix_ms: 6_000,
                failure: DeviceCertificateIssuanceFailure::MisboundCertificateRetired,
                certificate_sha256: Some(certificate_sha256),
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
            let format_version = stored.format_version();
            let encoded = serde_json::to_vec(&StoredState {
                format_version,
                state: stored,
            })
            .expect("state serialization");
            let decoded: StoredState = serde_json::from_slice(&encoded).expect("state parsing");
            assert!(decoded
                .state
                .supports_format_version(decoded.format_version));
            assert_eq!(
                decoded.state.kind(),
                StoredAuthorizationState::from_domain(&state).kind()
            );
            assert!(decoded
                .state
                .is_valid(&scope(), &[4; 32], &authorization_id, &csr_sha256));
            assert_eq!(
                decoded.state.deadline_unix_ms(),
                StoredAuthorizationState::from_domain(&state).deadline_unix_ms()
            );
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

        let pending_delivery =
            StoredAuthorizationState::from_domain(&DeviceAuthorizationState::DeliveryPending {
                approval_id: [13; 16],
                approver: identity(),
                decided_at_unix_ms: 4_000,
                certificate: certificate(),
                delivery_id: [14; 16],
                certificate_sha256: sha256(&certificate().certificate_der),
                delivery_deadline_unix_ms: 6_000,
            });
        assert_eq!(pending_delivery.kind(), "delivery_pending");
        assert_eq!(pending_delivery.approval_id(), Some([13; 16]));
        assert_eq!(pending_delivery.deadline_unix_ms(), Some(6_000));
        assert_eq!(
            pending_delivery.delivery_certificate_not_after_unix_ms(),
            Some(9_000)
        );

        let expired = StoredAuthorizationState::Expired;
        assert_eq!(expired.kind(), "expired");
        assert_eq!(expired.approval_id(), None);
        assert_eq!(expired.delivery_certificate_not_after_unix_ms(), None);
    }

    #[test]
    fn persisted_certificate_cannot_diverge_from_directory_binding() {
        let mut record = pending_record([21; 16], [22; 16], [23; 16], [24; 32], 4_000, 6_000);
        assert!(EncodedRecord::new(&record).is_ok());
        let DeviceAuthorizationState::DeliveryPending { certificate, .. } = &mut record.state
        else {
            unreachable!();
        };
        certificate.registration_binding_id = [25; 16];
        assert!(matches!(
            EncodedRecord::new(&record),
            Err(DeviceAuthorizationStoreError::Unavailable)
        ));
    }

    #[test]
    fn due_retirement_cas_requires_exact_delivery_projection() {
        let current = pending_record([31; 16], [32; 16], [33; 16], [34; 32], 4_000, 6_000);
        let DeviceAuthorizationState::DeliveryPending {
            approval_id,
            approver,
            certificate,
            delivery_id,
            certificate_sha256,
            ..
        } = &current.state
        else {
            unreachable!();
        };
        let mut replacement = current.clone();
        replacement.revision += 1;
        replacement.state = DeviceAuthorizationState::RetirementPending {
            approval_id: *approval_id,
            approver: approver.clone(),
            certificate: certificate.clone(),
            certificate_sha256: *certificate_sha256,
            delivery_id: Some(*delivery_id),
            reason: DeviceCertificateRetirementReason::DeliveryDeadlineReached,
            entered_at_unix_ms: 6_000,
            last_failure: None,
        };
        assert_eq!(
            due_retirement_entered_at(&current, &replacement),
            Some(6_000)
        );

        let DeviceAuthorizationState::RetirementPending { certificate, .. } =
            &mut replacement.state
        else {
            unreachable!();
        };
        certificate.device_key.device_id.push_str("-other");
        assert_eq!(due_retirement_entered_at(&current, &replacement), None);
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
        assert!(!state.is_valid(&scope(), &[4; 32], &[1; 16], &[6; 32]));
    }

    #[test]
    fn certificate_chain_payload_bounds_are_enforced() {
        let mut value = certificate();
        value.ca_chain_der = vec![vec![7; MAX_CERTIFICATE_BUNDLE_BYTES]];
        let state =
            StoredAuthorizationState::from_domain(&DeviceAuthorizationState::RetirementPending {
                approval_id: [2; 16],
                approver: identity(),
                certificate: value,
                certificate_sha256: sha256(&certificate().certificate_der),
                delivery_id: None,
                reason: DeviceCertificateRetirementReason::MisboundCertificate,
                entered_at_unix_ms: 4_000,
                last_failure: None,
            });
        assert!(!state.is_valid(&scope(), &[4; 32], &[1; 16], &[6; 32]));
    }

    #[test]
    fn delivery_ack_requires_matching_receipt_and_open_deadline() {
        let current = DeviceAuthorizationRecord {
            id: [9; 16],
            registration_binding: registration_binding(
                [15; 16],
                "device-test".to_owned(),
                1,
                [6; 32],
                [4; 32],
            ),
            device_code_hash: [2; 32],
            user_code_digest: VersionedUserCodeDigest::from_storage_parts(1, [3; 32])
                .expect("versioned digest"),
            scope: scope(),
            csr_der: vec![1, 2, 3],
            csr_sha256: [6; 32],
            spki_sha256: [4; 32],
            created_at_unix_ms: 1,
            expires_at_unix_ms: 10_000,
            poll_interval_ms: 1_000,
            last_poll_at_unix_ms: None,
            revision: 4,
            state: DeviceAuthorizationState::DeliveryPending {
                approval_id: [7; 16],
                approver: identity(),
                decided_at_unix_ms: 4_000,
                certificate: certificate(),
                delivery_id: [8; 16],
                certificate_sha256: sha256(&certificate().certificate_der),
                delivery_deadline_unix_ms: 6_000,
            },
        };
        let mut replacement = current.clone();
        replacement.revision = 5;
        replacement.state = DeviceAuthorizationState::Delivered {
            approval_id: [7; 16],
            receipt: DeviceCertificateDeliveryReceipt {
                authorization_id: current.id,
                delivery_id: [8; 16],
                device_id: "device-test".to_owned(),
                authorization_generation: 1,
                certificate_sha256: sha256(&certificate().certificate_der),
                csr_sha256: current.csr_sha256,
                csr_spki_sha256: current.spki_sha256,
                acknowledged_at_unix_ms: 5_999,
            },
        };
        assert!(valid_delivery_ack_transition(&current, &replacement));

        let DeviceAuthorizationState::Delivered { receipt, .. } = &mut replacement.state else {
            unreachable!();
        };
        receipt.acknowledged_at_unix_ms = 6_000;
        assert!(!valid_delivery_ack_transition(&current, &replacement));
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

        let delivery_migration =
            include_str!("../migrations/device_authorization/0002_delivery_ack.up.sql");
        assert!(delivery_migration.contains("state_deadline_unix_ms BIGINT"));
        assert!(delivery_migration.contains("authorizations_delivery_recovery_idx"));
        assert!(delivery_migration.contains("authorizations_retirement_recovery_idx"));
        assert!(delivery_migration.contains("WHERE state_kind = 'approved'"));
        assert!(!delivery_migration.contains("user_code TEXT"));
        assert!(!delivery_migration.contains("device_code TEXT"));

        let registration_migration = include_str!(
            "../migrations/device_authorization/0003_directory_registration_fence.up.sql"
        );
        assert!(registration_migration.contains("delivery_certificate_not_after_unix_ms BIGINT"));
        assert!(registration_migration.contains("(delivery_certificate_not_after_unix_ms, id)"));
        assert!(!registration_migration.contains("convert_from(state_payload"));
    }

    #[test]
    #[ignore = "requires an ephemeral PostgreSQL 17 server with verify-full TLS"]
    fn postgres_migrations_and_ack_deadline_are_transactional() {
        let migration_url =
            std::env::var("CYRENE_WORKSPACE_DEVICE_AUTHORIZATION_TEST_MIGRATOR_URL")
                .expect("test migrator URL configured");
        let app_url = std::env::var("CYRENE_WORKSPACE_DEVICE_AUTHORIZATION_TEST_APP_URL")
            .expect("test app URL configured");

        PostgresDeviceAuthorizationStore::migrate(&migration_url)
            .expect("apply authorization migrations with verify-full TLS");
        set_application_role_login(&migration_url, true);
        let store = PostgresDeviceAuthorizationStore::connect(&app_url)
            .expect("connect with the restricted runtime role");

        let now = current_unix_ms();
        let open_deadline = now.saturating_add(90_000);
        let open_record = pending_record(
            random_bytes(),
            random_bytes(),
            random_bytes(),
            random_bytes(),
            now,
            open_deadline,
        );
        let open_hash = open_record.device_code_hash;
        let open_user_digest = open_record.user_code_digest;
        let open_delivery_id = match &open_record.state {
            DeviceAuthorizationState::DeliveryPending { delivery_id, .. } => *delivery_id,
            _ => unreachable!(),
        };
        store
            .insert(open_record.clone())
            .expect("insert pending delivery");
        assert_eq!(
            store.user_code_key_versions().expect("read HMAC versions"),
            vec![1]
        );
        assert_eq!(
            store
                .by_device_code_hash(&open_hash)
                .expect("read by device hash"),
            Some(open_record.clone())
        );
        assert_eq!(
            store
                .by_user_code_candidates(&[open_user_digest])
                .expect("read by user-code MAC"),
            Some(open_record.clone())
        );

        let (open_approval_id, open_certificate_sha256) = match &open_record.state {
            DeviceAuthorizationState::DeliveryPending {
                approval_id,
                certificate_sha256,
                ..
            } => (*approval_id, *certificate_sha256),
            _ => unreachable!(),
        };
        let mut acknowledged = open_record.clone();
        acknowledged.revision = 1;
        acknowledged.state = DeviceAuthorizationState::Delivered {
            approval_id: open_approval_id,
            receipt: DeviceCertificateDeliveryReceipt {
                authorization_id: open_record.id,
                delivery_id: open_delivery_id,
                device_id: open_record.registration_binding.key().device_id.clone(),
                authorization_generation: open_record
                    .registration_binding
                    .authorization_generation(),
                certificate_sha256: open_certificate_sha256,
                csr_sha256: open_record.csr_sha256,
                csr_spki_sha256: open_record.spki_sha256,
                acknowledged_at_unix_ms: now,
            },
        };
        store
            .compare_and_swap_delivery_ack(0, acknowledged.clone())
            .expect("database-clock guarded ACK before the deadline");
        assert_eq!(
            store
                .by_device_code_hash(&open_hash)
                .expect("read acknowledged delivery"),
            Some(acknowledged)
        );

        let expired_deadline = now.saturating_sub(1_000);
        let expired_record = pending_record(
            random_bytes(),
            random_bytes(),
            random_bytes(),
            random_bytes(),
            now.saturating_sub(5_000),
            expired_deadline,
        );
        let expired_hash = expired_record.device_code_hash;
        store
            .insert(expired_record.clone())
            .expect("insert expired pending delivery");
        let mut late_ack = expired_record.clone();
        late_ack.revision = 1;
        let DeviceAuthorizationState::DeliveryPending {
            approval_id,
            delivery_id,
            certificate_sha256,
            ..
        } = &expired_record.state
        else {
            unreachable!();
        };
        late_ack.state = DeviceAuthorizationState::Delivered {
            approval_id: *approval_id,
            receipt: DeviceCertificateDeliveryReceipt {
                authorization_id: expired_record.id,
                delivery_id: *delivery_id,
                device_id: expired_record.registration_binding.key().device_id.clone(),
                authorization_generation: expired_record
                    .registration_binding
                    .authorization_generation(),
                certificate_sha256: *certificate_sha256,
                csr_sha256: expired_record.csr_sha256,
                csr_spki_sha256: expired_record.spki_sha256,
                acknowledged_at_unix_ms: expired_deadline.saturating_sub(1),
            },
        };
        assert_eq!(
            store.compare_and_swap(0, late_ack.clone()),
            Err(DeviceAuthorizationStoreError::Conflict),
            "generic CAS must not bypass the ACK path"
        );
        assert_eq!(
            store.compare_and_swap_delivery_ack(0, late_ack),
            Err(DeviceAuthorizationStoreError::Conflict),
            "database time must reject an ACK after its persisted deadline"
        );
        assert!(store
            .due_certificate_deliveries(now, 10)
            .expect("scan expired delivery deadlines")
            .contains(&expired_record));
        assert_eq!(
            store
                .by_device_code_hash(&expired_hash)
                .expect("read expired delivery"),
            Some(expired_record)
        );

        let mut retirement_record = pending_record(
            random_bytes(),
            random_bytes(),
            random_bytes(),
            random_bytes(),
            now.saturating_sub(2_000),
            now.saturating_add(60_000),
        );
        let (approval_id, certificate, certificate_sha256) = match &retirement_record.state {
            DeviceAuthorizationState::DeliveryPending {
                approval_id,
                certificate,
                certificate_sha256,
                ..
            } => (*approval_id, certificate.clone(), *certificate_sha256),
            _ => unreachable!(),
        };
        retirement_record.state = DeviceAuthorizationState::RetirementPending {
            approval_id,
            approver: identity(),
            certificate,
            certificate_sha256,
            delivery_id: None,
            reason: DeviceCertificateRetirementReason::MisboundCertificate,
            entered_at_unix_ms: now,
            last_failure: None,
        };
        store
            .insert(retirement_record.clone())
            .expect("insert recoverable retirement");
        assert!(store
            .recoverable_retirements(10)
            .expect("scan retirement recovery")
            .contains(&retirement_record));
        drop(store);
        set_application_role_login(&migration_url, false);
    }
}
