// ╔══════════════════════════════════════════════════════════════════════╗
// ║ File: framework/crates/cy-workspace-web-bff/src/product.rs          ║
// ║ Module: cy_workspace_web_bff::product                              ║
// ║ Role: Bind trusted Product contracts to Workspace v2 invocations.  ║
// ║                                                                    ║
// ║ 模块职责：将受信 Product 合同绑定到 Workspace v2 invocation。        ║
// ╚══════════════════════════════════════════════════════════════════════╝

use std::sync::Arc;

use async_trait::async_trait;
use cy_proto::cyrene::workspace::product::v2::ProductApiInvocationV2;
use cy_workspace_control_plane::workspace_v1::{
    workspace_api_request, workspace_api_response, WorkspaceApiRequest, WorkspaceApiResponse,
};
use cy_workspace_product_contracts::{
    ProductContractBundle, ProductOperation, TrustedProductPolicy,
};
use thiserror::Error;

/// Startup validation failure for the pinned Product v2 contract bundle.
///
/// 受 pin 的 Product v2 合同 bundle 启动校验失败。
#[derive(Debug, Error)]
pub enum ProductCatalogError {
    /// The pinned Product v2 bundle or its local reference closure is invalid.
    #[error("trusted Product v2 contract bundle is invalid")]
    ContractBundle,
    /// This build did not include a coordinated Product v2 release pin.
    #[error("Product v2 release pin is unavailable in this build")]
    ReleasePinUnavailable,
    /// The pinned Platform authorization policy could not be loaded.
    #[error("trusted Product v2 policy is invalid")]
    TrustedPolicy,
}

/// Read-only Product operation catalog compiled from a trusted versioned bundle.
///
/// 从受信版本化 bundle 编译出的只读 Product operation catalog。
#[derive(Clone)]
pub struct ProductOperationCatalog {
    bundle: Option<Arc<ProductContractBundle>>,
    policy: Option<Arc<TrustedProductPolicy>>,
}

impl std::fmt::Debug for ProductOperationCatalog {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProductOperationCatalog")
            .field("trusted_bundle_configured", &self.bundle.is_some())
            .field("trusted_policy_configured", &self.policy.is_some())
            .finish()
    }
}

impl ProductOperationCatalog {
    /// Wrap a bundle only after its source and digest pins have been verified.
    ///
    /// 只有来源与 digest pins 已验证后才能包装 bundle。
    pub(crate) fn from_verified_bundle(
        bundle: ProductContractBundle,
        policy: TrustedProductPolicy,
    ) -> Self {
        Self {
            bundle: Some(Arc::new(bundle)),
            policy: Some(Arc::new(policy)),
        }
    }

    pub fn from_arcs(
        bundle: Arc<ProductContractBundle>,
        policy: Arc<TrustedProductPolicy>,
    ) -> Self {
        Self {
            bundle: Some(bundle),
            policy: Some(policy),
        }
    }

    /// Construct a deny-all placeholder while the host fetches authenticated catalog snapshots.
    ///
    /// This value cannot authorize product operations; the Authority-backed gateway loads the
    /// current pinned catalog for each request before any product request is accepted.
    pub fn unavailable() -> Self {
        Self {
            bundle: None,
            policy: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn deny_all_for_tests() -> Self {
        Self {
            bundle: None,
            policy: None,
        }
    }

    /// Find one operation by the owner-published stable identifiers.
    ///
    /// 按 owner 发布的稳定标识查找 operation。
    pub fn get(&self, owner_id: &str, operation_id: &str) -> Option<&ProductOperation> {
        self.bundle.as_deref()?.operation(owner_id, operation_id)
    }

    /// Preflight whether the pinned Platform policy contains a grant for an operation.
    /// This is a fail-closed filter only; the Workspace control plane remains the authority.
    ///
    /// 预检查固定 Platform policy 是否包含该 operation 的 grant；实际授权仍只由 Workspace control plane 执行。
    pub fn has_grant(&self, owner_id: &str, operation_id: &str) -> bool {
        self.policy
            .as_deref()
            .is_some_and(|policy| policy.has_grant(owner_id, operation_id))
    }
}

/// Authenticated Workspace API adapter used by the BFF router.
///
/// BFF router 使用的已认证 Workspace API adapter。
#[async_trait]
pub trait WorkspaceProductGateway: Send + Sync + 'static {
    /// Load and validate the current Authority catalog view for this request.
    async fn catalog_for_request(
        &self,
        principal: &cy_workspace_control_plane::VerifiedWebPrincipal,
        access_token: &str,
        workspace_id: &str,
    ) -> Result<ProductOperationCatalog, WorkspaceGatewayError>;

    /// Dispatch one v2 Product request with the verified web identity context.
    ///
    /// 使用已验证的 Web identity context 派发一个 v2 Product request。
    async fn invoke(
        &self,
        principal: &cy_workspace_control_plane::VerifiedWebPrincipal,
        access_token: &str,
        request: WorkspaceApiRequest,
    ) -> Result<WorkspaceApiResponse, WorkspaceGatewayError>;

    /// Create a user-owned workload delegation through the Platform Authority.
    async fn create_delegation(
        &self,
        _principal: &cy_workspace_control_plane::VerifiedWebPrincipal,
        _access_token: &str,
        _workspace_id: &str,
        _request: CreateDelegationInput,
    ) -> Result<CreateDelegationOutput, WorkspaceGatewayError> {
        Err(WorkspaceGatewayError::Unavailable)
    }

    /// Revoke one delegation owned by the authenticated user.
    async fn revoke_delegation(
        &self,
        _principal: &cy_workspace_control_plane::VerifiedWebPrincipal,
        _access_token: &str,
        _workspace_id: &str,
        _delegation_id: &str,
    ) -> Result<RevokeDelegationOutput, WorkspaceGatewayError> {
        Err(WorkspaceGatewayError::Unavailable)
    }

    /// Read one delegation owned by the verified principal in the exact Workspace.
    async fn get_delegation(
        &self,
        _principal: &cy_workspace_control_plane::VerifiedWebPrincipal,
        _access_token: &str,
        _workspace_id: &str,
        _delegation_id: &str,
    ) -> Result<DelegationOutput, WorkspaceGatewayError> {
        Err(WorkspaceGatewayError::Unavailable)
    }
}

/// User request to grant one workload a narrow set of Product operations.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateDelegationInput {
    /// Authenticated workload audience.
    pub audience: String,
    /// Operations and exact resource scopes to grant.
    pub operations: Vec<DelegatedOperationInput>,
    /// Optional lifetime; Authority caps it at seven days and defaults to seven days.
    pub expires_in_seconds: Option<u32>,
}

/// One operation scope in the BFF request.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DelegatedOperationInput {
    /// Catalog owner.
    pub owner_id: String,
    /// Stable operation identifier.
    pub operation_id: String,
    /// Exact Product resource IDs.
    #[serde(default)]
    pub resource_ids: Vec<String>,
}

/// Persisted delegation projection returned by Authority.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateDelegationOutput {
    /// Opaque delegation ID.
    pub delegation_id: String,
    /// Current revocation fence generation.
    pub generation: u64,
    /// Absolute expiration in Unix milliseconds.
    pub expires_at_unix_ms: u64,
}

/// Revocation outcome returned by Authority.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RevokeDelegationOutput {
    /// Whether the grant is revoked after this call.
    pub revoked: bool,
    /// Current revocation fence generation.
    pub generation: u64,
}

/// Exact self-owned delegation projection for a first-party binding client.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DelegationOutput {
    /// Opaque delegation ID.
    pub delegation_id: String,
    /// Directory organization and workspace scope.
    pub organization_id: String,
    /// Exact workspace scope.
    pub workspace_id: String,
    /// Verified human anchor. This is returned only after Authority self-owner checks.
    pub principal_issuer: String,
    /// Verified human anchor. This is returned only after Authority self-owner checks.
    pub principal_subject: String,
    /// Fixed workload audience.
    pub audience: String,
    /// Server-configured workload identity bound to the original consent.
    pub managed_workload_id: String,
    /// Owner, operation, and exact resource allowlist.
    pub operations: Vec<DelegatedOperationInput>,
    /// Expiration in Unix milliseconds.
    pub expires_at_unix_ms: u64,
    /// Monotonic revocation generation.
    pub generation: u64,
    /// Whether the grant has been revoked.
    pub revoked: bool,
    /// Revocation time in Unix milliseconds when revoked.
    pub revoked_at_unix_ms: Option<u64>,
}

/// Stable failure categories from the Workspace transport adapter.
///
/// Workspace transport adapter 返回的稳定错误类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceGatewayError {
    /// Workspace API transport or authority is unavailable.
    Unavailable,
    /// Workspace API call exceeded its configured deadline.
    Timeout,
    /// Workspace refused the authenticated principal or operation.
    Forbidden,
    /// Workspace could not find this authorized Workspace resource.
    NotFound,
    /// The delegation request is malformed.
    InvalidRequest,
    /// Workspace returned an invalid or unexpected API response.
    InvalidResponse,
}

/// Construct the Workspace v2 envelope from values checked by the BFF.
///
/// 使用 BFF 已校验的值构造 Workspace v2 envelope。
pub(crate) struct WorkspaceProductRequestInput<'a> {
    pub(crate) owner_id: &'a str,
    pub(crate) operation_id: &'a str,
    pub(crate) workspace_id: &'a str,
    pub(crate) resource_id: Option<&'a str>,
    pub(crate) json_body: Option<&'a [u8]>,
    pub(crate) idempotency_key: Option<&'a str>,
    pub(crate) traceparent: &'a str,
    pub(crate) request_id: &'a str,
}

pub(crate) fn workspace_product_request(
    input: WorkspaceProductRequestInput<'_>,
) -> WorkspaceApiRequest {
    WorkspaceApiRequest {
        request_id: input.request_id.to_owned(),
        workspace_id: input.workspace_id.to_owned(),
        traceparent: input.traceparent.to_owned(),
        request: Some(workspace_api_request::Request::ProductApiV2(
            ProductApiInvocationV2 {
                owner_id: input.owner_id.to_owned(),
                operation_id: input.operation_id.to_owned(),
                json_body: input.json_body.unwrap_or_default().to_vec(),
                resource_id: input.resource_id.unwrap_or_default().to_owned(),
                idempotency_key: input.idempotency_key.unwrap_or_default().to_owned(),
            },
        )),
    }
}

/// Extract a valid Product v2 response from the Workspace oneof.
///
/// 从 Workspace oneof 提取有效 Product v2 response。
pub(crate) fn product_response(
    response: WorkspaceApiResponse,
    expected_request_id: &str,
) -> Result<(u16, &'static str, Vec<u8>), WorkspaceGatewayError> {
    if response.request_id != expected_request_id {
        return Err(WorkspaceGatewayError::InvalidResponse);
    }
    match response.outcome {
        Some(workspace_api_response::Outcome::ProductApiV2(product)) => {
            let status = u16::try_from(product.status_code)
                .ok()
                .filter(|value| (200..=599).contains(value))
                .ok_or(WorkspaceGatewayError::InvalidResponse)?;
            let content_type = match product.content_type.as_str() {
                "application/json" => "application/json",
                "application/problem+json" => "application/problem+json",
                _ => return Err(WorkspaceGatewayError::InvalidResponse),
            };
            Ok((status, content_type, product.json_body))
        }
        Some(workspace_api_response::Outcome::Error(error)) => match error.code {
            4 => Err(WorkspaceGatewayError::Timeout),
            5 => Err(WorkspaceGatewayError::NotFound),
            7 | 16 => Err(WorkspaceGatewayError::Forbidden),
            14 => Err(WorkspaceGatewayError::Unavailable),
            _ => Err(WorkspaceGatewayError::InvalidResponse),
        },
        _ => Err(WorkspaceGatewayError::InvalidResponse),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_request_uses_the_generic_v2_envelope_and_preserves_body_bytes() {
        let original_body = b"{ \"workspaceId\" : \"workspace-a\" }\n";
        let request = workspace_product_request(WorkspaceProductRequestInput {
            owner_id: "navigator",
            operation_id: "observeWorkspaceSnapshot",
            workspace_id: "workspace-a",
            resource_id: Some("snapshot-1"),
            json_body: Some(original_body),
            idempotency_key: Some("request-1"),
            traceparent: "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
            request_id: "request-1",
        });
        assert_eq!(request.workspace_id, "workspace-a");
        let Some(workspace_api_request::Request::ProductApiV2(invocation)) = request.request else {
            panic!("request must use the v2 Product invocation oneof");
        };
        assert_eq!(invocation.owner_id, "navigator");
        assert_eq!(invocation.operation_id, "observeWorkspaceSnapshot");
        assert_eq!(invocation.json_body, original_body);
        assert_eq!(invocation.resource_id, "snapshot-1");
        assert_eq!(invocation.idempotency_key, "request-1");
    }

    #[test]
    fn product_response_accepts_only_matching_v2_json_envelopes() {
        let response = WorkspaceApiResponse {
            request_id: "request-1".into(),
            outcome: Some(workspace_api_response::Outcome::ProductApiV2(
                cy_proto::cyrene::workspace::product::v2::ProductApiResponseV2 {
                    status_code: 200,
                    json_body: b"{}".to_vec(),
                    content_type: "application/json".into(),
                },
            )),
        };
        assert_eq!(
            product_response(response, "request-1"),
            Ok((200, "application/json", b"{}".to_vec()))
        );
    }

    #[test]
    fn product_response_rejects_the_v1_outcome() {
        let response = WorkspaceApiResponse {
            request_id: "request-1".into(),
            outcome: Some(workspace_api_response::Outcome::ProductApi(
                cy_workspace_control_plane::workspace_v1::WorkspaceProductApiResponse::default(),
            )),
        };
        assert_eq!(
            product_response(response, "request-1"),
            Err(WorkspaceGatewayError::InvalidResponse)
        );
    }
}
