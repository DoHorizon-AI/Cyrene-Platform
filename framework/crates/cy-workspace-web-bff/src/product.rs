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
    /// Dispatch one v2 Product request with the verified web identity context.
    ///
    /// 使用已验证的 Web identity context 派发一个 v2 Product request。
    async fn invoke(
        &self,
        principal: &cy_workspace_control_plane::VerifiedWebPrincipal,
        request: WorkspaceApiRequest,
    ) -> Result<WorkspaceApiResponse, WorkspaceGatewayError>;
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
    /// Workspace returned an invalid or unexpected API response.
    InvalidResponse,
}

/// Construct the Workspace v2 envelope from values checked by the BFF.
///
/// 使用 BFF 已校验的值构造 Workspace v2 envelope。
pub(crate) fn workspace_product_request(
    owner_id: &str,
    operation_id: &str,
    workspace_id: &str,
    resource_id: Option<&str>,
    json_body: Option<&[u8]>,
    idempotency_key: Option<&str>,
    traceparent: &str,
    request_id: String,
) -> WorkspaceApiRequest {
    WorkspaceApiRequest {
        request_id,
        workspace_id: workspace_id.to_owned(),
        traceparent: traceparent.to_owned(),
        request: Some(workspace_api_request::Request::ProductApiV2(
            ProductApiInvocationV2 {
                owner_id: owner_id.to_owned(),
                operation_id: operation_id.to_owned(),
                json_body: json_body.unwrap_or_default().to_vec(),
                resource_id: resource_id.unwrap_or_default().to_owned(),
                idempotency_key: idempotency_key.unwrap_or_default().to_owned(),
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
        let request = workspace_product_request(
            "navigator",
            "observeWorkspaceSnapshot",
            "workspace-a",
            Some("snapshot-1"),
            Some(original_body),
            Some("request-1"),
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
            "request-1".to_owned(),
        );
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
