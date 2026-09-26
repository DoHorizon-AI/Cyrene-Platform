//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 caller.rs                                                       │
//! │  Module: cy_workspace_fabric::caller                               │
//! │  Role: Verified Workspace caller context and authorization checks. │
//! │                                                                     │
//! │  模块职责：可信 Workspace 调用者上下文与授权检查。                     │
//! └─────────────────────────────────────────────────────────────────────┘

use std::collections::BTreeSet;

use cy_proto::core_v1::WorkloadIdentity;
use cy_proto::workspace_v1::{
    relay_forwarded_request, DeviceEnrollmentRef, RelayForwardedRequest, UserIdentityRef,
};
use thiserror::Error;

/// Directory-derived role that allows a user to read Workspace projections.
pub const WORKSPACE_MEMBER_ROLE: &str = "workspace.member";

/// Directory-derived role for the trusted Navigator Product service writer.
pub const NAVIGATOR_SERVICE_WRITER_ROLE: &str = "navigator.service-writer";

/// A typed identity established by a trusted authentication boundary.
#[derive(Debug, Clone, PartialEq)]
pub enum WorkspaceCallerPrincipal {
    /// A user identity verified against the session credential.
    User(UserIdentityRef),
    /// A Workspace device identity verified by the device authorization boundary.
    Device(WorkspaceDeviceIdentity),
    /// A workload identity verified by the workload authorization boundary.
    Workload(WorkloadIdentity),
}

/// Device identity used as a Workspace API caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceDeviceIdentity {
    /// Stable Directory device identifier.
    pub device_id: String,
}

/// Server-derived caller identity, scope, and Directory roles.
///
/// Instances are created by the authentication and Directory adapters. They
/// must never be populated from fields on a `WorkspaceApiRequest`.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceCallerContext {
    principal: WorkspaceCallerPrincipal,
    organization_id: String,
    workspace_id: String,
    roles: BTreeSet<String>,
}

impl WorkspaceCallerContext {
    /// Returns the authenticated principal.
    pub fn principal(&self) -> &WorkspaceCallerPrincipal {
        &self.principal
    }

    /// Returns the organization scope established by authentication.
    pub fn organization_id(&self) -> &str {
        &self.organization_id
    }

    /// Returns the Workspace scope established by authentication and Directory.
    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }

    /// Returns the roles resolved by the trusted Directory boundary.
    pub fn roles(&self) -> &BTreeSet<String> {
        &self.roles
    }

    /// Returns whether this context carries the Directory-derived member role.
    pub fn is_member(&self) -> bool {
        self.roles.contains(WORKSPACE_MEMBER_ROLE)
    }

    /// Creates a member context after an authoritative Directory lookup succeeds.
    pub(crate) fn user_member(
        user: UserIdentityRef,
        organization_id: impl Into<String>,
        workspace_id: impl Into<String>,
        mut directory_roles: BTreeSet<String>,
    ) -> Result<Self, WorkspaceCallerContextError> {
        directory_roles.insert(WORKSPACE_MEMBER_ROLE.to_string());
        Self::from_verified(
            WorkspaceCallerPrincipal::User(user),
            organization_id,
            workspace_id,
            directory_roles,
        )
    }

    /// Creates a context from identity and scope already verified by a trusted boundary.
    pub(crate) fn from_verified(
        principal: WorkspaceCallerPrincipal,
        organization_id: impl Into<String>,
        workspace_id: impl Into<String>,
        roles: BTreeSet<String>,
    ) -> Result<Self, WorkspaceCallerContextError> {
        let organization_id = organization_id.into();
        let workspace_id = workspace_id.into();
        if organization_id.trim().is_empty()
            || workspace_id.trim().is_empty()
            || roles.iter().any(|role| role.trim().is_empty())
            || !principal_matches_scope(&principal, &organization_id, &workspace_id)
        {
            return Err(WorkspaceCallerContextError::Invalid);
        }
        Ok(Self {
            principal,
            organization_id,
            workspace_id,
            roles,
        })
    }

    /// Converts this server-derived context to fields on the Relay-only wrapper.
    pub(crate) fn to_relay_fields(
        &self,
    ) -> (
        Option<relay_forwarded_request::CallerPrincipal>,
        String,
        String,
        Vec<String>,
    ) {
        let principal = match &self.principal {
            WorkspaceCallerPrincipal::User(user) => {
                relay_forwarded_request::CallerPrincipal::User(user.clone())
            }
            WorkspaceCallerPrincipal::Device(device) => {
                relay_forwarded_request::CallerPrincipal::Device(DeviceEnrollmentRef {
                    device_id: device.device_id.clone(),
                    workspace_id: self.workspace_id.clone(),
                    enrollment_state: String::new(),
                })
            }
            WorkspaceCallerPrincipal::Workload(workload) => {
                relay_forwarded_request::CallerPrincipal::Workload(workload.clone())
            }
        };
        (
            Some(principal),
            self.organization_id.clone(),
            self.workspace_id.clone(),
            self.roles.iter().cloned().collect(),
        )
    }

    /// Decodes a caller context from a frame received on an authenticated Relay stream.
    pub(crate) fn from_relay_forwarded(
        forwarded: &RelayForwardedRequest,
    ) -> Result<Self, WorkspaceCallerContextError> {
        let principal = match forwarded.caller_principal.as_ref() {
            Some(relay_forwarded_request::CallerPrincipal::User(user)) => {
                WorkspaceCallerPrincipal::User(user.clone())
            }
            Some(relay_forwarded_request::CallerPrincipal::Device(device)) => {
                if device.workspace_id != forwarded.caller_workspace_id
                    || !device.enrollment_state.is_empty()
                {
                    return Err(WorkspaceCallerContextError::Invalid);
                }
                WorkspaceCallerPrincipal::Device(WorkspaceDeviceIdentity {
                    device_id: device.device_id.clone(),
                })
            }
            Some(relay_forwarded_request::CallerPrincipal::Workload(workload)) => {
                WorkspaceCallerPrincipal::Workload(workload.clone())
            }
            None => return Err(WorkspaceCallerContextError::Invalid),
        };
        Self::from_verified(
            principal,
            forwarded.caller_organization_id.clone(),
            forwarded.caller_workspace_id.clone(),
            forwarded.caller_roles.iter().cloned().collect(),
        )
    }

    /// Allows a member to read only the Workspace established by this context.
    pub(crate) fn authorize_workspace_read(
        &self,
        request_workspace_id: &str,
    ) -> Result<(), WorkspaceAuthorizationError> {
        self.authorize_scope(request_workspace_id)?;
        if !matches!(self.principal, WorkspaceCallerPrincipal::User(_)) || !self.is_member() {
            return Err(WorkspaceAuthorizationError::MemberRequired);
        }
        Ok(())
    }

    /// Validates the scope before an operation that still has no role policy.
    pub(crate) fn authorize_workspace_scope(
        &self,
        request_workspace_id: &str,
    ) -> Result<(), WorkspaceAuthorizationError> {
        self.authorize_scope(request_workspace_id)
    }

    /// Denies commands until a Workspace command role policy is defined.
    pub(crate) fn authorize_workspace_command(
        &self,
        request_workspace_id: &str,
    ) -> Result<(), WorkspaceAuthorizationError> {
        self.authorize_scope(request_workspace_id)?;
        Err(WorkspaceAuthorizationError::CommandPolicyUnconfigured)
    }

    fn authorize_scope(
        &self,
        request_workspace_id: &str,
    ) -> Result<(), WorkspaceAuthorizationError> {
        if request_workspace_id != self.workspace_id {
            return Err(WorkspaceAuthorizationError::WorkspaceMismatch);
        }
        if self.organization_id.trim().is_empty() {
            return Err(WorkspaceAuthorizationError::Unauthenticated);
        }
        Ok(())
    }
}

fn principal_matches_scope(
    principal: &WorkspaceCallerPrincipal,
    organization_id: &str,
    workspace_id: &str,
) -> bool {
    match principal {
        WorkspaceCallerPrincipal::User(user) => {
            !user.issuer.trim().is_empty() && !user.subject.trim().is_empty()
        }
        WorkspaceCallerPrincipal::Device(device) => !device.device_id.trim().is_empty(),
        WorkspaceCallerPrincipal::Workload(workload) => {
            workload
                .identity
                .as_ref()
                .is_some_and(|identity| !identity.id.trim().is_empty() && identity.generation > 0)
                && workload.scope.as_ref().is_some_and(|scope| {
                    scope.organization_id == organization_id && scope.workspace_id == workspace_id
                })
        }
    }
}

/// Invalid or unavailable caller context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("WORKSPACE_CALLER_CONTEXT_INVALID")]
pub enum WorkspaceCallerContextError {
    /// The identity, organization, Workspace, or role set is incomplete.
    Invalid,
}

/// Authorization failures for Workspace API operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum WorkspaceAuthorizationError {
    /// No verified caller context is available.
    #[error("WORKSPACE_CALLER_UNAUTHENTICATED")]
    Unauthenticated,
    /// The request targets a Workspace other than the authenticated scope.
    #[error("WORKSPACE_AUTHORITY_MISMATCH")]
    WorkspaceMismatch,
    /// Workspace reads require a Directory-derived member user.
    #[error("WORKSPACE_MEMBERSHIP_DENIED")]
    MemberRequired,
    /// No command role policy is configured yet.
    #[error("WORKSPACE_COMMAND_POLICY_UNCONFIGURED")]
    CommandPolicyUnconfigured,
}
