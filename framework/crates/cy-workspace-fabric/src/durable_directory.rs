//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 durable_directory.rs                                            │
//! │  Module: cy_workspace_fabric::durable_directory                    │
//! │  Role: Async PostgreSQL membership, role, descriptor, and audit I/O. │
//! │                                                                     │
//! │  模块职责：异步 PostgreSQL 成员、角色、描述符及审计存储。                │
//! └─────────────────────────────────────────────────────────────────────┘

use std::collections::BTreeSet;
use std::str::FromStr;
use std::time::Duration;

use cy_proto::workspace_v1::{UserIdentityRef, WorkspaceConnectionDescriptor};
use prost::Message;
use sqlx::migrate::Migrator;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};
use sqlx::{PgPool, Postgres, Row, Transaction};
use thiserror::Error;
use uuid::Uuid;

use crate::directory::validate_descriptor;

const DATABASE_URL_ENV: &str = "CYRENE_WORKSPACE_DIRECTORY_DATABASE_URL";
const MIGRATION_DATABASE_URL_ENV: &str = "CYRENE_WORKSPACE_DIRECTORY_MIGRATION_DATABASE_URL";
const OPERATOR_DATABASE_URL_ENV: &str = "CYRENE_WORKSPACE_DIRECTORY_OPERATOR_DATABASE_URL";
const OPERATOR_ID_ENV: &str = "CYRENE_WORKSPACE_DIRECTORY_OPERATOR_ID";
const MAX_DESCRIPTOR_BYTES: usize = 1024 * 1024;
const MAX_OPERATOR_ID_BYTES: usize = 256;
const MAX_REASON_BYTES: usize = 2000;
const POOL_MAX_CONNECTIONS: u32 = 8;
const POOL_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(3);

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

/// Async, PostgreSQL-backed Directory reads.
///
/// This store intentionally does not implement the current synchronous
/// `WorkspaceDirectory` trait. A later async Directory adapter can compose
/// this port without blocking Relay or Direct request workers. It also does
/// not verify OIDC tokens: callers must pass an issuer and subject obtained
/// from a separately trusted authentication boundary.
///
/// 此存储暂不实现现有同步 trait；OIDC 验证仍由独立身份边界负责，本模块只按已验证 issuer/subject 查询持久授权。
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
}
