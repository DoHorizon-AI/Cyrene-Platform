//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 durable_directory.rs                                            │
//! │  Module: cy_workspace_fabric::durable_directory                    │
//! │  Role: Async PostgreSQL Directory and device-registration authority. │
//! │                                                                     │
//! │  模块职责：异步 PostgreSQL Directory 与设备 registration authority。  │
//! └─────────────────────────────────────────────────────────────────────┘

use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use cy_proto::workspace_v1::{UserIdentityRef, WorkspaceConnectionDescriptor};
use prost::Message;
use sha2::{Digest, Sha256};
use sqlx::migrate::Migrator;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};
use sqlx::{PgPool, Postgres, Row, Transaction};
use thiserror::Error;
use uuid::Uuid;

use crate::device_authorization::DeviceAuthorizationScope;
use crate::device_registry::WorkspaceDeviceKey;
use crate::directory::{validate_descriptor, WorkspaceDirectory, WorkspaceDirectoryError};

const DATABASE_URL_ENV: &str = "CYRENE_WORKSPACE_DIRECTORY_DATABASE_URL";
const MIGRATION_DATABASE_URL_ENV: &str = "CYRENE_WORKSPACE_DIRECTORY_MIGRATION_DATABASE_URL";
const OPERATOR_DATABASE_URL_ENV: &str = "CYRENE_WORKSPACE_DIRECTORY_OPERATOR_DATABASE_URL";
const OPERATOR_ID_ENV: &str = "CYRENE_WORKSPACE_DIRECTORY_OPERATOR_ID";
const DEVICE_REGISTRATION_DATABASE_URL_ENV: &str =
    "CYRENE_WORKSPACE_DEVICE_REGISTRATION_DATABASE_URL";
const MAX_DESCRIPTOR_BYTES: usize = 1024 * 1024;
const MAX_OPERATOR_ID_BYTES: usize = 256;
const MAX_REASON_BYTES: usize = 2000;
const POOL_MAX_CONNECTIONS: u32 = 8;
const POOL_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(3);
const REGISTRATION_KEY_DOMAIN: &[u8] = b"cyrene-workspace-device-registration-key:v1\0";

/// Directory-owned Workspace capability required to approve device enrollment.
///
/// This role is granted only by the audited Directory operator boundary. It is
/// intentionally separate from Product COMMAND authorization roles.
///
/// 设备 enrollment approval 所需的 Directory Workspace capability。
/// 该 role 仅能由带审计的 Directory operator 边界授予，且不属于 Product COMMAND role。
pub const WORKSPACE_DEVICE_ENROLLMENT_APPROVE_ROLE: &str = "workspace.device.enrollment.approve.v1";

static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

/// Errors from the durable Directory store and its restricted operator port.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum DurableDirectoryError {
    #[error("WORKSPACE_DIRECTORY_CONFIGURATION_INVALID")]
    Configuration,
    #[error("WORKSPACE_DIRECTORY_IDENTITY_INVALID")]
    InvalidIdentity,
    #[error("WORKSPACE_DIRECTORY_SCOPE_INVALID")]
    InvalidScope,
    #[error("WORKSPACE_DIRECTORY_ROLE_NOT_SUPPORTED")]
    UnsupportedRole,
    #[error("WORKSPACE_DIRECTORY_AUDIT_REASON_REQUIRED")]
    AuditReasonRequired,
    #[error("WORKSPACE_DIRECTORY_MEMBERSHIP_NOT_FOUND")]
    MembershipNotFound,
    #[error("WORKSPACE_DIRECTORY_IDENTITY_NOT_MAPPED")]
    NoMembershipMapping,
    #[error("WORKSPACE_DIRECTORY_IDENTITY_ORGANIZATION_AMBIGUOUS")]
    AmbiguousOrganizations,
    #[error("WORKSPACE_DIRECTORY_DESCRIPTOR_INVALID")]
    InvalidDescriptor,
    #[error("WORKSPACE_DIRECTORY_STORAGE_UNAVAILABLE")]
    Storage,
}

/// Failure while binding a device recovery credential to an immutable registration.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum DeviceRegistrationError {
    #[error("WORKSPACE_DEVICE_REGISTRATION_CONFIGURATION_INVALID")]
    Configuration,
    #[error("WORKSPACE_DEVICE_REGISTRATION_REQUEST_INVALID")]
    InvalidRequest,
    #[error("WORKSPACE_DEVICE_REGISTRATION_KEY_CONFLICT")]
    RegistrationKeyConflict,
    #[error("WORKSPACE_DEVICE_REGISTRATION_DEVICE_NOT_FOUND")]
    AuthenticatedDeviceNotFound,
    #[error("WORKSPACE_DEVICE_REGISTRATION_GENERATION_EXHAUSTED")]
    GenerationExhausted,
    #[error("WORKSPACE_DEVICE_REGISTRATION_STORAGE_UNAVAILABLE")]
    Storage,
}

/// Domain-separated digest of a high-entropy registration recovery credential.
///
/// The opaque bytes are suitable for persistence and lookup but remain redacted
/// from debug output to avoid turning a database key into a diagnostic token.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct DeviceRegistrationKeyDigest([u8; 32]);

impl DeviceRegistrationKeyDigest {
    /// Bytes to bind to PostgreSQL or a same-crate composite authorization store.
    pub(crate) fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for DeviceRegistrationKeyDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("DeviceRegistrationKeyDigest")
            .field(&"[REDACTED]")
            .finish()
    }
}

/// Directory result that permanently binds one registration credential to a
/// stable device identity, authorization generation, and validated key material.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRegistrationBinding {
    /// Opaque Directory binding handle; it is not an authorization ID or secret.
    binding_id: [u8; 16],
    /// Stable identity allocated by Directory or reused from authenticated mTLS context.
    key: WorkspaceDeviceKey,
    /// Monotonic generation for a new authorization on this stable device.
    authorization_generation: u64,
    /// Server-recomputed digest of the submitted CSR DER.
    csr_sha256: [u8; 32],
    /// Server-recomputed digest of the CSR subject public key information.
    spki_sha256: [u8; 32],
}

impl DeviceRegistrationBinding {
    /// Returns the opaque Directory binding handle, distinct from an authorization ID.
    pub fn binding_id(&self) -> &[u8; 16] {
        &self.binding_id
    }

    /// Returns the stable organization, Workspace, and device identity.
    pub fn key(&self) -> &WorkspaceDeviceKey {
        &self.key
    }

    /// Returns the monotonic generation assigned to this new authorization.
    pub fn authorization_generation(&self) -> u64 {
        self.authorization_generation
    }

    /// Returns the server-recomputed CSR DER SHA-256 digest.
    pub fn csr_sha256(&self) -> &[u8; 32] {
        &self.csr_sha256
    }

    /// Returns the server-recomputed SPKI SHA-256 digest.
    pub fn spki_sha256(&self) -> &[u8; 32] {
        &self.spki_sha256
    }

    /// Create a test-only binding fixture without exposing a production constructor.
    #[cfg(test)]
    pub(crate) fn test_fixture(
        binding_id: [u8; 16],
        key: WorkspaceDeviceKey,
        authorization_generation: u64,
        csr_sha256: [u8; 32],
        spki_sha256: [u8; 32],
    ) -> Self {
        Self {
            binding_id,
            key,
            authorization_generation,
            csr_sha256,
            spki_sha256,
        }
    }
}

/// Workspace device key captured from a verified current mTLS client identity.
///
/// The private field prevents external request adapters from constructing a
/// rotation authority from a client-supplied device ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedWorkspaceDevice {
    key: WorkspaceDeviceKey,
}

impl AuthenticatedWorkspaceDevice {
    /// Capture the device key after the caller has verified the current mTLS peer.
    ///
    /// This constructor is crate-private so public request fields cannot create
    /// the rotation marker. Only trusted authentication adapters may call it.
    #[allow(dead_code)] // Reserved for the separate mTLS enrollment route; this adapter stays unwired.
    pub(crate) fn from_verified_mtls_peer(key: WorkspaceDeviceKey) -> Self {
        Self { key }
    }

    /// Returns the exact device identity established by the mTLS verifier.
    pub fn key(&self) -> &WorkspaceDeviceKey {
        &self.key
    }
}

/// Async authority for stable Workspace device IDs and registration-key bindings.
///
/// Implementations bind a client-generated, cryptographically random 256-bit
/// recovery credential to exact scope and CSR digests. This port does not read or mutate authorization state; the caller
/// must combine it with the authorization store transaction before enabling
/// enrollment start or approval.
#[tonic::async_trait]
pub trait WorkspaceDeviceRegistrationAuthority: Send + Sync {
    /// Bind a recovery credential to a device registration.
    ///
    /// The client must generate `registration_key` with a CSPRNG; it is secret
    /// input and must not be logged or stored directly. `authenticated_device`
    /// must come only from a verified current
    /// Workspace device certificate, never from request fields. Supplying it
    /// creates a new authorization generation for that stable device; omitting
    /// it allocates a new stable device ID on first use. An exact credential
    /// retry returns its existing binding and generation. The caller must first
    /// validate CSR proof of possession and recompute both digests from the CSR;
    /// request-supplied digest fields are not authoritative.
    async fn bind_registration(
        &self,
        registration_key: &[u8; 32],
        scope: &DeviceAuthorizationScope,
        csr_sha256: &[u8; 32],
        spki_sha256: &[u8; 32],
        authenticated_device: Option<&AuthenticatedWorkspaceDevice>,
    ) -> Result<DeviceRegistrationBinding, DeviceRegistrationError>;
}

/// PostgreSQL-backed registration-key to stable device-ID authority.
///
/// Use the dedicated device registrar database role; this pool is separate
/// from the read-only Workspace Directory pool and operator provisioning pool.
#[derive(Clone)]
pub struct PostgresDeviceRegistrationAuthority {
    pool: PgPool,
}

impl PostgresDeviceRegistrationAuthority {
    /// Connect with the restricted registration writer URL from trusted process configuration.
    pub async fn connect_from_environment() -> Result<Self, DeviceRegistrationError> {
        let database_url = std::env::var(DEVICE_REGISTRATION_DATABASE_URL_ENV)
            .map_err(|_| DeviceRegistrationError::Configuration)?;
        Self::connect(&database_url).await
    }

    /// Connect using the registrar role with PostgreSQL TLS verification and bounded pooling.
    pub async fn connect(database_url: &str) -> Result<Self, DeviceRegistrationError> {
        let pool = connect_pool(database_url, "cyrene-device-registration-registrar")
            .await
            .map_err(map_registration_directory_error)?;
        Ok(Self { pool })
    }
}

#[tonic::async_trait]
impl WorkspaceDeviceRegistrationAuthority for PostgresDeviceRegistrationAuthority {
    async fn bind_registration(
        &self,
        registration_key: &[u8; 32],
        scope: &DeviceAuthorizationScope,
        csr_sha256: &[u8; 32],
        spki_sha256: &[u8; 32],
        authenticated_device: Option<&AuthenticatedWorkspaceDevice>,
    ) -> Result<DeviceRegistrationBinding, DeviceRegistrationError> {
        validate_registration_input(scope, registration_key, authenticated_device)?;
        let registration_digest = registration_key_digest(registration_key);
        let mut lock_prefix = [0; 8];
        lock_prefix.copy_from_slice(&registration_digest.as_bytes()[..8]);
        let lock_id = i64::from_be_bytes(lock_prefix);
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| DeviceRegistrationError::Storage)?;

        // Serialize exact-key retries, including concurrent first starts, before
        // either allocating an identity or advancing a rotation generation.
        // 用 registration-key 派生的事务锁串行化同 key 请求，避免并发创建多个 device ID。
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(lock_id)
            .execute(&mut *transaction)
            .await
            .map_err(|_| DeviceRegistrationError::Storage)?;

        let existing = sqlx::query(
            "SELECT binding_id, organization_id, workspace_id, device_id, \
                    authorization_generation, csr_sha256, spki_sha256 \
             FROM cyrene_workspace_directory.device_registration_bindings \
             WHERE registration_key_digest = $1",
        )
        .bind(registration_digest.as_bytes().as_slice())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| DeviceRegistrationError::Storage)?;

        if let Some(row) = existing {
            let binding = registration_binding_from_row(row)?;
            if binding.key.organization_id != scope.organization_id
                || binding.key.workspace_id != scope.workspace_id
                || binding.csr_sha256 != *csr_sha256
                || binding.spki_sha256 != *spki_sha256
                || authenticated_device.is_some_and(|device| device.key() != &binding.key)
            {
                return Err(DeviceRegistrationError::RegistrationKeyConflict);
            }
            transaction
                .commit()
                .await
                .map_err(|_| DeviceRegistrationError::Storage)?;
            return Ok(binding);
        }

        let (device_id, authorization_generation) = match authenticated_device {
            Some(authenticated) => {
                let device = authenticated.key();
                let generation = sqlx::query_scalar::<_, i64>(
                    "UPDATE cyrene_workspace_directory.workspace_device_identities \
                     SET current_authorization_generation = current_authorization_generation + 1, \
                         updated_at = clock_timestamp() \
                     WHERE organization_id = $1 AND workspace_id = $2 AND device_id = $3 \
                       AND current_authorization_generation < 9223372036854775807 \
                     RETURNING current_authorization_generation",
                )
                .bind(&device.organization_id)
                .bind(&device.workspace_id)
                .bind(&device.device_id)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(|_| DeviceRegistrationError::Storage)?;
                let Some(generation) = generation else {
                    let exists = sqlx::query_scalar::<_, bool>(
                        "SELECT EXISTS (SELECT 1 FROM \
                            cyrene_workspace_directory.workspace_device_identities \
                         WHERE organization_id = $1 AND workspace_id = $2 AND device_id = $3)",
                    )
                    .bind(&device.organization_id)
                    .bind(&device.workspace_id)
                    .bind(&device.device_id)
                    .fetch_one(&mut *transaction)
                    .await
                    .map_err(|_| DeviceRegistrationError::Storage)?;
                    return Err(if exists {
                        DeviceRegistrationError::GenerationExhausted
                    } else {
                        DeviceRegistrationError::AuthenticatedDeviceNotFound
                    });
                };
                (
                    device.device_id.clone(),
                    u64::try_from(generation).map_err(|_| DeviceRegistrationError::Storage)?,
                )
            }
            None => {
                let device_id = Uuid::new_v4().to_string();
                sqlx::query(
                    "INSERT INTO cyrene_workspace_directory.workspace_device_identities \
                        (organization_id, workspace_id, device_id, current_authorization_generation) \
                     VALUES ($1, $2, $3, 1)",
                )
                .bind(&scope.organization_id)
                .bind(&scope.workspace_id)
                .bind(&device_id)
                .execute(&mut *transaction)
                .await
                .map_err(|_| DeviceRegistrationError::Storage)?;
                (device_id, 1)
            }
        };

        let binding_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO cyrene_workspace_directory.device_registration_bindings \
                (binding_id, registration_key_digest, organization_id, workspace_id, device_id, \
                 authorization_generation, csr_sha256, spki_sha256) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(binding_id)
        .bind(registration_digest.as_bytes().as_slice())
        .bind(&scope.organization_id)
        .bind(&scope.workspace_id)
        .bind(&device_id)
        .bind(
            i64::try_from(authorization_generation)
                .map_err(|_| DeviceRegistrationError::Storage)?,
        )
        .bind(csr_sha256.as_slice())
        .bind(spki_sha256.as_slice())
        .execute(&mut *transaction)
        .await
        .map_err(|_| DeviceRegistrationError::Storage)?;

        transaction
            .commit()
            .await
            .map_err(|_| DeviceRegistrationError::Storage)?;
        Ok(DeviceRegistrationBinding {
            binding_id: *binding_id.as_bytes(),
            key: WorkspaceDeviceKey {
                organization_id: scope.organization_id.clone(),
                workspace_id: scope.workspace_id.clone(),
                device_id,
            },
            authorization_generation,
            csr_sha256: *csr_sha256,
            spki_sha256: *spki_sha256,
        })
    }
}

/// Async PostgreSQL Directory reads and stable device-registration bindings.
///
/// `PostgresWorkspaceDirectory` implements the async `WorkspaceDirectory`
/// adapter without blocking Relay or Direct workers. This module does not
/// verify OIDC tokens: callers must provide identities from a separately
/// trusted authentication boundary. Registration binding also does not create
/// or supersede authorization records; production start and approval must
/// compose it with the authorization store in one locked PostgreSQL transaction.
///
/// `PostgresWorkspaceDirectory` 通过异步 `WorkspaceDirectory` adapter 提供查询，不阻塞 Relay/Direct。
/// OIDC token 验证由独立可信边界负责。Registration binding 不创建或 supersede 授权记录；生产 start/approval
/// 必须在同一带锁 PostgreSQL transaction 中与授权存储组合。
#[derive(Clone)]
pub struct PostgresWorkspaceDirectory {
    pool: PgPool,
}

impl PostgresWorkspaceDirectory {
    /// Connect with TLS server and hostname verification using the read-only DB role.
    ///
    /// `database_url` is secret material; it is not retained in this value or
    /// included in returned errors. Configure `sslrootcert` in the URL when a
    /// private PostgreSQL CA is used.
    pub async fn connect(database_url: &str) -> Result<Self, DurableDirectoryError> {
        let pool = connect_pool(database_url, "cyrene-workspace-directory-reader").await?;
        Ok(Self { pool })
    }

    /// Read the database URL from trusted process configuration.
    pub async fn connect_from_environment() -> Result<Self, DurableDirectoryError> {
        let database_url =
            std::env::var(DATABASE_URL_ENV).map_err(|_| DurableDirectoryError::Configuration)?;
        Self::connect(&database_url).await
    }

    /// Check the live Directory connection and required membership projection without writes.
    pub async fn health_check(&self) -> Result<(), DurableDirectoryError> {
        sqlx::query("SELECT organization_id FROM cyrene_workspace_directory.memberships LIMIT 0")
            .fetch_all(&self.pool)
            .await
            .map_err(|_| DurableDirectoryError::Storage)?;
        Ok(())
    }

    /// Return distinct organization candidates for a verified OIDC issuer/subject pair.
    ///
    /// This narrow identity lookup does not select between organizations. The
    /// caller must reject zero or multiple candidates; it must not choose from
    /// a token or request field.
    pub async fn organizations_for_verified_identity(
        &self,
        user: &UserIdentityRef,
    ) -> Result<Vec<String>, DurableDirectoryError> {
        validate_identity(user)?;
        let organizations = sqlx::query_scalar::<_, String>(
            "SELECT DISTINCT organization_id \
             FROM cyrene_workspace_directory.memberships \
             WHERE issuer = $1 AND subject = $2 \
             ORDER BY organization_id",
        )
        .bind(&user.issuer)
        .bind(&user.subject)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| DurableDirectoryError::Storage)?;
        Ok(organizations)
    }

    /// Resolve exactly one organization for a verified OIDC issuer/subject pair.
    ///
    /// Multiple workspaces within one organization remain valid. No row or
    /// more than one distinct organization is rejected.
    pub async fn organization_for_verified_identity(
        &self,
        user: &UserIdentityRef,
    ) -> Result<String, DurableDirectoryError> {
        unique_organization(self.organizations_for_verified_identity(user).await?)
    }

    /// Return descriptors for active memberships in the requested organization.
    pub async fn discover(
        &self,
        user: &UserIdentityRef,
        organization_id: &str,
        now_unix_ms: u64,
    ) -> Result<Vec<WorkspaceConnectionDescriptor>, DurableDirectoryError> {
        validate_identity(user)?;
        validate_scope_part(organization_id)?;
        let rows = sqlx::query(
            "SELECT d.descriptor_proto \
             FROM cyrene_workspace_directory.descriptors AS d \
             INNER JOIN cyrene_workspace_directory.memberships AS m \
                 ON m.organization_id = d.organization_id \
                 AND m.workspace_id = d.workspace_id \
             WHERE m.issuer = $1 AND m.subject = $2 AND m.organization_id = $3 \
             ORDER BY d.workspace_id",
        )
        .bind(&user.issuer)
        .bind(&user.subject)
        .bind(organization_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| DurableDirectoryError::Storage)?;

        rows.into_iter()
            .map(|row| {
                let bytes = row
                    .try_get::<Vec<u8>, _>("descriptor_proto")
                    .map_err(|_| DurableDirectoryError::Storage)?;
                let descriptor = WorkspaceConnectionDescriptor::decode(bytes.as_slice())
                    .map_err(|_| DurableDirectoryError::InvalidDescriptor)?;
                validate_descriptor(&descriptor, now_unix_ms)
                    .map_err(|_| DurableDirectoryError::InvalidDescriptor)?;
                if descriptor.organization_id != organization_id {
                    return Err(DurableDirectoryError::InvalidDescriptor);
                }
                Ok(descriptor)
            })
            .collect()
    }

    /// Return whether this verified identity has a current scoped membership.
    pub async fn is_member(
        &self,
        user: &UserIdentityRef,
        organization_id: &str,
        workspace_id: &str,
    ) -> Result<bool, DurableDirectoryError> {
        validate_identity(user)?;
        validate_scope(organization_id, workspace_id)?;
        sqlx::query_scalar(
            "SELECT EXISTS ( \
                 SELECT 1 FROM cyrene_workspace_directory.memberships \
                 WHERE issuer = $1 AND subject = $2 \
                   AND organization_id = $3 AND workspace_id = $4 \
             )",
        )
        .bind(&user.issuer)
        .bind(&user.subject)
        .bind(organization_id)
        .bind(workspace_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|_| DurableDirectoryError::Storage)
    }

    /// Return explicit assigned roles, or `None` when the membership is absent.
    ///
    /// The derived `workspace.member` marker is added by the caller boundary;
    /// it is not operator-assignable or stored as a role row.
    pub async fn roles_for_member(
        &self,
        user: &UserIdentityRef,
        organization_id: &str,
        workspace_id: &str,
    ) -> Result<Option<BTreeSet<String>>, DurableDirectoryError> {
        validate_identity(user)?;
        validate_scope(organization_id, workspace_id)?;
        let rows = sqlx::query(
            "SELECT r.role_name \
             FROM cyrene_workspace_directory.memberships AS m \
             LEFT JOIN cyrene_workspace_directory.roles AS r \
                 ON r.issuer = m.issuer AND r.subject = m.subject \
                 AND r.organization_id = m.organization_id \
                 AND r.workspace_id = m.workspace_id \
             WHERE m.issuer = $1 AND m.subject = $2 \
               AND m.organization_id = $3 AND m.workspace_id = $4 \
             ORDER BY r.role_name",
        )
        .bind(&user.issuer)
        .bind(&user.subject)
        .bind(organization_id)
        .bind(workspace_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| DurableDirectoryError::Storage)?;

        if rows.is_empty() {
            return Ok(None);
        }
        let mut roles = BTreeSet::new();
        for row in rows {
            if let Some(role) = row
                .try_get::<Option<String>, _>("role_name")
                .map_err(|_| DurableDirectoryError::Storage)?
            {
                roles.insert(role);
            }
        }
        Ok(Some(roles))
    }
}

#[tonic::async_trait]
impl WorkspaceDirectory for PostgresWorkspaceDirectory {
    async fn discover(
        &self,
        user: &UserIdentityRef,
        organization_id: &str,
        now_unix_ms: u64,
    ) -> Result<Vec<WorkspaceConnectionDescriptor>, WorkspaceDirectoryError> {
        PostgresWorkspaceDirectory::discover(self, user, organization_id, now_unix_ms)
            .await
            .map_err(map_workspace_directory_error)
    }

    async fn is_member(
        &self,
        user: &UserIdentityRef,
        organization_id: &str,
        workspace_id: &str,
    ) -> Result<bool, WorkspaceDirectoryError> {
        PostgresWorkspaceDirectory::is_member(self, user, organization_id, workspace_id)
            .await
            .map_err(map_workspace_directory_error)
    }

    async fn roles_for_member(
        &self,
        user: &UserIdentityRef,
        organization_id: &str,
        workspace_id: &str,
    ) -> Result<Option<BTreeSet<String>>, WorkspaceDirectoryError> {
        PostgresWorkspaceDirectory::roles_for_member(self, user, organization_id, workspace_id)
            .await
            .map_err(map_workspace_directory_error)
    }
}

fn map_workspace_directory_error(error: DurableDirectoryError) -> WorkspaceDirectoryError {
    match error {
        DurableDirectoryError::InvalidIdentity
        | DurableDirectoryError::InvalidScope
        | DurableDirectoryError::NoMembershipMapping
        | DurableDirectoryError::AmbiguousOrganizations => {
            WorkspaceDirectoryError::Identity("Directory identity or scope is invalid".to_owned())
        }
        DurableDirectoryError::InvalidDescriptor => WorkspaceDirectoryError::Descriptor(
            "Directory returned an invalid descriptor".to_owned(),
        ),
        DurableDirectoryError::Configuration
        | DurableDirectoryError::UnsupportedRole
        | DurableDirectoryError::AuditReasonRequired
        | DurableDirectoryError::MembershipNotFound
        | DurableDirectoryError::Storage => {
            WorkspaceDirectoryError::Storage("Workspace Directory is unavailable".to_owned())
        }
    }
}

/// Explicit change applied by the restricted local provisioning tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectoryMutation {
    Changed,
    Unchanged,
}

/// Transactional operator provisioning backed by a separate writer credential.
///
/// Its actor ID comes only from trusted process configuration and cannot be
/// passed in a command-line request. The database role must be granted the
/// migration-created `cyrene_workspace_directory_operator` role.
pub struct DirectoryOperatorProvisioner {
    pool: PgPool,
    actor_id: String,
}

impl DirectoryOperatorProvisioner {
    /// Connect using a restricted writer URL and a host-configured operator ID.
    pub async fn connect_from_environment() -> Result<Self, DurableDirectoryError> {
        let database_url = std::env::var(OPERATOR_DATABASE_URL_ENV)
            .map_err(|_| DurableDirectoryError::Configuration)?;
        let actor_id =
            std::env::var(OPERATOR_ID_ENV).map_err(|_| DurableDirectoryError::Configuration)?;
        validate_operator_and_reason(&actor_id, "provisioning configuration")?;
        let pool = connect_pool(&database_url, "cyrene-workspace-directory-operator").await?;
        Ok(Self { pool, actor_id })
    }

    /// Apply the schema migrations using a separate deployment credential.
    pub async fn migrate_from_environment() -> Result<(), DurableDirectoryError> {
        let database_url = std::env::var(MIGRATION_DATABASE_URL_ENV)
            .map_err(|_| DurableDirectoryError::Configuration)?;
        let pool = connect_pool(&database_url, "cyrene-workspace-directory-migrator").await?;
        MIGRATOR
            .run(&pool)
            .await
            .map_err(|_| DurableDirectoryError::Storage)
    }

    /// Create a membership and any allowed roles in one auditable transaction.
    pub async fn grant_membership(
        &self,
        user: &UserIdentityRef,
        organization_id: &str,
        workspace_id: &str,
        roles: &BTreeSet<String>,
        reason: &str,
    ) -> Result<DirectoryMutation, DurableDirectoryError> {
        validate_identity(user)?;
        validate_scope(organization_id, workspace_id)?;
        validate_roles(roles)?;
        validate_operator_and_reason(&self.actor_id, reason)?;

        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| DurableDirectoryError::Storage)?;
        let membership_inserted = sqlx::query(
            "INSERT INTO cyrene_workspace_directory.memberships \
                (issuer, subject, organization_id, workspace_id) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (issuer, subject, organization_id, workspace_id) DO NOTHING",
        )
        .bind(&user.issuer)
        .bind(&user.subject)
        .bind(organization_id)
        .bind(workspace_id)
        .execute(&mut *transaction)
        .await
        .map_err(|_| DurableDirectoryError::Storage)?
        .rows_affected()
            > 0;

        // Serialize role grants against concurrent role or membership revocation.
        lock_membership(&mut transaction, user, organization_id, workspace_id).await?;

        let change_id = Uuid::new_v4();
        let mut event_ordinal = 0_i16;
        let mut changed = membership_inserted;
        let audit_context = AuditContext {
            actor_id: &self.actor_id,
            user,
            organization_id,
            workspace_id,
            reason,
        };
        if membership_inserted {
            insert_audit_event(
                &mut transaction,
                audit_context,
                change_id,
                event_ordinal,
                "membership.granted",
                &[],
            )
            .await?;
            event_ordinal += 1;
        }

        for role in roles {
            let role_inserted = sqlx::query(
                "INSERT INTO cyrene_workspace_directory.roles \
                    (issuer, subject, organization_id, workspace_id, role_name) \
                 VALUES ($1, $2, $3, $4, $5) \
                 ON CONFLICT (issuer, subject, organization_id, workspace_id, role_name) DO NOTHING",
            )
            .bind(&user.issuer)
            .bind(&user.subject)
            .bind(organization_id)
            .bind(workspace_id)
            .bind(role)
            .execute(&mut *transaction)
            .await
            .map_err(|_| DurableDirectoryError::Storage)?
            .rows_affected()
                > 0;
            if role_inserted {
                changed = true;
                insert_audit_event(
                    &mut transaction,
                    audit_context,
                    change_id,
                    event_ordinal,
                    "role.granted",
                    std::slice::from_ref(role),
                )
                .await?;
                event_ordinal += 1;
            }
        }
        transaction
            .commit()
            .await
            .map_err(|_| DurableDirectoryError::Storage)?;
        Ok(mutation_result(changed))
    }

    /// Add one supported role to an existing membership and audit atomically.
    pub async fn grant_role(
        &self,
        user: &UserIdentityRef,
        organization_id: &str,
        workspace_id: &str,
        role: &str,
        reason: &str,
    ) -> Result<DirectoryMutation, DurableDirectoryError> {
        validate_identity(user)?;
        validate_scope(organization_id, workspace_id)?;
        validate_role(role)?;
        validate_operator_and_reason(&self.actor_id, reason)?;

        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| DurableDirectoryError::Storage)?;
        lock_membership(&mut transaction, user, organization_id, workspace_id).await?;
        let inserted = sqlx::query(
            "INSERT INTO cyrene_workspace_directory.roles \
                (issuer, subject, organization_id, workspace_id, role_name) \
             VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT (issuer, subject, organization_id, workspace_id, role_name) DO NOTHING",
        )
        .bind(&user.issuer)
        .bind(&user.subject)
        .bind(organization_id)
        .bind(workspace_id)
        .bind(role)
        .execute(&mut *transaction)
        .await
        .map_err(|_| DurableDirectoryError::Storage)?
        .rows_affected()
            > 0;

        if inserted {
            insert_audit_event(
                &mut transaction,
                AuditContext {
                    actor_id: &self.actor_id,
                    user,
                    organization_id,
                    workspace_id,
                    reason,
                },
                Uuid::new_v4(),
                0,
                "role.granted",
                &[role.to_string()],
            )
            .await?;
        }
        transaction
            .commit()
            .await
            .map_err(|_| DurableDirectoryError::Storage)?;
        Ok(mutation_result(inserted))
    }

    /// Remove one supported role from an existing membership and audit atomically.
    pub async fn revoke_role(
        &self,
        user: &UserIdentityRef,
        organization_id: &str,
        workspace_id: &str,
        role: &str,
        reason: &str,
    ) -> Result<DirectoryMutation, DurableDirectoryError> {
        validate_identity(user)?;
        validate_scope(organization_id, workspace_id)?;
        validate_role(role)?;
        validate_operator_and_reason(&self.actor_id, reason)?;

        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| DurableDirectoryError::Storage)?;
        lock_membership(&mut transaction, user, organization_id, workspace_id).await?;
        let deleted = sqlx::query(
            "DELETE FROM cyrene_workspace_directory.roles \
             WHERE issuer = $1 AND subject = $2 AND organization_id = $3 \
               AND workspace_id = $4 AND role_name = $5",
        )
        .bind(&user.issuer)
        .bind(&user.subject)
        .bind(organization_id)
        .bind(workspace_id)
        .bind(role)
        .execute(&mut *transaction)
        .await
        .map_err(|_| DurableDirectoryError::Storage)?
        .rows_affected()
            > 0;

        if deleted {
            insert_audit_event(
                &mut transaction,
                AuditContext {
                    actor_id: &self.actor_id,
                    user,
                    organization_id,
                    workspace_id,
                    reason,
                },
                Uuid::new_v4(),
                0,
                "role.revoked",
                &[role.to_string()],
            )
            .await?;
        }
        transaction
            .commit()
            .await
            .map_err(|_| DurableDirectoryError::Storage)?;
        Ok(mutation_result(deleted))
    }

    /// Revoke a membership, its roles, and record the affected roles atomically.
    pub async fn revoke_membership(
        &self,
        user: &UserIdentityRef,
        organization_id: &str,
        workspace_id: &str,
        reason: &str,
    ) -> Result<DirectoryMutation, DurableDirectoryError> {
        validate_identity(user)?;
        validate_scope(organization_id, workspace_id)?;
        validate_operator_and_reason(&self.actor_id, reason)?;

        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| DurableDirectoryError::Storage)?;
        let rows = sqlx::query(
            "SELECT r.role_name \
             FROM cyrene_workspace_directory.memberships AS m \
             LEFT JOIN cyrene_workspace_directory.roles AS r \
                 ON r.issuer = m.issuer AND r.subject = m.subject \
                 AND r.organization_id = m.organization_id \
                 AND r.workspace_id = m.workspace_id \
             WHERE m.issuer = $1 AND m.subject = $2 \
               AND m.organization_id = $3 AND m.workspace_id = $4 \
             FOR UPDATE OF m",
        )
        .bind(&user.issuer)
        .bind(&user.subject)
        .bind(organization_id)
        .bind(workspace_id)
        .fetch_all(&mut *transaction)
        .await
        .map_err(|_| DurableDirectoryError::Storage)?;
        if rows.is_empty() {
            transaction
                .commit()
                .await
                .map_err(|_| DurableDirectoryError::Storage)?;
            return Ok(DirectoryMutation::Unchanged);
        }
        let mut affected_roles = BTreeSet::new();
        for row in rows {
            if let Some(role) = row
                .try_get::<Option<String>, _>("role_name")
                .map_err(|_| DurableDirectoryError::Storage)?
            {
                affected_roles.insert(role);
            }
        }
        sqlx::query(
            "DELETE FROM cyrene_workspace_directory.memberships \
             WHERE issuer = $1 AND subject = $2 AND organization_id = $3 AND workspace_id = $4",
        )
        .bind(&user.issuer)
        .bind(&user.subject)
        .bind(organization_id)
        .bind(workspace_id)
        .execute(&mut *transaction)
        .await
        .map_err(|_| DurableDirectoryError::Storage)?;
        let affected_roles = affected_roles.into_iter().collect::<Vec<_>>();
        insert_audit_event(
            &mut transaction,
            AuditContext {
                actor_id: &self.actor_id,
                user,
                organization_id,
                workspace_id,
                reason,
            },
            Uuid::new_v4(),
            0,
            "membership.revoked",
            &affected_roles,
        )
        .await?;
        transaction
            .commit()
            .await
            .map_err(|_| DurableDirectoryError::Storage)?;
        Ok(DirectoryMutation::Changed)
    }

    /// Publish a validated connection descriptor and append the audit row in one transaction.
    pub async fn publish_descriptor(
        &self,
        descriptor: &WorkspaceConnectionDescriptor,
        reason: &str,
    ) -> Result<DirectoryMutation, DurableDirectoryError> {
        validate_scope(&descriptor.organization_id, &descriptor.workspace_id)?;
        validate_operator_and_reason(&self.actor_id, reason)?;
        validate_descriptor(descriptor, now_unix_ms())
            .map_err(|_| DurableDirectoryError::InvalidDescriptor)?;
        let payload = descriptor.encode_to_vec();
        if payload.is_empty() || payload.len() > MAX_DESCRIPTOR_BYTES {
            return Err(DurableDirectoryError::InvalidDescriptor);
        }

        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| DurableDirectoryError::Storage)?;
        let changed = sqlx::query(
            "INSERT INTO cyrene_workspace_directory.descriptors \
                (organization_id, workspace_id, descriptor_proto, updated_by) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (organization_id, workspace_id) DO UPDATE \
                 SET descriptor_proto = EXCLUDED.descriptor_proto, \
                     updated_by = EXCLUDED.updated_by, \
                     updated_at = clock_timestamp() \
                 WHERE cyrene_workspace_directory.descriptors.descriptor_proto \
                     IS DISTINCT FROM EXCLUDED.descriptor_proto",
        )
        .bind(&descriptor.organization_id)
        .bind(&descriptor.workspace_id)
        .bind(payload)
        .bind(&self.actor_id)
        .execute(&mut *transaction)
        .await
        .map_err(|_| DurableDirectoryError::Storage)?
        .rows_affected()
            > 0;
        if changed {
            insert_descriptor_audit(
                &mut transaction,
                &self.actor_id,
                "descriptor.published",
                &descriptor.organization_id,
                &descriptor.workspace_id,
                reason,
            )
            .await?;
        }
        transaction
            .commit()
            .await
            .map_err(|_| DurableDirectoryError::Storage)?;
        Ok(mutation_result(changed))
    }

    /// Remove a descriptor and append the audit row in one transaction.
    pub async fn revoke_descriptor(
        &self,
        organization_id: &str,
        workspace_id: &str,
        reason: &str,
    ) -> Result<DirectoryMutation, DurableDirectoryError> {
        validate_scope(organization_id, workspace_id)?;
        validate_operator_and_reason(&self.actor_id, reason)?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| DurableDirectoryError::Storage)?;
        let deleted = sqlx::query(
            "DELETE FROM cyrene_workspace_directory.descriptors \
             WHERE organization_id = $1 AND workspace_id = $2",
        )
        .bind(organization_id)
        .bind(workspace_id)
        .execute(&mut *transaction)
        .await
        .map_err(|_| DurableDirectoryError::Storage)?
        .rows_affected()
            > 0;
        if deleted {
            insert_descriptor_audit(
                &mut transaction,
                &self.actor_id,
                "descriptor.revoked",
                organization_id,
                workspace_id,
                reason,
            )
            .await?;
        }
        transaction
            .commit()
            .await
            .map_err(|_| DurableDirectoryError::Storage)?;
        Ok(mutation_result(deleted))
    }
}

fn validate_registration_input(
    scope: &DeviceAuthorizationScope,
    registration_key: &[u8; 32],
    authenticated_device: Option<&AuthenticatedWorkspaceDevice>,
) -> Result<(), DeviceRegistrationError> {
    validate_scope(&scope.organization_id, &scope.workspace_id)
        .map_err(|_| DeviceRegistrationError::InvalidRequest)?;
    if registration_key.iter().all(|byte| *byte == 0) {
        return Err(DeviceRegistrationError::InvalidRequest);
    }
    if let Some(authenticated) = authenticated_device {
        let device = authenticated.key();
        if device.organization_id != scope.organization_id
            || device.workspace_id != scope.workspace_id
            || device.device_id.trim().is_empty()
            || device.device_id.len() > 256
            || device.device_id.chars().any(char::is_control)
        {
            return Err(DeviceRegistrationError::InvalidRequest);
        }
    }
    Ok(())
}

pub(crate) fn registration_key_digest(registration_key: &[u8; 32]) -> DeviceRegistrationKeyDigest {
    let mut digest = Sha256::new();
    digest.update(REGISTRATION_KEY_DOMAIN);
    digest.update(registration_key);
    DeviceRegistrationKeyDigest(digest.finalize().into())
}

fn registration_binding_from_row(
    row: sqlx::postgres::PgRow,
) -> Result<DeviceRegistrationBinding, DeviceRegistrationError> {
    let binding_id: Uuid = row
        .try_get("binding_id")
        .map_err(|_| DeviceRegistrationError::Storage)?;
    let authorization_generation: i64 = row
        .try_get("authorization_generation")
        .map_err(|_| DeviceRegistrationError::Storage)?;
    Ok(DeviceRegistrationBinding {
        binding_id: *binding_id.as_bytes(),
        key: WorkspaceDeviceKey {
            organization_id: row
                .try_get("organization_id")
                .map_err(|_| DeviceRegistrationError::Storage)?,
            workspace_id: row
                .try_get("workspace_id")
                .map_err(|_| DeviceRegistrationError::Storage)?,
            device_id: row
                .try_get("device_id")
                .map_err(|_| DeviceRegistrationError::Storage)?,
        },
        authorization_generation: u64::try_from(authorization_generation)
            .map_err(|_| DeviceRegistrationError::Storage)?,
        csr_sha256: digest_column(&row, "csr_sha256")?,
        spki_sha256: digest_column(&row, "spki_sha256")?,
    })
}

fn digest_column(
    row: &sqlx::postgres::PgRow,
    column: &str,
) -> Result<[u8; 32], DeviceRegistrationError> {
    let bytes: Vec<u8> = row
        .try_get(column)
        .map_err(|_| DeviceRegistrationError::Storage)?;
    bytes
        .as_slice()
        .try_into()
        .map_err(|_| DeviceRegistrationError::Storage)
}

fn map_registration_directory_error(error: DurableDirectoryError) -> DeviceRegistrationError {
    match error {
        DurableDirectoryError::Configuration => DeviceRegistrationError::Configuration,
        _ => DeviceRegistrationError::Storage,
    }
}

async fn connect_pool(
    database_url: &str,
    application_name: &'static str,
) -> Result<PgPool, DurableDirectoryError> {
    let options = PgConnectOptions::from_str(database_url)
        .map_err(|_| DurableDirectoryError::Configuration)?
        .ssl_mode(PgSslMode::VerifyFull)
        .application_name(application_name)
        .options([("statement_timeout", "5000"), ("lock_timeout", "3000")]);
    PgPoolOptions::new()
        .max_connections(POOL_MAX_CONNECTIONS)
        .acquire_timeout(POOL_ACQUIRE_TIMEOUT)
        .connect_with(options)
        .await
        .map_err(|_| DurableDirectoryError::Storage)
}

async fn lock_membership(
    transaction: &mut Transaction<'_, Postgres>,
    user: &UserIdentityRef,
    organization_id: &str,
    workspace_id: &str,
) -> Result<(), DurableDirectoryError> {
    let membership = sqlx::query_scalar::<_, String>(
        "SELECT workspace_id \
         FROM cyrene_workspace_directory.memberships \
         WHERE issuer = $1 AND subject = $2 \
           AND organization_id = $3 AND workspace_id = $4 \
         FOR UPDATE",
    )
    .bind(&user.issuer)
    .bind(&user.subject)
    .bind(organization_id)
    .bind(workspace_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|_| DurableDirectoryError::Storage)?;
    membership
        .map(|_| ())
        .ok_or(DurableDirectoryError::MembershipNotFound)
}

#[derive(Clone, Copy)]
struct AuditContext<'a> {
    actor_id: &'a str,
    user: &'a UserIdentityRef,
    organization_id: &'a str,
    workspace_id: &'a str,
    reason: &'a str,
}

async fn insert_audit_event(
    transaction: &mut Transaction<'_, Postgres>,
    context: AuditContext<'_>,
    change_id: Uuid,
    event_ordinal: i16,
    action: &str,
    affected_roles: &[String],
) -> Result<(), DurableDirectoryError> {
    sqlx::query(
        "INSERT INTO cyrene_workspace_directory.audit_events \
            (change_id, event_ordinal, actor_id, action, issuer, subject, \
             organization_id, workspace_id, affected_roles, reason) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(change_id)
    .bind(event_ordinal)
    .bind(context.actor_id)
    .bind(action)
    .bind(&context.user.issuer)
    .bind(&context.user.subject)
    .bind(context.organization_id)
    .bind(context.workspace_id)
    .bind(affected_roles)
    .bind(context.reason)
    .execute(&mut **transaction)
    .await
    .map_err(|_| DurableDirectoryError::Storage)?;
    Ok(())
}

async fn insert_descriptor_audit(
    transaction: &mut Transaction<'_, Postgres>,
    actor_id: &str,
    action: &str,
    organization_id: &str,
    workspace_id: &str,
    reason: &str,
) -> Result<(), DurableDirectoryError> {
    sqlx::query(
        "INSERT INTO cyrene_workspace_directory.audit_events \
            (change_id, event_ordinal, actor_id, action, organization_id, workspace_id, reason) \
         VALUES ($1, 0, $2, $3, $4, $5, $6)",
    )
    .bind(Uuid::new_v4())
    .bind(actor_id)
    .bind(action)
    .bind(organization_id)
    .bind(workspace_id)
    .bind(reason)
    .execute(&mut **transaction)
    .await
    .map_err(|_| DurableDirectoryError::Storage)?;
    Ok(())
}

fn unique_organization(organizations: Vec<String>) -> Result<String, DurableDirectoryError> {
    match organizations.as_slice() {
        [] => Err(DurableDirectoryError::NoMembershipMapping),
        [organization_id] => Ok(organization_id.clone()),
        _ => Err(DurableDirectoryError::AmbiguousOrganizations),
    }
}

fn validate_identity(user: &UserIdentityRef) -> Result<(), DurableDirectoryError> {
    if user.issuer.trim().is_empty()
        || user.subject.trim().is_empty()
        || user.issuer.len() > 2048
        || user.subject.len() > 2048
    {
        return Err(DurableDirectoryError::InvalidIdentity);
    }
    Ok(())
}

fn validate_scope(organization_id: &str, workspace_id: &str) -> Result<(), DurableDirectoryError> {
    validate_scope_part(organization_id)?;
    validate_scope_part(workspace_id)
}

fn validate_scope_part(value: &str) -> Result<(), DurableDirectoryError> {
    if value.trim().is_empty() || value.len() > 256 {
        return Err(DurableDirectoryError::InvalidScope);
    }
    Ok(())
}

fn validate_roles(roles: &BTreeSet<String>) -> Result<(), DurableDirectoryError> {
    for role in roles {
        validate_role(role)?;
    }
    Ok(())
}

fn validate_role(role: &str) -> Result<(), DurableDirectoryError> {
    if SUPPORTED_OPERATOR_ROLES.contains(&role) {
        return validate_product_command_role(role);
    }
    validate_device_enrollment_approval_role(role)
}

fn validate_product_command_role(role: &str) -> Result<(), DurableDirectoryError> {
    if SUPPORTED_OPERATOR_ROLES.contains(&role) {
        Ok(())
    } else {
        Err(DurableDirectoryError::UnsupportedRole)
    }
}

fn validate_device_enrollment_approval_role(role: &str) -> Result<(), DurableDirectoryError> {
    if role == WORKSPACE_DEVICE_ENROLLMENT_APPROVE_ROLE {
        Ok(())
    } else {
        Err(DurableDirectoryError::UnsupportedRole)
    }
}

fn validate_operator_and_reason(actor_id: &str, reason: &str) -> Result<(), DurableDirectoryError> {
    if actor_id.trim().is_empty()
        || actor_id.len() > MAX_OPERATOR_ID_BYTES
        || actor_id.chars().any(char::is_control)
    {
        return Err(DurableDirectoryError::Configuration);
    }
    if reason.trim().is_empty()
        || reason.len() > MAX_REASON_BYTES
        || reason.chars().any(char::is_control)
    {
        return Err(DurableDirectoryError::AuditReasonRequired);
    }
    Ok(())
}

fn mutation_result(changed: bool) -> DirectoryMutation {
    if changed {
        DirectoryMutation::Changed
    } else {
        DirectoryMutation::Unchanged
    }
}

fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

/// Roles currently accepted by Product authorization policy for a user.
///
/// Membership itself derives `workspace.member`; workload attestations are a
/// separate authority and are intentionally absent from this list.
pub const SUPPORTED_OPERATOR_ROLES: [&str; 5] = [
    "workspace.product.command.catalyst.create_dataset.v1",
    "workspace.product.command.yield.start_training_run.v1",
    "workspace.product.command.reactor.create_model_import.v1",
    "workspace.product.command.exchange.create_route_draft.v1",
    "workspace.product.command.echo.create_evaluation_suite.v1",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn organization_resolution_requires_exactly_one_distinct_mapping() {
        assert_eq!(
            unique_organization(Vec::new()),
            Err(DurableDirectoryError::NoMembershipMapping)
        );
        assert_eq!(
            unique_organization(vec!["organization-1".into()]),
            Ok("organization-1".into())
        );
        assert_eq!(
            unique_organization(vec!["organization-1".into(), "organization-2".into()]),
            Err(DurableDirectoryError::AmbiguousOrganizations)
        );
    }

    #[test]
    fn user_membership_role_and_workload_role_cannot_be_operator_assigned() {
        let roles = BTreeSet::from([
            "workspace.member".to_string(),
            "navigator.service-writer".to_string(),
        ]);
        assert_eq!(
            validate_roles(&roles),
            Err(DurableDirectoryError::UnsupportedRole)
        );
        assert!(validate_role(SUPPORTED_OPERATOR_ROLES[0]).is_ok());
    }

    #[test]
    fn operator_audit_configuration_requires_actor_and_reason() {
        assert_eq!(
            validate_operator_and_reason("", "provisioning configuration"),
            Err(DurableDirectoryError::Configuration)
        );
        assert_eq!(
            validate_operator_and_reason("operator-1", " "),
            Err(DurableDirectoryError::AuditReasonRequired)
        );
        assert!(validate_operator_and_reason("operator-1", "ticket 123").is_ok());
    }

    #[test]
    fn registration_key_digest_uses_its_own_domain_separator() {
        let registration_key = [7u8; 32];
        let digest = registration_key_digest(&registration_key);
        let plain_digest: [u8; 32] = Sha256::digest(registration_key).into();

        assert_eq!(digest, registration_key_digest(&registration_key));
        assert_ne!(*digest.as_bytes(), plain_digest);
        assert_ne!(digest.as_bytes().as_slice(), registration_key.as_slice());
        assert_eq!(
            format!("{digest:?}"),
            "DeviceRegistrationKeyDigest(\"[REDACTED]\")"
        );
    }

    #[test]
    fn registration_validation_requires_secret_and_verified_device_scope() {
        let scope = DeviceAuthorizationScope {
            organization_id: "organization-1".into(),
            workspace_id: "workspace-1".into(),
        };
        let registration_key = [9u8; 32];
        let matching_device = WorkspaceDeviceKey {
            organization_id: scope.organization_id.clone(),
            workspace_id: scope.workspace_id.clone(),
            device_id: "device-1".into(),
        };
        let authenticated_device =
            AuthenticatedWorkspaceDevice::from_verified_mtls_peer(matching_device.clone());

        assert!(validate_registration_input(
            &scope,
            &registration_key,
            Some(&authenticated_device)
        )
        .is_ok());
        assert_eq!(
            validate_registration_input(&scope, &[0; 32], None),
            Err(DeviceRegistrationError::InvalidRequest)
        );
        let wrong_scope_device =
            AuthenticatedWorkspaceDevice::from_verified_mtls_peer(WorkspaceDeviceKey {
                organization_id: "organization-2".into(),
                ..matching_device
            });
        assert_eq!(
            validate_registration_input(&scope, &registration_key, Some(&wrong_scope_device)),
            Err(DeviceRegistrationError::InvalidRequest)
        );
    }

    #[test]
    fn registration_binding_exposes_only_read_accessors() {
        let key = WorkspaceDeviceKey {
            organization_id: "organization-1".into(),
            workspace_id: "workspace-1".into(),
            device_id: "device-1".into(),
        };
        let binding =
            DeviceRegistrationBinding::test_fixture([1; 16], key.clone(), 3, [2; 32], [3; 32]);

        assert_eq!(binding.binding_id(), &[1; 16]);
        assert_eq!(binding.key(), &key);
        assert_eq!(binding.authorization_generation(), 3);
        assert_eq!(binding.csr_sha256(), &[2; 32]);
        assert_eq!(binding.spki_sha256(), &[3; 32]);
    }
}
