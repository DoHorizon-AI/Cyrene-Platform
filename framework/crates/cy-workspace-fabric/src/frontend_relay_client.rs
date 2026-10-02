//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 frontend_relay_client.rs                                       │
//! │  Module: cy_workspace_fabric::frontend_relay_client                │
//! │  Role: Bind a verified Web user to one outbound Relay session.     │
//! │                                                                     │
//! │  模块职责：将已验证 Web 用户绑定到单个出站 Relay 会话。                 │
//! └─────────────────────────────────────────────────────────────────────┘

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cy_proto::workspace_v1::{
    RelayHello, RelayParticipantRole, UserIdentityRef, WorkspaceApiRequest, WorkspaceApiResponse,
    WorkspaceConnectionCandidate, WorkspaceConnectionDescriptor,
};
use thiserror::Error;
use tonic::codegen::http::Uri;

use cy_workspace_client_sdk::{
    connect_relay_session, validate_workspace_descriptor as validate_descriptor, RelayClientConfig,
    RelaySession, RelayTransportError,
};
use cy_workspace_control_plane::{VerifiedWebPrincipal, WebRelaySessionCredentialIssuer};

const RELAY_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_WORKSPACE_DESCRIPTORS: usize = 1024;
const MAX_SCOPE_BYTES: usize = 256;
const MAX_REQUEST_ID_BYTES: usize = 128;
const MAX_SESSION_CREDENTIAL_BYTES: usize = 4096;
const SESSION_CREDENTIAL_MAX_TTL_MS: u64 = 60_000;

/// Safe failure categories for the Web Frontend Relay client.
///
/// Errors intentionally omit credentials, user claims, Workspace identifiers, and remote
/// diagnostics so callers can return them without reflecting security-sensitive inputs.
///
/// Web Frontend Relay 客户端的安全错误类别；不包含凭证、用户 claims、Workspace 标识或远端诊断。
#[derive(Debug, Error, Clone, Copy, Eq, PartialEq)]
pub enum FrontendRelayClientError {
    /// 配置的 endpoint、SNI 名称或 workload mTLS 材料无效。
    /// The configured endpoint, SNI name, or workload mTLS material is invalid.
    #[error("frontend relay client configuration is invalid")]
    Configuration,
    /// 已验证 Web principal 或短时 Relay credential 已过期或无效。
    /// The verified web principal or short-lived Relay credential is expired or invalid.
    #[error("frontend relay session credential is unavailable")]
    CredentialUnavailable,
    /// Relay 连接或操作超过有界时间。
    /// The connection or operation exceeded its bounded time window.
    #[error("frontend relay operation timed out")]
    Timeout,
    /// 远端 Relay stream 已关闭或不可达。
    /// The remote Relay stream closed or could not be reached.
    #[error("frontend relay is unavailable")]
    RelayUnavailable,
    /// 已认证 Relay session 已到达签名的过期时间。
    /// The authenticated Relay session reached its signed expiry.
    #[error("frontend relay session expired")]
    SessionExpired,
    /// Directory 返回的 descriptor 无效、过期或 scope 不匹配。
    /// The Directory returned an invalid, expired, or wrongly scoped descriptor.
    #[error("frontend relay Workspace descriptor is invalid")]
    DescriptorInvalid,
    /// request 指定的 Workspace 不在此 session 的 discovery 结果中。
    /// The request does not name a Workspace returned by this session's discovery.
    #[error("frontend relay Workspace was not discovered")]
    WorkspaceNotDiscovered,
    /// request identifier 为空或超出协议限制。
    /// The request identifier is empty or outside the protocol bound.
    #[error("frontend relay request identifier is invalid")]
    RequestInvalid,
    /// Relay response 与此客户端发送的 request 不匹配。
    /// The Relay response does not match the request sent by this client.
    #[error("frontend relay response does not match the request")]
    ResponseInvalid,
    /// 无法为内部 Relay correlation ID 获取密码学随机数。
    /// Cryptographic randomness could not be obtained for an internal Relay correlation ID.
    #[error("frontend relay request correlation is unavailable")]
    CorrelationUnavailable,
    /// 系统时钟无法提供有效 Unix timestamp。
    /// The system clock could not provide a valid Unix timestamp.
    #[error("frontend relay clock is unavailable")]
    ClockUnavailable,
}

/// Trusted Relay endpoint and BFF workload mTLS identity for one frontend client.
///
/// The client certificate and key identify the BFF workload to Relay. They must not be a
/// registered Workspace device certificate, and they are independent of the user's AAD bearer
/// token. The server CA and SNI name authenticate Relay to the BFF.
///
/// 可信 Relay endpoint 与 BFF workload mTLS 身份。客户端证书只标识 BFF workload，不得使用已注册的 Workspace device 证书，也不由用户 AAD bearer token 派生。
pub struct FrontendRelayClientConfig {
    relay: RelayClientConfig,
}

impl FrontendRelayClientConfig {
    /// Creates a frontend configuration from trusted service settings.
    ///
    /// `control_endpoint`, `server_name`, and workload certificate material must come from
    /// server-side configuration. The endpoint is restricted to an HTTPS origin without user
    /// info, path, or query data.
    ///
    /// 使用服务端可信配置创建客户端；endpoint 必须是无 user info、path 或 query 的 HTTPS origin。
    pub fn new(
        control_endpoint: impl Into<String>,
        server_name: impl Into<String>,
        ca_certificate_pem: Vec<u8>,
        workload_client_certificate_pem: Vec<u8>,
        workload_client_key_pem: Vec<u8>,
    ) -> Result<Self, FrontendRelayClientError> {
        let control_endpoint = control_endpoint.into();
        let server_name = server_name.into();
        validate_endpoint(&control_endpoint)?;
        if !valid_server_name(&server_name)
            || ca_certificate_pem.is_empty()
            || workload_client_certificate_pem.is_empty()
            || workload_client_key_pem.is_empty()
        {
            return Err(FrontendRelayClientError::Configuration);
        }

        Ok(Self {
            relay: RelayClientConfig {
                control_endpoint,
                server_name,
                ca_certificate_pem,
                client_certificate_pem: workload_client_certificate_pem,
                client_key_pem: workload_client_key_pem,
            },
        })
    }
}

/// One Web Frontend's authenticated Relay client and Directory-bounded Workspace set.
///
/// Callers cannot provide a `RelayHello`, session credential, user identity, organization, or
/// endpoint per request. This client creates the handshake only from a `VerifiedWebPrincipal`
/// and a trusted [`WebRelaySessionCredentialIssuer`].
///
/// 一个 Web Frontend 的认证 Relay 客户端及 Directory 绑定的 Workspace 集合。调用方不能逐请求传入 hello、credential、身份、组织或 endpoint。
pub struct FrontendRelayClient {
    session: RelaySession,
    relay_config: FrontendRelayClientConfig,
    user: UserIdentityRef,
    organization_id: String,
    session_expires_at_unix_ms: u64,
    workspaces: BTreeMap<String, WorkspaceConnectionDescriptor>,
}

/// Async transport seam for BFF code that sends typed Workspace API requests to Relay.
///
/// Keep each implementation bound to exactly one `VerifiedWebPrincipal`; do not share a
/// transport between users or select a session using only a Workspace identifier.
///
/// BFF 调用 Relay 的异步 transport port。每个实现必须绑定一个 `VerifiedWebPrincipal`，不得跨用户共享或只按 Workspace ID 选择会话。
#[tonic::async_trait]
pub trait FrontendWorkspaceTransport: Send {
    /// Discover the currently authorized Workspaces for the bound web principal.
    async fn discover_workspaces(
        &mut self,
    ) -> Result<Vec<WorkspaceConnectionDescriptor>, FrontendRelayClientError>;

    /// Execute one typed Workspace API request using a descriptor returned by discovery.
    async fn execute(
        &mut self,
        request: WorkspaceApiRequest,
    ) -> Result<WorkspaceApiResponse, FrontendRelayClientError>;
}

impl FrontendRelayClient {
    /// Opens a short-lived Relay session for one verified web principal.
    ///
    /// The injected credential issuer must be the trusted BFF-side handoff signer. The caller's
    /// AAD bearer token is not used as a TLS identity or copied into `RelayHello`.
    ///
    /// 为一个已验证 Web principal 打开短时 Relay 会话。注入的 issuer 必须是可信 BFF handoff signer；AAD bearer token 不用作 TLS 身份，也不会复制到 `RelayHello`。
    pub async fn connect(
        config: FrontendRelayClientConfig,
        principal: &VerifiedWebPrincipal,
        credential_issuer: &dyn WebRelaySessionCredentialIssuer,
    ) -> Result<Self, FrontendRelayClientError> {
        let now_unix_ms = current_unix_ms()?;
        let principal_expiry = u64::try_from(principal.expires_at_unix_ms())
            .map_err(|_| FrontendRelayClientError::CredentialUnavailable)?;
        if principal_expiry <= now_unix_ms {
            return Err(FrontendRelayClientError::CredentialUnavailable);
        }
        let session_expires_at_unix_ms = principal_expiry.min(
            now_unix_ms
                .checked_add(SESSION_CREDENTIAL_MAX_TTL_MS)
                .ok_or(FrontendRelayClientError::ClockUnavailable)?,
        );
        let credential = credential_issuer
            .issue(principal, now_unix_ms)
            .map_err(|_| FrontendRelayClientError::CredentialUnavailable)?;
        if credential.is_empty() || credential.len() > MAX_SESSION_CREDENTIAL_BYTES {
            return Err(FrontendRelayClientError::CredentialUnavailable);
        }
        let hello = make_frontend_hello(
            principal.identity(),
            principal.organization_id(),
            credential,
        );

        let session = tokio::time::timeout(
            RELAY_CONNECT_TIMEOUT,
            connect_relay_session(&config.relay, hello),
        )
        .await
        .map_err(|_| FrontendRelayClientError::Timeout)?
        .map_err(map_transport_error)?;

        if current_unix_ms()? >= session_expires_at_unix_ms {
            return Err(FrontendRelayClientError::SessionExpired);
        }

        Ok(Self {
            session,
            relay_config: config,
            user: principal.identity().clone(),
            organization_id: principal.organization_id().to_string(),
            session_expires_at_unix_ms,
            workspaces: BTreeMap::new(),
        })
    }

    /// Discovers and caches only descriptors for this exact user, organization, and Relay route.
    ///
    /// A failed or malformed refresh clears the old cache so callers cannot continue using a
    /// descriptor set after the authoritative Directory stopped answering.
    ///
    /// 仅发现并缓存当前用户、组织和 Relay route 的 descriptor。刷新失败或返回无效数据时清空旧缓存。
    pub async fn discover_workspaces(
        &mut self,
    ) -> Result<Vec<WorkspaceConnectionDescriptor>, FrontendRelayClientError> {
        self.ensure_session_active()?;
        self.workspaces.clear();
        let request_id = fresh_correlation_id()?;
        let descriptors = self
            .session
            .discover(request_id, self.user.clone(), self.organization_id.clone())
            .await
            .map_err(map_transport_error)?;

        if descriptors.len() > MAX_WORKSPACE_DESCRIPTORS {
            return Err(FrontendRelayClientError::DescriptorInvalid);
        }
        let now_unix_ms = current_unix_ms()?;
        let mut validated = BTreeMap::new();
        for descriptor in descriptors {
            validate_frontend_descriptor(
                &descriptor,
                &self.organization_id,
                &self.relay_config.relay,
                now_unix_ms,
            )?;
            if validated
                .insert(descriptor.workspace_id.clone(), descriptor)
                .is_some()
            {
                return Err(FrontendRelayClientError::DescriptorInvalid);
            }
        }
        let discovered = validated.values().cloned().collect();
        self.workspaces = validated;
        Ok(discovered)
    }

    /// Sends one Workspace API request through the authenticated Relay session.
    ///
    /// The request must name a still-valid descriptor discovered on this client. A fresh random
    /// wire correlation ID prevents a late response from a timed-out request from being mistaken
    /// for a later request that reuses the caller's ID. The caller's ID is restored only after an
    /// exact response match.
    ///
    /// 仅通过认证 Relay 发送已发现且仍有效的 Workspace 请求。每次使用随机 wire correlation ID，避免超时后的迟到响应匹配到重用的调用方 ID。
    pub async fn execute(
        &mut self,
        mut request: WorkspaceApiRequest,
    ) -> Result<WorkspaceApiResponse, FrontendRelayClientError> {
        self.ensure_session_active()?;
        if !valid_caller_request_id(&request.request_id) {
            return Err(FrontendRelayClientError::RequestInvalid);
        }
        let Some(descriptor) = self.workspaces.get(&request.workspace_id) else {
            return Err(FrontendRelayClientError::WorkspaceNotDiscovered);
        };
        validate_frontend_descriptor(
            descriptor,
            &self.organization_id,
            &self.relay_config.relay,
            current_unix_ms()?,
        )?;

        let caller_request_id = std::mem::take(&mut request.request_id);
        let wire_request_id = fresh_correlation_id()?;
        request.request_id = wire_request_id.clone();
        let mut response = self
            .session
            .execute(request)
            .await
            .map_err(map_transport_error)?;
        self.ensure_session_active()?;
        validate_response_id(&response, &wire_request_id)?;
        response.request_id = caller_request_id;
        Ok(response)
    }

    /// Returns the authenticated Relay session's opaque server-assigned identifier.
    pub fn relay_session_id(&self) -> &str {
        self.session.relay_session_id()
    }

    fn ensure_session_active(&mut self) -> Result<(), FrontendRelayClientError> {
        if current_unix_ms()? >= self.session_expires_at_unix_ms {
            self.workspaces.clear();
            return Err(FrontendRelayClientError::SessionExpired);
        }
        Ok(())
    }
}

#[tonic::async_trait]
impl FrontendWorkspaceTransport for FrontendRelayClient {
    async fn discover_workspaces(
        &mut self,
    ) -> Result<Vec<WorkspaceConnectionDescriptor>, FrontendRelayClientError> {
        FrontendRelayClient::discover_workspaces(self).await
    }

    async fn execute(
        &mut self,
        request: WorkspaceApiRequest,
    ) -> Result<WorkspaceApiResponse, FrontendRelayClientError> {
        FrontendRelayClient::execute(self, request).await
    }
}

fn make_frontend_hello(
    identity: &UserIdentityRef,
    organization_id: &str,
    credential: String,
) -> RelayHello {
    RelayHello {
        role: RelayParticipantRole::Frontend as i32,
        session_credential: credential,
        user: Some(identity.clone()),
        organization_id: organization_id.to_string(),
        workspace_id: String::new(),
        device: None,
    }
}

fn validate_frontend_descriptor(
    descriptor: &WorkspaceConnectionDescriptor,
    expected_organization_id: &str,
    relay_config: &RelayClientConfig,
    now_unix_ms: u64,
) -> Result<(), FrontendRelayClientError> {
    if descriptor.workspace_id.len() > MAX_SCOPE_BYTES
        || descriptor.organization_id != expected_organization_id
        || validate_descriptor(descriptor, now_unix_ms).is_err()
        || !descriptor
            .candidates
            .iter()
            .any(|candidate| candidate_matches_relay(candidate, relay_config))
    {
        return Err(FrontendRelayClientError::DescriptorInvalid);
    }
    Ok(())
}

fn candidate_matches_relay(
    candidate: &WorkspaceConnectionCandidate,
    relay_config: &RelayClientConfig,
) -> bool {
    candidate.mode == cy_proto::core_v1::ConnectivityMode::Relay as i32
        && candidate.connection_uri == relay_config.control_endpoint
        && candidate.server_name == relay_config.server_name
}

fn validate_response_id(
    response: &WorkspaceApiResponse,
    expected_request_id: &str,
) -> Result<(), FrontendRelayClientError> {
    if response.request_id != expected_request_id {
        return Err(FrontendRelayClientError::ResponseInvalid);
    }
    Ok(())
}

fn valid_caller_request_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_REQUEST_ID_BYTES
        && value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
}

fn validate_endpoint(endpoint: &str) -> Result<(), FrontendRelayClientError> {
    let uri = endpoint
        .parse::<Uri>()
        .map_err(|_| FrontendRelayClientError::Configuration)?;
    let authority = uri
        .authority()
        .ok_or(FrontendRelayClientError::Configuration)?;
    if uri.scheme_str() != Some("https")
        || uri.path() != "/"
        || uri.query().is_some()
        || authority.as_str().contains('@')
        || authority.host().is_empty()
    {
        return Err(FrontendRelayClientError::Configuration);
    }
    Ok(())
}

fn valid_server_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 253
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
}

fn current_unix_ms() -> Result<u64, FrontendRelayClientError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| FrontendRelayClientError::ClockUnavailable)?;
    u64::try_from(elapsed.as_millis()).map_err(|_| FrontendRelayClientError::ClockUnavailable)
}

fn fresh_correlation_id() -> Result<String, FrontendRelayClientError> {
    let mut random = [0_u8; 16];
    getrandom::getrandom(&mut random)
        .map_err(|_| FrontendRelayClientError::CorrelationUnavailable)?;
    let mut id = String::with_capacity(36);
    id.push_str("web-");
    for byte in random {
        write!(&mut id, "{byte:02x}")
            .map_err(|_| FrontendRelayClientError::CorrelationUnavailable)?;
    }
    Ok(id)
}

fn map_transport_error(error: RelayTransportError) -> FrontendRelayClientError {
    match error {
        RelayTransportError::ResponseTimeout => FrontendRelayClientError::Timeout,
        RelayTransportError::Endpoint(_)
        | RelayTransportError::Transport(_)
        | RelayTransportError::Closed
        | RelayTransportError::Protocol(_) => FrontendRelayClientError::RelayUnavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cy_proto::workspace_v1::WorkspaceConnectionCandidate;

    fn relay_config() -> RelayClientConfig {
        RelayClientConfig {
            control_endpoint: "https://relay.example.test".to_string(),
            server_name: "relay.example.test".to_string(),
            ca_certificate_pem: vec![1],
            client_certificate_pem: vec![2],
            client_key_pem: vec![3],
        }
    }

    fn descriptor() -> WorkspaceConnectionDescriptor {
        WorkspaceConnectionDescriptor {
            descriptor_version: "cyrene.workspace.connection.v1".to_string(),
            workspace_id: "workspace-1".to_string(),
            organization_id: "organization-1".to_string(),
            display_name: "Workspace 1".to_string(),
            candidates: vec![WorkspaceConnectionCandidate {
                mode: cy_proto::core_v1::ConnectivityMode::Relay as i32,
                provider_id: "relay-1".to_string(),
                connection_uri: "https://relay.example.test".to_string(),
                server_name: "relay.example.test".to_string(),
                priority: 1,
                routing_hint: Vec::new(),
            }],
            expires_at: Some(prost_types::Timestamp {
                seconds: 2_000_000_000,
                nanos: 0,
            }),
        }
    }

    #[test]
    fn relay_hello_is_frontend_user_scoped_and_omits_device_and_workspace() {
        let user = UserIdentityRef {
            issuer: "https://login.example.test/tenant/v2.0".to_string(),
            subject: "user-17".to_string(),
        };
        let hello = make_frontend_hello(&user, "org-4", "opaque-signed-handoff".to_string());

        assert_eq!(hello.role, RelayParticipantRole::Frontend as i32);
        assert_eq!(hello.session_credential, "opaque-signed-handoff");
        assert_eq!(hello.user, Some(user));
        assert_eq!(hello.organization_id, "org-4");
        assert!(hello.workspace_id.is_empty());
        assert!(hello.device.is_none());
    }

    #[test]
    fn endpoint_rejects_non_https_user_info_paths_and_queries() {
        for endpoint in [
            "http://relay.example.test",
            "https://user:password@relay.example.test",
            "https://relay.example.test/prefix",
            "https://relay.example.test?target=other",
        ] {
            assert_eq!(
                validate_endpoint(endpoint),
                Err(FrontendRelayClientError::Configuration),
                "accepted {endpoint}"
            );
        }
        assert!(validate_endpoint("https://relay.example.test").is_ok());
    }

    #[test]
    fn descriptor_must_match_directory_org_and_exact_relay_endpoint_and_sni() {
        let mut value = descriptor();
        assert!(validate_frontend_descriptor(
            &value,
            "organization-1",
            &relay_config(),
            1_900_000_000_000,
        )
        .is_ok());

        value.organization_id = "organization-2".to_string();
        assert_eq!(
            validate_frontend_descriptor(
                &value,
                "organization-1",
                &relay_config(),
                1_900_000_000_000,
            ),
            Err(FrontendRelayClientError::DescriptorInvalid)
        );

        let mut value = descriptor();
        value.candidates[0].server_name = "attacker.example.test".to_string();
        assert_eq!(
            validate_frontend_descriptor(
                &value,
                "organization-1",
                &relay_config(),
                1_900_000_000_000,
            ),
            Err(FrontendRelayClientError::DescriptorInvalid)
        );
    }

    #[test]
    fn descriptor_expiry_and_response_request_id_are_checked() {
        let mut value = descriptor();
        assert_eq!(
            validate_frontend_descriptor(
                &value,
                "organization-1",
                &relay_config(),
                2_000_000_001_000,
            ),
            Err(FrontendRelayClientError::DescriptorInvalid)
        );
        value.expires_at = Some(prost_types::Timestamp {
            seconds: 2_000_000_000,
            nanos: 0,
        });

        let response = WorkspaceApiResponse {
            request_id: "wire-id".to_string(),
            outcome: None,
        };
        assert!(validate_response_id(&response, "wire-id").is_ok());
        assert_eq!(
            validate_response_id(&response, "other-wire-id"),
            Err(FrontendRelayClientError::ResponseInvalid)
        );
    }

    #[test]
    fn caller_request_id_rejects_controls_unicode_and_oversized_values() {
        assert!(valid_caller_request_id("request-42"));
        assert!(!valid_caller_request_id(""));
        assert!(!valid_caller_request_id("request\n42"));
        assert!(!valid_caller_request_id("请求-42"));
        assert!(!valid_caller_request_id(
            &"x".repeat(MAX_REQUEST_ID_BYTES + 1)
        ));
    }

    #[test]
    fn correlation_ids_are_fresh_bounded_and_not_caller_supplied() {
        let first = fresh_correlation_id().unwrap();
        let second = fresh_correlation_id().unwrap();
        assert_ne!(first, second);
        assert!(first.starts_with("web-"));
        assert!(first.len() <= MAX_REQUEST_ID_BYTES);
    }
}
