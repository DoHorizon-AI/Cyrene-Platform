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

use crate::{RelayAuthenticator, SessionPrincipal, WorkspaceApi, WorkspaceDirectory};

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
            || !self
                .directory
                .is_member(&user, &claims.organization_id, &self.workspace_id)
        {
            return Err(Status::permission_denied("WORKSPACE_MEMBERSHIP_DENIED"));
        }
        tracing::info!(
            event.name = "platform.workspace.direct_request",
            workspace_id = %self.workspace_id,
            request_id = %api_request.request_id,
            message = "Workspace request used the private direct endpoint",
        );
        Ok(Response::new(self.api.handle(api_request).await))
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
    };
    use tonic::Code;

    use super::*;
    use crate::{
        DevelopmentSessionVerifier, InMemoryWorkspaceDirectory, RelaySessionClaims,
        WorkspaceMembership,
    };

    struct FixtureApi;

    #[tonic::async_trait]
    impl WorkspaceApi for FixtureApi {
        async fn handle(&self, request: WorkspaceApiRequest) -> WorkspaceApiResponse {
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
}
