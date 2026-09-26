//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 directory.rs                                                    │
//! │  Module: cy_workspace_fabric::directory                             │
//! │  Role: Account membership and Workspace discovery boundary.         │
//! │                                                                     │
//! │  模块职责：Account 成员关系与 Workspace 发现边界。                      │
//! └─────────────────────────────────────────────────────────────────────┘

use std::collections::BTreeSet;

use cy_proto::core_v1::ConnectivityMode;
use cy_proto::workspace_v1::{UserIdentityRef, WorkspaceConnectionDescriptor};
use thiserror::Error;

use crate::auth::same_user;
use crate::durable_directory::{DurableDirectoryError, PostgresWorkspaceDirectory};

/// Directory-owned membership. It carries no Product or execution state.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceMembership {
    pub user: UserIdentityRef,
    pub organization_id: String,
    pub workspace_id: String,
    pub roles: BTreeSet<String>,
}

/// Workspace discovery failure kept separate from Workspace API failures.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum WorkspaceDirectoryError {
    #[error("WORKSPACE_DIRECTORY_IDENTITY_INVALID: {0}")]
    Identity(String),
    #[error("WORKSPACE_DESCRIPTOR_INVALID: {0}")]
    Descriptor(String),
    #[error("WORKSPACE_DIRECTORY_STORAGE: {0}")]
    Storage(String),
}

impl WorkspaceDirectoryError {
    /// Stable gRPC status code and public message without backend diagnostics.
    pub(crate) fn rpc_error(&self) -> (i32, &'static str) {
        match self {
            Self::Identity(_) => (3, "WORKSPACE_DIRECTORY_INVALID_REQUEST"),
            Self::Descriptor(_) => (13, "WORKSPACE_DIRECTORY_DATA_INVALID"),
            Self::Storage(_) => (14, "WORKSPACE_DIRECTORY_UNAVAILABLE"),
        }
    }
}

/// Account/Directory port. Implementations own membership and descriptors only.
#[tonic::async_trait]
pub trait WorkspaceDirectory: Send + Sync {
    async fn discover(
        &self,
        user: &UserIdentityRef,
        organization_id: &str,
        now_unix_ms: u64,
    ) -> Result<Vec<WorkspaceConnectionDescriptor>, WorkspaceDirectoryError>;

    async fn is_member(
        &self,
        user: &UserIdentityRef,
        organization_id: &str,
        workspace_id: &str,
    ) -> Result<bool, WorkspaceDirectoryError>;

    /// Returns Directory-owned roles only when the user is a current member.
    ///
    /// Adapters that only expose membership can leave the role set empty;
    /// membership remains distinct from optional assigned roles.
    async fn roles_for_member(
        &self,
        user: &UserIdentityRef,
        organization_id: &str,
        workspace_id: &str,
    ) -> Result<Option<BTreeSet<String>>, WorkspaceDirectoryError> {
        Ok(self
            .is_member(user, organization_id, workspace_id)
            .await?
            .then(BTreeSet::new))
    }
}

/// Development/reference Directory with explicit membership records.
#[derive(Debug, Clone)]
pub struct InMemoryWorkspaceDirectory {
    memberships: Vec<WorkspaceMembership>,
    descriptors: Vec<WorkspaceConnectionDescriptor>,
}

impl InMemoryWorkspaceDirectory {
    pub fn new(
        memberships: Vec<WorkspaceMembership>,
        descriptors: Vec<WorkspaceConnectionDescriptor>,
    ) -> Result<Self, WorkspaceDirectoryError> {
        for descriptor in &descriptors {
            validate_descriptor(descriptor, 0)?;
        }
        Ok(Self {
            memberships,
            descriptors,
        })
    }

    pub(crate) fn discover_sync(
        &self,
        user: &UserIdentityRef,
        organization_id: &str,
        now_unix_ms: u64,
    ) -> Result<Vec<WorkspaceConnectionDescriptor>, WorkspaceDirectoryError> {
        validate_identity(user)?;
        validate_scope_part(organization_id, "organization")?;

        self.descriptors
            .iter()
            .filter(|descriptor| {
                descriptor.organization_id == organization_id
                    && self.memberships.iter().any(|membership| {
                        same_user(&membership.user, user)
                            && membership.organization_id == organization_id
                            && membership.workspace_id == descriptor.workspace_id
                    })
            })
            .map(|descriptor| {
                validate_descriptor(descriptor, now_unix_ms)?;
                Ok(descriptor.clone())
            })
            .collect()
    }

    pub(crate) fn is_member_sync(
        &self,
        user: &UserIdentityRef,
        organization_id: &str,
        workspace_id: &str,
    ) -> Result<bool, WorkspaceDirectoryError> {
        validate_identity(user)?;
        validate_scope_part(organization_id, "organization")?;
        validate_scope_part(workspace_id, "workspace")?;
        Ok(self.memberships.iter().any(|membership| {
            same_user(&membership.user, user)
                && membership.organization_id == organization_id
                && membership.workspace_id == workspace_id
        }))
    }

    pub(crate) fn roles_for_member_sync(
        &self,
        user: &UserIdentityRef,
        organization_id: &str,
        workspace_id: &str,
    ) -> Result<Option<BTreeSet<String>>, WorkspaceDirectoryError> {
        validate_identity(user)?;
        validate_scope_part(organization_id, "organization")?;
        validate_scope_part(workspace_id, "workspace")?;
        Ok(self
            .memberships
            .iter()
            .find(|membership| {
                same_user(&membership.user, user)
                    && membership.organization_id == organization_id
                    && membership.workspace_id == workspace_id
            })
            .map(|membership| membership.roles.clone()))
    }
}

#[tonic::async_trait]
impl WorkspaceDirectory for InMemoryWorkspaceDirectory {
    async fn discover(
        &self,
        user: &UserIdentityRef,
        organization_id: &str,
        now_unix_ms: u64,
    ) -> Result<Vec<WorkspaceConnectionDescriptor>, WorkspaceDirectoryError> {
        self.discover_sync(user, organization_id, now_unix_ms)
    }

    async fn is_member(
        &self,
        user: &UserIdentityRef,
        organization_id: &str,
        workspace_id: &str,
    ) -> Result<bool, WorkspaceDirectoryError> {
        self.is_member_sync(user, organization_id, workspace_id)
    }

    async fn roles_for_member(
        &self,
        user: &UserIdentityRef,
        organization_id: &str,
        workspace_id: &str,
    ) -> Result<Option<BTreeSet<String>>, WorkspaceDirectoryError> {
        self.roles_for_member_sync(user, organization_id, workspace_id)
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
            .map_err(map_durable_directory_error)
    }

    async fn is_member(
        &self,
        user: &UserIdentityRef,
        organization_id: &str,
        workspace_id: &str,
    ) -> Result<bool, WorkspaceDirectoryError> {
        PostgresWorkspaceDirectory::is_member(self, user, organization_id, workspace_id)
            .await
            .map_err(map_durable_directory_error)
    }

    async fn roles_for_member(
        &self,
        user: &UserIdentityRef,
        organization_id: &str,
        workspace_id: &str,
    ) -> Result<Option<BTreeSet<String>>, WorkspaceDirectoryError> {
        PostgresWorkspaceDirectory::roles_for_member(self, user, organization_id, workspace_id)
            .await
            .map_err(map_durable_directory_error)
    }
}

fn map_durable_directory_error(error: DurableDirectoryError) -> WorkspaceDirectoryError {
    match error {
        DurableDirectoryError::InvalidIdentity | DurableDirectoryError::InvalidScope => {
            WorkspaceDirectoryError::Identity("invalid identity or scope".to_string())
        }
        DurableDirectoryError::InvalidDescriptor => {
            WorkspaceDirectoryError::Descriptor("stored descriptor is invalid".to_string())
        }
        DurableDirectoryError::Configuration | DurableDirectoryError::Storage => {
            WorkspaceDirectoryError::Storage("Directory backend unavailable".to_string())
        }
        DurableDirectoryError::NoMembershipMapping => {
            WorkspaceDirectoryError::Identity("identity has no organization mapping".to_string())
        }
        DurableDirectoryError::AmbiguousOrganizations => {
            WorkspaceDirectoryError::Identity("identity organization is ambiguous".to_string())
        }
        DurableDirectoryError::UnsupportedRole
        | DurableDirectoryError::AuditReasonRequired
        | DurableDirectoryError::MembershipNotFound => {
            WorkspaceDirectoryError::Identity("invalid Directory operation".to_string())
        }
    }
}

fn validate_identity(user: &UserIdentityRef) -> Result<(), WorkspaceDirectoryError> {
    if user.issuer.trim().is_empty()
        || user.subject.trim().is_empty()
        || user.issuer.len() > 2048
        || user.subject.len() > 2048
    {
        return Err(WorkspaceDirectoryError::Identity(
            "issuer and subject are required".to_string(),
        ));
    }
    Ok(())
}

fn validate_scope_part(value: &str, label: &str) -> Result<(), WorkspaceDirectoryError> {
    if value.trim().is_empty() || value.len() > 256 {
        return Err(WorkspaceDirectoryError::Identity(format!(
            "{label} scope is invalid"
        )));
    }
    Ok(())
}

/// Validate transport-neutral descriptor shape and expiry.
pub fn validate_descriptor(
    descriptor: &WorkspaceConnectionDescriptor,
    now_unix_ms: u64,
) -> Result<(), WorkspaceDirectoryError> {
    if descriptor.descriptor_version != "cyrene.workspace.connection.v1"
        || descriptor.workspace_id.is_empty()
        || descriptor.organization_id.is_empty()
        || descriptor.display_name.is_empty()
        || descriptor.candidates.is_empty()
    {
        return Err(WorkspaceDirectoryError::Descriptor(
            "version, identities, display name, and candidates are required".to_string(),
        ));
    }
    let expires_at = descriptor
        .expires_at
        .as_ref()
        .map(timestamp_ms)
        .unwrap_or_default();
    if expires_at <= now_unix_ms {
        return Err(WorkspaceDirectoryError::Descriptor(
            "descriptor is expired".to_string(),
        ));
    }
    let mut candidates = BTreeSet::new();
    let mut direct_priority = None;
    let mut relay_priority = None;
    for candidate in &descriptor.candidates {
        let mode = ConnectivityMode::try_from(candidate.mode).map_err(|_| {
            WorkspaceDirectoryError::Descriptor("unknown connectivity mode".to_string())
        })?;
        if mode == ConnectivityMode::Unspecified
            || candidate.provider_id.is_empty()
            || candidate.connection_uri.is_empty()
            || candidate.server_name.is_empty()
        {
            return Err(WorkspaceDirectoryError::Descriptor(
                "candidate mode, provider, URI, and server name are required".to_string(),
            ));
        }
        if matches!(mode, ConnectivityMode::LanDirect | ConnectivityMode::Relay)
            && !candidate.connection_uri.starts_with("https://")
        {
            return Err(WorkspaceDirectoryError::Descriptor(
                "LAN_DIRECT and RELAY candidates require HTTPS".to_string(),
            ));
        }
        if !candidates.insert((
            candidate.mode,
            candidate.provider_id.clone(),
            candidate.connection_uri.clone(),
        )) {
            return Err(WorkspaceDirectoryError::Descriptor(
                "duplicate connectivity candidate".to_string(),
            ));
        }
        match mode {
            ConnectivityMode::LanDirect => {
                direct_priority =
                    Some(direct_priority.map_or(candidate.priority, |priority: u32| {
                        priority.min(candidate.priority)
                    }));
            }
            ConnectivityMode::Relay => {
                relay_priority =
                    Some(relay_priority.map_or(candidate.priority, |priority: u32| {
                        priority.min(candidate.priority)
                    }));
            }
            _ => {}
        }
    }
    if direct_priority
        .zip(relay_priority)
        .is_some_and(|(direct, relay)| direct >= relay)
    {
        return Err(WorkspaceDirectoryError::Descriptor(
            "LAN_DIRECT must precede RELAY".to_string(),
        ));
    }
    Ok(())
}

fn timestamp_ms(value: &prost_types::Timestamp) -> u64 {
    u64::try_from(value.seconds)
        .unwrap_or_default()
        .saturating_mul(1000)
        .saturating_add(u64::try_from(value.nanos).unwrap_or_default() / 1_000_000)
}

#[cfg(test)]
mod tests {
    use cy_proto::workspace_v1::WorkspaceConnectionCandidate;

    use super::*;

    fn descriptor() -> WorkspaceConnectionDescriptor {
        WorkspaceConnectionDescriptor {
            descriptor_version: "cyrene.workspace.connection.v1".to_string(),
            workspace_id: "workspace-1".to_string(),
            organization_id: "organization-1".to_string(),
            display_name: "Workspace One".to_string(),
            candidates: [
                ConnectivityMode::Local,
                ConnectivityMode::LanDirect,
                ConnectivityMode::Direct,
                ConnectivityMode::Overlay,
                ConnectivityMode::Relay,
            ]
            .into_iter()
            .enumerate()
            .map(|(priority, mode)| WorkspaceConnectionCandidate {
                mode: mode as i32,
                provider_id: format!("provider-{priority}"),
                connection_uri: format!("https://candidate-{priority}.test"),
                server_name: format!("candidate-{priority}.test"),
                priority: priority as u32,
                routing_hint: Vec::new(),
            })
            .collect(),
            expires_at: Some(prost_types::Timestamp {
                seconds: 10,
                nanos: 0,
            }),
        }
    }

    #[test]
    fn descriptor_supports_all_contract_modes_without_relay_lock_in() {
        assert!(validate_descriptor(&descriptor(), 1).is_ok());
    }

    #[test]
    fn direct_candidate_must_precede_relay_candidate() {
        let mut invalid = descriptor();
        invalid.candidates[1].priority = 5;
        assert!(validate_descriptor(&invalid, 1).is_err());
    }

    #[test]
    fn directory_rpc_errors_preserve_availability_class_without_backend_details() {
        assert_eq!(
            WorkspaceDirectoryError::Storage("postgres://secret".into()).rpc_error(),
            (14, "WORKSPACE_DIRECTORY_UNAVAILABLE")
        );
        assert_eq!(
            WorkspaceDirectoryError::Identity("invalid input".into()).rpc_error(),
            (3, "WORKSPACE_DIRECTORY_INVALID_REQUEST")
        );
        assert_eq!(
            WorkspaceDirectoryError::Descriptor("bad row".into()).rpc_error(),
            (13, "WORKSPACE_DIRECTORY_DATA_INVALID")
        );
    }

    #[tokio::test]
    async fn discovery_is_membership_scoped_and_contains_no_product_state() {
        let user = UserIdentityRef {
            issuer: "https://identity.test".to_string(),
            subject: "user-1".to_string(),
        };
        let directory = InMemoryWorkspaceDirectory::new(
            vec![WorkspaceMembership {
                user: user.clone(),
                organization_id: "organization-1".to_string(),
                workspace_id: "workspace-1".to_string(),
                roles: BTreeSet::from(["member".to_string()]),
            }],
            vec![descriptor()],
        )
        .unwrap();
        let discovered = directory
            .discover(&user, "organization-1", 1)
            .await
            .unwrap();
        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].workspace_id, "workspace-1");
    }

    #[tokio::test]
    async fn membership_reads_keep_invalid_input_distinct_from_non_membership() {
        let directory = InMemoryWorkspaceDirectory::new(Vec::new(), Vec::new()).unwrap();
        let invalid_user = UserIdentityRef {
            issuer: String::new(),
            subject: String::new(),
        };

        assert!(matches!(
            directory
                .is_member(&invalid_user, "organization-1", "workspace-1")
                .await,
            Err(WorkspaceDirectoryError::Identity(_))
        ));
        assert!(matches!(
            directory
                .roles_for_member(&invalid_user, "organization-1", "workspace-1")
                .await,
            Err(WorkspaceDirectoryError::Identity(_))
        ));
        assert_eq!(
            directory
                .is_member(
                    &UserIdentityRef {
                        issuer: "https://identity.test".to_string(),
                        subject: "missing-user".to_string(),
                    },
                    "organization-1",
                    "workspace-1"
                )
                .await,
            Ok(false)
        );
    }
}
