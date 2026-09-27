//! PostgreSQL Workspace device-certificate registry.
//!
//! A certificate enters this registry as inactive metadata. The only path to
//! `active` re-reads a durable Delivered receipt and the current Directory
//! generation while holding row locks. Ordinary import is deliberately
//! refused because it cannot prove device acknowledgement.
//!
//! Relay dispatch guards retain one PostgreSQL transaction on the worker. The
//! worker services only the guard-release channel until that transaction commits,
//! so queued revocations cannot overtake an admitted frame.
//!
//! PostgreSQL Workspace 设备证书注册表。
//!
//! 证书先以非活动元数据入库；只有在行锁保护下重新核对持久 Delivered 回执和当前
//! Directory 代次后才能激活。普通导入接口无法证明设备 ACK，因此在该适配器中拒绝。

use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest, Sha256};
use sqlx::migrate::Migrator;
use sqlx::pool::PoolConnection;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgRow, PgSslMode};
use sqlx::{PgPool, Postgres, Row, Transaction};
use thiserror::Error;
use uuid::Uuid;

use crate::device_authorization::DeviceAuthorizationId;
use crate::device_registry::{
    ApprovedWorkspaceDeviceCertificate, DeviceAuthorizationStatus,
    WorkspaceDeviceCertificateIdentity, WorkspaceDeviceDispatchFence, WorkspaceDeviceKey,
    WorkspaceDeviceRecord, WorkspaceDeviceRegistry,
};
use crate::directory::WorkspaceDirectoryError;

const DATABASE_URL_ENV: &str = "CYRENE_WORKSPACE_DEVICE_REGISTRY_DATABASE_URL";
const MIGRATION_DATABASE_URL_ENV: &str = "CYRENE_WORKSPACE_DEVICE_REGISTRY_MIGRATION_DATABASE_URL";
const REGISTRY_TABLE: &str = "cyrene_workspace_device_registry.certificate_records";
const AUTHORIZATION_TABLE: &str = "cyrene_workspace_device_authorization.authorizations";
const DIRECTORY_BINDINGS_TABLE: &str = "cyrene_workspace_directory.device_registration_bindings";
const DIRECTORY_IDENTITIES_TABLE: &str = "cyrene_workspace_directory.workspace_device_identities";
const QUEUE_CAPACITY: usize = 32;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);
const DATABASE_TIMEOUT: Duration = Duration::from_secs(5);
const STARTUP_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_SCAN_LIMIT: usize = 1_000;
const MAX_STATE_PAYLOAD_BYTES: usize = 32 * 1024 * 1024;
const MAX_CERTIFICATE_DER_BYTES: usize = 16 * 1024;
const MAX_CHAIN_CERTIFICATES: usize = 8;
const MAX_CHAIN_DER_BYTES: usize = 128 * 1024;
const MAX_SERIAL_NUMBER_BYTES: usize = 256;
const CERTIFICATE_VALIDATION_VERSION: u8 = 1;
const CERTIFICATE_VALIDATION_FRESHNESS_MS: u64 = 30_000;

static MIGRATOR: Migrator = sqlx::migrate!("./migrations/device_registry");

type RegistryResult<T> = Result<T, DeviceRegistryPostgresError>;
type Reply<T> = SyncSender<RegistryResult<T>>;

/// Stable connection or storage failure from the PostgreSQL registry.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum DeviceRegistryPostgresError {
    /// The configured URL is missing or malformed.
    #[error("Workspace device registry database configuration is invalid")]
    Configuration,
    /// The database, required schemas, or durable authorization evidence is unavailable.
    #[error("Workspace device registry is unavailable")]
    Unavailable,
}

/// Result of attempting to activate one staged certificate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceCertificateRegistryActivation {
    /// A durable Delivered ACK and current Directory generation were verified and committed.
    Activated,
    /// The exact receipt had already been activated; the retry is idempotent.
    AlreadyActive,
    /// The authorization has not reached durable Delivered state yet.
    AwaitingAcknowledgement,
    /// The certificate expired, became stale, or reached a terminal authorization state.
    Ineligible,
    /// No durable staging record exists for this authorization.
    NotStaged,
}

/// PostgreSQL implementation of the Workspace device certificate registry.
///
/// This adapter uses a dedicated bounded worker thread because the existing
/// `WorkspaceDeviceRegistry` port is synchronous. Configure separate runtime
/// and migration credentials. The migration must run only after Directory
/// binding and authorization schema V2 are installed.
pub struct PostgresWorkspaceDeviceRegistry {
    sender: Option<SyncSender<Command>>,
    release_sender: Option<Sender<RegistryWorkerControl>>,
    active_fences: Arc<AtomicUsize>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl PostgresWorkspaceDeviceRegistry {
    /// Connect using a trusted runtime database URL and validate dependent schemas.
    pub fn connect(database_url: &str) -> RegistryResult<Self> {
        Self::start(database_url, false)
    }

    /// Read the runtime database URL from server configuration.
    pub fn connect_from_environment() -> RegistryResult<Self> {
        let database_url = std::env::var(DATABASE_URL_ENV)
            .map_err(|_| DeviceRegistryPostgresError::Configuration)?;
        Self::connect(&database_url)
    }

    /// Apply registry migrations using a separately provisioned operator URL.
    pub fn migrate(database_url: &str) -> RegistryResult<()> {
        let registry = Self::start(database_url, true)?;
        drop(registry);
        Ok(())
    }

    /// Apply registry migrations with the configured operator credential.
    pub fn migrate_from_environment() -> RegistryResult<()> {
        let database_url = std::env::var(MIGRATION_DATABASE_URL_ENV)
            .map_err(|_| DeviceRegistryPostgresError::Configuration)?;
        Self::migrate(&database_url)
    }

    /// Persist an inactive snapshot before the certificate is returned to a device.
    ///
    /// Repeating this call for the same authorization and exact immutable snapshot is
    /// safe. A different snapshot or a stale Directory generation is rejected.
    pub fn stage_pending_delivery(
        &self,
        authorization_id: &DeviceAuthorizationId,
    ) -> RegistryResult<()> {
        let (reply, result) = mpsc::sync_channel(1);
        self.call(Command::Stage(*authorization_id, reply), result)
    }

    /// Activate an already staged certificate only after durable ACK validation.
    ///
    /// This method can be retried after a crash between the authorization ACK
    /// transaction and registry activation. It never treats a DeliveryPending
    /// row as active.
    pub fn activate_acknowledged_delivery(
        &self,
        authorization_id: &DeviceAuthorizationId,
    ) -> RegistryResult<DeviceCertificateRegistryActivation> {
        let (reply, result) = mpsc::sync_channel(1);
        self.call(Command::Activate(*authorization_id, reply), result)
    }

    /// Retry activation of a bounded set of staged records after process recovery.
    pub fn reconcile_pending_deliveries(&self, limit: usize) -> RegistryResult<usize> {
        let (reply, result) = mpsc::sync_channel(1);
        self.call(Command::Reconcile(limit.min(MAX_SCAN_LIMIT), reply), result)
    }

    fn start(database_url: &str, apply_migrations: bool) -> RegistryResult<Self> {
        let options = PgConnectOptions::from_str(database_url)
            .map_err(|_| DeviceRegistryPostgresError::Configuration)?
            .ssl_mode(PgSslMode::VerifyFull)
            .application_name("cyrene-workspace-device-registry")
            .options([("statement_timeout", "5000"), ("lock_timeout", "3000")]);
        let (sender, commands) = mpsc::sync_channel(QUEUE_CAPACITY);
        let (release_sender, releases) = mpsc::channel();
        let active_fences = Arc::new(AtomicUsize::new(0));
        let worker_active_fences = Arc::clone(&active_fences);
        let (ready_sender, ready) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("workspace-device-registry-postgres".to_owned())
            .spawn(move || {
                worker_main(
                    options,
                    commands,
                    releases,
                    worker_active_fences,
                    ready_sender,
                    apply_migrations,
                )
            })
            .map_err(|_| DeviceRegistryPostgresError::Unavailable)?;

        match ready.recv_timeout(STARTUP_TIMEOUT) {
            Ok(Ok(())) => Ok(Self {
                sender: Some(sender),
                release_sender: Some(release_sender),
                active_fences,
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
                Err(DeviceRegistryPostgresError::Unavailable)
            }
        }
    }

    fn call<T>(&self, command: Command, reply: Receiver<RegistryResult<T>>) -> RegistryResult<T> {
        let sender = self
            .sender
            .as_ref()
            .ok_or(DeviceRegistryPostgresError::Unavailable)?;
        match sender.try_send(command) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                return Err(DeviceRegistryPostgresError::Unavailable)
            }
        }
        reply
            .recv_timeout(REQUEST_TIMEOUT)
            .map_err(|_| DeviceRegistryPostgresError::Unavailable)?
    }
}

impl Drop for PostgresWorkspaceDeviceRegistry {
    fn drop(&mut self) {
        self.sender.take();
        self.release_sender.take();
        if let Ok(mut worker) = self.worker.lock() {
            if let Some(worker) = worker.take() {
                if self.active_fences.load(Ordering::Acquire) == 0 {
                    let _ = worker.join();
                }
            }
        }
    }
}

impl WorkspaceDeviceRegistry for PostgresWorkspaceDeviceRegistry {
    fn import_approved_device_certificate(
        &self,
        _certificate: ApprovedWorkspaceDeviceCertificate,
    ) -> Result<WorkspaceDeviceRecord, WorkspaceDirectoryError> {
        Err(WorkspaceDirectoryError::Identity(
            "PostgreSQL registry requires a durable device delivery ACK".to_owned(),
        ))
    }

    fn revoke_device(
        &self,
        key: &WorkspaceDeviceKey,
    ) -> Result<WorkspaceDeviceRecord, WorkspaceDirectoryError> {
        let (reply, result) = mpsc::sync_channel(1);
        self.call(Command::Revoke(key.clone(), reply), result)
            .map_err(to_directory_error)
    }

    fn find_device(
        &self,
        key: &WorkspaceDeviceKey,
    ) -> Result<Option<WorkspaceDeviceRecord>, WorkspaceDirectoryError> {
        let (reply, result) = mpsc::sync_channel(1);
        self.call(Command::FindByKey(key.clone(), reply), result)
            .map_err(to_directory_error)
    }

    fn find_device_by_certificate_fingerprint(
        &self,
        fingerprint_sha256: &str,
    ) -> Result<Option<WorkspaceDeviceRecord>, WorkspaceDirectoryError> {
        if !is_sha256_hex(fingerprint_sha256) {
            return Ok(None);
        }
        let (reply, result) = mpsc::sync_channel(1);
        self.call(
            Command::FindByFingerprint(fingerprint_sha256.to_owned(), reply),
            result,
        )
        .map_err(to_directory_error)
    }

    fn find_current_device_certificate_identity(
        &self,
        fingerprint_sha256: &str,
    ) -> Result<Option<WorkspaceDeviceCertificateIdentity>, WorkspaceDirectoryError> {
        if !is_sha256_hex(fingerprint_sha256) {
            return Ok(None);
        }
        let (reply, result) = mpsc::sync_channel(1);
        self.call(
            Command::FindCurrentCertificateIdentity(fingerprint_sha256.to_owned(), reply),
            result,
        )
        .map_err(to_directory_error)
    }

    fn acquire_relay_dispatch_fence(
        &self,
        expected: &WorkspaceDeviceCertificateIdentity,
    ) -> Result<Box<dyn WorkspaceDeviceDispatchFence>, WorkspaceDirectoryError> {
        let release_sender = self.release_sender.as_ref().cloned().ok_or_else(|| {
            WorkspaceDirectoryError::Storage("Workspace device registry is unavailable".to_owned())
        })?;
        let fence = PostgresRelayDispatchFence {
            fence_id: None,
            release_sender,
        };
        let (reply, result) = mpsc::sync_channel(1);
        match self
            .call(
                Command::AcquireRelayDispatchFence(expected.clone(), fence, reply),
                result,
            )
            .map_err(to_directory_error)?
        {
            Some(fence) => Ok(Box::new(fence)),
            None => Err(WorkspaceDirectoryError::Identity(
                "Workspace device certificate is no longer current".to_owned(),
            )),
        }
    }
}

struct PostgresRelayDispatchFence {
    fence_id: Option<u64>,
    release_sender: Sender<RegistryWorkerControl>,
}

impl Drop for PostgresRelayDispatchFence {
    fn drop(&mut self) {
        if let Some(fence_id) = self.fence_id {
            let _ = self
                .release_sender
                .send(RegistryWorkerControl::ReleaseDispatchFence(fence_id));
        }
    }
}

impl WorkspaceDeviceDispatchFence for PostgresRelayDispatchFence {}

/// Checked-out pool connection retained by the Registry worker between dispatch lock and release.
///
/// Normal paths consume it by explicitly returning committed connections or closing failed ones
/// inside the Tokio runtime. If the worker unwinds unexpectedly while retaining the connection,
/// the fallback Drop enters that runtime before dropping PoolConnection so SQLx can safely
/// schedule its return-to-pool cleanup.
struct RuntimeFenceConnection {
    connection: Option<PoolConnection<Postgres>>,
    runtime: tokio::runtime::Handle,
}

impl RuntimeFenceConnection {
    fn new(connection: PoolConnection<Postgres>, runtime: tokio::runtime::Handle) -> Self {
        Self {
            connection: Some(connection),
            runtime,
        }
    }
}

impl Drop for RuntimeFenceConnection {
    fn drop(&mut self) {
        if let Some(connection) = self.connection.take() {
            let _runtime_context = self.runtime.enter();
            drop(connection);
        }
    }
}

fn to_directory_error(error: DeviceRegistryPostgresError) -> WorkspaceDirectoryError {
    match error {
        DeviceRegistryPostgresError::Configuration | DeviceRegistryPostgresError::Unavailable => {
            WorkspaceDirectoryError::Storage("Workspace device registry is unavailable".to_owned())
        }
    }
}

enum Command {
    Stage(DeviceAuthorizationId, Reply<()>),
    Activate(
        DeviceAuthorizationId,
        Reply<DeviceCertificateRegistryActivation>,
    ),
    Reconcile(usize, Reply<usize>),
    Revoke(WorkspaceDeviceKey, Reply<WorkspaceDeviceRecord>),
    FindByKey(WorkspaceDeviceKey, Reply<Option<WorkspaceDeviceRecord>>),
    FindByFingerprint(String, Reply<Option<WorkspaceDeviceRecord>>),
    FindCurrentCertificateIdentity(String, Reply<Option<WorkspaceDeviceCertificateIdentity>>),
    AcquireRelayDispatchFence(
        WorkspaceDeviceCertificateIdentity,
        PostgresRelayDispatchFence,
        Reply<Option<PostgresRelayDispatchFence>>,
    ),
}

enum RegistryWorkerControl {
    ReleaseDispatchFence(u64),
}

fn worker_main(
    options: PgConnectOptions,
    commands: Receiver<Command>,
    releases: Receiver<RegistryWorkerControl>,
    active_fences: Arc<AtomicUsize>,
    ready: SyncSender<RegistryResult<()>>,
    apply_migrations: bool,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => {
            let _ = ready.send(Err(DeviceRegistryPostgresError::Unavailable));
            return;
        }
    };
    let pool = match runtime.block_on(initialize_pool(options, apply_migrations)) {
        Ok(pool) => pool,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    if ready.send(Ok(())).is_err() {
        return;
    }

    let mut active_fence: Option<(u64, RuntimeFenceConnection)> = None;
    let mut next_fence_id = 1_u64;
    loop {
        if let Some((active_id, _)) = active_fence.as_ref() {
            // The single pool connection is fenced. Release/COMMIT stays on this
            // worker and always runs before a queued revocation or Registry call.
            match releases.recv() {
                Ok(RegistryWorkerControl::ReleaseDispatchFence(released_id))
                    if released_id == *active_id =>
                {
                    if let Some((_, connection)) = active_fence.take() {
                        // Return explicitly while the worker runtime is driving SQLx. The
                        // connection then has no live checkout lease, so its later Drop cannot
                        // schedule pool work from this plain worker thread.
                        let _ = runtime.block_on(finish_and_return_dispatch_fence(connection));
                        active_fences.fetch_sub(1, Ordering::Release);
                    }
                }
                Ok(RegistryWorkerControl::ReleaseDispatchFence(_)) => {}
                Err(_) => {
                    if let Some((_, connection)) = active_fence.take() {
                        let _ =
                            runtime.block_on(finish_and_close_dispatch_fence(connection, false));
                        active_fences.fetch_sub(1, Ordering::Release);
                    }
                    return;
                }
            }
            continue;
        }

        let command = match commands.recv() {
            Ok(command) => command,
            Err(_) => return,
        };
        match command {
            Command::Stage(id, reply) => {
                let _ = reply.send(runtime.block_on(stage_pending_delivery(&pool, &id)));
            }
            Command::Activate(id, reply) => {
                let _ = reply.send(runtime.block_on(activate_acknowledged_delivery(&pool, &id)));
            }
            Command::Reconcile(limit, reply) => {
                let _ = reply.send(runtime.block_on(reconcile_pending_deliveries(&pool, limit)));
            }
            Command::Revoke(key, reply) => {
                let _ = reply.send(runtime.block_on(revoke_device(&pool, &key)));
            }
            Command::FindByKey(key, reply) => {
                let _ = reply.send(runtime.block_on(find_device(&pool, &key)));
            }
            Command::FindByFingerprint(fingerprint, reply) => {
                let _ =
                    reply.send(runtime.block_on(find_device_by_fingerprint(&pool, &fingerprint)));
            }
            Command::FindCurrentCertificateIdentity(fingerprint, reply) => {
                let _ = reply.send(runtime.block_on(find_current_device_certificate_identity(
                    &pool,
                    &fingerprint,
                )));
            }
            Command::AcquireRelayDispatchFence(expected, mut fence, reply) => {
                match runtime.block_on(acquire_relay_dispatch_fence(&pool, &expected)) {
                    Ok(Some(connection)) => {
                        let connection =
                            RuntimeFenceConnection::new(connection, runtime.handle().clone());
                        let Some(next_id) = next_fence_id.checked_add(1) else {
                            let _ = runtime
                                .block_on(finish_and_close_dispatch_fence(connection, false));
                            let _ = reply.send(Err(DeviceRegistryPostgresError::Unavailable));
                            continue;
                        };
                        let fence_id = next_fence_id;
                        next_fence_id = next_id;
                        fence.fence_id = Some(fence_id);
                        active_fence = Some((fence_id, connection));
                        active_fences.fetch_add(1, Ordering::AcqRel);
                        // Sending the RAII lease transfers release responsibility to the
                        // caller. If a timed-out receiver already disappeared, either send
                        // returns the lease or dropping the queued reply drops it; both paths
                        // enqueue release so the worker cannot remain fenced indefinitely.
                        let _ = reply.send(Ok(Some(fence)));
                    }
                    Ok(None) => {
                        let _ = reply.send(Ok(None));
                    }
                    Err(error) => {
                        let _ = reply.send(Err(error));
                    }
                }
            }
        }
    }
}

async fn acquire_relay_dispatch_fence(
    pool: &PgPool,
    expected: &WorkspaceDeviceCertificateIdentity,
) -> RegistryResult<Option<PoolConnection<Postgres>>> {
    let generation = i64::try_from(expected.authorization_generation)
        .map_err(|_| DeviceRegistryPostgresError::Unavailable)?;
    let not_after_unix_ms = i64::try_from(expected.not_after_unix_ms)
        .map_err(|_| DeviceRegistryPostgresError::Unavailable)?;
    let fingerprint = unhex_32(&expected.certificate_fingerprint_sha256)
        .ok_or(DeviceRegistryPostgresError::Unavailable)?;
    let binding_id = Uuid::from_bytes(expected.registration_binding_id);
    // An earlier ordinary query may have dropped its PoolConnection and queued
    // SQLx's asynchronous return-to-pool task just before this command arrived.
    // Await acquisition so the runtime can drive that task; keep the wait bounded.
    let mut connection = tokio::time::timeout(DATABASE_TIMEOUT, pool.acquire())
        .await
        .map_err(|_| DeviceRegistryPostgresError::Unavailable)?
        .map_err(|_| DeviceRegistryPostgresError::Unavailable)?;
    if sqlx::query("BEGIN")
        .execute(&mut *connection)
        .await
        .is_err()
    {
        let _ = connection.close().await;
        return Err(DeviceRegistryPostgresError::Unavailable);
    }

    let check = sqlx::query(
        "SELECT cyrene_workspace_device_registry.relay_dispatch_fence(\
             $1, $2, $3, $4, $5, $6, $7, $8, $9, $10\
         ) AS admitted",
    )
    .bind(&expected.key.organization_id)
    .bind(&expected.key.workspace_id)
    .bind(&expected.key.device_id)
    .bind(binding_id)
    .bind(generation)
    .bind(fingerprint.as_slice())
    .bind(expected.csr_sha256.as_slice())
    .bind(expected.spki_sha256.as_slice())
    .bind(&expected.serial_number)
    .bind(not_after_unix_ms)
    .fetch_one(&mut *connection)
    .await;

    match check {
        Ok(row) => {
            let admitted: bool = match row.try_get("admitted") {
                Ok(admitted) => admitted,
                Err(_) => {
                    let _ = finish_and_close_pool_connection(connection, false).await;
                    return Err(DeviceRegistryPostgresError::Unavailable);
                }
            };
            if admitted {
                Ok(Some(connection))
            } else {
                if !finish_and_close_pool_connection(connection, true).await {
                    return Err(DeviceRegistryPostgresError::Unavailable);
                }
                Ok(None)
            }
        }
        Err(_) => {
            let _ = finish_and_close_pool_connection(connection, false).await;
            Err(DeviceRegistryPostgresError::Unavailable)
        }
    }
}

async fn finish_and_close_dispatch_fence(
    mut connection: RuntimeFenceConnection,
    commit: bool,
) -> bool {
    let Some(pool_connection) = connection.connection.as_mut() else {
        return false;
    };
    let finished = finish_dispatch_fence(pool_connection, commit).await;
    let Some(pool_connection) = connection.connection.take() else {
        return false;
    };
    let closed = pool_connection.close().await.is_ok();
    finished && closed
}

async fn finish_and_return_dispatch_fence(mut connection: RuntimeFenceConnection) -> bool {
    let finished = match connection.connection.as_mut() {
        Some(pool_connection) => finish_dispatch_fence(pool_connection, true).await,
        None => return false,
    };
    if !finished {
        if let Some(pool_connection) = connection.connection.take() {
            let _ = pool_connection.close().await;
        }
        return false;
    }
    let Some(pool_connection) = connection.connection.as_mut() else {
        return false;
    };
    pool_connection.return_to_pool().await;
    true
}

async fn finish_and_close_pool_connection(
    mut connection: PoolConnection<Postgres>,
    commit: bool,
) -> bool {
    let finished = finish_dispatch_fence(&mut connection, commit).await;
    let closed = connection.close().await.is_ok();
    finished && closed
}

async fn finish_dispatch_fence(connection: &mut PoolConnection<Postgres>, commit: bool) -> bool {
    let command = if commit { "COMMIT" } else { "ROLLBACK" };
    if sqlx::query(command)
        .execute(&mut **connection)
        .await
        .is_ok()
    {
        return true;
    }
    if commit {
        let _ = sqlx::query("ROLLBACK").execute(&mut **connection).await;
    }
    false
}

async fn initialize_pool(
    options: PgConnectOptions,
    apply_migrations: bool,
) -> RegistryResult<PgPool> {
    let connect = PgPoolOptions::new()
        .max_connections(1)
        .min_connections(0)
        .acquire_timeout(DATABASE_TIMEOUT)
        .after_connect(|connection, _| {
            Box::pin(async move {
                sqlx::query("SET search_path TO cyrene_workspace_device_registry, pg_catalog")
                    .execute(connection)
                    .await?;
                Ok(())
            })
        })
        .connect_with(options);
    let pool = tokio::time::timeout(DATABASE_TIMEOUT, connect)
        .await
        .map_err(|_| DeviceRegistryPostgresError::Unavailable)?
        .map_err(|_| DeviceRegistryPostgresError::Unavailable)?;

    if apply_migrations {
        sqlx::query("CREATE SCHEMA IF NOT EXISTS cyrene_workspace_device_registry")
            .execute(&pool)
            .await
            .map_err(|_| DeviceRegistryPostgresError::Unavailable)?;
        tokio::time::timeout(DATABASE_TIMEOUT, MIGRATOR.run(&pool))
            .await
            .map_err(|_| DeviceRegistryPostgresError::Unavailable)?
            .map_err(|_| DeviceRegistryPostgresError::Unavailable)?;
    }
    verify_schema(&pool).await?;
    Ok(pool)
}

async fn verify_schema(pool: &PgPool) -> RegistryResult<()> {
    for statement in [
        format!("SELECT authorization_id, registration_binding_id, organization_id, workspace_id, device_id, authorization_generation, delivery_id, certificate_sha256, csr_sha256, spki_sha256, serial_number, not_after_unix_ms, delivery_deadline_unix_ms, state, acknowledged_at_unix_ms, activated_at FROM {REGISTRY_TABLE} LIMIT 0"),
        format!("SELECT registration_binding_id, device_id, authorization_generation, organization_id, workspace_id, csr_sha256, spki_sha256, approval_id, state_kind, state_deadline_unix_ms, state_payload FROM {AUTHORIZATION_TABLE} LIMIT 0"),
        format!("SELECT binding_id, organization_id, workspace_id, device_id, authorization_generation, csr_sha256, spki_sha256 FROM {DIRECTORY_BINDINGS_TABLE} LIMIT 0"),
        format!("SELECT organization_id, workspace_id, device_id, current_authorization_generation FROM {DIRECTORY_IDENTITIES_TABLE} LIMIT 0"),
    ] {
        sqlx::query(&statement)
            .fetch_all(pool)
            .await
            .map_err(|_| DeviceRegistryPostgresError::Unavailable)?;
    }
    let dispatch_fence_available: bool = sqlx::query_scalar(
        "SELECT to_regprocedure(\
            'cyrene_workspace_device_registry.relay_dispatch_fence(\
                text, text, text, uuid, bigint, bytea, bytea, bytea, bytea, bigint\
            )'\
        ) IS NOT NULL",
    )
    .fetch_one(pool)
    .await
    .map_err(|_| DeviceRegistryPostgresError::Unavailable)?;
    if !dispatch_fence_available {
        return Err(DeviceRegistryPostgresError::Unavailable);
    }
    let validation_gate_available: bool = sqlx::query_scalar(
        "SELECT to_regprocedure(\
            'cyrene_workspace_device_registry.certificate_validation_matches(\
                jsonb, bytea, uuid\
            )'\
        ) IS NOT NULL AND to_regprocedure(\
            'cyrene_workspace_device_registry.certificate_validation_is_fresh(\
                jsonb, bytea, uuid, bigint\
            )'\
        ) IS NOT NULL",
    )
    .fetch_one(pool)
    .await
    .map_err(|_| DeviceRegistryPostgresError::Unavailable)?;
    if !validation_gate_available {
        return Err(DeviceRegistryPostgresError::Unavailable);
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AuthorizationSnapshot {
    id: DeviceAuthorizationId,
    binding_id: Uuid,
    key: WorkspaceDeviceKey,
    generation: u64,
    csr_sha256: [u8; 32],
    spki_sha256: [u8; 32],
    approval_id: Option<DeviceAuthorizationId>,
    state_kind: String,
    state_deadline_unix_ms: Option<u64>,
    state_payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DeliverySnapshot {
    authorization_id: DeviceAuthorizationId,
    binding_id: Uuid,
    key: WorkspaceDeviceKey,
    generation: u64,
    delivery_id: DeviceAuthorizationId,
    certificate_sha256: [u8; 32],
    csr_sha256: [u8; 32],
    spki_sha256: [u8; 32],
    serial_number: Vec<u8>,
    not_after_unix_ms: u64,
    delivery_deadline_unix_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RegistryState {
    PendingAck,
    Active,
    Revoked,
    Expired,
    Stale,
    Ineligible,
}

impl RegistryState {
    fn parse(value: &str) -> RegistryResult<Self> {
        match value {
            "pending_ack" => Ok(Self::PendingAck),
            "active" => Ok(Self::Active),
            "revoked" => Ok(Self::Revoked),
            "expired" => Ok(Self::Expired),
            "stale" => Ok(Self::Stale),
            "ineligible" => Ok(Self::Ineligible),
            _ => Err(DeviceRegistryPostgresError::Unavailable),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RegistrySnapshot {
    delivery: DeliverySnapshot,
    state: RegistryState,
    acknowledged_at_unix_ms: Option<u64>,
}

impl RegistrySnapshot {
    fn record(&self) -> WorkspaceDeviceRecord {
        WorkspaceDeviceRecord {
            key: self.delivery.key.clone(),
            certificate_fingerprint_sha256: hex(&self.delivery.certificate_sha256),
            authorization_status: if self.state == RegistryState::Active {
                DeviceAuthorizationStatus::Approved
            } else {
                DeviceAuthorizationStatus::Revoked
            },
        }
    }

    fn certificate_identity(&self) -> WorkspaceDeviceCertificateIdentity {
        WorkspaceDeviceCertificateIdentity {
            key: self.delivery.key.clone(),
            certificate_fingerprint_sha256: hex(&self.delivery.certificate_sha256),
            registration_binding_id: *self.delivery.binding_id.as_bytes(),
            authorization_generation: self.delivery.generation,
            csr_sha256: self.delivery.csr_sha256,
            spki_sha256: self.delivery.spki_sha256,
            serial_number: self.delivery.serial_number.clone(),
            not_after_unix_ms: self.delivery.not_after_unix_ms,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredEnvelope {
    format_version: u8,
    state: StoredAuthorizationState,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum StoredAuthorizationState {
    DeliveryPending {
        approval_id: DeviceAuthorizationId,
        certificate: StoredCertificate,
        certificate_validation: StoredCertificateValidation,
        delivery_id: DeviceAuthorizationId,
        certificate_sha256: [u8; 32],
        delivery_deadline_unix_ms: u64,
    },
    Delivered {
        approval_id: DeviceAuthorizationId,
        receipt: StoredReceipt,
        certificate_validation: StoredCertificateValidation,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredCertificateValidation {
    validation_version: u8,
    certificate_sha256: [u8; 32],
    registration_binding_id: DeviceAuthorizationId,
    checked_at_unix_ms: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredCertificate {
    certificate_der: Vec<u8>,
    ca_chain_der: Vec<Vec<u8>>,
    serial_number: Vec<u8>,
    registration_binding_id: DeviceAuthorizationId,
    device_key: StoredDeviceKey,
    authorization_generation: u64,
    scope: StoredScope,
    spki_sha256: [u8; 32],
    not_after_unix_ms: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredDeviceKey {
    organization_id: String,
    workspace_id: String,
    device_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredScope {
    organization_id: String,
    workspace_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredReceipt {
    authorization_id: DeviceAuthorizationId,
    delivery_id: DeviceAuthorizationId,
    device_id: String,
    authorization_generation: u64,
    certificate_sha256: [u8; 32],
    csr_sha256: [u8; 32],
    csr_spki_sha256: [u8; 32],
    acknowledged_at_unix_ms: u64,
}

fn decode_authorization(row: sqlx::postgres::PgRow) -> RegistryResult<AuthorizationSnapshot> {
    let binding_id: Vec<u8> = row
        .try_get("registration_binding_id")
        .map_err(|_| DeviceRegistryPostgresError::Unavailable)?;
    let binding_id =
        Uuid::from_slice(&binding_id).map_err(|_| DeviceRegistryPostgresError::Unavailable)?;
    let generation: i64 = row
        .try_get("authorization_generation")
        .map_err(|_| DeviceRegistryPostgresError::Unavailable)?;
    let state_deadline: Option<i64> = row
        .try_get("state_deadline_unix_ms")
        .map_err(|_| DeviceRegistryPostgresError::Unavailable)?;
    let state_payload: Vec<u8> = row
        .try_get("state_payload")
        .map_err(|_| DeviceRegistryPostgresError::Unavailable)?;
    let approval_id: Option<Vec<u8>> = row
        .try_get("approval_id")
        .map_err(|_| DeviceRegistryPostgresError::Unavailable)?;
    if state_payload.is_empty() || state_payload.len() > MAX_STATE_PAYLOAD_BYTES {
        return Err(DeviceRegistryPostgresError::Unavailable);
    }
    Ok(AuthorizationSnapshot {
        id: fixed(
            row.try_get("id")
                .map_err(|_| DeviceRegistryPostgresError::Unavailable)?,
        )?,
        binding_id,
        key: WorkspaceDeviceKey {
            organization_id: row
                .try_get("organization_id")
                .map_err(|_| DeviceRegistryPostgresError::Unavailable)?,
            workspace_id: row
                .try_get("workspace_id")
                .map_err(|_| DeviceRegistryPostgresError::Unavailable)?,
            device_id: row
                .try_get("device_id")
                .map_err(|_| DeviceRegistryPostgresError::Unavailable)?,
        },
        generation: positive_u64(generation)?,
        csr_sha256: fixed(
            row.try_get("csr_sha256")
                .map_err(|_| DeviceRegistryPostgresError::Unavailable)?,
        )?,
        spki_sha256: fixed(
            row.try_get("spki_sha256")
                .map_err(|_| DeviceRegistryPostgresError::Unavailable)?,
        )?,
        approval_id: approval_id.map(fixed::<16>).transpose()?,
        state_kind: row
            .try_get("state_kind")
            .map_err(|_| DeviceRegistryPostgresError::Unavailable)?,
        state_deadline_unix_ms: state_deadline.map(positive_u64).transpose()?,
        state_payload,
    })
}

fn decode_delivery_with_validation(
    auth: &AuthorizationSnapshot,
) -> RegistryResult<(DeliverySnapshot, StoredCertificateValidation)> {
    if auth.state_kind != "delivery_pending" {
        return Err(DeviceRegistryPostgresError::Unavailable);
    }
    let envelope: StoredEnvelope = serde_json::from_slice(&auth.state_payload)
        .map_err(|_| DeviceRegistryPostgresError::Unavailable)?;
    if envelope.format_version != 5 {
        return Err(DeviceRegistryPostgresError::Unavailable);
    }
    let StoredAuthorizationState::DeliveryPending {
        approval_id,
        certificate,
        certificate_validation,
        delivery_id,
        certificate_sha256,
        delivery_deadline_unix_ms,
    } = envelope.state
    else {
        return Err(DeviceRegistryPostgresError::Unavailable);
    };
    if auth.approval_id != Some(approval_id) {
        return Err(DeviceRegistryPostgresError::Unavailable);
    }
    if certificate_validation.validation_version != CERTIFICATE_VALIDATION_VERSION
        || certificate_validation.certificate_sha256 != certificate_sha256
        || certificate_validation.registration_binding_id != *auth.binding_id.as_bytes()
        || certificate_validation.checked_at_unix_ms == 0
    {
        return Err(DeviceRegistryPostgresError::Unavailable);
    }
    let expected_binding = *auth.binding_id.as_bytes();
    let certificate_hash: [u8; 32] = Sha256::digest(&certificate.certificate_der).into();
    let delivery_deadline = auth
        .state_deadline_unix_ms
        .ok_or(DeviceRegistryPostgresError::Unavailable)?;
    let chain_bytes = certificate
        .ca_chain_der
        .iter()
        .try_fold(0usize, |total, chain| total.checked_add(chain.len()));
    if certificate.certificate_der.is_empty()
        || certificate.certificate_der.len() > MAX_CERTIFICATE_DER_BYTES
        || certificate.serial_number.is_empty()
        || certificate.serial_number.len() > MAX_SERIAL_NUMBER_BYTES
        || certificate.ca_chain_der.len() > MAX_CHAIN_CERTIFICATES
        || certificate
            .ca_chain_der
            .iter()
            .any(|chain| chain.is_empty() || chain.len() > MAX_CERTIFICATE_DER_BYTES)
        || chain_bytes.is_none_or(|total| total > MAX_CHAIN_DER_BYTES)
        || certificate.registration_binding_id != expected_binding
        || certificate.device_key.organization_id != auth.key.organization_id
        || certificate.device_key.workspace_id != auth.key.workspace_id
        || certificate.device_key.device_id != auth.key.device_id
        || certificate.scope.organization_id != auth.key.organization_id
        || certificate.scope.workspace_id != auth.key.workspace_id
        || certificate.authorization_generation != auth.generation
        || certificate.spki_sha256 != auth.spki_sha256
        || certificate_hash != certificate_sha256
        || delivery_deadline_unix_ms != delivery_deadline
        || delivery_deadline == 0
        || certificate.not_after_unix_ms == 0
    {
        return Err(DeviceRegistryPostgresError::Unavailable);
    }
    Ok((
        DeliverySnapshot {
            authorization_id: auth.id,
            binding_id: auth.binding_id,
            key: auth.key.clone(),
            generation: auth.generation,
            delivery_id,
            certificate_sha256,
            csr_sha256: auth.csr_sha256,
            spki_sha256: auth.spki_sha256,
            serial_number: certificate.serial_number,
            not_after_unix_ms: certificate.not_after_unix_ms,
            delivery_deadline_unix_ms: delivery_deadline,
        },
        certificate_validation,
    ))
}

fn decode_receipt_with_validation(
    auth: &AuthorizationSnapshot,
) -> RegistryResult<(StoredReceipt, StoredCertificateValidation)> {
    if auth.state_kind != "delivered" {
        return Err(DeviceRegistryPostgresError::Unavailable);
    }
    let envelope: StoredEnvelope = serde_json::from_slice(&auth.state_payload)
        .map_err(|_| DeviceRegistryPostgresError::Unavailable)?;
    if envelope.format_version != 5 {
        return Err(DeviceRegistryPostgresError::Unavailable);
    }
    let StoredAuthorizationState::Delivered {
        approval_id,
        receipt,
        certificate_validation,
    } = envelope.state
    else {
        return Err(DeviceRegistryPostgresError::Unavailable);
    };
    if auth.approval_id != Some(approval_id) {
        return Err(DeviceRegistryPostgresError::Unavailable);
    }
    if certificate_validation.validation_version != CERTIFICATE_VALIDATION_VERSION
        || certificate_validation.certificate_sha256 != receipt.certificate_sha256
        || certificate_validation.registration_binding_id != *auth.binding_id.as_bytes()
        || certificate_validation.checked_at_unix_ms == 0
    {
        return Err(DeviceRegistryPostgresError::Unavailable);
    }
    Ok((receipt, certificate_validation))
}

fn receipt_matches(delivery: &DeliverySnapshot, receipt: &StoredReceipt, now: u64) -> bool {
    receipt.authorization_id == delivery.authorization_id
        && receipt.delivery_id == delivery.delivery_id
        && receipt.device_id == delivery.key.device_id
        && receipt.authorization_generation == delivery.generation
        && receipt.certificate_sha256 == delivery.certificate_sha256
        && receipt.csr_sha256 == delivery.csr_sha256
        && receipt.csr_spki_sha256 == delivery.spki_sha256
        && receipt.acknowledged_at_unix_ms < delivery.delivery_deadline_unix_ms
        && receipt.acknowledged_at_unix_ms <= now
        && receipt.acknowledged_at_unix_ms < delivery.not_after_unix_ms
}

fn certificate_validation_is_fresh(checked_at_unix_ms: u64, now_unix_ms: u64) -> bool {
    checked_at_unix_ms <= now_unix_ms
        && now_unix_ms - checked_at_unix_ms <= CERTIFICATE_VALIDATION_FRESHNESS_MS
}

fn delivery_matches_registry(delivery: &DeliverySnapshot, registry: &RegistrySnapshot) -> bool {
    delivery == &registry.delivery
}

fn authorization_matches_delivery_identity(
    auth: &AuthorizationSnapshot,
    delivery: &DeliverySnapshot,
) -> bool {
    auth.id == delivery.authorization_id
        && auth.binding_id == delivery.binding_id
        && auth.key == delivery.key
        && auth.generation == delivery.generation
        && auth.csr_sha256 == delivery.csr_sha256
        && auth.spki_sha256 == delivery.spki_sha256
}

async fn directory_binding_matches_delivery(
    transaction: &mut Transaction<'_, Postgres>,
    delivery: &DeliverySnapshot,
) -> RegistryResult<bool> {
    let generation = to_i64(delivery.generation)?;
    let query = format!(
        "SELECT EXISTS (\
            SELECT 1 FROM {DIRECTORY_IDENTITIES_TABLE} i \
            JOIN {DIRECTORY_BINDINGS_TABLE} b ON b.binding_id = $1 \
            WHERE i.organization_id = $2 AND i.workspace_id = $3 \
              AND i.device_id = $4 AND i.current_authorization_generation = $5 \
              AND b.organization_id = $2 AND b.workspace_id = $3 \
              AND b.device_id = $4 AND b.authorization_generation = $5 \
              AND b.csr_sha256 = $6 AND b.spki_sha256 = $7\
        )"
    );
    sqlx::query_scalar(&query)
        .bind(delivery.binding_id)
        .bind(&delivery.key.organization_id)
        .bind(&delivery.key.workspace_id)
        .bind(&delivery.key.device_id)
        .bind(generation)
        .bind(delivery.csr_sha256.as_slice())
        .bind(delivery.spki_sha256.as_slice())
        .fetch_one(&mut **transaction)
        .await
        .map_err(|_| DeviceRegistryPostgresError::Unavailable)
}

async fn stage_pending_delivery(
    pool: &PgPool,
    authorization_id: &DeviceAuthorizationId,
) -> RegistryResult<()> {
    let mut transaction = pool
        .begin()
        .await
        .map_err(|_| DeviceRegistryPostgresError::Unavailable)?;
    let auth = select_authorization_unlocked(&mut transaction, authorization_id)
        .await?
        .ok_or(DeviceRegistryPostgresError::Unavailable)?;
    if auth.id != *authorization_id {
        return Err(DeviceRegistryPostgresError::Unavailable);
    }
    let now = database_now_ms(&mut transaction).await?;

    if let Some(existing) = select_registry(&mut transaction, authorization_id, true).await? {
        let reusable = match auth.state_kind.as_str() {
            "delivery_pending" => {
                let (delivery, certificate_validation) = decode_delivery_with_validation(&auth)?;
                delivery_matches_registry(&delivery, &existing)
                    && existing.state == RegistryState::PendingAck
                    && existing.acknowledged_at_unix_ms.is_none()
                    && delivery.delivery_deadline_unix_ms > now
                    && delivery.not_after_unix_ms > now
                    && certificate_validation.checked_at_unix_ms <= now
                    && directory_binding_matches_delivery(&mut transaction, &delivery).await?
            }
            "delivered" => {
                let (receipt, certificate_validation) = decode_receipt_with_validation(&auth)?;
                authorization_matches_delivery_identity(&auth, &existing.delivery)
                    && receipt_matches(&existing.delivery, &receipt, now)
                    && certificate_validation.checked_at_unix_ms <= now
                    && existing.delivery.not_after_unix_ms > now
                    && match existing.state {
                        RegistryState::PendingAck => existing.acknowledged_at_unix_ms.is_none(),
                        RegistryState::Active => {
                            existing.acknowledged_at_unix_ms
                                == Some(receipt.acknowledged_at_unix_ms)
                        }
                        _ => false,
                    }
                    && directory_binding_matches_delivery(&mut transaction, &existing.delivery)
                        .await?
            }
            _ => false,
        };
        if !reusable {
            return Err(DeviceRegistryPostgresError::Unavailable);
        }
        return transaction
            .commit()
            .await
            .map_err(|_| DeviceRegistryPostgresError::Unavailable);
    }

    let (delivery, certificate_validation) = decode_delivery_with_validation(&auth)?;
    if delivery.delivery_deadline_unix_ms <= now
        || delivery.not_after_unix_ms <= now
        || !certificate_validation_is_fresh(certificate_validation.checked_at_unix_ms, now)
        || !directory_binding_matches_delivery(&mut transaction, &delivery).await?
    {
        return Err(DeviceRegistryPostgresError::Unavailable);
    }

    let insert = format!(
        "INSERT INTO {REGISTRY_TABLE} (authorization_id, registration_binding_id, \
         organization_id, workspace_id, device_id, authorization_generation, delivery_id, \
         certificate_sha256, csr_sha256, spki_sha256, serial_number, not_after_unix_ms, \
         delivery_deadline_unix_ms, state) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, \
         $10, $11, $12, $13, 'pending_ack') ON CONFLICT (authorization_id) DO NOTHING"
    );
    sqlx::query(&insert)
        .bind(delivery.authorization_id.as_slice())
        .bind(delivery.binding_id)
        .bind(&delivery.key.organization_id)
        .bind(&delivery.key.workspace_id)
        .bind(&delivery.key.device_id)
        .bind(to_i64(delivery.generation)?)
        .bind(delivery.delivery_id.as_slice())
        .bind(delivery.certificate_sha256.as_slice())
        .bind(delivery.csr_sha256.as_slice())
        .bind(delivery.spki_sha256.as_slice())
        .bind(&delivery.serial_number)
        .bind(to_i64(delivery.not_after_unix_ms)?)
        .bind(to_i64(delivery.delivery_deadline_unix_ms)?)
        .execute(&mut *transaction)
        .await
        .map_err(|_| DeviceRegistryPostgresError::Unavailable)?;

    let existing = select_registry(&mut transaction, authorization_id, true)
        .await?
        .ok_or(DeviceRegistryPostgresError::Unavailable)?;
    if !delivery_matches_registry(&delivery, &existing)
        || !matches!(
            existing.state,
            RegistryState::PendingAck | RegistryState::Active
        )
    {
        return Err(DeviceRegistryPostgresError::Unavailable);
    }
    transaction
        .commit()
        .await
        .map_err(|_| DeviceRegistryPostgresError::Unavailable)
}

async fn select_authorization_unlocked(
    transaction: &mut Transaction<'_, Postgres>,
    authorization_id: &DeviceAuthorizationId,
) -> RegistryResult<Option<AuthorizationSnapshot>> {
    let query = format!(
        "SELECT id, registration_binding_id, organization_id, workspace_id, device_id, approval_id, \
         authorization_generation, csr_sha256, spki_sha256, state_kind, \
         state_deadline_unix_ms, state_payload FROM {AUTHORIZATION_TABLE} WHERE id = $1"
    );
    let row = sqlx::query(&query)
        .bind(authorization_id.as_slice())
        .fetch_optional(&mut **transaction)
        .await
        .map_err(|_| DeviceRegistryPostgresError::Unavailable)?;
    row.map(decode_authorization).transpose()
}

async fn database_now_ms(transaction: &mut Transaction<'_, Postgres>) -> RegistryResult<u64> {
    let row =
        sqlx::query("SELECT floor(extract(epoch FROM clock_timestamp()) * 1000)::BIGINT AS now_ms")
            .fetch_one(&mut **transaction)
            .await
            .map_err(|_| DeviceRegistryPostgresError::Unavailable)?;
    positive_u64(
        row.try_get("now_ms")
            .map_err(|_| DeviceRegistryPostgresError::Unavailable)?,
    )
}

async fn select_registry(
    transaction: &mut Transaction<'_, Postgres>,
    authorization_id: &DeviceAuthorizationId,
    for_update: bool,
) -> RegistryResult<Option<RegistrySnapshot>> {
    let lock = if for_update { " FOR UPDATE" } else { "" };
    let query = format!(
        "SELECT authorization_id, registration_binding_id, organization_id, workspace_id, \
         device_id, authorization_generation, delivery_id, certificate_sha256, csr_sha256, \
         spki_sha256, serial_number, not_after_unix_ms, delivery_deadline_unix_ms, state, \
         acknowledged_at_unix_ms FROM {REGISTRY_TABLE} WHERE authorization_id = $1{lock}"
    );
    let row = sqlx::query(&query)
        .bind(authorization_id.as_slice())
        .fetch_optional(&mut **transaction)
        .await
        .map_err(|_| DeviceRegistryPostgresError::Unavailable)?;
    row.map(decode_registry).transpose()
}

fn decode_registry(row: PgRow) -> RegistryResult<RegistrySnapshot> {
    let binding_id: Uuid = row
        .try_get("registration_binding_id")
        .map_err(|_| DeviceRegistryPostgresError::Unavailable)?;
    let generation: i64 = row
        .try_get("authorization_generation")
        .map_err(|_| DeviceRegistryPostgresError::Unavailable)?;
    let acknowledged_at: Option<i64> = row
        .try_get("acknowledged_at_unix_ms")
        .map_err(|_| DeviceRegistryPostgresError::Unavailable)?;
    Ok(RegistrySnapshot {
        delivery: DeliverySnapshot {
            authorization_id: fixed(
                row.try_get("authorization_id")
                    .map_err(|_| DeviceRegistryPostgresError::Unavailable)?,
            )?,
            binding_id,
            key: WorkspaceDeviceKey {
                organization_id: row
                    .try_get("organization_id")
                    .map_err(|_| DeviceRegistryPostgresError::Unavailable)?,
                workspace_id: row
                    .try_get("workspace_id")
                    .map_err(|_| DeviceRegistryPostgresError::Unavailable)?,
                device_id: row
                    .try_get("device_id")
                    .map_err(|_| DeviceRegistryPostgresError::Unavailable)?,
            },
            generation: positive_u64(generation)?,
            delivery_id: fixed(
                row.try_get("delivery_id")
                    .map_err(|_| DeviceRegistryPostgresError::Unavailable)?,
            )?,
            certificate_sha256: fixed(
                row.try_get("certificate_sha256")
                    .map_err(|_| DeviceRegistryPostgresError::Unavailable)?,
            )?,
            csr_sha256: fixed(
                row.try_get("csr_sha256")
                    .map_err(|_| DeviceRegistryPostgresError::Unavailable)?,
            )?,
            spki_sha256: fixed(
                row.try_get("spki_sha256")
                    .map_err(|_| DeviceRegistryPostgresError::Unavailable)?,
            )?,
            serial_number: row
                .try_get("serial_number")
                .map_err(|_| DeviceRegistryPostgresError::Unavailable)?,
            not_after_unix_ms: positive_u64(
                row.try_get("not_after_unix_ms")
                    .map_err(|_| DeviceRegistryPostgresError::Unavailable)?,
            )?,
            delivery_deadline_unix_ms: positive_u64(
                row.try_get("delivery_deadline_unix_ms")
                    .map_err(|_| DeviceRegistryPostgresError::Unavailable)?,
            )?,
        },
        state: RegistryState::parse(
            &row.try_get::<String, _>("state")
                .map_err(|_| DeviceRegistryPostgresError::Unavailable)?,
        )?,
        acknowledged_at_unix_ms: acknowledged_at.map(positive_u64).transpose()?,
    })
}

async fn activate_acknowledged_delivery(
    pool: &PgPool,
    authorization_id: &DeviceAuthorizationId,
) -> RegistryResult<DeviceCertificateRegistryActivation> {
    let row = sqlx::query(
        "SELECT cyrene_workspace_device_registry.activate_acknowledged_delivery($1) AS outcome",
    )
    .bind(authorization_id.as_slice())
    .fetch_one(pool)
    .await
    .map_err(|_| DeviceRegistryPostgresError::Unavailable)?;
    let outcome: String = row
        .try_get("outcome")
        .map_err(|_| DeviceRegistryPostgresError::Unavailable)?;
    match outcome.as_str() {
        "activated" => Ok(DeviceCertificateRegistryActivation::Activated),
        "already_active" => Ok(DeviceCertificateRegistryActivation::AlreadyActive),
        "awaiting_acknowledgement" => {
            Ok(DeviceCertificateRegistryActivation::AwaitingAcknowledgement)
        }
        "ineligible" => Ok(DeviceCertificateRegistryActivation::Ineligible),
        "not_staged" => Ok(DeviceCertificateRegistryActivation::NotStaged),
        _ => Err(DeviceRegistryPostgresError::Unavailable),
    }
}

async fn reconcile_pending_deliveries(pool: &PgPool, limit: usize) -> RegistryResult<usize> {
    if limit == 0 {
        return Ok(0);
    }
    let query = format!(
        "SELECT authorization_id FROM {REGISTRY_TABLE} WHERE state = 'pending_ack' \
         ORDER BY created_at, authorization_id LIMIT $1"
    );
    let rows = sqlx::query(&query)
        .bind(i64::try_from(limit).map_err(|_| DeviceRegistryPostgresError::Unavailable)?)
        .fetch_all(pool)
        .await
        .map_err(|_| DeviceRegistryPostgresError::Unavailable)?;
    let ids = rows
        .into_iter()
        .map(|row| {
            fixed::<16>(
                row.try_get("authorization_id")
                    .map_err(|_| DeviceRegistryPostgresError::Unavailable)?,
            )
        })
        .collect::<RegistryResult<Vec<_>>>()?;
    let mut activated = 0;
    for id in ids {
        if matches!(
            activate_acknowledged_delivery(pool, &id).await?,
            DeviceCertificateRegistryActivation::Activated
        ) {
            activated += 1;
        }
    }
    Ok(activated)
}

async fn revoke_device(
    pool: &PgPool,
    key: &WorkspaceDeviceKey,
) -> RegistryResult<WorkspaceDeviceRecord> {
    let row = sqlx::query(
        "SELECT cyrene_workspace_device_registry.revoke_workspace_device($1, $2, $3) AS existed",
    )
    .bind(&key.organization_id)
    .bind(&key.workspace_id)
    .bind(&key.device_id)
    .fetch_one(pool)
    .await
    .map_err(|_| DeviceRegistryPostgresError::Unavailable)?;
    let existed: bool = row
        .try_get("existed")
        .map_err(|_| DeviceRegistryPostgresError::Unavailable)?;
    if !existed {
        return Err(DeviceRegistryPostgresError::Unavailable);
    }
    let snapshot = select_latest_registry_by_key(pool, key)
        .await?
        .ok_or(DeviceRegistryPostgresError::Unavailable)?;
    Ok(snapshot.record())
}

async fn find_device(
    pool: &PgPool,
    key: &WorkspaceDeviceKey,
) -> RegistryResult<Option<WorkspaceDeviceRecord>> {
    let Some(snapshot) = select_latest_registry_by_key(pool, key).await? else {
        return Ok(None);
    };
    if snapshot.state == RegistryState::Active {
        return Ok(verify_active_record(pool, &snapshot)
            .await?
            .map(|current| current.record()));
    }
    Ok(Some(snapshot.record()))
}

async fn find_device_by_fingerprint(
    pool: &PgPool,
    fingerprint_sha256: &str,
) -> RegistryResult<Option<WorkspaceDeviceRecord>> {
    let digest = unhex_32(fingerprint_sha256).ok_or(DeviceRegistryPostgresError::Unavailable)?;
    let query = format!(
        "SELECT authorization_id, registration_binding_id, organization_id, workspace_id, \
         device_id, authorization_generation, delivery_id, certificate_sha256, csr_sha256, \
         spki_sha256, serial_number, not_after_unix_ms, delivery_deadline_unix_ms, state, \
         acknowledged_at_unix_ms FROM {REGISTRY_TABLE} WHERE certificate_sha256 = $1"
    );
    let row = sqlx::query(&query)
        .bind(digest.as_slice())
        .fetch_optional(pool)
        .await
        .map_err(|_| DeviceRegistryPostgresError::Unavailable)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let snapshot = decode_registry(row)?;
    if snapshot.state == RegistryState::Active {
        return Ok(verify_active_record(pool, &snapshot)
            .await?
            .map(|current| current.record()));
    }
    Ok(Some(snapshot.record()))
}

async fn find_current_device_certificate_identity(
    pool: &PgPool,
    fingerprint_sha256: &str,
) -> RegistryResult<Option<WorkspaceDeviceCertificateIdentity>> {
    let digest = unhex_32(fingerprint_sha256).ok_or(DeviceRegistryPostgresError::Unavailable)?;
    let query = format!(
        "SELECT authorization_id, registration_binding_id, organization_id, workspace_id, \
         device_id, authorization_generation, delivery_id, certificate_sha256, csr_sha256, \
         spki_sha256, serial_number, not_after_unix_ms, delivery_deadline_unix_ms, state, \
         acknowledged_at_unix_ms FROM {REGISTRY_TABLE} WHERE certificate_sha256 = $1"
    );
    let row = sqlx::query(&query)
        .bind(digest.as_slice())
        .fetch_optional(pool)
        .await
        .map_err(|_| DeviceRegistryPostgresError::Unavailable)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let snapshot = decode_registry(row)?;
    if snapshot.state != RegistryState::Active {
        return Ok(None);
    }
    Ok(verify_active_record(pool, &snapshot)
        .await?
        .map(|current| current.certificate_identity()))
}

async fn select_latest_registry_by_key(
    pool: &PgPool,
    key: &WorkspaceDeviceKey,
) -> RegistryResult<Option<RegistrySnapshot>> {
    let query = format!(
        "SELECT authorization_id, registration_binding_id, organization_id, workspace_id, \
         device_id, authorization_generation, delivery_id, certificate_sha256, csr_sha256, \
         spki_sha256, serial_number, not_after_unix_ms, delivery_deadline_unix_ms, state, \
         acknowledged_at_unix_ms FROM {REGISTRY_TABLE} \
         WHERE organization_id = $1 AND workspace_id = $2 AND device_id = $3 \
         ORDER BY authorization_generation DESC LIMIT 1"
    );
    let row = sqlx::query(&query)
        .bind(&key.organization_id)
        .bind(&key.workspace_id)
        .bind(&key.device_id)
        .fetch_optional(pool)
        .await
        .map_err(|_| DeviceRegistryPostgresError::Unavailable)?;
    row.map(decode_registry).transpose()
}

async fn verify_active_record(
    pool: &PgPool,
    initial: &RegistrySnapshot,
) -> RegistryResult<Option<RegistrySnapshot>> {
    // One MVCC statement snapshot ties authorization, binding, current generation,
    // registry status, and database time together for this mTLS decision.
    let query = format!(
        "SELECT r.authorization_id, r.registration_binding_id, r.organization_id, \
         r.workspace_id, r.device_id, r.authorization_generation, r.delivery_id, \
         r.certificate_sha256, r.csr_sha256, r.spki_sha256, r.serial_number, \
         r.not_after_unix_ms, r.delivery_deadline_unix_ms, r.state, \
         r.acknowledged_at_unix_ms, a.id AS auth_id, \
         a.registration_binding_id AS auth_binding_id, a.organization_id AS auth_org, \
         a.workspace_id AS auth_workspace, a.device_id AS auth_device, \
         a.authorization_generation AS auth_generation, a.csr_sha256 AS auth_csr_sha256, \
         a.spki_sha256 AS auth_spki_sha256, a.approval_id AS auth_approval_id, \
         a.state_kind AS auth_state_kind, a.state_payload AS auth_state_payload, \
         i.current_authorization_generation AS directory_generation, \
         b.binding_id AS directory_binding_id, \
         floor(extract(epoch FROM clock_timestamp()) * 1000)::BIGINT AS now_ms \
         FROM {REGISTRY_TABLE} r \
         JOIN {AUTHORIZATION_TABLE} a ON a.id = r.authorization_id \
         JOIN {DIRECTORY_IDENTITIES_TABLE} i ON \
             i.organization_id = r.organization_id AND i.workspace_id = r.workspace_id \
             AND i.device_id = r.device_id \
             AND i.current_authorization_generation = r.authorization_generation \
         JOIN {DIRECTORY_BINDINGS_TABLE} b ON \
             b.binding_id = r.registration_binding_id \
             AND b.organization_id = r.organization_id \
             AND b.workspace_id = r.workspace_id AND b.device_id = r.device_id \
             AND b.authorization_generation = r.authorization_generation \
             AND b.csr_sha256 = r.csr_sha256 AND b.spki_sha256 = r.spki_sha256 \
         WHERE r.authorization_id = $1 AND r.state = 'active' \
           AND a.state_kind = 'delivered' AND a.registration_binding_id = uuid_send(r.registration_binding_id) \
           AND a.organization_id = r.organization_id AND a.workspace_id = r.workspace_id \
           AND a.device_id = r.device_id AND a.authorization_generation = r.authorization_generation \
           AND a.csr_sha256 = r.csr_sha256 AND a.spki_sha256 = r.spki_sha256 \
           AND r.not_after_unix_ms > floor(extract(epoch FROM clock_timestamp()) * 1000)::BIGINT"
    );
    let row = sqlx::query(&query)
        .bind(initial.delivery.authorization_id.as_slice())
        .fetch_optional(pool)
        .await
        .map_err(|_| DeviceRegistryPostgresError::Unavailable)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let auth = decode_active_authorization(&row)?;
    let now: i64 = row
        .try_get("now_ms")
        .map_err(|_| DeviceRegistryPostgresError::Unavailable)?;
    let now = positive_u64(now)?;
    let current = decode_registry(row)?;
    if current.delivery != initial.delivery || current.state != RegistryState::Active {
        return Ok(None);
    }
    if current.delivery.not_after_unix_ms <= now {
        return Ok(None);
    }
    let (receipt, certificate_validation) = decode_receipt_with_validation(&auth)?;
    if !receipt_matches(&current.delivery, &receipt, now)
        || certificate_validation.checked_at_unix_ms > now
        || current.acknowledged_at_unix_ms != Some(receipt.acknowledged_at_unix_ms)
    {
        return Err(DeviceRegistryPostgresError::Unavailable);
    }
    Ok(Some(current))
}

fn decode_active_authorization(row: &PgRow) -> RegistryResult<AuthorizationSnapshot> {
    let binding_id: Vec<u8> = row
        .try_get("auth_binding_id")
        .map_err(|_| DeviceRegistryPostgresError::Unavailable)?;
    let binding_id =
        Uuid::from_slice(&binding_id).map_err(|_| DeviceRegistryPostgresError::Unavailable)?;
    let approval_id: Option<Vec<u8>> = row
        .try_get("auth_approval_id")
        .map_err(|_| DeviceRegistryPostgresError::Unavailable)?;
    let state_payload: Vec<u8> = row
        .try_get("auth_state_payload")
        .map_err(|_| DeviceRegistryPostgresError::Unavailable)?;
    if state_payload.is_empty() || state_payload.len() > MAX_STATE_PAYLOAD_BYTES {
        return Err(DeviceRegistryPostgresError::Unavailable);
    }
    Ok(AuthorizationSnapshot {
        id: fixed(
            row.try_get("auth_id")
                .map_err(|_| DeviceRegistryPostgresError::Unavailable)?,
        )?,
        binding_id,
        key: WorkspaceDeviceKey {
            organization_id: row
                .try_get("auth_org")
                .map_err(|_| DeviceRegistryPostgresError::Unavailable)?,
            workspace_id: row
                .try_get("auth_workspace")
                .map_err(|_| DeviceRegistryPostgresError::Unavailable)?,
            device_id: row
                .try_get("auth_device")
                .map_err(|_| DeviceRegistryPostgresError::Unavailable)?,
        },
        generation: positive_u64(
            row.try_get("auth_generation")
                .map_err(|_| DeviceRegistryPostgresError::Unavailable)?,
        )?,
        csr_sha256: fixed(
            row.try_get("auth_csr_sha256")
                .map_err(|_| DeviceRegistryPostgresError::Unavailable)?,
        )?,
        spki_sha256: fixed(
            row.try_get("auth_spki_sha256")
                .map_err(|_| DeviceRegistryPostgresError::Unavailable)?,
        )?,
        approval_id: approval_id.map(fixed::<16>).transpose()?,
        state_kind: row
            .try_get("auth_state_kind")
            .map_err(|_| DeviceRegistryPostgresError::Unavailable)?,
        state_deadline_unix_ms: None,
        state_payload,
    })
}

fn fixed<const N: usize>(value: Vec<u8>) -> RegistryResult<[u8; N]> {
    value
        .as_slice()
        .try_into()
        .map_err(|_| DeviceRegistryPostgresError::Unavailable)
}

fn positive_u64(value: i64) -> RegistryResult<u64> {
    let value = u64::try_from(value).map_err(|_| DeviceRegistryPostgresError::Unavailable)?;
    if value == 0 {
        return Err(DeviceRegistryPostgresError::Unavailable);
    }
    Ok(value)
}

fn to_i64(value: u64) -> RegistryResult<i64> {
    i64::try_from(value).map_err(|_| DeviceRegistryPostgresError::Unavailable)
}

#[cfg(test)]
fn sha256(value: &[u8]) -> [u8; 32] {
    Sha256::digest(value).into()
}

fn hex(value: &[u8]) -> String {
    value.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn unhex_32(value: &str) -> Option<[u8; 32]> {
    if !is_sha256_hex(value) {
        return None;
    }
    let mut digest = [0; 32];
    for (index, slot) in digest.iter_mut().enumerate() {
        let pair = &value.as_bytes()[index * 2..index * 2 + 2];
        *slot = (hex_digit(pair[0])? << 4) | hex_digit(pair[1])?;
    }
    Some(digest)
}

fn hex_digit(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn binding_bytes() -> [u8; 16] {
        [7; 16]
    }

    fn sample_auth(state_kind: &str, state: serde_json::Value) -> AuthorizationSnapshot {
        AuthorizationSnapshot {
            id: [1; 16],
            binding_id: Uuid::from_bytes(binding_bytes()),
            key: WorkspaceDeviceKey {
                organization_id: "org-test".to_owned(),
                workspace_id: "workspace-test".to_owned(),
                device_id: "device-test".to_owned(),
            },
            generation: 4,
            csr_sha256: [2; 32],
            spki_sha256: [3; 32],
            approval_id: Some([9; 16]),
            state_kind: state_kind.to_owned(),
            state_deadline_unix_ms: Some(8_000),
            state_payload: serde_json::to_vec(&json!({
                "format_version": 5,
                "state": state,
            }))
            .unwrap(),
        }
    }

    fn pending_state(certificate_der: &[u8]) -> serde_json::Value {
        let certificate_sha256 = sha256(certificate_der);
        json!({
            "kind": "delivery_pending",
            "approval_id": vec![9; 16],
            "certificate_validation": {
                "validation_version": 1,
                "certificate_sha256": certificate_sha256,
                "registration_binding_id": binding_bytes(),
                "checked_at_unix_ms": 7_000,
            },
            "certificate": {
                "certificate_der": certificate_der,
                "ca_chain_der": [[48, 1, 0]],
                "serial_number": [1, 2, 3],
                "registration_binding_id": binding_bytes(),
                "device_key": {
                    "organization_id": "org-test",
                    "workspace_id": "workspace-test",
                    "device_id": "device-test",
                },
                "authorization_generation": 4,
                "scope": {
                    "organization_id": "org-test",
                    "workspace_id": "workspace-test",
                },
                "spki_sha256": vec![3; 32],
                "not_after_unix_ms": 20_000,
            },
            "delivery_id": vec![5; 16],
            "certificate_sha256": certificate_sha256,
            "delivery_deadline_unix_ms": 8_000,
        })
    }

    fn sample_delivery() -> DeliverySnapshot {
        DeliverySnapshot {
            authorization_id: [1; 16],
            binding_id: Uuid::from_bytes(binding_bytes()),
            key: WorkspaceDeviceKey {
                organization_id: "org-test".to_owned(),
                workspace_id: "workspace-test".to_owned(),
                device_id: "device-test".to_owned(),
            },
            generation: 4,
            delivery_id: [5; 16],
            certificate_sha256: [6; 32],
            csr_sha256: [2; 32],
            spki_sha256: [3; 32],
            serial_number: vec![1, 2, 3],
            not_after_unix_ms: 20_000,
            delivery_deadline_unix_ms: 8_000,
        }
    }

    #[test]
    fn delivery_snapshot_requires_v5_validation_marker_and_matching_der_digest() {
        let der = [11, 22, 33];
        let auth = sample_auth("delivery_pending", pending_state(&der));
        let (snapshot, _) = decode_delivery_with_validation(&auth).unwrap();
        assert_eq!(snapshot.authorization_id, [1; 16]);
        assert_eq!(snapshot.delivery_id, [5; 16]);
        assert_eq!(snapshot.binding_id.as_bytes(), &binding_bytes());
        assert_eq!(snapshot.certificate_sha256, sha256(&der));

        let mut mismatched_state = pending_state(&der);
        mismatched_state["certificate_sha256"] = json!(vec![0; 32]);
        let mismatched = sample_auth("delivery_pending", mismatched_state);
        assert_eq!(
            decode_delivery_with_validation(&mismatched).map(|(snapshot, _)| snapshot),
            Err(DeviceRegistryPostgresError::Unavailable)
        );
    }

    #[test]
    fn receipt_must_match_exact_ack_tuple_and_deadline() {
        let delivery = sample_delivery();
        let receipt = StoredReceipt {
            authorization_id: delivery.authorization_id,
            delivery_id: delivery.delivery_id,
            device_id: delivery.key.device_id.clone(),
            authorization_generation: delivery.generation,
            certificate_sha256: delivery.certificate_sha256,
            csr_sha256: delivery.csr_sha256,
            csr_spki_sha256: delivery.spki_sha256,
            acknowledged_at_unix_ms: 7_999,
        };
        assert!(receipt_matches(&delivery, &receipt, 8_001));
        assert!(!receipt_matches(&delivery, &receipt, 7_998));

        let mut old_generation = receipt;
        old_generation.authorization_generation -= 1;
        assert!(!receipt_matches(&delivery, &old_generation, 8_001));
    }

    #[test]
    fn invalid_or_uppercase_fingerprints_never_reach_storage_lookup() {
        assert!(is_sha256_hex(&"ab".repeat(32)));
        assert!(!is_sha256_hex(&"AB".repeat(32)));
        assert_eq!(unhex_32(&"0f".repeat(32)).unwrap(), [15; 32]);
        assert!(unhex_32(&"g0".repeat(32)).is_none());
    }

    #[test]
    fn migration_keeps_new_rows_inert_and_guards_activation_with_ack_and_generation() {
        let migration =
            include_str!("../migrations/device_registry/0001_device_certificate_registry.up.sql");
        assert!(migration.contains("NEW.state <> 'pending_ack'"));
        assert!(migration.contains("auth_row.state_kind IS DISTINCT FROM 'delivered'"));
        assert!(migration.contains("current_authorization_generation"));
        assert!(migration.contains("receipt -> 'certificate_sha256'"));
        assert!(migration.contains("certificate ->> 'not_after_unix_ms'"));
    }
}
