//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 auth.rs                                                         │
//! │  Module: cy_workspace_fabric::auth                                  │
//! │  Role: Short-lived relay session verification seam.                 │
//! │                                                                     │
//! │  模块职责：短期 Relay 会话凭证校验边界。                                │
//! └─────────────────────────────────────────────────────────────────────┘

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use cy_proto::workspace_v1::{RelayHello, RelayParticipantRole, UserIdentityRef};
use thiserror::Error;

/// Principal kind carried by one short-lived relay session.
///
/// `WorkspaceDevice` is the identity of an inbound connector as seen by the
/// Relay. It must not be treated as the Relay service identity when a
/// Workspace receiver decides whether to trust forwarded caller roles.
///
/// `WorkspaceDevice` 表示 Relay 所见的入站 Connector 身份，不能作为 Workspace 接收端信任转发 caller role 时的 Relay 服务身份。
#[derive(Debug, Clone, PartialEq)]
pub enum SessionPrincipal {
    User(UserIdentityRef),
    WorkspaceDevice {
        workspace_id: String,
        device_id: String,
    },
}

/// Verified claims. The opaque credential itself is deliberately absent.
#[derive(Debug, Clone, PartialEq)]
pub struct RelaySessionClaims {
    pub principal: SessionPrincipal,
    pub organization_id: String,
    /// Optional frontend scope; empty means membership-scoped discovery.
    pub workspace_id: String,
    pub expires_at_unix_ms: u64,
}

/// Authentication failure before a relay participant can register.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum RelayAuthenticationError {
    #[error("RELAY_CREDENTIAL_INVALID")]
    InvalidCredential,
    #[error("RELAY_CREDENTIAL_EXPIRED")]
    Expired,
    #[error("RELAY_PRINCIPAL_MISMATCH")]
    PrincipalMismatch,
}

/// Replaceable verifier for Frontend OIDC or development session tokens.
pub trait RelayAuthenticator: Send + Sync {
    fn authenticate(
        &self,
        hello: &RelayHello,
        now_unix_ms: u64,
    ) -> Result<RelaySessionClaims, RelayAuthenticationError>;
}

/// Development-only verifier backed by opaque, pre-registered session tokens.
///
/// Tokens are never returned from this component and should be short-lived.
#[derive(Debug, Clone, Default)]
pub struct DevelopmentSessionVerifier {
    sessions: Arc<Mutex<BTreeMap<String, RelaySessionClaims>>>,
}

impl DevelopmentSessionVerifier {
    pub fn new(entries: impl IntoIterator<Item = (String, RelaySessionClaims)>) -> Self {
        Self {
            sessions: Arc::new(Mutex::new(entries.into_iter().collect())),
        }
    }
}

impl RelayAuthenticator for DevelopmentSessionVerifier {
    fn authenticate(
        &self,
        hello: &RelayHello,
        now_unix_ms: u64,
    ) -> Result<RelaySessionClaims, RelayAuthenticationError> {
        let claims = self
            .sessions
            .lock()
            .map_err(|_| RelayAuthenticationError::InvalidCredential)?
            .get(&hello.session_credential)
            .cloned()
            .ok_or(RelayAuthenticationError::InvalidCredential)?;
        if claims.expires_at_unix_ms <= now_unix_ms {
            return Err(RelayAuthenticationError::Expired);
        }
        if claims.organization_id != hello.organization_id {
            return Err(RelayAuthenticationError::PrincipalMismatch);
        }
        let role = RelayParticipantRole::try_from(hello.role)
            .map_err(|_| RelayAuthenticationError::PrincipalMismatch)?;
        let matches = match (&claims.principal, role) {
            (SessionPrincipal::User(expected), RelayParticipantRole::Frontend) => {
                hello.workspace_id.is_empty()
                    && hello.device.is_none()
                    && hello
                        .user
                        .as_ref()
                        .is_some_and(|actual| same_user(expected, actual))
            }
            _ => false,
        };
        matches
            .then_some(claims)
            .ok_or(RelayAuthenticationError::PrincipalMismatch)
    }
}

pub(crate) fn same_user(left: &UserIdentityRef, right: &UserIdentityRef) -> bool {
    left.issuer == right.issuer && left.subject == right.subject
}

#[cfg(test)]
mod tests {
    use cy_proto::workspace_v1::{DeviceEnrollmentRef, RelayParticipantRole};

    use super::*;

    #[test]
    fn development_user_verifier_does_not_authenticate_workspace_connectors() {
        let claims = RelaySessionClaims {
            principal: SessionPrincipal::User(UserIdentityRef {
                issuer: "https://identity.test".to_string(),
                subject: "user-1".to_string(),
            }),
            organization_id: "organization-1".to_string(),
            workspace_id: "workspace-1".to_string(),
            expires_at_unix_ms: 20,
        };
        let verifier = DevelopmentSessionVerifier::new([("user-token".to_string(), claims)]);
        let workspace_hello = RelayHello {
            role: RelayParticipantRole::WorkspaceConnector as i32,
            session_credential: "user-token".to_string(),
            user: None,
            organization_id: "organization-1".to_string(),
            workspace_id: "workspace-1".to_string(),
            device: Some(DeviceEnrollmentRef {
                device_id: "device-1".to_string(),
                workspace_id: "workspace-1".to_string(),
                enrollment_state: "approved".to_string(),
            }),
        };
        assert_eq!(
            verifier.authenticate(&workspace_hello, 1),
            Err(RelayAuthenticationError::PrincipalMismatch)
        );
    }

    #[test]
    fn organization_user_authenticates_before_workspace_discovery() {
        let user = UserIdentityRef {
            issuer: "https://identity.test".to_string(),
            subject: "user-1".to_string(),
        };
        let claims = RelaySessionClaims {
            principal: SessionPrincipal::User(user.clone()),
            organization_id: "organization-1".to_string(),
            workspace_id: String::new(),
            expires_at_unix_ms: 20,
        };
        let verifier = DevelopmentSessionVerifier::new([(
            "organization-user-token".to_string(),
            claims.clone(),
        )]);
        let frontend_hello = RelayHello {
            role: RelayParticipantRole::Frontend as i32,
            session_credential: "organization-user-token".to_string(),
            user: Some(user),
            organization_id: "organization-1".to_string(),
            workspace_id: String::new(),
            device: None,
        };
        assert_eq!(verifier.authenticate(&frontend_hello, 1), Ok(claims));
    }
}
