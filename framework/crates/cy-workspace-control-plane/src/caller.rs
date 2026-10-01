//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 caller.rs                                                       │
//! │  Module: cy_workspace_control_plane::caller                               │
//! │  Role: Verified Workspace caller context and authorization checks. │
//! │                                                                     │
//! │  模块职责：可信 Workspace 调用者上下文与授权检查。                     │
//! └─────────────────────────────────────────────────────────────────────┘

use std::collections::BTreeSet;
use std::time::{SystemTime, UNIX_EPOCH};

use cy_proto::core_v1::WorkloadIdentity;
use cy_proto::workspace_v1::{
    relay_forwarded_request, DeviceEnrollmentRef, RelayForwardedRequest, UserIdentityRef,
    WorkspaceApiRequest,
};
use thiserror::Error;

use crate::auth::{RelaySessionClaims, SessionPrincipal};
use crate::directory::WorkspaceDirectory;
use crate::web_identity::VerifiedWebPrincipal;

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

    /// Creates a user caller only after rechecking the verified principal's current Directory membership.
    ///
    /// Identity and organization are derived from the immutable verifier result. The Directory
    /// supplies the role set and must report current membership for this exact workspace. Caller
    /// roles, actor fields, and organization scope cannot be supplied separately by an HTTP request.
    ///
    /// 只有重新核验已验证 principal 的当前 Directory membership 后才创建 user caller。
    /// issuer、subject 与 organization 来自不可变 verifier 结果，roles 来自该 Workspace 的 Directory 查询。
    pub async fn from_verified_web_member(
        principal: &VerifiedWebPrincipal,
        workspace_id: impl Into<String>,
        directory: &dyn WorkspaceDirectory,
    ) -> Result<Self, WorkspaceCallerContextError> {
        Self::from_verified_web_identity(
            principal.identity().clone(),
            principal.organization_id().to_owned(),
            principal.expires_at_unix_ms(),
            workspace_id.into(),
            directory,
        )
        .await
    }

    /// Resolves a caller from authenticated Relay claims and current Directory membership.
    ///
    /// The caller principal and organization come from the Relay authenticator's claims; roles
    /// are loaded from the Directory for the exact requested Workspace. This never accepts caller
    /// roles or organization scope from a Workspace API request.
    pub async fn from_relay_session_member(
        claims: &RelaySessionClaims,
        workspace_id: impl Into<String>,
        directory: &dyn WorkspaceDirectory,
    ) -> Result<Self, WorkspaceCallerContextError> {
        let workspace_id = workspace_id.into();
        let SessionPrincipal::User(identity) = &claims.principal else {
            return Err(WorkspaceCallerContextError::Invalid);
        };
        let now_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|duration| i64::try_from(duration.as_millis()).ok())
            .ok_or(WorkspaceCallerContextError::Invalid)?;
        if i64::try_from(claims.expires_at_unix_ms).unwrap_or(i64::MAX) <= now_unix_ms {
            return Err(WorkspaceCallerContextError::PrincipalExpired);
        }
        if claims.organization_id.trim().is_empty()
            || workspace_id.trim().is_empty()
            || (!claims.workspace_id.is_empty() && claims.workspace_id != workspace_id)
        {
            return Err(WorkspaceCallerContextError::Invalid);
        }
        let roles = directory
            .roles_for_member(identity, &claims.organization_id, &workspace_id)
            .await
            .map_err(|_| WorkspaceCallerContextError::DirectoryUnavailable)?
            .ok_or(WorkspaceCallerContextError::NotMember)?;
        Self::user_member(
            identity.clone(),
            claims.organization_id.clone(),
            workspace_id,
            roles,
        )
    }

    async fn from_verified_web_identity(
        identity: UserIdentityRef,
        organization_id: String,
        expires_at_unix_ms: i64,
        workspace_id: String,
        directory: &dyn WorkspaceDirectory,
    ) -> Result<Self, WorkspaceCallerContextError> {
        let now_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|duration| i64::try_from(duration.as_millis()).ok())
            .ok_or(WorkspaceCallerContextError::Invalid)?;
        if expires_at_unix_ms <= now_unix_ms {
            return Err(WorkspaceCallerContextError::PrincipalExpired);
        }
        if identity.issuer.trim().is_empty()
            || identity.subject.trim().is_empty()
            || organization_id.trim().is_empty()
            || workspace_id.trim().is_empty()
        {
            return Err(WorkspaceCallerContextError::Invalid);
        }
        let directory_roles = directory
            .roles_for_member(&identity, &organization_id, &workspace_id)
            .await
            .map_err(|_| WorkspaceCallerContextError::DirectoryUnavailable)?
            .ok_or(WorkspaceCallerContextError::NotMember)?;
        Self::user_member(identity, organization_id, workspace_id, directory_roles)
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

    /// Builds the Relay forwarding envelope from this Directory-derived caller context.
    ///
    /// This only projects an already verified identity and scope. It does not create caller
    /// contexts or authorize a Workspace operation; the receiving Workspace control plane still
    /// applies its operation policy.
    pub fn relay_forwarded_request(
        &self,
        frontend_session_id: impl Into<String>,
        request: WorkspaceApiRequest,
    ) -> Result<RelayForwardedRequest, WorkspaceAuthorizationError> {
        self.authorize_scope(&request.workspace_id)?;
        let frontend_session_id = frontend_session_id.into();
        if frontend_session_id.trim().is_empty() {
            return Err(WorkspaceAuthorizationError::Unauthenticated);
        }
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
        Ok(RelayForwardedRequest {
            frontend_session_id,
            request: Some(request),
            caller_principal: Some(principal),
            caller_organization_id: self.organization_id.clone(),
            caller_workspace_id: self.workspace_id.clone(),
            caller_roles: self.roles.iter().cloned().collect(),
        })
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
pub enum WorkspaceCallerContextError {
    /// The identity, organization, Workspace, or role set is incomplete.
    #[error("WORKSPACE_CALLER_CONTEXT_INVALID")]
    Invalid,
    /// The verified access token has expired.
    #[error("WORKSPACE_CALLER_PRINCIPAL_EXPIRED")]
    PrincipalExpired,
    /// The verified user is not a current member of the requested Workspace.
    #[error("WORKSPACE_CALLER_NOT_MEMBER")]
    NotMember,
    /// The authoritative membership Directory could not answer.
    #[error("WORKSPACE_CALLER_DIRECTORY_UNAVAILABLE")]
    DirectoryUnavailable,
}

impl WorkspaceCallerContextError {
    /// Maps trusted-context construction failures to the BFF's closed public status categories.
    pub fn http_status(&self) -> u16 {
        match self {
            Self::Invalid | Self::PrincipalExpired => 401,
            Self::NotMember => 404,
            Self::DirectoryUnavailable => 503,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::WorkspaceDirectoryError;
    use cy_proto::workspace_v1::WorkspaceConnectionDescriptor;

    enum DirectoryReply {
        Member(BTreeSet<String>),
        NotMember,
        Unavailable,
    }

    struct FixtureDirectory {
        reply: DirectoryReply,
        lookups: AtomicUsize,
    }

    #[tonic::async_trait]
    impl WorkspaceDirectory for FixtureDirectory {
        async fn discover(
            &self,
            _user: &UserIdentityRef,
            _organization_id: &str,
            _now_unix_ms: u64,
        ) -> Result<Vec<WorkspaceConnectionDescriptor>, WorkspaceDirectoryError> {
            Ok(Vec::new())
        }

        async fn is_member(
            &self,
            _user: &UserIdentityRef,
            _organization_id: &str,
            _workspace_id: &str,
        ) -> Result<bool, WorkspaceDirectoryError> {
            Ok(matches!(self.reply, DirectoryReply::Member(_)))
        }

        async fn roles_for_member(
            &self,
            _user: &UserIdentityRef,
            _organization_id: &str,
            _workspace_id: &str,
        ) -> Result<Option<BTreeSet<String>>, WorkspaceDirectoryError> {
            self.lookups.fetch_add(1, Ordering::SeqCst);
            match &self.reply {
                DirectoryReply::Member(roles) => Ok(Some(roles.clone())),
                DirectoryReply::NotMember => Ok(None),
                DirectoryReply::Unavailable => Err(WorkspaceDirectoryError::Storage(
                    "fixture outage".to_string(),
                )),
            }
        }
    }

    fn identity() -> UserIdentityRef {
        UserIdentityRef {
            issuer: "https://issuer.example".to_string(),
            subject: "subject-1".to_string(),
        }
    }

    #[tokio::test]
    async fn expired_web_identity_is_rejected_before_directory_lookup() {
        let directory = FixtureDirectory {
            reply: DirectoryReply::Member(BTreeSet::new()),
            lookups: AtomicUsize::new(0),
        };
        let error = WorkspaceCallerContext::from_verified_web_identity(
            identity(),
            "org-1".to_string(),
            1,
            "ws-1".to_string(),
            &directory,
        )
        .await
        .expect_err("expired identities must fail closed");

        assert_eq!(error.http_status(), 401);
        assert_eq!(directory.lookups.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn caller_roles_are_reloaded_from_directory_for_exact_scope() {
        let mut roles = BTreeSet::new();
        roles.insert("workspace.operator".to_string());
        let directory = FixtureDirectory {
            reply: DirectoryReply::Member(roles),
            lookups: AtomicUsize::new(0),
        };
        let caller = WorkspaceCallerContext::from_verified_web_identity(
            identity(),
            "org-1".to_string(),
            i64::MAX,
            "ws-1".to_string(),
            &directory,
        )
        .await
        .expect("current Directory membership should create a caller");

        assert_eq!(caller.organization_id(), "org-1");
        assert_eq!(caller.workspace_id(), "ws-1");
        assert!(caller.is_member());
        assert!(caller.roles().contains("workspace.operator"));
        assert_eq!(directory.lookups.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn workspace_directory_outage_is_not_member_or_success() {
        let directory = FixtureDirectory {
            reply: DirectoryReply::Unavailable,
            lookups: AtomicUsize::new(0),
        };
        let error = WorkspaceCallerContext::from_verified_web_identity(
            identity(),
            "org-1".to_string(),
            i64::MAX,
            "ws-1".to_string(),
            &directory,
        )
        .await
        .expect_err("Directory outage must fail closed");

        assert_eq!(error.http_status(), 503);
    }

    #[tokio::test]
    async fn absent_membership_is_hidden_as_not_found() {
        let directory = FixtureDirectory {
            reply: DirectoryReply::NotMember,
            lookups: AtomicUsize::new(0),
        };
        let error = WorkspaceCallerContext::from_verified_web_identity(
            identity(),
            "org-1".to_string(),
            i64::MAX,
            "ws-1".to_string(),
            &directory,
        )
        .await
        .expect_err("absent membership must fail closed");

        assert_eq!(error.http_status(), 404);
    }
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
