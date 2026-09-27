//! PostgreSQL persistence for Workspace WebAuthn credentials and ceremonies.
//!
//! The WebAuthn store port is synchronous. This adapter runs SQLx on a
//! dedicated Tokio worker with a bounded queue; queue, timeout, connection,
//! decoding, and transaction failures all fail closed.

use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use cy_proto::workspace_v1::UserIdentityRef;
use sha2::{Digest, Sha256};
use sqlx::migrate::Migrator;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgRow, PgSslMode};
use sqlx::{PgPool, Postgres, Row, Transaction};
use thiserror::Error;
use webauthn_rs::prelude::{Credential, Passkey};

#[cfg(test)]
use crate::device_authorization::DeviceAuthorizationDeviceKey;
use crate::device_authorization::{DeviceAuthorizationId, WebAuthnAuthenticationContext};
use crate::webauthn_credential_store::{
    PersistedWebAuthnCeremony, PersistedWebAuthnRegistration, StoredWebAuthnCredential,
    StoredWebAuthnCredentialSet, WebAuthnAuthenticationCommit, WebAuthnCredentialStore,
    WebAuthnCredentialStoreError,
};
use crate::webauthn_verifier::{
    context_digest, credential_set_digest, same_owner, valid_user_handle, validate_context,
    validate_owner, VerifiedWebAuthnPasskey, WebAuthnAuditAction, WebAuthnAuditEvent,
    WebAuthnAuditFailure, WebAuthnCredentialRevocationReason,
};

const DATABASE_URL_ENV: &str = "CYRENE_WORKSPACE_WEBAUTHN_DATABASE_URL";
const MIGRATION_DATABASE_URL_ENV: &str = "CYRENE_WORKSPACE_WEBAUTHN_MIGRATION_DATABASE_URL";
const QUEUE_CAPACITY: usize = 32;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);
const DATABASE_TIMEOUT: Duration = Duration::from_secs(5);
const STARTUP_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_OPAQUE_STATE_BYTES: usize = 64 * 1024;
const MAX_PASSKEY_BYTES: usize = 64 * 1024;
const MAX_CREDENTIAL_ID_BYTES: usize = 1024;
const MAX_AUDIT_PAGE: usize = 1000;

static MIGRATOR: Migrator = sqlx::migrate!("./migrations/webauthn");

type StoreResult<T> = Result<T, WebAuthnCredentialStoreError>;
type StoreReply<T> = SyncSender<StoreResult<T>>;

struct AuthenticationCommitCommand {
    context: WebAuthnAuthenticationContext,
    opaque_state: Vec<u8>,
    assertion_sha256: [u8; 32],
    credential_id: Vec<u8>,
    expected_counter: u32,
    expected_passkey_sha256: [u8; 32],
    updated_passkey_json: Vec<u8>,
    now_unix_ms: u64,
    user_verified: bool,
    backup_eligible: bool,
    backup_state: bool,
}

struct RegistrationCommitCommand {
    registration_id: DeviceAuthorizationId,
    owner: UserIdentityRef,
    opaque_state: Vec<u8>,
    response_sha256: [u8; 32],
    passkey_json: Vec<u8>,
    signature_counter: u32,
    backup_eligible: bool,
    backup_state: bool,
    now_unix_ms: u64,
}

/// Configuration or startup errors from the PostgreSQL WebAuthn adapter.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum WebAuthnPostgresStoreError {
    /// The database URL is missing or malformed.
    #[error("WebAuthn database configuration is invalid")]
    Configuration,
    /// The database could not be reached, migrated, or validated.
    #[error("WebAuthn database is unavailable")]
    Unavailable,
}

/// PostgreSQL-backed credential, ceremony, replay, and audit store.
///
/// Use a separate database principal that is a member of the migration's
/// `cyrene_workspace_webauthn_app` NOLOGIN role. Runtime connections require
/// PostgreSQL TLS with certificate and hostname verification. Credentials,
/// challenge state, assertion consumption, counters, and audit rows are
/// transactionally stored in the dedicated `cyrene_workspace_webauthn`
/// schema; the adapter never falls back to local disk.
pub struct PostgresWebAuthnCredentialStore {
    sender: Option<SyncSender<Command>>,
    shutdown: Arc<AtomicBool>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl PostgresWebAuthnCredentialStore {
    /// Connect using a trusted runtime database URL and verify the schema.
    pub fn connect(database_url: &str) -> Result<Self, WebAuthnPostgresStoreError> {
        Self::start(database_url, false)
    }

    /// Read the runtime database URL from server environment configuration.
    pub fn connect_from_environment() -> Result<Self, WebAuthnPostgresStoreError> {
        let database_url = std::env::var(DATABASE_URL_ENV)
            .map_err(|_| WebAuthnPostgresStoreError::Configuration)?;
        Self::connect(&database_url)
    }

    /// Apply schema migrations with a separately provisioned operator URL.
    /// This is a deployment action; the runtime database principal must not
    /// receive DDL or migration-table ownership.
    pub fn migrate(database_url: &str) -> Result<(), WebAuthnPostgresStoreError> {
        let store = Self::start(database_url, true)?;
        drop(store);
        Ok(())
    }

    /// Apply migrations using the dedicated operator environment variable.
    pub fn migrate_from_environment() -> Result<(), WebAuthnPostgresStoreError> {
        let database_url = std::env::var(MIGRATION_DATABASE_URL_ENV)
            .map_err(|_| WebAuthnPostgresStoreError::Configuration)?;
        Self::migrate(&database_url)
    }

    fn start(
        database_url: &str,
        apply_migrations: bool,
    ) -> Result<Self, WebAuthnPostgresStoreError> {
        let options = PgConnectOptions::from_str(database_url)
            .map_err(|_| WebAuthnPostgresStoreError::Configuration)?
            .ssl_mode(PgSslMode::VerifyFull)
            .application_name("cyrene-workspace-webauthn")
            .options([("statement_timeout", "5000"), ("lock_timeout", "3000")]);

        let (sender, commands) = mpsc::sync_channel(QUEUE_CAPACITY);
        let (ready_sender, ready) = mpsc::sync_channel(1);
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_shutdown = Arc::clone(&shutdown);
        let worker = thread::Builder::new()
            .name("workspace-webauthn-postgres".to_owned())
            .spawn(move || {
                worker_main(
                    options,
                    commands,
                    ready_sender,
                    apply_migrations,
                    worker_shutdown,
                )
            })
            .map_err(|_| WebAuthnPostgresStoreError::Unavailable)?;

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
                Err(WebAuthnPostgresStoreError::Unavailable)
            }
        }
    }

    fn call<T>(&self, command: Command, reply: Receiver<StoreResult<T>>) -> StoreResult<T> {
        let sender = self
            .sender
            .as_ref()
            .ok_or(WebAuthnCredentialStoreError::Unavailable)?;
        match sender.try_send(command) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                return Err(WebAuthnCredentialStoreError::Unavailable)
            }
        }
        reply
            .recv_timeout(REQUEST_TIMEOUT)
            .map_err(|_| WebAuthnCredentialStoreError::Unavailable)?
    }
}

impl Drop for PostgresWebAuthnCredentialStore {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        self.sender.take();
        if let Ok(mut worker) = self.worker.lock() {
            if let Some(worker) = worker.take() {
                let _ = worker.join();
            }
        }
    }
}

enum Command {
    EnsureUserHandle(UserIdentityRef, StoreReply<Vec<u8>>),
    Credentials(
        UserIdentityRef,
        StoreReply<Option<StoredWebAuthnCredentialSet>>,
    ),
    BeginAuthentication(
        WebAuthnAuthenticationContext,
        Vec<u8>,
        Vec<u8>,
        [u8; 32],
        u64,
        u64,
        StoreReply<()>,
    ),
    AuthenticationCeremony(
        DeviceAuthorizationId,
        StoreReply<Option<PersistedWebAuthnCeremony>>,
    ),
    CommitAuthentication(AuthenticationCommitCommand, StoreReply<()>),
    BeginRegistration(
        DeviceAuthorizationId,
        UserIdentityRef,
        Vec<u8>,
        Vec<u8>,
        u64,
        u64,
        StoreReply<()>,
    ),
    RegistrationCeremony(
        DeviceAuthorizationId,
        StoreReply<Option<PersistedWebAuthnRegistration>>,
    ),
    CommitRegistration(RegistrationCommitCommand, StoreReply<()>),
    RevokeCredential(
        UserIdentityRef,
        Vec<u8>,
        WebAuthnCredentialRevocationReason,
        u64,
        StoreReply<bool>,
    ),
    RecordRejection(
        UserIdentityRef,
        DeviceAuthorizationId,
        Option<[u8; 32]>,
        WebAuthnAuditFailure,
        u64,
        StoreReply<()>,
    ),
    AuditEvents(
        UserIdentityRef,
        u64,
        usize,
        StoreReply<Vec<WebAuthnAuditEvent>>,
    ),
}

fn worker_main(
    options: PgConnectOptions,
    commands: Receiver<Command>,
    ready: SyncSender<Result<(), WebAuthnPostgresStoreError>>,
    apply_migrations: bool,
    shutdown: Arc<AtomicBool>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => {
            let _ = ready.send(Err(WebAuthnPostgresStoreError::Unavailable));
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
            Command::EnsureUserHandle(owner, reply) => {
                let _ = reply.send(runtime.block_on(ensure_user_handle(&pool, &owner)));
            }
            Command::Credentials(owner, reply) => {
                let _ = reply.send(runtime.block_on(load_credentials(&pool, &owner)));
            }
            Command::BeginAuthentication(context, handle, state, set_hash, expires, now, reply) => {
                let _ = reply.send(runtime.block_on(begin_authentication(
                    &pool, &context, &handle, &state, set_hash, expires, now,
                )));
            }
            Command::AuthenticationCeremony(id, reply) => {
                let _ = reply.send(runtime.block_on(authentication_ceremony(&pool, &id)));
            }
            Command::CommitAuthentication(commit, reply) => {
                let _ = reply.send(runtime.block_on(commit_authentication(&pool, commit)));
            }
            Command::BeginRegistration(id, owner, handle, state, expires, now, reply) => {
                let _ = reply.send(runtime.block_on(begin_registration(
                    &pool, &id, &owner, &handle, &state, expires, now,
                )));
            }
            Command::RegistrationCeremony(id, reply) => {
                let _ = reply.send(runtime.block_on(registration_ceremony(&pool, &id)));
            }
            Command::CommitRegistration(commit, reply) => {
                let _ = reply.send(runtime.block_on(commit_registration(&pool, commit)));
            }
            Command::RevokeCredential(owner, credential_id, reason, now, reply) => {
                let _ = reply.send(runtime.block_on(revoke_credential(
                    &pool,
                    &owner,
                    &credential_id,
                    reason,
                    now,
                )));
            }
            Command::RecordRejection(owner, id, credential_hash, reason, now, reply) => {
                let _ = reply.send(runtime.block_on(record_rejection(
                    &pool,
                    &owner,
                    id,
                    credential_hash,
                    reason,
                    now,
                )));
            }
            Command::AuditEvents(owner, after, limit, reply) => {
                let _ = reply.send(runtime.block_on(audit_events(&pool, &owner, after, limit)));
            }
        }
    }
}

async fn initialize_pool(
    options: PgConnectOptions,
    apply_migrations: bool,
) -> Result<PgPool, WebAuthnPostgresStoreError> {
    let connect = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(DATABASE_TIMEOUT)
        .after_connect(|connection, _| {
            Box::pin(async move {
                sqlx::query("SET search_path TO cyrene_workspace_webauthn, pg_catalog")
                    .execute(connection)
                    .await?;
                Ok(())
            })
        })
        .connect_with(options);
    let pool = tokio::time::timeout(DATABASE_TIMEOUT, connect)
        .await
        .map_err(|_| WebAuthnPostgresStoreError::Unavailable)?
        .map_err(|_| WebAuthnPostgresStoreError::Unavailable)?;

    if apply_migrations {
        sqlx::query("CREATE SCHEMA IF NOT EXISTS cyrene_workspace_webauthn")
            .execute(&pool)
            .await
            .map_err(|_| WebAuthnPostgresStoreError::Unavailable)?;
        tokio::time::timeout(DATABASE_TIMEOUT, MIGRATOR.run(&pool))
            .await
            .map_err(|_| WebAuthnPostgresStoreError::Unavailable)?
            .map_err(|_| WebAuthnPostgresStoreError::Unavailable)?;
    } else {
        sqlx::query(
            "SELECT approval_id FROM cyrene_workspace_webauthn.authentication_ceremonies LIMIT 0",
        )
        .fetch_all(&pool)
        .await
        .map_err(|_| WebAuthnPostgresStoreError::Unavailable)?;
    }
    Ok(pool)
}

impl WebAuthnCredentialStore for PostgresWebAuthnCredentialStore {
    fn ensure_user_handle(&self, owner: &UserIdentityRef) -> StoreResult<Vec<u8>> {
        validate_owner(owner).map_err(|_| WebAuthnCredentialStoreError::InvalidRecord)?;
        let (reply, result) = mpsc::sync_channel(1);
        self.call(Command::EnsureUserHandle(owner.clone(), reply), result)
    }

    fn credentials_for_owner(
        &self,
        owner: &UserIdentityRef,
    ) -> StoreResult<Option<StoredWebAuthnCredentialSet>> {
        validate_owner(owner).map_err(|_| WebAuthnCredentialStoreError::InvalidRecord)?;
        let (reply, result) = mpsc::sync_channel(1);
        self.call(Command::Credentials(owner.clone(), reply), result)
    }

    fn begin_authentication(
        &self,
        context: &WebAuthnAuthenticationContext,
        user_handle: &[u8],
        opaque_state: &[u8],
        credential_set_sha256: [u8; 32],
        expires_at_unix_ms: u64,
        now_unix_ms: u64,
    ) -> StoreResult<()> {
        validate_context(context).map_err(|_| WebAuthnCredentialStoreError::InvalidRecord)?;
        if !valid_user_handle(user_handle)
            || opaque_state.is_empty()
            || opaque_state.len() > MAX_OPAQUE_STATE_BYTES
            || expires_at_unix_ms <= now_unix_ms
        {
            return Err(WebAuthnCredentialStoreError::InvalidRecord);
        }
        let (reply, result) = mpsc::sync_channel(1);
        self.call(
            Command::BeginAuthentication(
                context.clone(),
                user_handle.to_vec(),
                opaque_state.to_vec(),
                credential_set_sha256,
                expires_at_unix_ms,
                now_unix_ms,
                reply,
            ),
            result,
        )
    }

    fn authentication_ceremony(
        &self,
        approval_id: &DeviceAuthorizationId,
    ) -> StoreResult<Option<PersistedWebAuthnCeremony>> {
        let (reply, result) = mpsc::sync_channel(1);
        self.call(Command::AuthenticationCeremony(*approval_id, reply), result)
    }

    fn commit_authentication(&self, commit: WebAuthnAuthenticationCommit<'_>) -> StoreResult<()> {
        let WebAuthnAuthenticationCommit {
            context,
            opaque_state,
            assertion_sha256,
            credential_id,
            expected_counter,
            expected_passkey_sha256,
            updated_passkey,
            now_unix_ms,
            user_verified,
            backup_eligible,
            backup_state,
        } = commit;
        let internal = Credential::from(updated_passkey.clone());
        if !user_verified
            || internal.cred_id.as_ref() != credential_id
            || (expected_counter > 0 && internal.counter <= expected_counter)
        {
            return Err(WebAuthnCredentialStoreError::InvalidRecord);
        }
        let encoded = serde_json::to_vec(updated_passkey)
            .map_err(|_| WebAuthnCredentialStoreError::InvalidRecord)?;
        if encoded.is_empty() || encoded.len() > MAX_PASSKEY_BYTES {
            return Err(WebAuthnCredentialStoreError::InvalidRecord);
        }
        let (reply, result) = mpsc::sync_channel(1);
        self.call(
            Command::CommitAuthentication(
                AuthenticationCommitCommand {
                    context: context.clone(),
                    opaque_state: opaque_state.to_vec(),
                    assertion_sha256,
                    credential_id: credential_id.to_vec(),
                    expected_counter,
                    expected_passkey_sha256,
                    updated_passkey_json: encoded,
                    now_unix_ms,
                    user_verified,
                    backup_eligible,
                    backup_state,
                },
                reply,
            ),
            result,
        )
    }

    fn begin_registration(
        &self,
        registration_id: &DeviceAuthorizationId,
        owner: &UserIdentityRef,
        user_handle: &[u8],
        opaque_state: &[u8],
        expires_at_unix_ms: u64,
        now_unix_ms: u64,
    ) -> StoreResult<()> {
        validate_owner(owner).map_err(|_| WebAuthnCredentialStoreError::InvalidRecord)?;
        if !valid_user_handle(user_handle)
            || opaque_state.is_empty()
            || opaque_state.len() > MAX_OPAQUE_STATE_BYTES
            || expires_at_unix_ms <= now_unix_ms
        {
            return Err(WebAuthnCredentialStoreError::InvalidRecord);
        }
        let (reply, result) = mpsc::sync_channel(1);
        self.call(
            Command::BeginRegistration(
                *registration_id,
                owner.clone(),
                user_handle.to_vec(),
                opaque_state.to_vec(),
                expires_at_unix_ms,
                now_unix_ms,
                reply,
            ),
            result,
        )
    }

    fn registration_ceremony(
        &self,
        registration_id: &DeviceAuthorizationId,
    ) -> StoreResult<Option<PersistedWebAuthnRegistration>> {
        let (reply, result) = mpsc::sync_channel(1);
        self.call(
            Command::RegistrationCeremony(*registration_id, reply),
            result,
        )
    }

    fn commit_registration(
        &self,
        registration_id: &DeviceAuthorizationId,
        owner: &UserIdentityRef,
        opaque_state: &[u8],
        response_sha256: [u8; 32],
        credential: &VerifiedWebAuthnPasskey,
        now_unix_ms: u64,
    ) -> StoreResult<()> {
        let internal = Credential::from(credential.passkey().clone());
        let passkey_json = serde_json::to_vec(credential.passkey())
            .map_err(|_| WebAuthnCredentialStoreError::InvalidRecord)?;
        if !same_owner(credential.owner(), owner)
            || !valid_user_handle(credential.user_handle())
            || !internal.user_verified
            || internal.counter != credential.signature_counter()
            || internal.backup_eligible != credential.backup_eligible()
            || internal.backup_state != credential.backup_state()
            || passkey_json.is_empty()
            || passkey_json.len() > MAX_PASSKEY_BYTES
        {
            return Err(WebAuthnCredentialStoreError::InvalidRecord);
        }
        let (reply, result) = mpsc::sync_channel(1);
        self.call(
            Command::CommitRegistration(
                RegistrationCommitCommand {
                    registration_id: *registration_id,
                    owner: owner.clone(),
                    opaque_state: opaque_state.to_vec(),
                    response_sha256,
                    passkey_json,
                    signature_counter: credential.signature_counter(),
                    backup_eligible: credential.backup_eligible(),
                    backup_state: credential.backup_state(),
                    now_unix_ms,
                },
                reply,
            ),
            result,
        )
    }

    fn revoke_credential(
        &self,
        owner: &UserIdentityRef,
        credential_id: &[u8],
        reason: WebAuthnCredentialRevocationReason,
        now_unix_ms: u64,
    ) -> StoreResult<bool> {
        validate_owner(owner).map_err(|_| WebAuthnCredentialStoreError::InvalidRecord)?;
        if credential_id.is_empty() || credential_id.len() > MAX_CREDENTIAL_ID_BYTES {
            return Err(WebAuthnCredentialStoreError::InvalidRecord);
        }
        let (reply, result) = mpsc::sync_channel(1);
        self.call(
            Command::RevokeCredential(
                owner.clone(),
                credential_id.to_vec(),
                reason,
                now_unix_ms,
                reply,
            ),
            result,
        )
    }

    fn record_rejection(
        &self,
        owner: &UserIdentityRef,
        correlation_id: &DeviceAuthorizationId,
        credential_id_sha256: Option<[u8; 32]>,
        reason: WebAuthnAuditFailure,
        now_unix_ms: u64,
    ) -> StoreResult<()> {
        validate_owner(owner).map_err(|_| WebAuthnCredentialStoreError::InvalidRecord)?;
        let (reply, result) = mpsc::sync_channel(1);
        self.call(
            Command::RecordRejection(
                owner.clone(),
                *correlation_id,
                credential_id_sha256,
                reason,
                now_unix_ms,
                reply,
            ),
            result,
        )
    }

    fn audit_events(
        &self,
        owner: &UserIdentityRef,
        after_sequence: u64,
        limit: usize,
    ) -> StoreResult<Vec<WebAuthnAuditEvent>> {
        validate_owner(owner).map_err(|_| WebAuthnCredentialStoreError::InvalidRecord)?;
        let (reply, result) = mpsc::sync_channel(1);
        self.call(
            Command::AuditEvents(owner.clone(), after_sequence, limit, reply),
            result,
        )
    }
}

async fn ensure_user_handle(pool: &PgPool, owner: &UserIdentityRef) -> StoreResult<Vec<u8>> {
    let mut tx = pool.begin().await.map_err(map_database_error)?;
    if let Some(handle) = owner_handle_tx(&mut tx, owner).await? {
        tx.commit().await.map_err(map_database_error)?;
        return Ok(handle);
    }
    for _ in 0..8 {
        let handle = uuid::Uuid::new_v4().as_bytes().to_vec();
        let inserted = sqlx::query(
            "INSERT INTO cyrene_workspace_webauthn.owners (issuer, subject, user_handle) \
             VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
        )
        .bind(&owner.issuer)
        .bind(&owner.subject)
        .bind(&handle)
        .execute(&mut *tx)
        .await
        .map_err(map_database_error)?
        .rows_affected();
        if inserted == 1 {
            tx.commit().await.map_err(map_database_error)?;
            return Ok(handle);
        }
        if let Some(existing) = owner_handle_tx(&mut tx, owner).await? {
            tx.commit().await.map_err(map_database_error)?;
            return Ok(existing);
        }
    }
    Err(WebAuthnCredentialStoreError::Unavailable)
}

async fn load_credentials(
    pool: &PgPool,
    owner: &UserIdentityRef,
) -> StoreResult<Option<StoredWebAuthnCredentialSet>> {
    let Some(handle) = sqlx::query_scalar::<_, Vec<u8>>(
        "SELECT user_handle FROM cyrene_workspace_webauthn.owners WHERE issuer = $1 AND subject = $2",
    )
    .bind(&owner.issuer)
    .bind(&owner.subject)
    .fetch_optional(pool)
    .await
    .map_err(map_database_error)? else {
        return Ok(None);
    };
    let rows = sqlx::query(
        "SELECT credential_id, passkey_json, signature_counter \
         FROM cyrene_workspace_webauthn.credentials WHERE issuer = $1 AND subject = $2 \
         ORDER BY credential_id",
    )
    .bind(&owner.issuer)
    .bind(&owner.subject)
    .fetch_all(pool)
    .await
    .map_err(map_database_error)?;
    let credentials = rows
        .into_iter()
        .map(decode_credential)
        .collect::<StoreResult<Vec<_>>>()?;
    Ok(Some(StoredWebAuthnCredentialSet {
        user_handle: validate_handle_from_db(handle)?,
        credentials,
    }))
}

async fn load_credentials_tx(
    tx: &mut Transaction<'_, Postgres>,
    owner: &UserIdentityRef,
) -> StoreResult<Option<StoredWebAuthnCredentialSet>> {
    let Some(handle) = owner_handle_tx(tx, owner).await? else {
        return Ok(None);
    };
    let rows = sqlx::query(
        "SELECT credential_id, passkey_json, signature_counter \
         FROM cyrene_workspace_webauthn.credentials WHERE issuer = $1 AND subject = $2 \
         ORDER BY credential_id FOR SHARE",
    )
    .bind(&owner.issuer)
    .bind(&owner.subject)
    .fetch_all(&mut **tx)
    .await
    .map_err(map_database_error)?;
    let credentials = rows
        .into_iter()
        .map(decode_credential)
        .collect::<StoreResult<Vec<_>>>()?;
    Ok(Some(StoredWebAuthnCredentialSet {
        user_handle: validate_handle_from_db(handle)?,
        credentials,
    }))
}

fn decode_credential(row: PgRow) -> StoreResult<StoredWebAuthnCredential> {
    let credential_id: Vec<u8> = row.try_get("credential_id").map_err(map_decode_error)?;
    let passkey_json: Vec<u8> = row.try_get("passkey_json").map_err(map_decode_error)?;
    let counter = decode_counter(row.try_get("signature_counter").map_err(map_decode_error)?)?;
    let passkey: Passkey = serde_json::from_slice(&passkey_json)
        .map_err(|_| WebAuthnCredentialStoreError::Unavailable)?;
    let internal = Credential::from(passkey.clone());
    if passkey_json.is_empty()
        || passkey_json.len() > MAX_PASSKEY_BYTES
        || credential_id.as_slice() != passkey.cred_id().as_ref()
        || counter != internal.counter
    {
        return Err(WebAuthnCredentialStoreError::Unavailable);
    }
    Ok(StoredWebAuthnCredential {
        credential_id,
        passkey,
        signature_counter: counter,
        serialized_sha256: sha256(&passkey_json),
    })
}

async fn begin_authentication(
    pool: &PgPool,
    context: &WebAuthnAuthenticationContext,
    user_handle: &[u8],
    opaque_state: &[u8],
    credential_set_sha256: [u8; 32],
    expires_at_unix_ms: u64,
    now_unix_ms: u64,
) -> StoreResult<()> {
    let mut tx = pool.begin().await.map_err(map_database_error)?;
    // Lock the ceremony before owner/credential rows. Assertion completion
    // uses the same order, preventing a retried start from deadlocking against
    // a concurrent successful assertion.
    let existing = sqlx::query(
        "SELECT authorization_id, issuer, subject, user_handle, context_sha256, opaque_state, \
         credential_set_sha256, expires_at_unix_ms FROM \
         cyrene_workspace_webauthn.authentication_ceremonies WHERE approval_id = $1 FOR UPDATE",
    )
    .bind(context.approval_id.as_slice())
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_database_error)?;
    let has_existing = existing.is_some();
    if let Some(row) = existing {
        let authorization_id: Vec<u8> =
            row.try_get("authorization_id").map_err(map_decode_error)?;
        let issuer: String = row.try_get("issuer").map_err(map_decode_error)?;
        let subject: String = row.try_get("subject").map_err(map_decode_error)?;
        let existing_handle: Vec<u8> = row.try_get("user_handle").map_err(map_decode_error)?;
        let existing_context: Vec<u8> = row.try_get("context_sha256").map_err(map_decode_error)?;
        let existing_state: Vec<u8> = row.try_get("opaque_state").map_err(map_decode_error)?;
        let existing_set: Vec<u8> = row
            .try_get("credential_set_sha256")
            .map_err(map_decode_error)?;
        let existing_expiry: i64 = row
            .try_get("expires_at_unix_ms")
            .map_err(map_decode_error)?;
        let same = authorization_id.as_slice() == context.authorization_id
            && issuer == context.approver.issuer
            && subject == context.approver.subject
            && existing_handle == user_handle
            && existing_context.as_slice() == context_digest(context)
            && existing_state == opaque_state
            && existing_set.as_slice() == credential_set_sha256
            && existing_expiry == to_i64(expires_at_unix_ms)?;
        if !same {
            return Err(WebAuthnCredentialStoreError::CeremonyConflict);
        }
    }
    let current = load_credentials_tx(&mut tx, &context.approver)
        .await?
        .ok_or(WebAuthnCredentialStoreError::CeremonyConflict)?;
    if current.credentials.is_empty()
        || current.user_handle != user_handle
        || credential_set_digest(&current.credentials) != credential_set_sha256
    {
        return Err(WebAuthnCredentialStoreError::CeremonyConflict);
    }
    if has_existing {
        tx.commit().await.map_err(map_database_error)?;
        return Ok(());
    }
    sqlx::query(
        "DELETE FROM cyrene_workspace_webauthn.authentication_ceremonies \
         WHERE status = 0 AND expires_at_unix_ms <= $1",
    )
    .bind(to_i64(now_unix_ms)?)
    .execute(&mut *tx)
    .await
    .map_err(map_database_error)?;
    sqlx::query(
        "INSERT INTO cyrene_workspace_webauthn.authentication_ceremonies \
         (approval_id, authorization_id, issuer, subject, user_handle, context_sha256, \
          opaque_state, state_sha256, credential_set_sha256, expires_at_unix_ms) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
    )
    .bind(context.approval_id.as_slice())
    .bind(context.authorization_id.as_slice())
    .bind(&context.approver.issuer)
    .bind(&context.approver.subject)
    .bind(user_handle)
    .bind(context_digest(context).as_slice())
    .bind(opaque_state)
    .bind(sha256(opaque_state).as_slice())
    .bind(credential_set_sha256.as_slice())
    .bind(to_i64(expires_at_unix_ms)?)
    .execute(&mut *tx)
    .await
    .map_err(map_database_error)?;
    insert_audit(
        &mut tx,
        &context.approver,
        now_unix_ms,
        WebAuthnAuditAction::AuthenticationChallengeIssued,
        Some(&context.approval_id),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .await?;
    tx.commit().await.map_err(map_database_error)
}

async fn authentication_ceremony(
    pool: &PgPool,
    approval_id: &DeviceAuthorizationId,
) -> StoreResult<Option<PersistedWebAuthnCeremony>> {
    let row = sqlx::query(
        "SELECT authorization_id, issuer, subject, user_handle, context_sha256, opaque_state, \
         state_sha256, credential_set_sha256, expires_at_unix_ms, assertion_sha256, credential_id \
         FROM cyrene_workspace_webauthn.authentication_ceremonies WHERE approval_id = $1",
    )
    .bind(approval_id.as_slice())
    .fetch_optional(pool)
    .await
    .map_err(map_database_error)?;
    row.map(decode_authentication_ceremony).transpose()
}

fn decode_authentication_ceremony(row: PgRow) -> StoreResult<PersistedWebAuthnCeremony> {
    let authorization_id = fixed::<16>(row.try_get("authorization_id").map_err(map_decode_error)?)?;
    let owner = UserIdentityRef {
        issuer: row.try_get("issuer").map_err(map_decode_error)?,
        subject: row.try_get("subject").map_err(map_decode_error)?,
    };
    validate_owner(&owner).map_err(|_| WebAuthnCredentialStoreError::Unavailable)?;
    let user_handle =
        validate_handle_from_db(row.try_get("user_handle").map_err(map_decode_error)?)?;
    let opaque_state: Vec<u8> = row.try_get("opaque_state").map_err(map_decode_error)?;
    if opaque_state.is_empty() || opaque_state.len() > MAX_OPAQUE_STATE_BYTES {
        return Err(WebAuthnCredentialStoreError::Unavailable);
    }
    let state_sha256 = fixed::<32>(row.try_get("state_sha256").map_err(map_decode_error)?)?;
    if state_sha256 != sha256(&opaque_state) {
        return Err(WebAuthnCredentialStoreError::Unavailable);
    }
    Ok(PersistedWebAuthnCeremony {
        owner,
        authorization_id,
        user_handle,
        context_sha256: fixed::<32>(row.try_get("context_sha256").map_err(map_decode_error)?)?,
        opaque_state,
        state_sha256,
        credential_set_sha256: fixed::<32>(
            row.try_get("credential_set_sha256")
                .map_err(map_decode_error)?,
        )?,
        expires_at_unix_ms: from_i64(
            row.try_get("expires_at_unix_ms")
                .map_err(map_decode_error)?,
        )?,
        consumed_assertion_sha256: row
            .try_get::<Option<Vec<u8>>, _>("assertion_sha256")
            .map_err(map_decode_error)?
            .map(fixed::<32>)
            .transpose()?,
        consumed_credential_id: row.try_get("credential_id").map_err(map_decode_error)?,
    })
}

async fn commit_authentication(
    pool: &PgPool,
    command: AuthenticationCommitCommand,
) -> StoreResult<()> {
    let AuthenticationCommitCommand {
        context,
        opaque_state,
        assertion_sha256,
        credential_id,
        expected_counter,
        expected_passkey_sha256,
        updated_passkey_json,
        now_unix_ms,
        user_verified,
        backup_eligible,
        backup_state,
    } = command;
    let context = &context;
    let opaque_state = opaque_state.as_slice();
    let credential_id = credential_id.as_slice();
    let updated_passkey_json = updated_passkey_json.as_slice();
    let updated_passkey: Passkey = serde_json::from_slice(updated_passkey_json)
        .map_err(|_| WebAuthnCredentialStoreError::InvalidRecord)?;
    let updated = Credential::from(updated_passkey);
    if !user_verified
        || updated.cred_id.as_ref() != credential_id
        || (expected_counter > 0 && updated.counter <= expected_counter)
    {
        return Err(WebAuthnCredentialStoreError::InvalidRecord);
    }
    let mut tx = pool.begin().await.map_err(map_database_error)?;
    let ceremony = sqlx::query(
        "SELECT authorization_id, issuer, subject, context_sha256, opaque_state, state_sha256, \
         expires_at_unix_ms, status, assertion_sha256, credential_id \
         FROM cyrene_workspace_webauthn.authentication_ceremonies \
         WHERE approval_id = $1 FOR UPDATE",
    )
    .bind(context.approval_id.as_slice())
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_database_error)?
    .ok_or(WebAuthnCredentialStoreError::CeremonyConflict)?;
    let authorization_id: Vec<u8> = ceremony
        .try_get("authorization_id")
        .map_err(map_decode_error)?;
    let issuer: String = ceremony.try_get("issuer").map_err(map_decode_error)?;
    let subject: String = ceremony.try_get("subject").map_err(map_decode_error)?;
    let stored_context: Vec<u8> = ceremony
        .try_get("context_sha256")
        .map_err(map_decode_error)?;
    let stored_state: Vec<u8> = ceremony.try_get("opaque_state").map_err(map_decode_error)?;
    let stored_state_hash: Vec<u8> = ceremony.try_get("state_sha256").map_err(map_decode_error)?;
    let expires: i64 = ceremony
        .try_get("expires_at_unix_ms")
        .map_err(map_decode_error)?;
    let status: i16 = ceremony.try_get("status").map_err(map_decode_error)?;
    let consumed_hash: Option<Vec<u8>> = ceremony
        .try_get("assertion_sha256")
        .map_err(map_decode_error)?;
    let consumed_credential: Option<Vec<u8>> = ceremony
        .try_get("credential_id")
        .map_err(map_decode_error)?;
    if authorization_id.as_slice() != context.authorization_id
        || issuer != context.approver.issuer
        || subject != context.approver.subject
        || stored_context.as_slice() != context_digest(context)
        || stored_state.as_slice() != opaque_state
        || stored_state_hash.as_slice() != sha256(opaque_state)
    {
        return Err(WebAuthnCredentialStoreError::CeremonyConflict);
    }
    if status == 1 {
        if consumed_hash.as_deref() == Some(assertion_sha256.as_slice())
            && consumed_credential.as_deref() == Some(credential_id)
        {
            tx.commit().await.map_err(map_database_error)?;
            return Ok(());
        }
        return Err(WebAuthnCredentialStoreError::CeremonyConflict);
    }
    if status != 0 || now_unix_ms >= from_i64(expires)? {
        return Err(WebAuthnCredentialStoreError::CeremonyConflict);
    }
    let row = sqlx::query(
        "SELECT passkey_json, signature_counter FROM cyrene_workspace_webauthn.credentials \
         WHERE issuer = $1 AND subject = $2 AND credential_id = $3 FOR UPDATE",
    )
    .bind(&context.approver.issuer)
    .bind(&context.approver.subject)
    .bind(credential_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_database_error)?
    .ok_or(WebAuthnCredentialStoreError::CredentialNotFound)?;
    let current_json: Vec<u8> = row.try_get("passkey_json").map_err(map_decode_error)?;
    let current_counter =
        decode_counter(row.try_get("signature_counter").map_err(map_decode_error)?)?;
    let current_passkey: Passkey = serde_json::from_slice(&current_json)
        .map_err(|_| WebAuthnCredentialStoreError::Unavailable)?;
    let current_internal = Credential::from(current_passkey.clone());
    if current_internal.cred_id.as_ref() != credential_id
        || current_internal.counter != current_counter
        || current_counter != expected_counter
        || sha256(&current_json) != expected_passkey_sha256
        || (current_counter > 0 && updated.counter <= current_counter)
    {
        return Err(WebAuthnCredentialStoreError::CeremonyConflict);
    }
    let updated_rows = sqlx::query(
        "UPDATE cyrene_workspace_webauthn.credentials SET passkey_json = $1, signature_counter = $2 \
         WHERE issuer = $3 AND subject = $4 AND credential_id = $5 \
           AND signature_counter = $6 AND passkey_json = $7",
    )
    .bind(updated_passkey_json)
    .bind(i64::from(updated.counter))
    .bind(&context.approver.issuer)
    .bind(&context.approver.subject)
    .bind(credential_id)
    .bind(i64::from(expected_counter))
    .bind(&current_json)
    .execute(&mut *tx)
    .await
    .map_err(map_database_error)?
    .rows_affected();
    if updated_rows != 1 {
        return Err(WebAuthnCredentialStoreError::CeremonyConflict);
    }
    let consumed_rows = sqlx::query(
        "UPDATE cyrene_workspace_webauthn.authentication_ceremonies \
         SET status = 1, assertion_sha256 = $1, credential_id = $2 \
         WHERE approval_id = $3 AND status = 0",
    )
    .bind(assertion_sha256.as_slice())
    .bind(credential_id)
    .bind(context.approval_id.as_slice())
    .execute(&mut *tx)
    .await
    .map_err(map_database_error)?
    .rows_affected();
    if consumed_rows != 1 {
        return Err(WebAuthnCredentialStoreError::CeremonyConflict);
    }
    insert_audit(
        &mut tx,
        &context.approver,
        now_unix_ms,
        WebAuthnAuditAction::AssertionAccepted,
        Some(&context.approval_id),
        Some(sha256(credential_id)),
        Some(expected_counter),
        Some(updated.counter),
        Some(user_verified),
        Some(backup_eligible),
        Some(backup_state),
        None,
    )
    .await?;
    tx.commit().await.map_err(map_database_error)
}

async fn begin_registration(
    pool: &PgPool,
    registration_id: &DeviceAuthorizationId,
    owner: &UserIdentityRef,
    user_handle: &[u8],
    opaque_state: &[u8],
    expires_at_unix_ms: u64,
    now_unix_ms: u64,
) -> StoreResult<()> {
    let mut tx = pool.begin().await.map_err(map_database_error)?;
    // Use the same ceremony-then-owner lock order as registration completion.
    let existing = sqlx::query(
        "SELECT issuer, subject, user_handle, opaque_state, expires_at_unix_ms \
         FROM cyrene_workspace_webauthn.registration_ceremonies WHERE registration_id = $1 FOR UPDATE",
    )
    .bind(registration_id.as_slice())
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_database_error)?;
    if let Some(row) = existing {
        let issuer: String = row.try_get("issuer").map_err(map_decode_error)?;
        let subject: String = row.try_get("subject").map_err(map_decode_error)?;
        let handle: Vec<u8> = row.try_get("user_handle").map_err(map_decode_error)?;
        let state: Vec<u8> = row.try_get("opaque_state").map_err(map_decode_error)?;
        let expiry: i64 = row
            .try_get("expires_at_unix_ms")
            .map_err(map_decode_error)?;
        if issuer == owner.issuer
            && subject == owner.subject
            && handle == user_handle
            && state == opaque_state
            && expiry == to_i64(expires_at_unix_ms)?
        {
            tx.commit().await.map_err(map_database_error)?;
            return Ok(());
        }
        return Err(WebAuthnCredentialStoreError::CeremonyConflict);
    }
    let stored_handle = owner_handle_tx(&mut tx, owner)
        .await?
        .ok_or(WebAuthnCredentialStoreError::CredentialNotFound)?;
    if stored_handle != user_handle {
        return Err(WebAuthnCredentialStoreError::UserHandleConflict);
    }
    sqlx::query(
        "DELETE FROM cyrene_workspace_webauthn.registration_ceremonies \
         WHERE status = 0 AND expires_at_unix_ms <= $1",
    )
    .bind(to_i64(now_unix_ms)?)
    .execute(&mut *tx)
    .await
    .map_err(map_database_error)?;
    sqlx::query(
        "INSERT INTO cyrene_workspace_webauthn.registration_ceremonies \
         (registration_id, issuer, subject, user_handle, opaque_state, state_sha256, expires_at_unix_ms) \
         VALUES ($1,$2,$3,$4,$5,$6,$7)",
    )
    .bind(registration_id.as_slice())
    .bind(&owner.issuer)
    .bind(&owner.subject)
    .bind(user_handle)
    .bind(opaque_state)
    .bind(sha256(opaque_state).as_slice())
    .bind(to_i64(expires_at_unix_ms)?)
    .execute(&mut *tx)
    .await
    .map_err(map_database_error)?;
    insert_audit(
        &mut tx,
        owner,
        now_unix_ms,
        WebAuthnAuditAction::CredentialRegistrationStarted,
        Some(registration_id),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .await?;
    tx.commit().await.map_err(map_database_error)
}

async fn registration_ceremony(
    pool: &PgPool,
    registration_id: &DeviceAuthorizationId,
) -> StoreResult<Option<PersistedWebAuthnRegistration>> {
    let row = sqlx::query(
        "SELECT issuer, subject, user_handle, opaque_state, state_sha256, expires_at_unix_ms, \
         response_sha256, credential_id FROM cyrene_workspace_webauthn.registration_ceremonies \
         WHERE registration_id = $1",
    )
    .bind(registration_id.as_slice())
    .fetch_optional(pool)
    .await
    .map_err(map_database_error)?;
    row.map(decode_registration_ceremony).transpose()
}

fn decode_registration_ceremony(row: PgRow) -> StoreResult<PersistedWebAuthnRegistration> {
    let owner = UserIdentityRef {
        issuer: row.try_get("issuer").map_err(map_decode_error)?,
        subject: row.try_get("subject").map_err(map_decode_error)?,
    };
    validate_owner(&owner).map_err(|_| WebAuthnCredentialStoreError::Unavailable)?;
    let user_handle =
        validate_handle_from_db(row.try_get("user_handle").map_err(map_decode_error)?)?;
    let opaque_state: Vec<u8> = row.try_get("opaque_state").map_err(map_decode_error)?;
    if opaque_state.is_empty() || opaque_state.len() > MAX_OPAQUE_STATE_BYTES {
        return Err(WebAuthnCredentialStoreError::Unavailable);
    }
    let state_sha256 = fixed::<32>(row.try_get("state_sha256").map_err(map_decode_error)?)?;
    if sha256(&opaque_state) != state_sha256 {
        return Err(WebAuthnCredentialStoreError::Unavailable);
    }
    Ok(PersistedWebAuthnRegistration {
        owner,
        user_handle,
        opaque_state,
        state_sha256,
        expires_at_unix_ms: from_i64(
            row.try_get("expires_at_unix_ms")
                .map_err(map_decode_error)?,
        )?,
        consumed_response_sha256: row
            .try_get::<Option<Vec<u8>>, _>("response_sha256")
            .map_err(map_decode_error)?
            .map(fixed::<32>)
            .transpose()?,
        consumed_credential_id: row.try_get("credential_id").map_err(map_decode_error)?,
    })
}

async fn commit_registration(pool: &PgPool, command: RegistrationCommitCommand) -> StoreResult<()> {
    let RegistrationCommitCommand {
        registration_id,
        owner,
        opaque_state,
        response_sha256,
        passkey_json,
        signature_counter,
        backup_eligible,
        backup_state,
        now_unix_ms,
    } = command;
    let registration_id = &registration_id;
    let owner = &owner;
    let opaque_state = opaque_state.as_slice();
    let passkey_json = passkey_json.as_slice();
    let passkey: Passkey = serde_json::from_slice(passkey_json)
        .map_err(|_| WebAuthnCredentialStoreError::InvalidRecord)?;
    let internal = Credential::from(passkey.clone());
    if !internal.user_verified
        || internal.counter != signature_counter
        || internal.backup_eligible != backup_eligible
        || internal.backup_state != backup_state
    {
        return Err(WebAuthnCredentialStoreError::InvalidRecord);
    }
    let credential_id = passkey.cred_id().as_ref();
    if credential_id.is_empty() || credential_id.len() > MAX_CREDENTIAL_ID_BYTES {
        return Err(WebAuthnCredentialStoreError::InvalidRecord);
    }
    let mut tx = pool.begin().await.map_err(map_database_error)?;
    let ceremony = sqlx::query(
        "SELECT issuer, subject, user_handle, opaque_state, state_sha256, expires_at_unix_ms, \
         status, response_sha256, credential_id FROM \
         cyrene_workspace_webauthn.registration_ceremonies \
         WHERE registration_id = $1 FOR UPDATE",
    )
    .bind(registration_id.as_slice())
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_database_error)?
    .ok_or(WebAuthnCredentialStoreError::CeremonyConflict)?;
    let issuer: String = ceremony.try_get("issuer").map_err(map_decode_error)?;
    let subject: String = ceremony.try_get("subject").map_err(map_decode_error)?;
    let handle: Vec<u8> = ceremony.try_get("user_handle").map_err(map_decode_error)?;
    let state: Vec<u8> = ceremony.try_get("opaque_state").map_err(map_decode_error)?;
    let state_hash: Vec<u8> = ceremony.try_get("state_sha256").map_err(map_decode_error)?;
    let expiry: i64 = ceremony
        .try_get("expires_at_unix_ms")
        .map_err(map_decode_error)?;
    let status: i16 = ceremony.try_get("status").map_err(map_decode_error)?;
    let consumed_hash: Option<Vec<u8>> = ceremony
        .try_get("response_sha256")
        .map_err(map_decode_error)?;
    let consumed_credential: Option<Vec<u8>> = ceremony
        .try_get("credential_id")
        .map_err(map_decode_error)?;
    let stored_handle = owner_handle_tx(&mut tx, owner)
        .await?
        .ok_or(WebAuthnCredentialStoreError::CredentialNotFound)?;
    if issuer != owner.issuer
        || subject != owner.subject
        || handle != stored_handle
        || state.as_slice() != opaque_state
        || state_hash.as_slice() != sha256(opaque_state)
    {
        return Err(WebAuthnCredentialStoreError::CeremonyConflict);
    }
    if status == 1 {
        if consumed_hash.as_deref() == Some(response_sha256.as_slice())
            && consumed_credential.as_deref() == Some(credential_id)
        {
            tx.commit().await.map_err(map_database_error)?;
            return Ok(());
        }
        return Err(WebAuthnCredentialStoreError::CeremonyConflict);
    }
    if status != 0 || now_unix_ms >= from_i64(expiry)? {
        return Err(WebAuthnCredentialStoreError::CeremonyConflict);
    }
    sqlx::query(
        "INSERT INTO cyrene_workspace_webauthn.credentials \
         (credential_id, issuer, subject, passkey_json, signature_counter) VALUES ($1,$2,$3,$4,$5)",
    )
    .bind(credential_id)
    .bind(&owner.issuer)
    .bind(&owner.subject)
    .bind(passkey_json)
    .bind(i64::from(signature_counter))
    .execute(&mut *tx)
    .await
    .map_err(map_database_error)?;
    let consumed_rows = sqlx::query(
        "UPDATE cyrene_workspace_webauthn.registration_ceremonies \
         SET status = 1, response_sha256 = $1, credential_id = $2 \
         WHERE registration_id = $3 AND status = 0",
    )
    .bind(response_sha256.as_slice())
    .bind(credential_id)
    .bind(registration_id.as_slice())
    .execute(&mut *tx)
    .await
    .map_err(map_database_error)?
    .rows_affected();
    if consumed_rows != 1 {
        return Err(WebAuthnCredentialStoreError::CeremonyConflict);
    }
    insert_audit(
        &mut tx,
        owner,
        now_unix_ms,
        WebAuthnAuditAction::CredentialRegistered,
        Some(registration_id),
        Some(sha256(credential_id)),
        None,
        Some(signature_counter),
        Some(true),
        Some(backup_eligible),
        Some(backup_state),
        None,
    )
    .await?;
    tx.commit().await.map_err(map_database_error)
}

async fn revoke_credential(
    pool: &PgPool,
    owner: &UserIdentityRef,
    credential_id: &[u8],
    reason: WebAuthnCredentialRevocationReason,
    now_unix_ms: u64,
) -> StoreResult<bool> {
    let mut tx = pool.begin().await.map_err(map_database_error)?;
    let row = sqlx::query(
        "SELECT passkey_json, signature_counter FROM cyrene_workspace_webauthn.credentials \
         WHERE issuer = $1 AND subject = $2 AND credential_id = $3 FOR UPDATE",
    )
    .bind(&owner.issuer)
    .bind(&owner.subject)
    .bind(credential_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_database_error)?;
    let Some(row) = row else {
        tx.commit().await.map_err(map_database_error)?;
        return Ok(false);
    };
    let passkey_json: Vec<u8> = row.try_get("passkey_json").map_err(map_decode_error)?;
    let passkey: Passkey = serde_json::from_slice(&passkey_json)
        .map_err(|_| WebAuthnCredentialStoreError::Unavailable)?;
    let internal = Credential::from(passkey);
    let counter = decode_counter(row.try_get("signature_counter").map_err(map_decode_error)?)?;
    if internal.counter != counter || internal.cred_id.as_ref() != credential_id {
        return Err(WebAuthnCredentialStoreError::Unavailable);
    }
    sqlx::query(
        "DELETE FROM cyrene_workspace_webauthn.credentials \
         WHERE issuer = $1 AND subject = $2 AND credential_id = $3",
    )
    .bind(&owner.issuer)
    .bind(&owner.subject)
    .bind(credential_id)
    .execute(&mut *tx)
    .await
    .map_err(map_database_error)?;
    insert_audit(
        &mut tx,
        owner,
        now_unix_ms,
        WebAuthnAuditAction::CredentialRevoked,
        None,
        Some(sha256(credential_id)),
        Some(counter),
        None,
        None,
        Some(internal.backup_eligible),
        Some(internal.backup_state),
        Some(reason.code()),
    )
    .await?;
    tx.commit().await.map_err(map_database_error)?;
    Ok(true)
}

async fn record_rejection(
    pool: &PgPool,
    owner: &UserIdentityRef,
    correlation_id: DeviceAuthorizationId,
    credential_id_sha256: Option<[u8; 32]>,
    reason: WebAuthnAuditFailure,
    now_unix_ms: u64,
) -> StoreResult<()> {
    let mut tx = pool.begin().await.map_err(map_database_error)?;
    insert_audit(
        &mut tx,
        owner,
        now_unix_ms,
        WebAuthnAuditAction::SecurityRejected,
        Some(&correlation_id),
        credential_id_sha256,
        None,
        None,
        None,
        None,
        None,
        Some(reason.code()),
    )
    .await?;
    tx.commit().await.map_err(map_database_error)
}

#[allow(clippy::too_many_arguments)]
async fn insert_audit(
    tx: &mut Transaction<'_, Postgres>,
    owner: &UserIdentityRef,
    occurred_at_unix_ms: u64,
    action: WebAuthnAuditAction,
    correlation_id: Option<&DeviceAuthorizationId>,
    credential_id_sha256: Option<[u8; 32]>,
    previous_counter: Option<u32>,
    new_counter: Option<u32>,
    user_verified: Option<bool>,
    backup_eligible: Option<bool>,
    backup_state: Option<bool>,
    reason_code: Option<&str>,
) -> StoreResult<()> {
    sqlx::query(
        "INSERT INTO cyrene_workspace_webauthn.audit_events \
         (occurred_at_unix_ms, issuer, subject, action, correlation_id, credential_id_sha256, \
          previous_counter, new_counter, user_verified, backup_eligible, backup_state, reason_code) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)",
    )
    .bind(to_i64(occurred_at_unix_ms)?)
    .bind(&owner.issuer)
    .bind(&owner.subject)
    .bind(action.code())
    .bind(correlation_id.map(|id| id.as_slice()))
    .bind(credential_id_sha256.map(|digest| digest.to_vec()))
    .bind(previous_counter.map(i64::from))
    .bind(new_counter.map(i64::from))
    .bind(user_verified)
    .bind(backup_eligible)
    .bind(backup_state)
    .bind(reason_code)
    .execute(&mut **tx)
    .await
    .map_err(map_database_error)?;
    Ok(())
}

async fn audit_events(
    pool: &PgPool,
    owner: &UserIdentityRef,
    after_sequence: u64,
    limit: usize,
) -> StoreResult<Vec<WebAuthnAuditEvent>> {
    let rows = sqlx::query(
        "SELECT sequence, occurred_at_unix_ms, action, correlation_id, credential_id_sha256, \
         previous_counter, new_counter, user_verified, backup_eligible, backup_state, reason_code \
         FROM cyrene_workspace_webauthn.audit_events WHERE issuer = $1 AND subject = $2 \
         AND sequence > $3 ORDER BY sequence ASC LIMIT $4",
    )
    .bind(&owner.issuer)
    .bind(&owner.subject)
    .bind(to_i64(after_sequence)?)
    .bind(
        i64::try_from(limit.min(MAX_AUDIT_PAGE))
            .map_err(|_| WebAuthnCredentialStoreError::InvalidRecord)?,
    )
    .fetch_all(pool)
    .await
    .map_err(map_database_error)?;
    rows.into_iter().map(decode_audit_event).collect()
}

fn decode_audit_event(row: PgRow) -> StoreResult<WebAuthnAuditEvent> {
    let sequence = from_i64(row.try_get("sequence").map_err(map_decode_error)?)?;
    let occurred_at_unix_ms = from_i64(
        row.try_get("occurred_at_unix_ms")
            .map_err(map_decode_error)?,
    )?;
    let action: String = row.try_get("action").map_err(map_decode_error)?;
    let action =
        WebAuthnAuditAction::parse(&action).ok_or(WebAuthnCredentialStoreError::Unavailable)?;
    let correlation_id = row
        .try_get::<Option<Vec<u8>>, _>("correlation_id")
        .map_err(map_decode_error)?
        .map(fixed::<16>)
        .transpose()?;
    let credential_id_sha256 = row
        .try_get::<Option<Vec<u8>>, _>("credential_id_sha256")
        .map_err(map_decode_error)?
        .map(fixed::<32>)
        .transpose()?;
    Ok(WebAuthnAuditEvent {
        sequence,
        occurred_at_unix_ms,
        action,
        correlation_id,
        credential_id_sha256,
        previous_counter: row
            .try_get::<Option<i64>, _>("previous_counter")
            .map_err(map_decode_error)?
            .map(decode_counter)
            .transpose()?,
        new_counter: row
            .try_get::<Option<i64>, _>("new_counter")
            .map_err(map_decode_error)?
            .map(decode_counter)
            .transpose()?,
        user_verified: row.try_get("user_verified").map_err(map_decode_error)?,
        backup_eligible: row.try_get("backup_eligible").map_err(map_decode_error)?,
        backup_state: row.try_get("backup_state").map_err(map_decode_error)?,
        reason_code: row.try_get("reason_code").map_err(map_decode_error)?,
    })
}

async fn owner_handle_tx(
    tx: &mut Transaction<'_, Postgres>,
    owner: &UserIdentityRef,
) -> StoreResult<Option<Vec<u8>>> {
    let handle = sqlx::query_scalar::<_, Vec<u8>>(
        "SELECT user_handle FROM cyrene_workspace_webauthn.owners WHERE issuer = $1 AND subject = $2",
    )
        .bind(&owner.issuer)
        .bind(&owner.subject)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_database_error)?;
    handle.map(validate_handle_from_db).transpose()
}

fn validate_handle_from_db(handle: Vec<u8>) -> StoreResult<Vec<u8>> {
    if valid_user_handle(&handle) {
        Ok(handle)
    } else {
        Err(WebAuthnCredentialStoreError::Unavailable)
    }
}

fn decode_counter(value: i64) -> StoreResult<u32> {
    u32::try_from(value).map_err(|_| WebAuthnCredentialStoreError::Unavailable)
}

fn fixed<const N: usize>(value: Vec<u8>) -> StoreResult<[u8; N]> {
    value
        .try_into()
        .map_err(|_| WebAuthnCredentialStoreError::Unavailable)
}

fn to_i64(value: u64) -> StoreResult<i64> {
    i64::try_from(value).map_err(|_| WebAuthnCredentialStoreError::InvalidRecord)
}

fn from_i64(value: i64) -> StoreResult<u64> {
    u64::try_from(value).map_err(|_| WebAuthnCredentialStoreError::Unavailable)
}

fn sha256(value: &[u8]) -> [u8; 32] {
    Sha256::digest(value).into()
}

fn map_decode_error(_: sqlx::Error) -> WebAuthnCredentialStoreError {
    WebAuthnCredentialStoreError::Unavailable
}

fn map_database_error(error: sqlx::Error) -> WebAuthnCredentialStoreError {
    if let Some(database_error) = error.as_database_error() {
        if database_error.code().as_deref() == Some("23505") {
            return match database_error.constraint() {
                Some("webauthn_credentials_pk") => {
                    WebAuthnCredentialStoreError::CredentialAlreadyRegistered
                }
                Some("webauthn_owners_handle_unique") => {
                    WebAuthnCredentialStoreError::UserHandleConflict
                }
                Some(
                    "webauthn_authentication_ceremonies_pk" | "webauthn_registration_ceremonies_pk",
                ) => WebAuthnCredentialStoreError::CeremonyConflict,
                _ => WebAuthnCredentialStoreError::Unavailable,
            };
        }
    }
    WebAuthnCredentialStoreError::Unavailable
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use webauthn_authenticator_rs::prelude::{
        CreationChallengeResponse, RequestChallengeResponse, WebauthnAuthenticator,
    };
    use webauthn_authenticator_rs::softpasskey::SoftPasskey;

    use super::*;
    use crate::device_authorization::{
        DeviceAuthorizationClockPort, DeviceAuthorizationPortError, DeviceAuthorizationScope,
        WebAuthnAuthenticationPort,
    };
    use crate::webauthn_verifier::{
        WebAuthnAuthenticationVerifier, WebAuthnCredentialEnrollmentAuthorizer,
        WebAuthnCredentialEnrollmentService, WebAuthnCredentialManagementAction,
        WebAuthnVerifierConfig,
    };

    struct FixedClock(AtomicU64);

    impl DeviceAuthorizationClockPort for FixedClock {
        fn current_unix_ms(&self) -> Result<u64, DeviceAuthorizationPortError> {
            Ok(self.0.load(Ordering::SeqCst))
        }
    }

    struct TestEnrollmentAuthorizer;

    impl WebAuthnCredentialEnrollmentAuthorizer for TestEnrollmentAuthorizer {
        fn authorize_credential_management(
            &self,
            _owner: &UserIdentityRef,
            _action: WebAuthnCredentialManagementAction,
        ) -> Result<(), DeviceAuthorizationPortError> {
            Ok(())
        }
    }

    /// Full crypto and persistence exercise for a disposable TLS PostgreSQL
    /// fixture. Set both URLs and explicitly run ignored tests to enable it.
    #[test]
    #[ignore = "requires a disposable TLS PostgreSQL database and provisioned app role"]
    fn postgres_store_verifies_registered_assertion_and_persists_counter() {
        let migration_url = std::env::var("CYRENE_WORKSPACE_WEBAUTHN_TEST_MIGRATION_URL")
            .expect("operator migration URL for disposable PostgreSQL fixture");
        let runtime_url = std::env::var("CYRENE_WORKSPACE_WEBAUTHN_TEST_RUNTIME_URL")
            .expect("runtime URL for disposable PostgreSQL fixture");
        PostgresWebAuthnCredentialStore::migrate(&migration_url)
            .expect("apply isolated WebAuthn store migrations");

        let owner = UserIdentityRef {
            issuer: "https://identity.example.test/".to_owned(),
            subject: format!("workspace-test-{}", uuid::Uuid::new_v4()),
        };
        let config = WebAuthnVerifierConfig::new(
            "workspace.example.test",
            url::Url::parse("https://workspace.example.test/").expect("test HTTPS origin"),
        )
        .expect("fixed RP config");
        let clock: Arc<dyn DeviceAuthorizationClockPort> =
            Arc::new(FixedClock(AtomicU64::new(100_000)));
        let store: Arc<dyn WebAuthnCredentialStore> = Arc::new(
            PostgresWebAuthnCredentialStore::connect(&runtime_url)
                .expect("connect runtime principal with verified TLS"),
        );
        let registrar = WebAuthnCredentialEnrollmentService::new(
            config.clone(),
            store.clone(),
            clock.clone(),
            Arc::new(TestEnrollmentAuthorizer),
        )
        .expect("create protected registrar");

        let registration = registrar
            .start_registration(&owner)
            .expect("start trusted registration ceremony");
        let creation_options: CreationChallengeResponse =
            serde_json::from_slice(&registration.credential_creation_options_json)
                .expect("decode creation options");
        let mut authenticator = WebauthnAuthenticator::new(SoftPasskey::new(true));
        let registration_response = authenticator
            .do_registration(config.rp_origin().clone(), creation_options)
            .expect("complete actual soft authenticator registration");
        registrar
            .finish_registration(
                &owner,
                &registration.registration_id,
                &serde_json::to_vec(&registration_response).expect("encode registration response"),
            )
            .expect("verify and persist registration");
        drop(registrar);
        drop(store);

        let store: Arc<dyn WebAuthnCredentialStore> = Arc::new(
            PostgresWebAuthnCredentialStore::connect(&runtime_url)
                .expect("reconnect runtime principal after simulated process restart"),
        );
        let verifier = WebAuthnAuthenticationVerifier::new(config.clone(), store.clone(), clock)
            .expect("create assertion verifier");
        let context = WebAuthnAuthenticationContext {
            approval_id: *uuid::Uuid::new_v4().as_bytes(),
            authorization_id: *uuid::Uuid::new_v4().as_bytes(),
            registration_binding_id: [0x55; 16],
            device_key: DeviceAuthorizationDeviceKey {
                organization_id: "org-test".to_owned(),
                workspace_id: "workspace-test".to_owned(),
                device_id: "device-test".to_owned(),
            },
            authorization_generation: 1,
            approver: owner.clone(),
            scope: DeviceAuthorizationScope {
                organization_id: "org-test".to_owned(),
                workspace_id: "workspace-test".to_owned(),
            },
            csr_sha256: [0x11; 32],
            spki_sha256: [0x22; 32],
            expires_at_unix_ms: 200_000,
        };
        let challenge = verifier
            .start_authentication(&context)
            .expect("issue server-backed authentication challenge");
        let request_options: RequestChallengeResponse =
            serde_json::from_slice(&challenge.credential_request_options_json)
                .expect("decode request options");
        let assertion = authenticator
            .do_authentication(config.rp_origin().clone(), request_options)
            .expect("complete signed user-verifying assertion");
        let assertion_json = serde_json::to_vec(&assertion).expect("encode assertion");
        verifier
            .finish_authentication(&context, &challenge.opaque_state, &assertion_json, 100_000)
            .expect("verify assertion and atomically advance counter");
        verifier
            .finish_authentication(&context, &challenge.opaque_state, &assertion_json, 100_000)
            .expect("same assertion retry is idempotent");

        let stored = store
            .credentials_for_owner(&owner)
            .expect("read PostgreSQL credentials")
            .expect("owner remains registered");
        assert_eq!(stored.credentials.len(), 1);
        assert_eq!(stored.credentials[0].signature_counter, 1);
        let audit = store
            .audit_events(&owner, 0, 100)
            .expect("read audit events");
        assert!(audit.iter().any(|event| {
            event.action == WebAuthnAuditAction::CredentialRegistered
                && event.user_verified == Some(true)
        }));
        assert!(audit.iter().any(|event| {
            event.action == WebAuthnAuditAction::AssertionAccepted
                && event.previous_counter == Some(0)
                && event.new_counter == Some(1)
                && event.user_verified == Some(true)
        }));
    }
}
