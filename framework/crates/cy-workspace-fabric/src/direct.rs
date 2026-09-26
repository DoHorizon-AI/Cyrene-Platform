//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 direct.rs                                                       │
//! │  Module: cy_workspace_fabric::direct                                │
//! │  Role: Authenticated private Workspace API endpoint.                │
//! │                                                                     │
//! │  模块职责：认证后的私有 Workspace API 直连端点。                       │
//! └─────────────────────────────────────────────────────────────────────┘

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use cy_proto::workspace_v1::workspace_direct_service_server::WorkspaceDirectService;
use cy_proto::workspace_v1::{RelayParticipantRole, WorkspaceApiResponse, WorkspaceDirectRequest};
use tonic::{Request, Response, Status};

use crate::{
    RelayAuthenticator, SessionPrincipal, WorkspaceApi, WorkspaceCallerContext, WorkspaceDirectory,
};

/// Serve a Workspace API over a private, mutually authenticated TLS listener.
///
/// The caller must configure the gRPC server with a server certificate and a
/// client CA. Application authorization is checked for every request, including
/// after a previously valid short-lived session credential expires.
pub struct DirectWorkspaceServer {
    workspace_id: String,
    directory: Arc<dyn WorkspaceDirectory>,
    authenticator: Arc<dyn RelayAuthenticator>,
    api: Arc<dyn WorkspaceApi>,
}

impl DirectWorkspaceServer {
    pub fn new(
        workspace_id: impl Into<String>,
        directory: Arc<dyn WorkspaceDirectory>,
        authenticator: Arc<dyn RelayAuthenticator>,
        api: Arc<dyn WorkspaceApi>,
    ) -> Self {
        Self {
            workspace_id: workspace_id.into(),
            directory,
            authenticator,
            api,
        }
    }
}

#[tonic::async_trait]
impl WorkspaceDirectService for DirectWorkspaceServer {
    async fn execute(
        &self,
        request: Request<WorkspaceDirectRequest>,
    ) -> Result<Response<WorkspaceApiResponse>, Status> {
        let direct = request.into_inner();
        let hello = direct
            .frontend
            .ok_or_else(|| Status::unauthenticated("frontend session is required"))?;
        if hello.role != RelayParticipantRole::Frontend as i32 {
            return Err(Status::permission_denied("frontend role is required"));
        }
        let claims = self
            .authenticator
            .authenticate(&hello, now_unix_ms())
            .map_err(|error| Status::unauthenticated(error.to_string()))?;
        let SessionPrincipal::User(user) = claims.principal else {
            return Err(Status::permission_denied("user identity is required"));
        };
        let api_request = direct
            .request
            .ok_or_else(|| Status::invalid_argument("Workspace request is required"))?;
        if api_request.workspace_id != self.workspace_id
            || (!claims.workspace_id.is_empty() && claims.workspace_id != self.workspace_id)
        {
            return Err(Status::permission_denied("WORKSPACE_MEMBERSHIP_DENIED"));
        }
        let directory_roles = match self
            .directory
            .roles_for_member(&user, &claims.organization_id, &self.workspace_id)
            .await
        {
            Ok(Some(roles)) => roles,
            Ok(None) => {
                return Err(Status::permission_denied("WORKSPACE_MEMBERSHIP_DENIED"));
            }
            Err(error) => {
                let (code, message) = error.rpc_error();
                return Err(match code {
                    3 => Status::invalid_argument(message),
                    13 => Status::internal(message),
                    _ => Status::unavailable(message),
                });
            }
        };
        let caller = WorkspaceCallerContext::user_member(
            user,
            claims.organization_id,
            self.workspace_id.clone(),
            directory_roles,
        )
        .map_err(|_| Status::permission_denied("WORKSPACE_MEMBERSHIP_DENIED"))?;
        tracing::info!(
            event.name = "platform.workspace.direct_request",
            workspace_id = %self.workspace_id,
            request_id = %api_request.request_id,
            message = "Workspace request used the private direct endpoint",
        );
        Ok(Response::new(
            crate::api::dispatch_authenticated_workspace_request(
                self.api.as_ref(),
                api_request,
                caller,
            )
            .await,
        ))
    }
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use cy_proto::workspace_v1::{
        RelayHello, RelayParticipantRole, UserIdentityRef, WorkspaceApiRequest,
        WorkspaceConnectionDescriptor,
    };
    use tonic::Code;

    use super::*;
    use crate::{
        DevelopmentSessionVerifier, InMemoryWorkspaceDirectory, RelaySessionClaims,
        WorkspaceCallerPrincipal, WorkspaceDirectoryError, WorkspaceMembership,
    };

    struct UnavailableDirectory;

    #[tonic::async_trait]
    impl WorkspaceDirectory for UnavailableDirectory {
        async fn discover(
            &self,
            _user: &UserIdentityRef,
            _organization_id: &str,
            _now_unix_ms: u64,
        ) -> Result<Vec<WorkspaceConnectionDescriptor>, WorkspaceDirectoryError> {
            Err(WorkspaceDirectoryError::Storage(
                "private database diagnostics".into(),
            ))
        }

        async fn is_member(
            &self,
            _user: &UserIdentityRef,
            _organization_id: &str,
            _workspace_id: &str,
        ) -> Result<bool, WorkspaceDirectoryError> {
            Err(WorkspaceDirectoryError::Storage(
                "private database diagnostics".into(),
            ))
        }

        async fn roles_for_member(
            &self,
            _user: &UserIdentityRef,
            _organization_id: &str,
            _workspace_id: &str,
        ) -> Result<Option<BTreeSet<String>>, WorkspaceDirectoryError> {
            Err(WorkspaceDirectoryError::Storage(
                "private database diagnostics".into(),
            ))
        }
    }

    struct FixtureApi;

    #[tonic::async_trait]
    impl WorkspaceApi for FixtureApi {
        async fn handle_authenticated(
            &self,
            request: WorkspaceApiRequest,
            caller: WorkspaceCallerContext,
        ) -> WorkspaceApiResponse {
            assert_eq!(caller.workspace_id(), "workspace-1");
            assert!(caller.is_member());
            assert!(matches!(
                caller.principal(),
                WorkspaceCallerPrincipal::User(_)
            ));
            WorkspaceApiResponse {
                request_id: request.request_id,
                outcome: None,
            }
        }
    }

    fn user() -> UserIdentityRef {
        UserIdentityRef {
            issuer: "https://identity.test".to_string(),
            subject: "user-1".to_string(),
        }
    }

    fn direct_request() -> Request<WorkspaceDirectRequest> {
        Request::new(WorkspaceDirectRequest {
            frontend: Some(RelayHello {
                role: RelayParticipantRole::Frontend as i32,
                session_credential: "frontend-session".to_string(),
                user: Some(user()),
                organization_id: "organization-1".to_string(),
                workspace_id: String::new(),
                device: None,
            }),
            request: Some(WorkspaceApiRequest {
                request_id: "request-1".to_string(),
                workspace_id: "workspace-1".to_string(),
                traceparent: String::new(),
                request: None,
            }),
        })
    }

    fn server(member: bool, expires_at_unix_ms: u64) -> DirectWorkspaceServer {
        let memberships = if member {
            vec![WorkspaceMembership {
                user: user(),
                organization_id: "organization-1".to_string(),
                workspace_id: "workspace-1".to_string(),
                roles: BTreeSet::new(),
            }]
        } else {
            Vec::new()
        };
        let directory = Arc::new(InMemoryWorkspaceDirectory::new(memberships, Vec::new()).unwrap());
        let authenticator = Arc::new(DevelopmentSessionVerifier::new([(
            "frontend-session".to_string(),
            RelaySessionClaims {
                principal: SessionPrincipal::User(user()),
                organization_id: "organization-1".to_string(),
                workspace_id: String::new(),
                expires_at_unix_ms,
            },
        )]));
        DirectWorkspaceServer::new(
            "workspace-1",
            directory,
            authenticator,
            Arc::new(FixtureApi),
        )
    }

    #[tokio::test]
    async fn direct_endpoint_requires_live_session_and_workspace_membership() {
        let live = now_unix_ms().saturating_add(60_000);
        let allowed = server(true, live).execute(direct_request()).await.unwrap();
        assert_eq!(allowed.into_inner().request_id, "request-1");

        let denied = server(false, live).execute(direct_request()).await;
        assert_eq!(denied.unwrap_err().code(), Code::PermissionDenied);

        let expired = server(true, 1).execute(direct_request()).await;
        assert_eq!(expired.unwrap_err().code(), Code::Unauthenticated);
    }

    #[tokio::test]
    async fn direct_endpoint_rejects_a_forged_user_claim() {
        let mut request = direct_request();
        request
            .get_mut()
            .frontend
            .as_mut()
            .unwrap()
            .user
            .as_mut()
            .unwrap()
            .subject = "user-2".to_string();

        let denied = server(true, now_unix_ms().saturating_add(60_000))
            .execute(request)
            .await;
        assert_eq!(denied.unwrap_err().code(), Code::Unauthenticated);
    }

    #[tokio::test]
    async fn direct_endpoint_rejects_workspace_connector_identity() {
        let mut request = direct_request();
        request.get_mut().frontend.as_mut().unwrap().role =
            RelayParticipantRole::WorkspaceConnector as i32;

        let rejected = server(true, now_unix_ms().saturating_add(60_000))
            .execute(request)
            .await;
        assert_eq!(rejected.unwrap_err().code(), Code::PermissionDenied);
    }

    #[tokio::test]
    async fn direct_directory_outage_is_unavailable_without_exposing_diagnostics() {
        let authenticator = Arc::new(DevelopmentSessionVerifier::new([(
            "frontend-session".to_string(),
            RelaySessionClaims {
                principal: SessionPrincipal::User(user()),
                organization_id: "organization-1".to_string(),
                workspace_id: String::new(),
                expires_at_unix_ms: now_unix_ms().saturating_add(60_000),
            },
        )]));
        let server = DirectWorkspaceServer::new(
            "workspace-1",
            Arc::new(UnavailableDirectory),
            authenticator,
            Arc::new(FixtureApi),
        );

        let error = server.execute(direct_request()).await.unwrap_err();
        assert_eq!(error.code(), Code::Unavailable);
        assert_eq!(error.message(), "WORKSPACE_DIRECTORY_UNAVAILABLE");
        assert!(!error.message().contains("private database diagnostics"));
    }
}
