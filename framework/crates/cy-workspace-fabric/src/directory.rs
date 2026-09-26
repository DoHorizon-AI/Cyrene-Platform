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

/// Account/Directory port. Implementations own membership and descriptors only.
pub trait WorkspaceDirectory: Send + Sync {
    fn discover(
        &self,
        user: &UserIdentityRef,
        organization_id: &str,
        now_unix_ms: u64,
    ) -> Result<Vec<WorkspaceConnectionDescriptor>, WorkspaceDirectoryError>;

    fn is_member(&self, user: &UserIdentityRef, organization_id: &str, workspace_id: &str) -> bool;
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
}

impl WorkspaceDirectory for InMemoryWorkspaceDirectory {
    fn discover(
        &self,
        user: &UserIdentityRef,
        organization_id: &str,
        now_unix_ms: u64,
    ) -> Result<Vec<WorkspaceConnectionDescriptor>, WorkspaceDirectoryError> {
        if user.issuer.is_empty() || user.subject.is_empty() || organization_id.is_empty() {
            return Err(WorkspaceDirectoryError::Identity(
                "issuer, subject, and organization are required".to_string(),
            ));
        }
        self.descriptors
            .iter()
            .filter(|descriptor| {
                descriptor.organization_id == organization_id
                    && self.is_member(user, organization_id, &descriptor.workspace_id)
            })
            .map(|descriptor| {
                validate_descriptor(descriptor, now_unix_ms)?;
                Ok(descriptor.clone())
            })
            .collect()
    }

    fn is_member(&self, user: &UserIdentityRef, organization_id: &str, workspace_id: &str) -> bool {
        self.memberships.iter().any(|membership| {
            same_user(&membership.user, user)
                && membership.organization_id == organization_id
                && membership.workspace_id == workspace_id
        })
    }
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
    fn discovery_is_membership_scoped_and_contains_no_product_state() {
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
        let discovered = directory.discover(&user, "organization-1", 1).unwrap();
        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].workspace_id, "workspace-1");
    }
}
