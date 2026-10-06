//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 restricted_device_ca.rs                                        │
//! │  Module: cy_workspace_postgres_storage::restricted_device_ca       │
//! │  Role: Durable local device CA issuance and signed CRL status.      │
//! │                                                                     │
//! │  模块职责：提供持久幂等设备证书签发、撤销与签名 CRL 查询。              │
//! └─────────────────────────────────────────────────────────────────────┘
//!
//! This adapter keeps the private CA key in a separately permission-checked
//! OS file while PostgreSQL stores idempotency receipts and the current signed
//! CRL. Certificates are built from the CSR public key only: subject and SAN
//! identity always come from the Directory-bound registration snapshot.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use openssl::asn1::Asn1Time;
use openssl::bn::BigNum;
use openssl::hash::MessageDigest;
use openssl::pkey::{Id, PKey, Private};
use openssl::x509::extension::{
    AuthorityKeyIdentifier, BasicConstraints, CrlNumber, ExtendedKeyUsage, KeyUsage,
    SubjectAlternativeName,
};
use openssl::x509::{
    CrlStatus, X509Crl, X509CrlBuilder, X509Name, X509Req, X509RevokedBuilder, X509,
};
use rustls::pki_types::CertificateDer;
use rustls::RootCertStore;
use sha2::{Digest, Sha256};
use sqlx::migrate::Migrator;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};
use sqlx::{PgPool, Row};
use thiserror::Error;
use zeroize::Zeroizing;

use crate::device_authorization::{
    DeviceAuthorizationClockPort, DeviceAuthorizationDeviceKey, DeviceAuthorizationId,
    DeviceAuthorizationPortError, DeviceAuthorizationRegistrationBinding, DeviceAuthorizationScope,
    DeviceCertificateIssuanceError, DeviceCertificateIssuer, DeviceCertificateRetirementError,
    DeviceCertificateRetirementPort, DeviceCertificateRetirementReason, IssuedDeviceCertificate,
};
use crate::device_certificate_validation::{
    device_identity_uri, DeviceCertificateRevocationCheckError, DeviceCertificateRevocationChecker,
    DeviceCertificateRevocationQuery,
};
use crate::device_enrollment_authorization_service::{
    DeviceCertificatePublicMetadata, DeviceCertificatePublicMetadataPort,
};
use cy_workspace_control_plane::device_registry::{
    CurrentRelayPeerRevocationEvidence, RelayPeerCertificateRevocationChecker,
    RelayPeerRevocationCheckError, RelayPeerRevocationQuery, WorkspaceDeviceKey,
    WorkspaceDevicePeerCertificateStatusChecker,
};

const DATABASE_URL_ENV: &str = "CYRENE_WORKSPACE_DEVICE_CA_DATABASE_URL";
const RELAY_DATABASE_URL_ENV: &str = "CYRENE_WORKSPACE_RELAY_DEVICE_CA_DATABASE_URL";
const MIGRATION_DATABASE_URL_ENV: &str = "CYRENE_WORKSPACE_DEVICE_CA_MIGRATION_DATABASE_URL";
const SIGNING_KEY_FILE_ENV: &str = "CYRENE_WORKSPACE_DEVICE_CA_SIGNING_KEY_FILE";
const CERTIFICATE_FILE_ENV: &str = "CYRENE_WORKSPACE_DEVICE_CA_CERTIFICATE_FILE";
const ISSUER_ID_ENV: &str = "CYRENE_WORKSPACE_DEVICE_CA_ISSUER_ID";
const CERTIFICATE_LIFETIME_SECONDS: u64 = 90 * 24 * 60 * 60;
const CRL_LIFETIME_SECONDS: u64 = 5 * 60;
const CRL_REFRESH_INTERVAL: Duration = Duration::from_secs(60);
const MAX_CA_FILE_BYTES: u64 = 1024 * 1024;
const MAX_CERTIFICATE_DER_BYTES: usize = 16 * 1024;
const MAX_CSR_DER_BYTES: usize = 16 * 1024;
const MAX_SERIAL_BYTES: usize = 20;
const QUEUE_CAPACITY: usize = 32;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);
const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const TABLE: &str = "cyrene_workspace_device_ca.issued_certificates";
const CRL_TABLE: &str = "cyrene_workspace_device_ca.current_crl";

static MIGRATOR: Migrator = sqlx::migrate!("./migrations/device_ca");

/// Fixed startup errors for the local restricted signer.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum RestrictedDeviceCaError {
    /// Database URL, CA key, certificate, or issuer identity configuration is invalid.
    #[error("restricted Workspace device CA configuration is invalid")]
    Configuration,
    /// PostgreSQL, the installed CA schema, or the signer worker is unavailable.
    #[error("restricted Workspace device CA is unavailable")]
    Unavailable,
}

/// PostgreSQL-backed signer and revocation authority for one private Workspace device CA.
///
/// The runtime role only reads and writes the dedicated CA schema. Keep the
/// private key at an absolute, owner-only path outside application release and
/// version directories. Use a separate operator URL to apply the migration.
#[derive(Clone)]
pub struct PostgresRestrictedDeviceCa {
    inner: Arc<CaInner>,
}

struct CaInner {
    sender: Option<SyncSender<Command>>,
    worker: Mutex<Option<JoinHandle<()>>>,
    issuer_id: String,
    certificate_der: Vec<u8>,
}

impl PostgresRestrictedDeviceCa {
    /// Connect and validate the current signer schema before production routes are mounted.
    pub fn connect_from_environment() -> Result<Self, RestrictedDeviceCaError> {
        let database_url =
            std::env::var(DATABASE_URL_ENV).map_err(|_| RestrictedDeviceCaError::Configuration)?;
        let key_path = required_path(SIGNING_KEY_FILE_ENV)?;
        let certificate_path = required_path(CERTIFICATE_FILE_ENV)?;
        let issuer_id =
            std::env::var(ISSUER_ID_ENV).map_err(|_| RestrictedDeviceCaError::Configuration)?;
        Self::connect(&database_url, &key_path, &certificate_path, issuer_id)
    }

    /// Connect with explicit runtime and separately protected key paths.
    pub fn connect(
        database_url: &str,
        signing_key_path: &Path,
        certificate_path: &Path,
        issuer_id: impl Into<String>,
    ) -> Result<Self, RestrictedDeviceCaError> {
        let issuer_id = issuer_id.into();
        if issuer_id.trim().is_empty()
            || issuer_id.trim() != issuer_id
            || issuer_id.len() > 256
            || issuer_id.chars().any(char::is_control)
        {
            return Err(RestrictedDeviceCaError::Configuration);
        }
        let signing_key_pem = read_restricted_key(signing_key_path)?;
        let certificate_pem = read_regular_file(certificate_path, MAX_CA_FILE_BYTES)?;
        let mut certificates = X509::stack_from_pem(&certificate_pem)
            .map_err(|_| RestrictedDeviceCaError::Configuration)?;
        if certificates.len() != 1 {
            return Err(RestrictedDeviceCaError::Configuration);
        }
        let certificate = certificates
            .pop()
            .ok_or(RestrictedDeviceCaError::Configuration)?;
        let signing_key = PKey::private_key_from_pem(&signing_key_pem)
            .map_err(|_| RestrictedDeviceCaError::Configuration)?;
        validate_ca_material(&certificate, &signing_key)?;

        let options = PgConnectOptions::from_str(database_url)
            .map_err(|_| RestrictedDeviceCaError::Configuration)?
            .ssl_mode(PgSslMode::VerifyFull)
            .application_name("cyrene-workspace-device-ca")
            .options([("statement_timeout", "5000"), ("lock_timeout", "3000")]);
        let (sender, commands) = mpsc::sync_channel(QUEUE_CAPACITY);
        let (ready_sender, ready) = mpsc::sync_channel(1);
        let certificate_der = certificate
            .to_der()
            .map_err(|_| RestrictedDeviceCaError::Configuration)?;
        let certificate_der_for_checker = certificate_der.clone();
        let worker = thread::Builder::new()
            .name("workspace-device-ca-postgres".to_owned())
            .spawn(move || {
                worker_main(
                    options,
                    certificate_der,
                    signing_key_pem,
                    commands,
                    ready_sender,
                )
            })
            .map_err(|_| RestrictedDeviceCaError::Unavailable)?;

        match ready.recv_timeout(STARTUP_TIMEOUT) {
            Ok(Ok(())) => Ok(Self {
                inner: Arc::new(CaInner {
                    sender: Some(sender),
                    worker: Mutex::new(Some(worker)),
                    issuer_id,
                    certificate_der: certificate_der_for_checker,
                }),
            }),
            Ok(Err(error)) => {
                drop(sender);
                let _ = worker.join();
                Err(error)
            }
            Err(_) => {
                drop(sender);
                let _ = worker.join();
                Err(RestrictedDeviceCaError::Unavailable)
            }
        }
    }

    /// Apply CA state tables with a separately provisioned database operator credential.
    pub fn migrate(database_url: &str) -> Result<(), RestrictedDeviceCaError> {
        let options = PgConnectOptions::from_str(database_url)
            .map_err(|_| RestrictedDeviceCaError::Configuration)?
            .ssl_mode(PgSslMode::VerifyFull)
            .application_name("cyrene-workspace-device-ca-migration")
            .options([("statement_timeout", "5000"), ("lock_timeout", "3000")]);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| RestrictedDeviceCaError::Unavailable)?;
        runtime.block_on(async move {
            let pool = PgPoolOptions::new()
                .max_connections(1)
                .acquire_timeout(STARTUP_TIMEOUT)
                .after_connect(|connection, _| {
                    Box::pin(async move {
                        // SQLx stores its migration ledger in the active search_path. Create the
                        // CA schema first, then keep this component's version history separate
                        // from the Directory migrator's public `_sqlx_migrations` table.
                        sqlx::query("CREATE SCHEMA IF NOT EXISTS cyrene_workspace_device_ca")
                            .execute(&mut *connection)
                            .await?;
                        sqlx::query("SET search_path TO cyrene_workspace_device_ca, pg_catalog")
                            .execute(&mut *connection)
                            .await?;
                        Ok(())
                    })
                })
                .connect_with(options)
                .await
                .map_err(|_| RestrictedDeviceCaError::Unavailable)?;
            MIGRATOR
                .run(&pool)
                .await
                .map_err(|_| RestrictedDeviceCaError::Unavailable)?;
            pool.close().await;
            Ok(())
        })
    }

    /// Apply CA migrations with the separately provisioned operator URL.
    pub fn migrate_from_environment() -> Result<(), RestrictedDeviceCaError> {
        let database_url = std::env::var(MIGRATION_DATABASE_URL_ENV)
            .map_err(|_| RestrictedDeviceCaError::Configuration)?;
        Self::migrate(&database_url)
    }

    /// Return a CRL-backed checker implementing both issued-leaf and Relay-peer validation.
    pub fn revocation_checker(&self) -> Arc<PostgresSignedCrlChecker> {
        Arc::new(PostgresSignedCrlChecker { ca: self.clone() })
    }

    /// Returns the issuer identifier parsed from trusted startup configuration.
    pub fn issuer_id(&self) -> &str {
        &self.inner.issuer_id
    }

    /// Returns the configured CA certificate DER as a public trust anchor.
    pub fn trusted_root_der(&self) -> &[u8] {
        &self.inner.certificate_der
    }

    /// Verify the database-held signed CRL and return readiness only when it is current.
    pub fn check_current_crl(&self) -> Result<(), RestrictedDeviceCaError> {
        let (reply, result) = mpsc::sync_channel(1);
        self.call(Command::CheckHealth(reply), result)
            .map_err(|_| RestrictedDeviceCaError::Unavailable)
    }

    /// Retire every issued certificate for one exact device key and re-sign the CRL after each.
    ///
    /// Registry revocation must commit before this method is called. Replaying it is safe because
    /// each certificate is retired through the existing idempotent retirement path.
    pub fn retire_device_certificates(
        &self,
        key: &WorkspaceDeviceKey,
    ) -> Result<(), RestrictedDeviceCaError> {
        let device_key = DeviceAuthorizationDeviceKey {
            organization_id: key.organization_id.clone(),
            workspace_id: key.workspace_id.clone(),
            device_id: key.device_id.clone(),
        };
        let (reply, result) = mpsc::sync_channel(1);
        self.call(Command::RetireDevice(device_key, reply), result)
            .map_err(|_| RestrictedDeviceCaError::Unavailable)
    }

    /// Look up an exact durable issuance without creating a new certificate.
    /// The Directory binding, CSR, SPKI, authorization ID, and issue time must
    /// match the committed receipt before the certificate is returned.
    pub fn lookup_issuance(
        &self,
        authorization_id: &DeviceAuthorizationId,
        registration_binding: &DeviceAuthorizationRegistrationBinding,
        csr_der: &[u8],
        issued_at_unix_ms: u64,
    ) -> Result<Option<IssuedDeviceCertificate>, DeviceCertificateIssuanceError> {
        let input = IssueInput::new(
            *authorization_id,
            registration_binding,
            csr_der,
            issued_at_unix_ms,
        )
        .map_err(|_| DeviceCertificateIssuanceError::DefinitiveNoCommit)?;
        let (reply, result) = mpsc::sync_channel(1);
        self.call(Command::Lookup(input, reply), result)
            .map_err(|error| match error {
                CaError::Conflict | CaError::Rejected => {
                    DeviceCertificateIssuanceError::DefinitiveNoCommit
                }
                CaError::Unavailable => DeviceCertificateIssuanceError::OutcomeUnknown,
            })
    }

    fn call<T>(&self, command: Command, reply: Receiver<CaResult<T>>) -> CaResult<T> {
        let sender = self.inner.sender.as_ref().ok_or(CaError::Unavailable)?;
        match sender.try_send(command) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                return Err(CaError::Unavailable)
            }
        }
        reply
            .recv_timeout(REQUEST_TIMEOUT)
            .map_err(|_| CaError::Unavailable)?
    }
}

impl Drop for CaInner {
    fn drop(&mut self) {
        self.sender.take();
        if let Ok(mut worker) = self.worker.lock() {
            if let Some(worker) = worker.take() {
                let _ = worker.join();
            }
        }
    }
}

/// Signed CRL checker shared by issued-certificate acceptance and native Relay peer validation.
pub struct PostgresSignedCrlChecker {
    ca: PostgresRestrictedDeviceCa,
}

/// Read-only native Relay checker for signed CRL and issuance state.
///
/// This worker receives only the public CA certificate and a read-only database role. It does not
/// load or retain the issuer private key used by `PostgresRestrictedDeviceCa`.
pub struct PostgresRelayPeerSignedCrlChecker {
    sender: Option<SyncSender<RelayCrlCommand>>,
    worker: Mutex<Option<JoinHandle<()>>>,
    certificate_der: Vec<u8>,
}

enum RelayCrlCommand {
    Check {
        certificate_der: Vec<u8>,
        serial_number: Vec<u8>,
        certificate_sha256: [u8; 32],
        checked_at_unix_ms: u64,
        reply: SyncSender<Result<RelayCrlStatus, RelayPeerRevocationCheckError>>,
    },
    Health(SyncSender<Result<(), RestrictedDeviceCaError>>),
}

#[derive(Clone, Copy)]
enum RelayCrlStatus {
    Good {
        this_update_unix_ms: u64,
        next_update_unix_ms: u64,
    },
    Revoked,
}

struct RelayCrlRuntime {
    pool: PgPool,
    certificate: X509,
}

impl DeviceCertificateRevocationChecker for PostgresSignedCrlChecker {
    fn require_current_good_status(
        &self,
        query: &DeviceCertificateRevocationQuery<'_>,
    ) -> Result<(), DeviceCertificateRevocationCheckError> {
        if !query_binds_to_configured_ca(
            query.certificate_der(),
            query.serial_number(),
            query.certificate_sha256(),
            query.ca_chain_der(),
            query.trusted_roots_der(),
            &self.ca,
        ) {
            return Err(DeviceCertificateRevocationCheckError::Unknown);
        }
        let (reply, result) = mpsc::sync_channel(1);
        let status = self
            .ca
            .call(
                Command::CheckCertificate {
                    certificate_der: query.certificate_der().to_vec(),
                    serial_number: query.serial_number().to_vec(),
                    certificate_sha256: *query.certificate_sha256(),
                    checked_at_unix_ms: query.checked_at_unix_ms(),
                    reply,
                },
                result,
            )
            .map_err(|_| DeviceCertificateRevocationCheckError::Unknown)?;
        match status {
            CaRevocationStatus::Good { .. } => Ok(()),
            CaRevocationStatus::Revoked => Err(DeviceCertificateRevocationCheckError::Revoked),
        }
    }
}

impl RelayPeerCertificateRevocationChecker for PostgresSignedCrlChecker {
    fn require_current_good_status(
        &self,
        query: &RelayPeerRevocationQuery<'_>,
    ) -> Result<CurrentRelayPeerRevocationEvidence, RelayPeerRevocationCheckError> {
        if !query_bindings_match_ca(
            query.certificate_der(),
            query.serial_number(),
            query.certificate_sha256(),
            query.intermediate_chain_der(),
            query.trusted_roots_der(),
            &self.ca,
        ) {
            return Err(RelayPeerRevocationCheckError::Unknown);
        }
        let (reply, result) = mpsc::sync_channel(1);
        let status = self
            .ca
            .call(
                Command::CheckCertificate {
                    certificate_der: query.certificate_der().to_vec(),
                    serial_number: query.serial_number().to_vec(),
                    certificate_sha256: *query.certificate_sha256(),
                    checked_at_unix_ms: query.checked_at_unix_ms(),
                    reply,
                },
                result,
            )
            .map_err(|_| RelayPeerRevocationCheckError::Unknown)?;
        let CaRevocationStatus::Good {
            this_update_unix_ms,
            next_update_unix_ms,
        } = status
        else {
            return Err(RelayPeerRevocationCheckError::Unknown);
        };
        Ok(
            CurrentRelayPeerRevocationEvidence::from_verified_good_status(
                *query.certificate_sha256(),
                this_update_unix_ms,
                next_update_unix_ms,
            ),
        )
    }
}

impl PostgresRelayPeerSignedCrlChecker {
    /// Connect using the CA schema's restricted read-only runtime URL and public certificate.
    ///
    /// The Relay process must not receive `CYRENE_WORKSPACE_DEVICE_CA_SIGNING_KEY_FILE`.
    pub fn connect_from_environment() -> Result<Self, RestrictedDeviceCaError> {
        let database_url = std::env::var(RELAY_DATABASE_URL_ENV)
            .map_err(|_| RestrictedDeviceCaError::Configuration)?;
        let certificate_path = required_path(CERTIFICATE_FILE_ENV)?;
        let issuer_id =
            std::env::var(ISSUER_ID_ENV).map_err(|_| RestrictedDeviceCaError::Configuration)?;
        Self::connect(&database_url, &certificate_path, &issuer_id)
    }

    /// Connect without reading or accepting any CA private-key material.
    pub fn connect(
        database_url: &str,
        certificate_path: &Path,
        issuer_id: &str,
    ) -> Result<Self, RestrictedDeviceCaError> {
        if issuer_id.trim().is_empty()
            || issuer_id.trim() != issuer_id
            || issuer_id.len() > 256
            || issuer_id.chars().any(char::is_control)
        {
            return Err(RestrictedDeviceCaError::Configuration);
        }
        let certificate_pem = read_regular_file(certificate_path, MAX_CA_FILE_BYTES)?;
        let mut certificates = X509::stack_from_pem(&certificate_pem)
            .map_err(|_| RestrictedDeviceCaError::Configuration)?;
        if certificates.len() != 1 {
            return Err(RestrictedDeviceCaError::Configuration);
        }
        let certificate = certificates
            .pop()
            .ok_or(RestrictedDeviceCaError::Configuration)?;
        validate_public_ca_certificate(&certificate)?;
        let certificate_der = certificate
            .to_der()
            .map_err(|_| RestrictedDeviceCaError::Configuration)?;
        let options = PgConnectOptions::from_str(database_url)
            .map_err(|_| RestrictedDeviceCaError::Configuration)?
            .ssl_mode(PgSslMode::VerifyFull)
            .application_name("cyrene-workspace-relay-crl-checker")
            .options([("statement_timeout", "5000"), ("lock_timeout", "3000")]);
        let (sender, commands) = mpsc::sync_channel(QUEUE_CAPACITY);
        let (ready_sender, ready) = mpsc::sync_channel(1);
        let certificate_for_worker = certificate.clone();
        let worker = thread::Builder::new()
            .name("workspace-relay-signed-crl-reader".to_owned())
            .spawn(move || {
                relay_crl_worker(options, certificate_for_worker, commands, ready_sender)
            })
            .map_err(|_| RestrictedDeviceCaError::Unavailable)?;
        match ready.recv_timeout(STARTUP_TIMEOUT) {
            Ok(Ok(())) => Ok(Self {
                sender: Some(sender),
                worker: Mutex::new(Some(worker)),
                certificate_der,
            }),
            Ok(Err(error)) => {
                drop(sender);
                let _ = worker.join();
                Err(error)
            }
            Err(_) => {
                drop(sender);
                let _ = worker.join();
                Err(RestrictedDeviceCaError::Unavailable)
            }
        }
    }

    /// Check database time, current CRL signature, CRL freshness, and its complete revocation ledger.
    pub fn check_current_crl(&self) -> Result<(), RestrictedDeviceCaError> {
        let (reply, result) = mpsc::sync_channel(1);
        self.call(RelayCrlCommand::Health(reply), result)
    }

    fn call(
        &self,
        command: RelayCrlCommand,
        reply: Receiver<Result<(), RestrictedDeviceCaError>>,
    ) -> Result<(), RestrictedDeviceCaError> {
        let sender = self
            .sender
            .as_ref()
            .ok_or(RestrictedDeviceCaError::Unavailable)?;
        match sender.try_send(command) {
            Ok(()) => reply
                .recv_timeout(REQUEST_TIMEOUT)
                .map_err(|_| RestrictedDeviceCaError::Unavailable)?,
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                Err(RestrictedDeviceCaError::Unavailable)
            }
        }
    }
}

impl RelayPeerCertificateRevocationChecker for PostgresRelayPeerSignedCrlChecker {
    fn require_current_good_status(
        &self,
        query: &RelayPeerRevocationQuery<'_>,
    ) -> Result<CurrentRelayPeerRevocationEvidence, RelayPeerRevocationCheckError> {
        if !query_bindings_match_trusted_ca(
            query.certificate_der(),
            query.serial_number(),
            query.certificate_sha256(),
            query.intermediate_chain_der(),
            query.trusted_roots_der(),
            &self.certificate_der,
        ) {
            return Err(RelayPeerRevocationCheckError::Unknown);
        }
        let (reply, result) = mpsc::sync_channel(1);
        self.sender
            .as_ref()
            .ok_or(RelayPeerRevocationCheckError::Unknown)?
            .try_send(RelayCrlCommand::Check {
                certificate_der: query.certificate_der().to_vec(),
                serial_number: query.serial_number().to_vec(),
                certificate_sha256: *query.certificate_sha256(),
                checked_at_unix_ms: query.checked_at_unix_ms(),
                reply,
            })
            .map_err(|_| RelayPeerRevocationCheckError::Unknown)?;
        match result
            .recv_timeout(REQUEST_TIMEOUT)
            .map_err(|_| RelayPeerRevocationCheckError::Unknown)?
        {
            Ok(RelayCrlStatus::Good {
                this_update_unix_ms,
                next_update_unix_ms,
            }) => Ok(
                CurrentRelayPeerRevocationEvidence::from_verified_good_status(
                    *query.certificate_sha256(),
                    this_update_unix_ms,
                    next_update_unix_ms,
                ),
            ),
            Ok(RelayCrlStatus::Revoked) => Err(RelayPeerRevocationCheckError::Revoked),
            Err(error) => Err(error),
        }
    }
}

impl WorkspaceDevicePeerCertificateStatusChecker for PostgresRelayPeerSignedCrlChecker {
    fn require_current_good_status_for_peer_chain(
        &self,
        leaf_certificate_der: &[u8],
        intermediate_chain_der: &[Vec<u8>],
        checked_at_unix_ms: u64,
    ) -> Result<CurrentRelayPeerRevocationEvidence, RelayPeerRevocationCheckError> {
        let certificate = X509::from_der(leaf_certificate_der)
            .map_err(|_| RelayPeerRevocationCheckError::Unknown)?;
        let serial_number =
            canonical_serial(&certificate).map_err(|_| RelayPeerRevocationCheckError::Unknown)?;
        let certificate_sha256 = Sha256::digest(leaf_certificate_der).into();
        let trusted_roots_der = vec![self.certificate_der.clone()];
        let query = RelayPeerRevocationQuery::new(
            leaf_certificate_der,
            intermediate_chain_der,
            &trusted_roots_der,
            &serial_number,
            certificate_sha256,
            checked_at_unix_ms,
        );

        RelayPeerCertificateRevocationChecker::require_current_good_status(self, &query)
    }
}

impl Drop for PostgresRelayPeerSignedCrlChecker {
    fn drop(&mut self) {
        drop(self.sender.take());
        if let Ok(worker) = self.worker.get_mut() {
            if let Some(worker) = worker.take() {
                let _ = worker.join();
            }
        }
    }
}

fn relay_crl_worker(
    options: PgConnectOptions,
    certificate: X509,
    commands: Receiver<RelayCrlCommand>,
    ready: SyncSender<Result<(), RestrictedDeviceCaError>>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => {
            let _ = ready.send(Err(RestrictedDeviceCaError::Unavailable));
            return;
        }
    };
    let pool = match runtime.block_on(async {
        PgPoolOptions::new()
            .max_connections(2)
            .acquire_timeout(STARTUP_TIMEOUT)
            .after_connect(|connection, _| {
                Box::pin(async move {
                    sqlx::query("SET search_path TO cyrene_workspace_device_ca, pg_catalog")
                        .execute(connection)
                        .await?;
                    Ok(())
                })
            })
            .connect_with(options)
            .await
            .map_err(|_| RestrictedDeviceCaError::Unavailable)
    }) {
        Ok(pool) => pool,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    let state = RelayCrlRuntime { pool, certificate };
    match runtime.block_on(check_relay_crl_health(&state)) {
        Ok(()) => {
            if ready.send(Ok(())).is_err() {
                runtime.block_on(state.pool.close());
                return;
            }
        }
        Err(_) => {
            let _ = ready.send(Err(RestrictedDeviceCaError::Unavailable));
            runtime.block_on(state.pool.close());
            return;
        }
    }

    while let Ok(command) = commands.recv() {
        match command {
            RelayCrlCommand::Health(reply) => {
                let result = runtime
                    .block_on(check_relay_crl_health(&state))
                    .map_err(|_| RestrictedDeviceCaError::Unavailable);
                let _ = reply.send(result);
            }
            RelayCrlCommand::Check {
                certificate_der,
                serial_number,
                certificate_sha256,
                checked_at_unix_ms,
                reply,
            } => {
                let result = runtime.block_on(check_relay_certificate_status(
                    &state,
                    &certificate_der,
                    &serial_number,
                    &certificate_sha256,
                    checked_at_unix_ms,
                ));
                let _ = reply.send(result);
            }
        }
    }
    runtime.block_on(state.pool.close());
}

impl DeviceCertificateIssuer for PostgresRestrictedDeviceCa {
    fn issue_device_certificate(
        &self,
        authorization_id: &DeviceAuthorizationId,
        registration_binding: &DeviceAuthorizationRegistrationBinding,
        csr_der: &[u8],
        issued_at_unix_ms: u64,
    ) -> Result<IssuedDeviceCertificate, DeviceCertificateIssuanceError> {
        let input = IssueInput::new(
            *authorization_id,
            registration_binding,
            csr_der,
            issued_at_unix_ms,
        )
        .map_err(|_| DeviceCertificateIssuanceError::DefinitiveNoCommit)?;
        let (reply, result) = mpsc::sync_channel(1);
        self.call(Command::Issue(input, reply), result)
            .map_err(|error| match error {
                CaError::Conflict | CaError::Rejected => {
                    DeviceCertificateIssuanceError::DefinitiveNoCommit
                }
                CaError::Unavailable => DeviceCertificateIssuanceError::OutcomeUnknown,
            })
    }
}

impl DeviceCertificateRetirementPort for PostgresRestrictedDeviceCa {
    fn declared_hard_timeout(&self) -> Option<Duration> {
        Some(REQUEST_TIMEOUT)
    }

    fn retire_or_confirm(
        &self,
        authorization_id: &DeviceAuthorizationId,
        certificate_sha256: &[u8; 32],
        certificate: &IssuedDeviceCertificate,
        _reason: DeviceCertificateRetirementReason,
    ) -> Result<(), DeviceCertificateRetirementError> {
        let input = RetireInput {
            authorization_id: *authorization_id,
            certificate_sha256: *certificate_sha256,
            certificate_der: certificate.certificate_der.clone(),
            serial_number: certificate.serial_number.clone(),
            registration_binding_id: certificate.registration_binding_id,
            device_key: certificate.device_key.clone(),
            authorization_generation: certificate.authorization_generation,
            spki_sha256: certificate.spki_sha256,
        };
        let (reply, result) = mpsc::sync_channel(1);
        self.call(Command::Retire(input, reply), result)
            .map_err(|error| match error {
                CaError::Rejected | CaError::Conflict => DeviceCertificateRetirementError::Rejected,
                CaError::Unavailable => DeviceCertificateRetirementError::OutcomeUnknown,
            })
    }
}

impl DeviceAuthorizationClockPort for PostgresRestrictedDeviceCa {
    fn current_unix_ms(&self) -> Result<u64, DeviceAuthorizationPortError> {
        let (reply, result) = mpsc::sync_channel(1);
        self.call(Command::DatabaseTime(reply), result)
            .map_err(|_| DeviceAuthorizationPortError::Unavailable)
    }
}

impl DeviceCertificatePublicMetadataPort for PostgresRestrictedDeviceCa {
    fn public_metadata(
        &self,
        delivery: &crate::device_authorization::DeviceCertificateDelivery,
    ) -> Result<DeviceCertificatePublicMetadata, DeviceAuthorizationPortError> {
        let certificate = X509::from_der(&delivery.certificate_der)
            .map_err(|_| DeviceAuthorizationPortError::Unavailable)?;
        let certificate_der = certificate
            .to_der()
            .map_err(|_| DeviceAuthorizationPortError::Unavailable)?;
        let (_, parsed) = x509_parser::parse_x509_certificate(&certificate_der)
            .map_err(|_| DeviceAuthorizationPortError::Unavailable)?;
        let not_before_unix_ms =
            u64::try_from(parsed.tbs_certificate.validity.not_before.timestamp())
                .ok()
                .and_then(|seconds| seconds.checked_mul(1_000))
                .ok_or(DeviceAuthorizationPortError::Unavailable)?;
        Ok(DeviceCertificatePublicMetadata {
            issuer_id: self.inner.issuer_id.clone(),
            not_before_unix_ms,
        })
    }
}

#[derive(Clone)]
struct IssueInput {
    authorization_id: DeviceAuthorizationId,
    registration_binding_id: [u8; 16],
    device_key: DeviceAuthorizationDeviceKey,
    authorization_generation: u64,
    csr_der: Vec<u8>,
    csr_sha256: [u8; 32],
    spki_sha256: [u8; 32],
    scope: DeviceAuthorizationScope,
    issued_at_unix_ms: u64,
    request_sha256: [u8; 32],
}

impl IssueInput {
    fn new(
        authorization_id: DeviceAuthorizationId,
        binding: &DeviceAuthorizationRegistrationBinding,
        csr_der: &[u8],
        issued_at_unix_ms: u64,
    ) -> Result<Self, ()> {
        if csr_der.is_empty() || csr_der.len() > MAX_CSR_DER_BYTES || issued_at_unix_ms == 0 {
            return Err(());
        }
        let device_key = binding.key().clone();
        if device_key.organization_id.trim().is_empty()
            || device_key.workspace_id.trim().is_empty()
            || device_key.device_id.trim().is_empty()
            || binding.authorization_generation() == 0
            || <[u8; 32]>::from(Sha256::digest(csr_der)) != *binding.csr_sha256()
        {
            return Err(());
        }
        let csr = X509Req::from_der(csr_der).map_err(|_| ())?;
        if csr.to_der().map_err(|_| ())?.as_slice() != csr_der {
            return Err(());
        }
        let public_key = csr.public_key().map_err(|_| ())?;
        if !csr.verify(&public_key).map_err(|_| ())?
            || <[u8; 32]>::from(Sha256::digest(
                public_key.public_key_to_der().map_err(|_| ())?,
            )) != *binding.spki_sha256()
        {
            return Err(());
        }
        let scope = DeviceAuthorizationScope {
            organization_id: device_key.organization_id.clone(),
            workspace_id: device_key.workspace_id.clone(),
        };
        let mut digest = Sha256::new();
        digest.update(b"cyrene.workspace.device-ca.issue.v1\0");
        digest.update(authorization_id);
        digest.update(binding.binding_id());
        hash_field(&mut digest, device_key.organization_id.as_bytes());
        hash_field(&mut digest, device_key.workspace_id.as_bytes());
        hash_field(&mut digest, device_key.device_id.as_bytes());
        digest.update(binding.authorization_generation().to_be_bytes());
        digest.update(binding.csr_sha256());
        digest.update(binding.spki_sha256());
        digest.update(issued_at_unix_ms.to_be_bytes());
        hash_field(&mut digest, csr_der);
        Ok(Self {
            authorization_id,
            registration_binding_id: *binding.binding_id(),
            device_key,
            authorization_generation: binding.authorization_generation(),
            csr_der: csr_der.to_vec(),
            csr_sha256: *binding.csr_sha256(),
            spki_sha256: *binding.spki_sha256(),
            scope,
            issued_at_unix_ms,
            request_sha256: digest.finalize().into(),
        })
    }
}

struct RetireInput {
    authorization_id: DeviceAuthorizationId,
    certificate_sha256: [u8; 32],
    certificate_der: Vec<u8>,
    serial_number: Vec<u8>,
    registration_binding_id: [u8; 16],
    device_key: DeviceAuthorizationDeviceKey,
    authorization_generation: u64,
    spki_sha256: [u8; 32],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaError {
    Rejected,
    Conflict,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaRevocationStatus {
    Good {
        this_update_unix_ms: u64,
        next_update_unix_ms: u64,
    },
    Revoked,
}

type CaResult<T> = Result<T, CaError>;
type Reply<T> = SyncSender<CaResult<T>>;

enum Command {
    Issue(IssueInput, Reply<IssuedDeviceCertificate>),
    Lookup(IssueInput, Reply<Option<IssuedDeviceCertificate>>),
    Retire(RetireInput, Reply<()>),
    RetireDevice(DeviceAuthorizationDeviceKey, Reply<()>),
    CheckCertificate {
        certificate_der: Vec<u8>,
        serial_number: Vec<u8>,
        certificate_sha256: [u8; 32],
        checked_at_unix_ms: u64,
        reply: Reply<CaRevocationStatus>,
    },
    CheckHealth(Reply<()>),
    DatabaseTime(Reply<u64>),
}

struct CaRuntime {
    pool: PgPool,
    certificate: X509,
    signer: PKey<Private>,
}

fn worker_main(
    options: PgConnectOptions,
    certificate_der: Vec<u8>,
    signing_key_pem: Zeroizing<Vec<u8>>,
    commands: Receiver<Command>,
    ready: SyncSender<Result<(), RestrictedDeviceCaError>>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => {
            let _ = ready.send(Err(RestrictedDeviceCaError::Unavailable));
            return;
        }
    };
    let setup = runtime.block_on(async {
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .acquire_timeout(STARTUP_TIMEOUT)
            .after_connect(|connection, _| {
                Box::pin(async move {
                    sqlx::query("SET search_path TO cyrene_workspace_device_ca, pg_catalog")
                        .execute(connection)
                        .await?;
                    Ok(())
                })
            })
            .connect_with(options)
            .await
            .map_err(|_| RestrictedDeviceCaError::Unavailable)?;
        let migration_check = sqlx::query_scalar::<_, bool>(
            "SELECT to_regclass($1) IS NOT NULL AND to_regclass($2) IS NOT NULL",
        )
        .bind(TABLE)
        .bind(CRL_TABLE)
        .fetch_one(&pool)
        .await
        .map_err(|_| RestrictedDeviceCaError::Unavailable)?;
        if !migration_check {
            return Err(RestrictedDeviceCaError::Unavailable);
        }
        let certificate =
            X509::from_der(&certificate_der).map_err(|_| RestrictedDeviceCaError::Configuration)?;
        let signer = PKey::private_key_from_pem(&signing_key_pem)
            .map_err(|_| RestrictedDeviceCaError::Configuration)?;
        validate_ca_material(&certificate, &signer)?;
        let mut state = CaRuntime {
            pool,
            certificate,
            signer,
        };
        ensure_fresh_crl(&mut state).await?;
        Ok(state)
    });
    let mut state = match setup {
        Ok(state) => {
            let _ = ready.send(Ok(()));
            state
        }
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };

    let mut next_crl_refresh = Instant::now() + CRL_REFRESH_INTERVAL;
    loop {
        let wait = next_crl_refresh.saturating_duration_since(Instant::now());
        let command = match commands.recv_timeout(wait) {
            Ok(command) => Some(command),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        if Instant::now() >= next_crl_refresh {
            if runtime.block_on(ensure_fresh_crl(&mut state)).is_err() {
                tracing::warn!(
                    event.name = "platform.workspace_device_ca.crl_refresh_failed",
                    message =
                        "Signed device CRL refresh failed; certificate checks remain fail-closed",
                );
            }
            next_crl_refresh = Instant::now() + CRL_REFRESH_INTERVAL;
        }
        let Some(command) = command else {
            continue;
        };
        match command {
            Command::Issue(input, reply) => {
                let result = runtime.block_on(issue_certificate(&mut state, input));
                let _ = reply.send(result);
            }
            Command::Lookup(input, reply) => {
                let result = runtime.block_on(lookup_issuance(&mut state, input));
                let _ = reply.send(result);
            }
            Command::Retire(input, reply) => {
                let result = runtime.block_on(retire_certificate(&mut state, input));
                let _ = reply.send(result);
            }
            Command::RetireDevice(key, reply) => {
                let result = runtime.block_on(retire_device_certificates(&mut state, key));
                let _ = reply.send(result);
            }
            Command::CheckCertificate {
                certificate_der,
                serial_number,
                certificate_sha256,
                checked_at_unix_ms,
                reply,
            } => {
                let result = runtime.block_on(check_certificate_status(
                    &state,
                    &certificate_der,
                    &serial_number,
                    &certificate_sha256,
                    checked_at_unix_ms,
                ));
                let _ = reply.send(result);
            }
            Command::CheckHealth(reply) => {
                let result = runtime.block_on(check_crl_health(&state));
                let _ = reply.send(result);
            }
            Command::DatabaseTime(reply) => {
                let result = runtime.block_on(async {
                    sqlx::query_scalar::<_, i64>(
                        "SELECT floor(extract(epoch FROM clock_timestamp()) * 1000)::BIGINT",
                    )
                    .fetch_one(&state.pool)
                    .await
                    .map_err(|_| CaError::Unavailable)
                    .and_then(|value| u64::try_from(value).map_err(|_| CaError::Unavailable))
                });
                let _ = reply.send(result);
            }
        }
    }
    runtime.block_on(state.pool.close());
}

async fn issue_certificate(
    state: &mut CaRuntime,
    input: IssueInput,
) -> CaResult<IssuedDeviceCertificate> {
    let mut transaction = state.pool.begin().await.map_err(|_| CaError::Unavailable)?;
    lock_authorization_id(&mut transaction, &input.authorization_id).await?;
    if let Some(record) = load_issued_record(&mut transaction, &input.authorization_id).await? {
        if record.request_sha256 != input.request_sha256 {
            return Err(CaError::Conflict);
        }
        return issued_from_record(state, record, &input);
    }

    let certificate = sign_device_leaf(state, &input)?;
    let certificate_der = certificate.to_der().map_err(|_| CaError::Rejected)?;
    if certificate_der.is_empty() || certificate_der.len() > MAX_CERTIFICATE_DER_BYTES {
        return Err(CaError::Rejected);
    }
    let serial_number = canonical_serial(&certificate)?;
    let not_after_unix_ms =
        x509_timestamp_millis(&certificate_der, true).ok_or(CaError::Rejected)?;
    let certificate_sha256: [u8; 32] = Sha256::digest(&certificate_der).into();

    sqlx::query(
        "INSERT INTO cyrene_workspace_device_ca.issued_certificates \
         (authorization_id, request_sha256, registration_binding_id, organization_id, workspace_id, \
          device_id, authorization_generation, csr_der, csr_sha256, spki_sha256, issued_at_unix_ms, \
          certificate_der, certificate_sha256, serial_number, not_after_unix_ms) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)",
    )
    .bind(input.authorization_id.as_slice())
    .bind(input.request_sha256.as_slice())
    .bind(uuid::Uuid::from_bytes(input.registration_binding_id))
    .bind(&input.device_key.organization_id)
    .bind(&input.device_key.workspace_id)
    .bind(&input.device_key.device_id)
    .bind(i64::try_from(input.authorization_generation).map_err(|_| CaError::Rejected)?)
    .bind(&input.csr_der)
    .bind(input.csr_sha256.as_slice())
    .bind(input.spki_sha256.as_slice())
    .bind(i64::try_from(input.issued_at_unix_ms).map_err(|_| CaError::Rejected)?)
    .bind(&certificate_der)
    .bind(certificate_sha256.as_slice())
    .bind(&serial_number)
    .bind(i64::try_from(not_after_unix_ms).map_err(|_| CaError::Rejected)?)
    .execute(&mut *transaction)
    .await
    .map_err(|_| CaError::Unavailable)?;

    // The signer uses RSA PKCS#1 v1.5, a request-derived serial, and fixed
    // validity times, so an exact retry after an ambiguous commit signs the
    // same DER and serial before PostgreSQL deduplicates by authorization ID.
    transaction
        .commit()
        .await
        .map_err(|_| CaError::Unavailable)?;
    Ok(IssuedDeviceCertificate {
        certificate_der,
        ca_chain_der: Vec::new(),
        serial_number,
        registration_binding_id: input.registration_binding_id,
        device_key: input.device_key.clone(),
        authorization_generation: input.authorization_generation,
        scope: input.scope.clone(),
        spki_sha256: input.spki_sha256,
        not_after_unix_ms,
    })
}

async fn lookup_issuance(
    state: &mut CaRuntime,
    input: IssueInput,
) -> CaResult<Option<IssuedDeviceCertificate>> {
    let mut transaction = state.pool.begin().await.map_err(|_| CaError::Unavailable)?;
    lock_authorization_id(&mut transaction, &input.authorization_id).await?;
    let Some(record) = load_issued_record(&mut transaction, &input.authorization_id).await? else {
        transaction
            .rollback()
            .await
            .map_err(|_| CaError::Unavailable)?;
        return Ok(None);
    };
    let issued = issued_from_record(state, record, &input)?;
    transaction
        .commit()
        .await
        .map_err(|_| CaError::Unavailable)?;
    Ok(Some(issued))
}

async fn retire_certificate(state: &mut CaRuntime, input: RetireInput) -> CaResult<()> {
    let expected_fingerprint: [u8; 32] = Sha256::digest(&input.certificate_der).into();
    if input.certificate_der.is_empty()
        || input.certificate_der.len() > MAX_CERTIFICATE_DER_BYTES
        || expected_fingerprint != input.certificate_sha256
    {
        return Err(CaError::Rejected);
    }
    let mut transaction = state.pool.begin().await.map_err(|_| CaError::Unavailable)?;
    lock_authorization_id(&mut transaction, &input.authorization_id).await?;
    let row = sqlx::query(
        "SELECT request_sha256, registration_binding_id, organization_id, workspace_id, device_id, \
                authorization_generation, csr_sha256, spki_sha256, certificate_der, certificate_sha256, \
                serial_number, not_after_unix_ms, revoked_at_unix_ms \
         FROM cyrene_workspace_device_ca.issued_certificates WHERE authorization_id = $1 FOR UPDATE",
    )
    .bind(input.authorization_id.as_slice())
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|_| CaError::Unavailable)?
    .ok_or(CaError::Rejected)?;
    let certificate_der: Vec<u8> = row
        .try_get("certificate_der")
        .map_err(|_| CaError::Unavailable)?;
    let certificate_sha256: Vec<u8> = row
        .try_get("certificate_sha256")
        .map_err(|_| CaError::Unavailable)?;
    let serial_number: Vec<u8> = row
        .try_get("serial_number")
        .map_err(|_| CaError::Unavailable)?;
    let registration_binding_id: uuid::Uuid = row
        .try_get("registration_binding_id")
        .map_err(|_| CaError::Unavailable)?;
    let generation: i64 = row
        .try_get("authorization_generation")
        .map_err(|_| CaError::Unavailable)?;
    let spki_sha256: Vec<u8> = row
        .try_get("spki_sha256")
        .map_err(|_| CaError::Unavailable)?;
    let organization_id: String = row
        .try_get("organization_id")
        .map_err(|_| CaError::Unavailable)?;
    let workspace_id: String = row
        .try_get("workspace_id")
        .map_err(|_| CaError::Unavailable)?;
    let device_id: String = row.try_get("device_id").map_err(|_| CaError::Unavailable)?;
    if certificate_der != input.certificate_der
        || certificate_sha256.as_slice() != input.certificate_sha256
        || serial_number != input.serial_number
        || *registration_binding_id.as_bytes() != input.registration_binding_id
        || generation
            != i64::try_from(input.authorization_generation).map_err(|_| CaError::Rejected)?
        || spki_sha256.as_slice() != input.spki_sha256
        || organization_id != input.device_key.organization_id
        || workspace_id != input.device_key.workspace_id
        || device_id != input.device_key.device_id
    {
        return Err(CaError::Conflict);
    }
    let revoked_at: Option<i64> = row
        .try_get("revoked_at_unix_ms")
        .map_err(|_| CaError::Unavailable)?;
    if revoked_at.is_none() {
        let now = database_time_unix_ms(&state.pool).await?;
        sqlx::query(
            "UPDATE cyrene_workspace_device_ca.issued_certificates \
             SET revoked_at_unix_ms = $2 WHERE authorization_id = $1 AND revoked_at_unix_ms IS NULL",
        )
        .bind(input.authorization_id.as_slice())
        .bind(i64::try_from(now).map_err(|_| CaError::Unavailable)?)
        .execute(&mut *transaction)
        .await
        .map_err(|_| CaError::Unavailable)?;
        regenerate_crl_in_transaction(state, &mut transaction, now).await?;
    } else {
        // Re-sign from the durable revocation ledger on every replay. The ACK
        // is returned only after this transaction contains the matching CRL.
        let now = database_time_unix_ms(&state.pool).await?;
        regenerate_crl_in_transaction(state, &mut transaction, now).await?;
    }
    transaction.commit().await.map_err(|_| CaError::Unavailable)
}

async fn retire_device_certificates(
    state: &mut CaRuntime,
    device_key: DeviceAuthorizationDeviceKey,
) -> CaResult<()> {
    let rows = sqlx::query(
        "SELECT authorization_id, registration_binding_id, authorization_generation, \
                organization_id, workspace_id, device_id, certificate_der, certificate_sha256, \
                serial_number, spki_sha256 \
         FROM cyrene_workspace_device_ca.issued_certificates \
         WHERE organization_id = $1 AND workspace_id = $2 AND device_id = $3 \
         ORDER BY authorization_generation, authorization_id",
    )
    .bind(&device_key.organization_id)
    .bind(&device_key.workspace_id)
    .bind(&device_key.device_id)
    .fetch_all(&state.pool)
    .await
    .map_err(|_| CaError::Unavailable)?;

    // Query every generation because the Registry's latest fingerprint does not cover old active
    // or pending issuance records.
    for row in rows {
        let authorization_id: Vec<u8> = row
            .try_get("authorization_id")
            .map_err(|_| CaError::Unavailable)?;
        let authorization_id: DeviceAuthorizationId = authorization_id
            .try_into()
            .map_err(|_| CaError::Unavailable)?;
        let registration_binding_id: uuid::Uuid = row
            .try_get("registration_binding_id")
            .map_err(|_| CaError::Unavailable)?;
        let authorization_generation: i64 = row
            .try_get("authorization_generation")
            .map_err(|_| CaError::Unavailable)?;
        let organization_id: String = row
            .try_get("organization_id")
            .map_err(|_| CaError::Unavailable)?;
        let workspace_id: String = row
            .try_get("workspace_id")
            .map_err(|_| CaError::Unavailable)?;
        let device_id: String = row.try_get("device_id").map_err(|_| CaError::Unavailable)?;
        let certificate_der: Vec<u8> = row
            .try_get("certificate_der")
            .map_err(|_| CaError::Unavailable)?;
        let certificate_sha256: [u8; 32] = array32(
            row.try_get("certificate_sha256")
                .map_err(|_| CaError::Unavailable)?,
        )?;
        let serial_number: Vec<u8> = row
            .try_get("serial_number")
            .map_err(|_| CaError::Unavailable)?;
        let spki_sha256: [u8; 32] = array32(
            row.try_get("spki_sha256")
                .map_err(|_| CaError::Unavailable)?,
        )?;
        let input = RetireInput {
            authorization_id,
            certificate_sha256,
            certificate_der,
            serial_number,
            registration_binding_id: *registration_binding_id.as_bytes(),
            device_key: DeviceAuthorizationDeviceKey {
                organization_id,
                workspace_id,
                device_id,
            },
            authorization_generation: u64::try_from(authorization_generation)
                .map_err(|_| CaError::Unavailable)?,
            spki_sha256,
        };
        retire_certificate(state, input).await?;
    }
    Ok(())
}

async fn check_certificate_status(
    state: &CaRuntime,
    certificate_der: &[u8],
    serial_number: &[u8],
    certificate_sha256: &[u8; 32],
    checked_at_unix_ms: u64,
) -> CaResult<CaRevocationStatus> {
    let actual_fingerprint: [u8; 32] = Sha256::digest(certificate_der).into();
    let certificate = X509::from_der(certificate_der).map_err(|_| CaError::Rejected)?;
    let actual_serial = canonical_serial(&certificate)?;
    if actual_fingerprint != *certificate_sha256 || actual_serial != serial_number {
        return Err(CaError::Rejected);
    }
    let database_now = database_time_unix_ms(&state.pool).await?;
    if database_now.abs_diff(checked_at_unix_ms) > 30_000 {
        return Err(CaError::Unavailable);
    }
    let current_crl = load_current_crl(state).await?;
    let (this_update_unix_ms, next_update_unix_ms) = verify_crl(state, &current_crl, database_now)?;
    if !crl_matches_revocation_ledger(state, &current_crl).await? {
        return Err(CaError::Unavailable);
    }
    let issued = sqlx::query(
        "SELECT csr_der, certificate_der, certificate_sha256, serial_number, spki_sha256, \
                organization_id, workspace_id, device_id, authorization_generation, \
                csr_sha256, issued_at_unix_ms, not_after_unix_ms, revoked_at_unix_ms \
         FROM cyrene_workspace_device_ca.issued_certificates \
         WHERE certificate_sha256 = $1 AND serial_number = $2",
    )
    .bind(certificate_sha256.as_slice())
    .bind(serial_number)
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| CaError::Unavailable)?
    .ok_or(CaError::Unavailable)?;
    let csr_der: Vec<u8> = issued
        .try_get("csr_der")
        .map_err(|_| CaError::Unavailable)?;
    let issued_der: Vec<u8> = issued
        .try_get("certificate_der")
        .map_err(|_| CaError::Unavailable)?;
    let issued_fingerprint: Vec<u8> = issued
        .try_get("certificate_sha256")
        .map_err(|_| CaError::Unavailable)?;
    let issued_serial: Vec<u8> = issued
        .try_get("serial_number")
        .map_err(|_| CaError::Unavailable)?;
    let spki_sha256: Vec<u8> = issued
        .try_get("spki_sha256")
        .map_err(|_| CaError::Unavailable)?;
    let organization_id: String = issued
        .try_get("organization_id")
        .map_err(|_| CaError::Unavailable)?;
    let workspace_id: String = issued
        .try_get("workspace_id")
        .map_err(|_| CaError::Unavailable)?;
    let device_id: String = issued
        .try_get("device_id")
        .map_err(|_| CaError::Unavailable)?;
    let generation: i64 = issued
        .try_get("authorization_generation")
        .map_err(|_| CaError::Unavailable)?;
    let csr_sha256: Vec<u8> = issued
        .try_get("csr_sha256")
        .map_err(|_| CaError::Unavailable)?;
    let issued_at: i64 = issued
        .try_get("issued_at_unix_ms")
        .map_err(|_| CaError::Unavailable)?;
    let not_after: i64 = issued
        .try_get("not_after_unix_ms")
        .map_err(|_| CaError::Unavailable)?;
    let issued_record_revoked_at: Option<i64> = issued
        .try_get("revoked_at_unix_ms")
        .map_err(|_| CaError::Unavailable)?;
    let actual_spki: [u8; 32] = Sha256::digest(
        certificate
            .public_key()
            .map_err(|_| CaError::Rejected)?
            .public_key_to_der()
            .map_err(|_| CaError::Rejected)?,
    )
    .into();
    let actual_csr_sha256: [u8; 32] = Sha256::digest(&csr_der).into();
    let csr = X509Req::from_der(&csr_der).map_err(|_| CaError::Unavailable)?;
    let csr_public_key = csr.public_key().map_err(|_| CaError::Unavailable)?;
    let expected_not_after = u64::try_from(not_after).map_err(|_| CaError::Unavailable)?;
    let csr_sha256 = array32(csr_sha256)?;
    let expected_identity_uri = device_identity_uri(
        &WorkspaceDeviceKey {
            organization_id,
            workspace_id,
            device_id,
        },
        u64::try_from(generation).map_err(|_| CaError::Unavailable)?,
        &csr_sha256,
    )
    .map_err(|_| CaError::Rejected)?;
    let (_, issued_parsed) =
        x509_parser::parse_x509_certificate(&issued_der).map_err(|_| CaError::Unavailable)?;
    let issued_san = issued_parsed
        .subject_alternative_name()
        .map_err(|_| CaError::Unavailable)?
        .ok_or(CaError::Unavailable)?;
    if issued_der != certificate_der
        || issued_fingerprint.as_slice() != certificate_sha256
        || issued_serial.as_slice() != serial_number
        || array32(spki_sha256)? != actual_spki
        || actual_csr_sha256 != csr_sha256
        || !csr
            .verify(&csr_public_key)
            .map_err(|_| CaError::Unavailable)?
        || <[u8; 32]>::from(Sha256::digest(
            csr_public_key
                .public_key_to_der()
                .map_err(|_| CaError::Unavailable)?,
        )) != actual_spki
        || issued_at <= 0
        || not_after <= issued_at
        || issued_record_revoked_at.is_some()
        || issued_parsed
            .tbs_certificate
            .validity
            .not_after
            .timestamp()
            .checked_mul(1_000)
            != Some(i64::try_from(expected_not_after).map_err(|_| CaError::Unavailable)?)
        || issued_parsed
            .tbs_certificate
            .validity
            .not_before
            .timestamp()
            .checked_mul(1_000)
            != Some((issued_at / 1_000) * 1_000)
        || !issued_san.critical
        || issued_san.value.general_names.len() != 1
        || !matches!(issued_san.value.general_names.first(), Some(x509_parser::extensions::GeneralName::URI(uri)) if *uri == expected_identity_uri)
    {
        if issued_record_revoked_at.is_some() {
            return Ok(CaRevocationStatus::Revoked);
        }
        return Err(CaError::Unavailable);
    }
    let ca_public_key = state
        .certificate
        .public_key()
        .map_err(|_| CaError::Unavailable)?;
    if certificate
        .issuer_name()
        .to_der()
        .map_err(|_| CaError::Unavailable)?
        != state
            .certificate
            .subject_name()
            .to_der()
            .map_err(|_| CaError::Unavailable)?
        || !certificate
            .verify(&ca_public_key)
            .map_err(|_| CaError::Unavailable)?
    {
        return Err(CaError::Unavailable);
    }
    let crl = X509Crl::from_der(&current_crl.der).map_err(|_| CaError::Unavailable)?;
    let serial_bn = BigNum::from_slice(serial_number).map_err(|_| CaError::Rejected)?;
    let serial_asn1 = serial_bn.to_asn1_integer().map_err(|_| CaError::Rejected)?;
    match crl.get_by_serial(&serial_asn1) {
        CrlStatus::NotRevoked => Ok(CaRevocationStatus::Good {
            this_update_unix_ms,
            next_update_unix_ms,
        }),
        CrlStatus::Revoked(_) | CrlStatus::RemoveFromCrl(_) => Ok(CaRevocationStatus::Revoked),
    }
}

async fn ensure_fresh_crl(state: &mut CaRuntime) -> Result<(), RestrictedDeviceCaError> {
    let now = database_time_unix_ms(&state.pool)
        .await
        .map_err(|_| RestrictedDeviceCaError::Unavailable)?;
    let existing = load_current_crl(state).await;
    if let Ok(row) = existing {
        if verify_crl(state, &row, now).is_ok()
            && crl_matches_revocation_ledger(state, &row)
                .await
                .unwrap_or(false)
        {
            return Ok(());
        }
    }
    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|_| RestrictedDeviceCaError::Unavailable)?;
    regenerate_crl_in_transaction(state, &mut transaction, now)
        .await
        .map_err(|_| RestrictedDeviceCaError::Unavailable)?;
    transaction
        .commit()
        .await
        .map_err(|_| RestrictedDeviceCaError::Unavailable)
}

async fn check_crl_health(state: &CaRuntime) -> CaResult<()> {
    let now = database_time_unix_ms(&state.pool).await?;
    let current = load_current_crl(state).await?;
    verify_crl(state, &current, now)?;
    if !crl_matches_revocation_ledger(state, &current).await? {
        return Err(CaError::Unavailable);
    }
    Ok(())
}

async fn regenerate_crl_in_transaction(
    state: &CaRuntime,
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    now_unix_ms: u64,
) -> CaResult<()> {
    let previous_number = sqlx::query_scalar::<_, Option<i64>>(
        "SELECT crl_number FROM cyrene_workspace_device_ca.current_crl \
         WHERE singleton = TRUE FOR UPDATE",
    )
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|_| CaError::Unavailable)?
    .flatten()
    .unwrap_or(0);
    let next_number = previous_number.checked_add(1).ok_or(CaError::Unavailable)?;
    let revoked_rows = sqlx::query(
        "SELECT serial_number, revoked_at_unix_ms FROM cyrene_workspace_device_ca.issued_certificates \
         WHERE revoked_at_unix_ms IS NOT NULL ORDER BY serial_number",
    )
    .fetch_all(&mut **transaction)
    .await
    .map_err(|_| CaError::Unavailable)?;
    let mut revoked = Vec::with_capacity(revoked_rows.len());
    for row in revoked_rows {
        let serial: Vec<u8> = row
            .try_get("serial_number")
            .map_err(|_| CaError::Unavailable)?;
        let revoked_at: i64 = row
            .try_get("revoked_at_unix_ms")
            .map_err(|_| CaError::Unavailable)?;
        revoked.push((
            serial,
            u64::try_from(revoked_at).map_err(|_| CaError::Unavailable)?,
        ));
    }
    let der = build_signed_crl(state, next_number, now_unix_ms, &revoked)?;
    let this_update = now_unix_ms;
    let next_update = now_unix_ms
        .checked_add(CRL_LIFETIME_SECONDS * 1_000)
        .ok_or(CaError::Unavailable)?;
    sqlx::query(
        "INSERT INTO cyrene_workspace_device_ca.current_crl \
         (singleton, crl_number, this_update_unix_ms, next_update_unix_ms, issuer_certificate_sha256, crl_der) \
         VALUES (TRUE, $1, $2, $3, $4, $5) \
         ON CONFLICT (singleton) DO UPDATE SET \
           crl_number = EXCLUDED.crl_number, this_update_unix_ms = EXCLUDED.this_update_unix_ms, \
           next_update_unix_ms = EXCLUDED.next_update_unix_ms, \
           issuer_certificate_sha256 = EXCLUDED.issuer_certificate_sha256, crl_der = EXCLUDED.crl_der",
    )
    .bind(next_number)
    .bind(i64::try_from(this_update).map_err(|_| CaError::Unavailable)?)
    .bind(i64::try_from(next_update).map_err(|_| CaError::Unavailable)?)
    .bind(Sha256::digest(state.certificate.to_der().map_err(|_| CaError::Unavailable)?).as_slice())
    .bind(der)
    .execute(&mut **transaction)
    .await
    .map_err(|_| CaError::Unavailable)?;
    Ok(())
}

fn build_signed_crl(
    state: &CaRuntime,
    crl_number: i64,
    now_unix_ms: u64,
    revoked: &[(Vec<u8>, u64)],
) -> CaResult<Vec<u8>> {
    if revoked.len() > 100_000 {
        return Err(CaError::Unavailable);
    }
    let now_seconds = i64::try_from(now_unix_ms / 1_000).map_err(|_| CaError::Unavailable)?;
    let next_seconds = now_seconds
        .checked_add(i64::try_from(CRL_LIFETIME_SECONDS).map_err(|_| CaError::Unavailable)?)
        .ok_or(CaError::Unavailable)?;
    let this_update = Asn1Time::from_unix(now_seconds).map_err(|_| CaError::Unavailable)?;
    let next_update = Asn1Time::from_unix(next_seconds).map_err(|_| CaError::Unavailable)?;
    let mut builder = X509CrlBuilder::new().map_err(|_| CaError::Unavailable)?;
    builder
        .set_issuer_name(state.certificate.subject_name())
        .map_err(|_| CaError::Unavailable)?;
    builder
        .set_last_update(&this_update)
        .map_err(|_| CaError::Unavailable)?;
    builder
        .set_next_update(&next_update)
        .map_err(|_| CaError::Unavailable)?;
    for (serial, revoked_at) in revoked {
        let revoked_at = Asn1Time::from_unix(
            i64::try_from(revoked_at / 1_000).map_err(|_| CaError::Unavailable)?,
        )
        .map_err(|_| CaError::Unavailable)?;
        let serial_bn = BigNum::from_slice(serial).map_err(|_| CaError::Unavailable)?;
        let serial_asn1 = serial_bn
            .to_asn1_integer()
            .map_err(|_| CaError::Unavailable)?;
        let mut entry = X509RevokedBuilder::new().map_err(|_| CaError::Unavailable)?;
        entry
            .set_serial_number(&serial_asn1)
            .map_err(|_| CaError::Unavailable)?;
        entry
            .set_revocation_date(&revoked_at)
            .map_err(|_| CaError::Unavailable)?;
        builder
            .add_revoked(entry.build())
            .map_err(|_| CaError::Unavailable)?;
    }
    builder.sort().map_err(|_| CaError::Unavailable)?;
    let context_builder = X509::builder().map_err(|_| CaError::Unavailable)?;
    let context = context_builder.x509v3_context(Some(&state.certificate), None);
    let authority_key_identifier = AuthorityKeyIdentifier::new()
        .keyid(true)
        .build(&context)
        .map_err(|_| CaError::Unavailable)?;
    let crl_number = CrlNumber::new(
        BigNum::from_dec_str(&crl_number.to_string()).map_err(|_| CaError::Unavailable)?,
    )
    .map_err(|_| CaError::Unavailable)?
    .build()
    .map_err(|_| CaError::Unavailable)?;
    builder
        .append_extension(authority_key_identifier)
        .map_err(|_| CaError::Unavailable)?;
    builder
        .append_extension(crl_number)
        .map_err(|_| CaError::Unavailable)?;
    builder
        .sign(&state.signer, ca_signature_digest(&state.signer))
        .map_err(|_| CaError::Unavailable)?;
    let crl = builder.build().map_err(|_| CaError::Unavailable)?;
    crl.to_der().map_err(|_| CaError::Unavailable)
}

async fn load_current_crl(state: &CaRuntime) -> CaResult<CurrentCrl> {
    let row = sqlx::query(
        "SELECT crl_number, this_update_unix_ms, next_update_unix_ms, issuer_certificate_sha256, crl_der \
         FROM cyrene_workspace_device_ca.current_crl WHERE singleton = TRUE",
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| CaError::Unavailable)?
    .ok_or(CaError::Unavailable)?;
    current_crl_from_row(row)
}

struct CurrentCrl {
    number: i64,
    this_update_unix_ms: u64,
    next_update_unix_ms: u64,
    issuer_certificate_sha256: Vec<u8>,
    der: Vec<u8>,
}

fn current_crl_from_row(row: sqlx::postgres::PgRow) -> CaResult<CurrentCrl> {
    let number: i64 = row
        .try_get("crl_number")
        .map_err(|_| CaError::Unavailable)?;
    let this_update: i64 = row
        .try_get("this_update_unix_ms")
        .map_err(|_| CaError::Unavailable)?;
    let next_update: i64 = row
        .try_get("next_update_unix_ms")
        .map_err(|_| CaError::Unavailable)?;
    Ok(CurrentCrl {
        number,
        this_update_unix_ms: u64::try_from(this_update).map_err(|_| CaError::Unavailable)?,
        next_update_unix_ms: u64::try_from(next_update).map_err(|_| CaError::Unavailable)?,
        issuer_certificate_sha256: row
            .try_get("issuer_certificate_sha256")
            .map_err(|_| CaError::Unavailable)?,
        der: row.try_get("crl_der").map_err(|_| CaError::Unavailable)?,
    })
}

fn verify_crl(state: &CaRuntime, current: &CurrentCrl, now_unix_ms: u64) -> CaResult<(u64, u64)> {
    verify_crl_for_certificate(&state.certificate, current, now_unix_ms)
}

fn verify_crl_for_certificate(
    certificate: &X509,
    current: &CurrentCrl,
    now_unix_ms: u64,
) -> CaResult<(u64, u64)> {
    let root_der = certificate.to_der().map_err(|_| CaError::Unavailable)?;
    let root_sha256: [u8; 32] = Sha256::digest(&root_der).into();
    if current.number <= 0
        || current.issuer_certificate_sha256.as_slice() != root_sha256
        || current.next_update_unix_ms <= current.this_update_unix_ms
        || current.this_update_unix_ms > now_unix_ms.saturating_add(30_000)
        || now_unix_ms.saturating_sub(current.this_update_unix_ms) > CRL_LIFETIME_SECONDS * 1_000
        || now_unix_ms >= current.next_update_unix_ms
    {
        return Err(CaError::Unavailable);
    }
    let crl = X509Crl::from_der(&current.der).map_err(|_| CaError::Unavailable)?;
    let certificate_public_key = certificate.public_key().map_err(|_| CaError::Unavailable)?;
    if crl
        .issuer_name()
        .to_der()
        .map_err(|_| CaError::Unavailable)?
        != certificate
            .subject_name()
            .to_der()
            .map_err(|_| CaError::Unavailable)?
        || !crl
            .verify(&certificate_public_key)
            .map_err(|_| CaError::Unavailable)?
    {
        return Err(CaError::Unavailable);
    }
    let (remaining, parsed_crl) =
        x509_parser::parse_x509_crl(&current.der).map_err(|_| CaError::Unavailable)?;
    if !remaining.is_empty()
        || parsed_crl.issuer().as_raw()
            != certificate
                .subject_name()
                .to_der()
                .map_err(|_| CaError::Unavailable)?
        || parsed_crl
            .crl_number()
            .is_none_or(|number| number.to_str_radix(10) != current.number.to_string())
    {
        return Err(CaError::Unavailable);
    }
    let signed_this_update = u64::try_from(parsed_crl.last_update().timestamp())
        .ok()
        .and_then(|seconds| seconds.checked_mul(1_000))
        .ok_or(CaError::Unavailable)?;
    let signed_next_update = parsed_crl
        .next_update()
        .and_then(|time| u64::try_from(time.timestamp()).ok())
        .and_then(|seconds| seconds.checked_mul(1_000))
        .ok_or(CaError::Unavailable)?;
    if signed_this_update != (current.this_update_unix_ms / 1_000) * 1_000
        || signed_next_update != (current.next_update_unix_ms / 1_000) * 1_000
        || now_unix_ms < signed_this_update
        || now_unix_ms >= signed_next_update
    {
        return Err(CaError::Unavailable);
    }
    Ok((current.this_update_unix_ms, current.next_update_unix_ms))
}

async fn check_relay_crl_health(state: &RelayCrlRuntime) -> CaResult<()> {
    let now = database_time_unix_ms(&state.pool).await?;
    let current = load_current_crl_from_pool(&state.pool).await?;
    verify_crl_for_certificate(&state.certificate, &current, now)?;
    if !relay_crl_matches_revocation_ledger(&state.pool, &current).await? {
        return Err(CaError::Unavailable);
    }
    Ok(())
}

async fn check_relay_certificate_status(
    state: &RelayCrlRuntime,
    certificate_der: &[u8],
    serial_number: &[u8],
    certificate_sha256: &[u8; 32],
    checked_at_unix_ms: u64,
) -> Result<RelayCrlStatus, RelayPeerRevocationCheckError> {
    let unavailable = || RelayPeerRevocationCheckError::Unknown;
    let actual_fingerprint: [u8; 32] = Sha256::digest(certificate_der).into();
    let certificate = X509::from_der(certificate_der).map_err(|_| unavailable())?;
    let actual_serial = canonical_serial(&certificate).map_err(|_| unavailable())?;
    if actual_fingerprint != *certificate_sha256 || actual_serial != serial_number {
        return Err(unavailable());
    }
    let database_now = database_time_unix_ms(&state.pool)
        .await
        .map_err(|_| unavailable())?;
    if database_now.abs_diff(checked_at_unix_ms) > 30_000 {
        return Err(unavailable());
    }
    let current_crl = load_current_crl_from_pool(&state.pool)
        .await
        .map_err(|_| unavailable())?;
    let (this_update_unix_ms, next_update_unix_ms) =
        verify_crl_for_certificate(&state.certificate, &current_crl, database_now)
            .map_err(|_| unavailable())?;
    if !relay_crl_matches_revocation_ledger(&state.pool, &current_crl)
        .await
        .map_err(|_| unavailable())?
    {
        return Err(unavailable());
    }
    let issued = sqlx::query(
        "SELECT certificate_der, certificate_sha256, serial_number, revoked_at_unix_ms \
         FROM cyrene_workspace_device_ca.issued_certificates \
         WHERE certificate_sha256 = $1 AND serial_number = $2",
    )
    .bind(certificate_sha256.as_slice())
    .bind(serial_number)
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| unavailable())?
    .ok_or_else(unavailable)?;
    let issued_der: Vec<u8> = issued
        .try_get("certificate_der")
        .map_err(|_| unavailable())?;
    let issued_fingerprint: Vec<u8> = issued
        .try_get("certificate_sha256")
        .map_err(|_| unavailable())?;
    let issued_serial: Vec<u8> = issued.try_get("serial_number").map_err(|_| unavailable())?;
    let revoked_at: Option<i64> = issued
        .try_get("revoked_at_unix_ms")
        .map_err(|_| unavailable())?;
    if issued_der != certificate_der
        || issued_fingerprint.as_slice() != certificate_sha256
        || issued_serial.as_slice() != serial_number
    {
        return Err(unavailable());
    }
    let crl = X509Crl::from_der(&current_crl.der).map_err(|_| unavailable())?;
    let serial = BigNum::from_slice(serial_number).map_err(|_| unavailable())?;
    let serial = serial.to_asn1_integer().map_err(|_| unavailable())?;
    match crl.get_by_serial(&serial) {
        CrlStatus::Revoked(_) | CrlStatus::RemoveFromCrl(_) if revoked_at.is_some() => {
            Ok(RelayCrlStatus::Revoked)
        }
        CrlStatus::Revoked(_) | CrlStatus::RemoveFromCrl(_) => Err(unavailable()),
        CrlStatus::NotRevoked if revoked_at.is_some() => Err(unavailable()),
        CrlStatus::NotRevoked => Ok(RelayCrlStatus::Good {
            this_update_unix_ms,
            next_update_unix_ms,
        }),
    }
}

async fn load_current_crl_from_pool(pool: &PgPool) -> CaResult<CurrentCrl> {
    let row = sqlx::query(
        "SELECT crl_number, this_update_unix_ms, next_update_unix_ms, issuer_certificate_sha256, crl_der \
         FROM cyrene_workspace_device_ca.current_crl WHERE singleton = TRUE",
    )
    .fetch_optional(pool)
    .await
    .map_err(|_| CaError::Unavailable)?
    .ok_or(CaError::Unavailable)?;
    current_crl_from_row(row)
}

async fn relay_crl_matches_revocation_ledger(
    pool: &PgPool,
    current: &CurrentCrl,
) -> CaResult<bool> {
    let rows = sqlx::query(
        "SELECT serial_number, revoked_at_unix_ms \
         FROM cyrene_workspace_device_ca.issued_certificates \
         WHERE revoked_at_unix_ms IS NOT NULL ORDER BY serial_number",
    )
    .fetch_all(pool)
    .await
    .map_err(|_| CaError::Unavailable)?;
    let mut expected = BTreeMap::new();
    for row in rows {
        let serial: Vec<u8> = row
            .try_get("serial_number")
            .map_err(|_| CaError::Unavailable)?;
        let revoked_at: i64 = row
            .try_get("revoked_at_unix_ms")
            .map_err(|_| CaError::Unavailable)?;
        if serial.is_empty()
            || expected
                .insert(
                    serial,
                    u64::try_from(revoked_at).map_err(|_| CaError::Unavailable)? / 1_000,
                )
                .is_some()
        {
            return Err(CaError::Unavailable);
        }
    }
    let (remaining, parsed_crl) =
        x509_parser::parse_x509_crl(&current.der).map_err(|_| CaError::Unavailable)?;
    if !remaining.is_empty() {
        return Err(CaError::Unavailable);
    }
    let mut actual = BTreeMap::new();
    for revoked in parsed_crl.iter_revoked_certificates() {
        let serial = canonical_serial_from_der(revoked.raw_serial()).ok_or(CaError::Unavailable)?;
        let revoked_at =
            u64::try_from(revoked.revocation_date.timestamp()).map_err(|_| CaError::Unavailable)?;
        if actual.insert(serial, revoked_at).is_some() {
            return Err(CaError::Unavailable);
        }
    }
    Ok(expected == actual)
}

async fn crl_matches_revocation_ledger(state: &CaRuntime, current: &CurrentCrl) -> CaResult<bool> {
    let rows = sqlx::query(
        "SELECT serial_number, revoked_at_unix_ms \
         FROM cyrene_workspace_device_ca.issued_certificates \
         WHERE revoked_at_unix_ms IS NOT NULL ORDER BY serial_number",
    )
    .fetch_all(&state.pool)
    .await
    .map_err(|_| CaError::Unavailable)?;
    let mut expected = BTreeMap::new();
    for row in rows {
        let serial: Vec<u8> = row
            .try_get("serial_number")
            .map_err(|_| CaError::Unavailable)?;
        let revoked_at: i64 = row
            .try_get("revoked_at_unix_ms")
            .map_err(|_| CaError::Unavailable)?;
        if serial.is_empty()
            || expected
                .insert(
                    serial,
                    u64::try_from(revoked_at).map_err(|_| CaError::Unavailable)? / 1_000,
                )
                .is_some()
        {
            return Err(CaError::Unavailable);
        }
    }
    let (remaining, parsed_crl) =
        x509_parser::parse_x509_crl(&current.der).map_err(|_| CaError::Unavailable)?;
    if !remaining.is_empty() {
        return Err(CaError::Unavailable);
    }
    let mut actual = BTreeMap::new();
    for revoked in parsed_crl.iter_revoked_certificates() {
        let serial = canonical_serial_from_der(revoked.raw_serial()).ok_or(CaError::Unavailable)?;
        let revoked_at =
            u64::try_from(revoked.revocation_date.timestamp()).map_err(|_| CaError::Unavailable)?;
        if actual.insert(serial, revoked_at).is_some() {
            return Err(CaError::Unavailable);
        }
    }
    Ok(expected == actual)
}

fn canonical_serial_from_der(raw_serial: &[u8]) -> Option<Vec<u8>> {
    let serial = if raw_serial.first() == Some(&0) {
        if raw_serial.get(1).is_none_or(|byte| byte & 0x80 == 0) {
            return None;
        }
        &raw_serial[1..]
    } else {
        raw_serial
    };
    if serial.is_empty()
        || serial.len() > MAX_SERIAL_BYTES
        || serial[0] & 0x80 != 0
        || serial.iter().all(|byte| *byte == 0)
    {
        return None;
    }
    Some(serial.to_vec())
}

async fn lock_authorization_id(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    authorization_id: &DeviceAuthorizationId,
) -> CaResult<()> {
    let key = lowercase_hex(authorization_id);
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(key)
        .execute(&mut **transaction)
        .await
        .map_err(|_| CaError::Unavailable)?;
    Ok(())
}

async fn load_issued_record(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    authorization_id: &DeviceAuthorizationId,
) -> CaResult<Option<IssuedRecord>> {
    let row = sqlx::query(
        "SELECT request_sha256, registration_binding_id, organization_id, workspace_id, device_id, \
                authorization_generation, csr_der, csr_sha256, spki_sha256, issued_at_unix_ms, certificate_der, \
                serial_number, not_after_unix_ms, revoked_at_unix_ms \
         FROM cyrene_workspace_device_ca.issued_certificates WHERE authorization_id = $1",
    )
    .bind(authorization_id.as_slice())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|_| CaError::Unavailable)?;
    row.map(|row| {
        let binding_id: uuid::Uuid = row
            .try_get("registration_binding_id")
            .map_err(|_| CaError::Unavailable)?;
        Ok(IssuedRecord {
            request_sha256: array32(
                row.try_get("request_sha256")
                    .map_err(|_| CaError::Unavailable)?,
            )?,
            registration_binding_id: *binding_id.as_bytes(),
            organization_id: row
                .try_get("organization_id")
                .map_err(|_| CaError::Unavailable)?,
            workspace_id: row
                .try_get("workspace_id")
                .map_err(|_| CaError::Unavailable)?,
            device_id: row.try_get("device_id").map_err(|_| CaError::Unavailable)?,
            authorization_generation: u64::try_from(
                row.try_get::<i64, _>("authorization_generation")
                    .map_err(|_| CaError::Unavailable)?,
            )
            .map_err(|_| CaError::Unavailable)?,
            csr_der: row.try_get("csr_der").map_err(|_| CaError::Unavailable)?,
            csr_sha256: array32(
                row.try_get("csr_sha256")
                    .map_err(|_| CaError::Unavailable)?,
            )?,
            spki_sha256: array32(
                row.try_get("spki_sha256")
                    .map_err(|_| CaError::Unavailable)?,
            )?,
            issued_at_unix_ms: u64::try_from(
                row.try_get::<i64, _>("issued_at_unix_ms")
                    .map_err(|_| CaError::Unavailable)?,
            )
            .map_err(|_| CaError::Unavailable)?,
            certificate_der: row
                .try_get("certificate_der")
                .map_err(|_| CaError::Unavailable)?,
            serial_number: row
                .try_get("serial_number")
                .map_err(|_| CaError::Unavailable)?,
            not_after_unix_ms: u64::try_from(
                row.try_get::<i64, _>("not_after_unix_ms")
                    .map_err(|_| CaError::Unavailable)?,
            )
            .map_err(|_| CaError::Unavailable)?,
            revoked_at_unix_ms: row
                .try_get("revoked_at_unix_ms")
                .map_err(|_| CaError::Unavailable)?,
        })
    })
    .transpose()
}

struct IssuedRecord {
    request_sha256: [u8; 32],
    registration_binding_id: [u8; 16],
    organization_id: String,
    workspace_id: String,
    device_id: String,
    authorization_generation: u64,
    csr_der: Vec<u8>,
    csr_sha256: [u8; 32],
    spki_sha256: [u8; 32],
    issued_at_unix_ms: u64,
    certificate_der: Vec<u8>,
    serial_number: Vec<u8>,
    not_after_unix_ms: u64,
    revoked_at_unix_ms: Option<i64>,
}

fn issued_from_record(
    state: &CaRuntime,
    record: IssuedRecord,
    input: &IssueInput,
) -> CaResult<IssuedDeviceCertificate> {
    if record.registration_binding_id != input.registration_binding_id
        || record.organization_id != input.device_key.organization_id
        || record.workspace_id != input.device_key.workspace_id
        || record.device_id != input.device_key.device_id
        || record.authorization_generation != input.authorization_generation
        || record.csr_der != input.csr_der
        || record.csr_sha256 != input.csr_sha256
        || record.spki_sha256 != input.spki_sha256
        || record.issued_at_unix_ms != input.issued_at_unix_ms
        || record.request_sha256 != input.request_sha256
        || record.certificate_der.is_empty()
        || record.certificate_der.len() > MAX_CERTIFICATE_DER_BYTES
    {
        return Err(CaError::Conflict);
    }
    if record.revoked_at_unix_ms.is_some() {
        return Err(CaError::Rejected);
    }
    let expected = sign_device_leaf(state, input)?
        .to_der()
        .map_err(|_| CaError::Unavailable)?;
    let expected_serial = deterministic_serial(&input.request_sha256);
    let expected_not_after = input
        .issued_at_unix_ms
        .checked_div(1_000)
        .and_then(|issued_at| issued_at.checked_add(CERTIFICATE_LIFETIME_SECONDS))
        .and_then(|not_after| not_after.checked_mul(1_000))
        .ok_or(CaError::Conflict)?;
    if record.certificate_der != expected
        || record.serial_number != expected_serial
        || record.not_after_unix_ms != expected_not_after
        || <[u8; 32]>::from(Sha256::digest(&record.certificate_der))
            != <[u8; 32]>::from(Sha256::digest(&expected))
    {
        return Err(CaError::Conflict);
    }
    Ok(IssuedDeviceCertificate {
        certificate_der: record.certificate_der,
        ca_chain_der: Vec::new(),
        serial_number: record.serial_number,
        registration_binding_id: record.registration_binding_id,
        device_key: input.device_key.clone(),
        authorization_generation: record.authorization_generation,
        scope: input.scope.clone(),
        spki_sha256: record.spki_sha256,
        not_after_unix_ms: record.not_after_unix_ms,
    })
}

fn sign_device_leaf(state: &CaRuntime, input: &IssueInput) -> CaResult<X509> {
    let request = X509Req::from_der(&input.csr_der).map_err(|_| CaError::Rejected)?;
    let public_key = request.public_key().map_err(|_| CaError::Rejected)?;
    if !request.verify(&public_key).map_err(|_| CaError::Rejected)?
        || <[u8; 32]>::from(Sha256::digest(
            public_key
                .public_key_to_der()
                .map_err(|_| CaError::Rejected)?,
        )) != input.spki_sha256
    {
        return Err(CaError::Rejected);
    }
    let not_before_seconds =
        i64::try_from(input.issued_at_unix_ms / 1_000).map_err(|_| CaError::Rejected)?;
    let not_after_seconds = not_before_seconds
        .checked_add(i64::try_from(CERTIFICATE_LIFETIME_SECONDS).map_err(|_| CaError::Rejected)?)
        .ok_or(CaError::Rejected)?;
    let not_before = Asn1Time::from_unix(not_before_seconds).map_err(|_| CaError::Rejected)?;
    let not_after = Asn1Time::from_unix(not_after_seconds).map_err(|_| CaError::Rejected)?;
    let serial_bytes = deterministic_serial(&input.request_sha256);
    let serial_bn = BigNum::from_slice(&serial_bytes).map_err(|_| CaError::Rejected)?;
    let serial = serial_bn.to_asn1_integer().map_err(|_| CaError::Rejected)?;
    let identity_uri = device_identity_uri(
        &WorkspaceDeviceKey {
            organization_id: input.device_key.organization_id.clone(),
            workspace_id: input.device_key.workspace_id.clone(),
            device_id: input.device_key.device_id.clone(),
        },
        input.authorization_generation,
        &input.csr_sha256,
    )
    .map_err(|_| CaError::Rejected)?;

    let mut builder = X509::builder().map_err(|_| CaError::Unavailable)?;
    builder.set_version(2).map_err(|_| CaError::Unavailable)?;
    builder
        .set_serial_number(&serial)
        .map_err(|_| CaError::Unavailable)?;
    builder
        .set_issuer_name(state.certificate.subject_name())
        .map_err(|_| CaError::Unavailable)?;
    let empty_subject = X509Name::builder()
        .map_err(|_| CaError::Unavailable)?
        .build();
    builder
        .set_subject_name(&empty_subject)
        .map_err(|_| CaError::Unavailable)?;
    builder
        .set_pubkey(&public_key)
        .map_err(|_| CaError::Unavailable)?;
    builder
        .set_not_before(&not_before)
        .map_err(|_| CaError::Unavailable)?;
    builder
        .set_not_after(&not_after)
        .map_err(|_| CaError::Unavailable)?;
    builder
        .append_extension(
            BasicConstraints::new()
                .critical()
                .build()
                .map_err(|_| CaError::Unavailable)?,
        )
        .map_err(|_| CaError::Unavailable)?;
    builder
        .append_extension(
            KeyUsage::new()
                .critical()
                .digital_signature()
                .build()
                .map_err(|_| CaError::Unavailable)?,
        )
        .map_err(|_| CaError::Unavailable)?;
    builder
        .append_extension(
            ExtendedKeyUsage::new()
                .client_auth()
                .build()
                .map_err(|_| CaError::Unavailable)?,
        )
        .map_err(|_| CaError::Unavailable)?;
    let context = builder.x509v3_context(Some(&state.certificate), None);
    let subject_alt_name = SubjectAlternativeName::new()
        .uri(&identity_uri)
        .critical()
        .build(&context)
        .map_err(|_| CaError::Unavailable)?;
    builder
        .append_extension(subject_alt_name)
        .map_err(|_| CaError::Unavailable)?;
    builder
        .sign(&state.signer, ca_signature_digest(&state.signer))
        .map_err(|_| CaError::Unavailable)?;
    Ok(builder.build())
}

async fn database_time_unix_ms(pool: &PgPool) -> CaResult<u64> {
    sqlx::query_scalar::<_, i64>(
        "SELECT floor(extract(epoch FROM clock_timestamp()) * 1000)::BIGINT",
    )
    .fetch_one(pool)
    .await
    .map_err(|_| CaError::Unavailable)
    .and_then(|value| u64::try_from(value).map_err(|_| CaError::Unavailable))
}

fn validate_ca_material(
    certificate: &X509,
    signer: &PKey<Private>,
) -> Result<(), RestrictedDeviceCaError> {
    let public_key = certificate
        .public_key()
        .map_err(|_| RestrictedDeviceCaError::Configuration)?;
    if !certificate
        .verify(&public_key)
        .map_err(|_| RestrictedDeviceCaError::Configuration)?
        || !public_key.public_eq(signer)
        || signer.id() != Id::RSA
        || signer.bits() < 3_072
        || certificate
            .subject_name()
            .to_der()
            .map_err(|_| RestrictedDeviceCaError::Configuration)?
            != certificate
                .issuer_name()
                .to_der()
                .map_err(|_| RestrictedDeviceCaError::Configuration)?
    {
        return Err(RestrictedDeviceCaError::Configuration);
    }
    let now = Asn1Time::days_from_now(0).map_err(|_| RestrictedDeviceCaError::Configuration)?;
    if certificate
        .not_before()
        .compare(&now)
        .map_err(|_| RestrictedDeviceCaError::Configuration)?
        == std::cmp::Ordering::Greater
        || certificate
            .not_after()
            .compare(&now)
            .map_err(|_| RestrictedDeviceCaError::Configuration)?
            != std::cmp::Ordering::Greater
    {
        return Err(RestrictedDeviceCaError::Configuration);
    }
    let der = certificate
        .to_der()
        .map_err(|_| RestrictedDeviceCaError::Configuration)?;
    let (_, parsed) = x509_parser::parse_x509_certificate(&der)
        .map_err(|_| RestrictedDeviceCaError::Configuration)?;
    let basic = parsed
        .basic_constraints()
        .map_err(|_| RestrictedDeviceCaError::Configuration)?
        .ok_or(RestrictedDeviceCaError::Configuration)?;
    let key_usage = parsed
        .key_usage()
        .map_err(|_| RestrictedDeviceCaError::Configuration)?
        .ok_or(RestrictedDeviceCaError::Configuration)?;
    if !basic.critical
        || !basic.value.ca
        || basic.value.path_len_constraint != Some(0)
        || !key_usage.critical
        || !key_usage.value.key_cert_sign()
        || !key_usage.value.crl_sign()
    {
        return Err(RestrictedDeviceCaError::Configuration);
    }
    Ok(())
}

fn query_binds_to_configured_ca(
    certificate_der: &[u8],
    serial_number: &[u8],
    certificate_sha256: &[u8; 32],
    ca_chain_der: &[Vec<u8>],
    trusted_roots_der: &[Vec<u8>],
    ca: &PostgresRestrictedDeviceCa,
) -> bool {
    query_bindings_match_ca(
        certificate_der,
        serial_number,
        certificate_sha256,
        ca_chain_der,
        trusted_roots_der,
        ca,
    )
}

fn query_bindings_match_ca(
    certificate_der: &[u8],
    serial_number: &[u8],
    certificate_sha256: &[u8; 32],
    ca_chain_der: &[Vec<u8>],
    trusted_roots_der: &[Vec<u8>],
    ca: &PostgresRestrictedDeviceCa,
) -> bool {
    query_bindings_match_trusted_ca(
        certificate_der,
        serial_number,
        certificate_sha256,
        ca_chain_der,
        trusted_roots_der,
        ca.trusted_root_der(),
    )
}

fn query_bindings_match_trusted_ca(
    certificate_der: &[u8],
    serial_number: &[u8],
    certificate_sha256: &[u8; 32],
    ca_chain_der: &[Vec<u8>],
    trusted_roots_der: &[Vec<u8>],
    configured_ca_der: &[u8],
) -> bool {
    let Ok(configured_ca) = X509::from_der(configured_ca_der) else {
        return false;
    };
    let Ok(configured_ca_public_key) = configured_ca.public_key() else {
        return false;
    };
    let Ok(certificate) = X509::from_der(certificate_der) else {
        return false;
    };
    let Ok(actual_serial) = canonical_serial(&certificate) else {
        return false;
    };
    let leaf_fingerprint: [u8; 32] = Sha256::digest(certificate_der).into();
    let Ok(_root_store) = roots_contain_exact_ca(trusted_roots_der, configured_ca_der) else {
        return false;
    };
    actual_serial == serial_number
        && leaf_fingerprint == *certificate_sha256
        && ca_chain_der.is_empty()
        && certificate.issuer_name().to_der().ok() == configured_ca.subject_name().to_der().ok()
        && certificate
            .verify(&configured_ca_public_key)
            .unwrap_or(false)
}

fn roots_contain_exact_ca(roots: &[Vec<u8>], ca_der: &[u8]) -> Result<RootCertStore, ()> {
    if roots.len() != 1 || roots.first().is_none_or(|root| root.as_slice() != ca_der) {
        return Err(());
    }
    let mut store = RootCertStore::empty();
    store
        .add(CertificateDer::from(ca_der.to_vec()))
        .map_err(|_| ())?;
    Ok(store)
}

fn deterministic_serial(request_sha256: &[u8; 32]) -> Vec<u8> {
    let mut serial = request_sha256[..MAX_SERIAL_BYTES].to_vec();
    serial[0] &= 0x7f;
    if serial.iter().all(|byte| *byte == 0) {
        serial[MAX_SERIAL_BYTES - 1] = 1;
    }
    serial
}

fn canonical_serial(certificate: &X509) -> CaResult<Vec<u8>> {
    let serial = certificate
        .serial_number()
        .to_bn()
        .map_err(|_| CaError::Unavailable)?
        .to_vec();
    if serial.is_empty() || serial.len() > MAX_SERIAL_BYTES || serial.iter().all(|byte| *byte == 0)
    {
        return Err(CaError::Rejected);
    }
    Ok(serial)
}

fn ca_signature_digest(key: &PKey<Private>) -> MessageDigest {
    // Signer startup rejects non-RSA keys so each exact request produces a
    // byte-identical PKCS#1 v1.5 certificate across process restarts.
    let _ = key;
    MessageDigest::sha256()
}

fn hash_field(hasher: &mut Sha256, field: &[u8]) {
    hasher.update(u64::try_from(field.len()).unwrap_or(u64::MAX).to_be_bytes());
    hasher.update(field);
}

fn array32(value: Vec<u8>) -> CaResult<[u8; 32]> {
    value.try_into().map_err(|_| CaError::Unavailable)
}

fn lowercase_hex(value: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(value.len() * 2);
    for byte in value {
        output.push(HEX[usize::from(byte >> 4)] as char);
        output.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    output
}

fn x509_timestamp_millis(certificate_der: &[u8], not_after: bool) -> Option<u64> {
    let (remaining, parsed) = x509_parser::parse_x509_certificate(certificate_der).ok()?;
    if !remaining.is_empty() {
        return None;
    }
    let timestamp = if not_after {
        parsed.tbs_certificate.validity.not_after.timestamp()
    } else {
        parsed.tbs_certificate.validity.not_before.timestamp()
    };
    u64::try_from(timestamp).ok()?.checked_mul(1_000)
}

fn required_path(name: &str) -> Result<PathBuf, RestrictedDeviceCaError> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or(RestrictedDeviceCaError::Configuration)
}

fn read_regular_file(path: &Path, maximum_bytes: u64) -> Result<Vec<u8>, RestrictedDeviceCaError> {
    let file = File::open(path).map_err(|_| RestrictedDeviceCaError::Configuration)?;
    let metadata = file
        .metadata()
        .map_err(|_| RestrictedDeviceCaError::Configuration)?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > maximum_bytes {
        return Err(RestrictedDeviceCaError::Configuration);
    }
    let mut contents = Vec::with_capacity(
        usize::try_from(metadata.len()).map_err(|_| RestrictedDeviceCaError::Configuration)?,
    );
    file.take(maximum_bytes + 1)
        .read_to_end(&mut contents)
        .map_err(|_| RestrictedDeviceCaError::Configuration)?;
    if contents.is_empty() || contents.len() as u64 > maximum_bytes {
        return Err(RestrictedDeviceCaError::Configuration);
    }
    Ok(contents)
}

fn read_restricted_key(path: &Path) -> Result<Zeroizing<Vec<u8>>, RestrictedDeviceCaError> {
    if !path.is_absolute()
        || path.components().any(|component| {
            component.as_os_str() == "versions"
                || component.as_os_str() == "releases"
                || component.as_os_str() == "current"
        })
    {
        return Err(RestrictedDeviceCaError::Configuration);
    }
    let file = open_restricted_key(path)?;
    let metadata = file
        .metadata()
        .map_err(|_| RestrictedDeviceCaError::Configuration)?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_CA_FILE_BYTES {
        return Err(RestrictedDeviceCaError::Configuration);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let process_metadata =
            fs::metadata("/proc/self").map_err(|_| RestrictedDeviceCaError::Configuration)?;
        let parent = path
            .parent()
            .ok_or(RestrictedDeviceCaError::Configuration)?;
        let parent_metadata =
            fs::symlink_metadata(parent).map_err(|_| RestrictedDeviceCaError::Configuration)?;
        if metadata.permissions().mode() & 0o077 != 0
            || parent_metadata.permissions().mode() & 0o077 != 0
            || metadata.uid() != process_metadata.uid()
            || parent_metadata.uid() != process_metadata.uid()
            || !parent_metadata.is_dir()
        {
            return Err(RestrictedDeviceCaError::Configuration);
        }
    }

    #[cfg(not(unix))]
    return Err(RestrictedDeviceCaError::Configuration);

    let mut contents = Zeroizing::new(Vec::with_capacity(
        usize::try_from(metadata.len()).map_err(|_| RestrictedDeviceCaError::Configuration)?,
    ));
    file.take(MAX_CA_FILE_BYTES + 1)
        .read_to_end(&mut contents)
        .map_err(|_| RestrictedDeviceCaError::Configuration)?;
    if contents.is_empty() || contents.len() as u64 > MAX_CA_FILE_BYTES {
        return Err(RestrictedDeviceCaError::Configuration);
    }
    Ok(contents)
}

fn validate_public_ca_certificate(certificate: &X509) -> Result<(), RestrictedDeviceCaError> {
    let public_key = certificate
        .public_key()
        .map_err(|_| RestrictedDeviceCaError::Configuration)?;
    if !certificate
        .verify(&public_key)
        .map_err(|_| RestrictedDeviceCaError::Configuration)?
        || public_key.id() != Id::RSA
        || public_key.bits() < 3_072
        || certificate
            .subject_name()
            .to_der()
            .map_err(|_| RestrictedDeviceCaError::Configuration)?
            != certificate
                .issuer_name()
                .to_der()
                .map_err(|_| RestrictedDeviceCaError::Configuration)?
    {
        return Err(RestrictedDeviceCaError::Configuration);
    }
    let now = Asn1Time::days_from_now(0).map_err(|_| RestrictedDeviceCaError::Configuration)?;
    if certificate
        .not_before()
        .compare(&now)
        .map_err(|_| RestrictedDeviceCaError::Configuration)?
        == std::cmp::Ordering::Greater
        || certificate
            .not_after()
            .compare(&now)
            .map_err(|_| RestrictedDeviceCaError::Configuration)?
            != std::cmp::Ordering::Greater
    {
        return Err(RestrictedDeviceCaError::Configuration);
    }
    let der = certificate
        .to_der()
        .map_err(|_| RestrictedDeviceCaError::Configuration)?;
    let (_, parsed) = x509_parser::parse_x509_certificate(&der)
        .map_err(|_| RestrictedDeviceCaError::Configuration)?;
    let basic = parsed
        .basic_constraints()
        .map_err(|_| RestrictedDeviceCaError::Configuration)?
        .ok_or(RestrictedDeviceCaError::Configuration)?;
    let key_usage = parsed
        .key_usage()
        .map_err(|_| RestrictedDeviceCaError::Configuration)?
        .ok_or(RestrictedDeviceCaError::Configuration)?;
    if !basic.critical
        || !basic.value.ca
        || basic.value.path_len_constraint != Some(0)
        || !key_usage.critical
        || !key_usage.value.key_cert_sign()
        || !key_usage.value.crl_sign()
    {
        return Err(RestrictedDeviceCaError::Configuration);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn open_restricted_key(path: &Path) -> Result<File, RestrictedDeviceCaError> {
    use std::os::unix::fs::OpenOptionsExt;

    // Linux O_NOFOLLOW prevents swapping the final key path to a symlink
    // between the metadata and read checks.
    OpenOptions::new()
        .read(true)
        .custom_flags(0x20000 | 0x80000)
        .open(path)
        .map_err(|_| RestrictedDeviceCaError::Configuration)
}

#[cfg(all(unix, not(target_os = "linux")))]
fn open_restricted_key(path: &Path) -> Result<File, RestrictedDeviceCaError> {
    if fs::symlink_metadata(path)
        .map_err(|_| RestrictedDeviceCaError::Configuration)?
        .file_type()
        .is_symlink()
    {
        return Err(RestrictedDeviceCaError::Configuration);
    }
    File::open(path).map_err(|_| RestrictedDeviceCaError::Configuration)
}

#[cfg(not(unix))]
fn open_restricted_key(_path: &Path) -> Result<File, RestrictedDeviceCaError> {
    Err(RestrictedDeviceCaError::Configuration)
}
