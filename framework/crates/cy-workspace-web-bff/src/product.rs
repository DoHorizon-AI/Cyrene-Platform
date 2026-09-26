// ╔══════════════════════════════════════════════════════════════════════╗
// ║ File: framework/crates/cy-workspace-web-bff/src/product.rs          ║
// ║ Module: cy_workspace_web_bff::product                              ║
// ║ Role: Validate closed Product operation contracts and Workspace IO.║
// ║                                                                    ║
// ║ 模块：cy_workspace_web_bff::product                                ║
// ║ 职责：校验封闭 Product operation 合同并承载 Workspace I/O。          ║
// ╚══════════════════════════════════════════════════════════════════════╝

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use cy_workspace_fabric::workspace_v1::{
    workspace_api_request, workspace_api_response, WorkspaceApiRequest, WorkspaceApiResponse,
    WorkspaceProductApiContentType, WorkspaceProductApiOperation, WorkspaceProductApiRequest,
};
use http::Method;
use serde_json::Value;
use thiserror::Error;

use crate::manifest::{product_projection_manifest, ProductProjectionEntry};

/// Owner-provided JSON Schema validator compiled from the matching Product OpenAPI contract.
///
/// 由对应 Product OpenAPI 合同编译出的 owner JSON Schema 校验器。
pub trait ProductJsonSchema: Send + Sync + 'static {
    /// Validate one request or response JSON value against an owner schema.
    ///
    /// 根据 owner schema 校验一个请求或响应 JSON value。
    fn validate(&self, value: &Value) -> bool;

    /// Validate an owner response selected by its upstream status code.
    ///
    /// 根据上游 status code 校验 owner response。
    fn validate_response(&self, _status: u16, value: &Value) -> bool {
        self.validate(value)
    }

    /// Validate one opaque value against a declared Product path-parameter schema.
    ///
    /// 根据 Product 声明的 path-parameter schema 校验不透明参数值。
    fn validate_path_parameter(&self, _name: &str, _value: &str) -> bool {
        true
    }
}

/// One Product OpenAPI path parameter and whether the owner requires it.
///
/// 一个 Product OpenAPI path parameter 及 owner 是否要求该参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductPathParameter {
    /// Exact case-sensitive parameter name in the owner OpenAPI contract.
    pub name: String,
    /// Whether the Product operation requires a value for this path parameter.
    pub required: bool,
}

/// A response JSON pointer declared as a ProductResourceReference field.
///
/// 一个在响应 schema 中声明为 ProductResourceReference 的 JSON pointer。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductResourceReferenceField {
    /// RFC 6901 JSON pointer to the owner-declared resource-reference field.
    pub json_pointer: String,
    /// Whether the owner response schema requires the field to be present.
    pub required: bool,
}

/// Product-owned HTTP request-body rules derived from its OpenAPI operation.
///
/// 从 Product OpenAPI operation 派生的请求 body 规则。
#[derive(Clone)]
pub struct ProductRequestBodyContract {
    /// Whether this owner operation accepts an application/json body.
    pub allowed: bool,
    /// Whether this owner operation requires that JSON body.
    pub required: bool,
    /// Compiled request schema; required when allowed is true.
    pub schema: Option<Arc<dyn ProductJsonSchema>>,
}

impl std::fmt::Debug for ProductRequestBodyContract {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProductRequestBodyContract")
            .field("allowed", &self.allowed)
            .field("required", &self.required)
            .field("schema_configured", &self.schema.is_some())
            .finish()
    }
}

/// Product OpenAPI response contract and resource-reference fields.
///
/// Product OpenAPI 响应合同及资源引用字段。
#[derive(Clone)]
pub struct ProductResponseContract {
    /// Compiled response schema for the selected status and content type.
    pub schema: Option<Arc<dyn ProductJsonSchema>>,
    /// Some empty means the accepted schema declares no resource references.
    /// None means no accepted owner response schema is available.
    pub resource_reference_fields: Option<Vec<ProductResourceReferenceField>>,
}

impl std::fmt::Debug for ProductResponseContract {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProductResponseContract")
            .field("schema_configured", &self.schema.is_some())
            .field(
                "resource_reference_fields_configured",
                &self.resource_reference_fields.is_some(),
            )
            .finish()
    }
}

/// Server-resolved Product operation policy; browser input cannot construct it.
///
/// 服务端解析出的 Product operation policy，浏览器输入不能构造该对象。
#[derive(Clone)]
pub struct ProductOperationContract {
    /// Canonical owner, operation key, operationId, and semantic kind from the TCK.
    pub projection: ProductProjectionEntry,
    /// Upstream method declared by this operation in its owner OpenAPI document.
    pub upstream_method: Method,
    /// Exact owner OpenAPI path parameters.
    pub path_parameters: Vec<ProductPathParameter>,
    /// Compiled path-parameter schemas declared by the owner OpenAPI document.
    pub path_schema: Option<Arc<dyn ProductJsonSchema>>,
    /// Whether this operation requires unsupported query or header input.
    pub has_unsupported_query_or_header_parameters: bool,
    /// Owner request-body rules and compiled schema.
    pub request_body: ProductRequestBodyContract,
    /// Whether the operation accepts an idempotency key from the BFF envelope.
    pub allows_idempotency_key: bool,
    /// Whether the owner marks Idempotency-Key as required.
    pub requires_idempotency_key: bool,
    /// Owner response schema and any declared relative resource references.
    pub response: ProductResponseContract,
}

impl std::fmt::Debug for ProductOperationContract {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProductOperationContract")
            .field("projection", &self.projection)
            .field("upstream_method", &self.upstream_method)
            .field("path_parameters", &self.path_parameters)
            .field("path_schema_configured", &self.path_schema.is_some())
            .field(
                "has_unsupported_query_or_header_parameters",
                &self.has_unsupported_query_or_header_parameters,
            )
            .field("request_body", &self.request_body)
            .field("allows_idempotency_key", &self.allows_idempotency_key)
            .field("requires_idempotency_key", &self.requires_idempotency_key)
            .field("response", &self.response)
            .finish()
    }
}

/// Startup validation failure for owner contracts or canonical TCK parity.
///
/// owner 合同或规范 TCK parity 的启动校验错误。
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ProductCatalogError {
    /// The generated Workspace enum did not match the canonical TCK.
    #[error("canonical Product projection manifest is invalid")]
    Manifest,
    /// Owner OpenAPI data did not exactly match its TCK operation row.
    #[error("owner Product contract did not match the canonical projection")]
    ProjectionMismatch,
    /// The owner operation declares internally inconsistent JSON body rules.
    #[error("owner Product request body schema configuration is invalid")]
    RequestBodySchema,
    /// There is not exactly one owner contract for each canonical operation.
    #[error("owner Product contract operation set is incomplete or contains duplicates")]
    OperationSet,
    /// The owner method cannot be represented as a Product API operation.
    #[error("owner Product operation uses an unsupported HTTP method")]
    UpstreamMethod,
}

/// Validated closed Product projection catalog with no browser-controlled routing fields.
///
/// 已校验的封闭 Product projection catalog，不包含浏览器可控的路由字段。
#[derive(Clone)]
pub struct ProductOperationCatalog {
    entries: BTreeMap<i32, ProductOperationContract>,
}

impl ProductOperationCatalog {
    /// Build the catalog from server-loaded owner OpenAPI operation contracts.
    ///
    /// 使用服务端加载的 owner OpenAPI operation 合同构建 catalog。
    pub fn new(
        contracts: impl IntoIterator<Item = ProductOperationContract>,
    ) -> Result<Self, ProductCatalogError> {
        let manifest = product_projection_manifest().map_err(|_| ProductCatalogError::Manifest)?;
        let mut entries = BTreeMap::new();
        for contract in contracts {
            let canonical = manifest
                .iter()
                .find(|entry| entry.operation == contract.projection.operation)
                .ok_or(ProductCatalogError::ProjectionMismatch)?;
            if canonical != &contract.projection {
                return Err(ProductCatalogError::ProjectionMismatch);
            }
            if !is_supported_method(&contract.upstream_method) {
                return Err(ProductCatalogError::UpstreamMethod);
            }
            if (contract.request_body.required && !contract.request_body.allowed)
                || (contract.request_body.allowed && contract.request_body.schema.is_none())
                || (contract.requires_idempotency_key && !contract.allows_idempotency_key)
            {
                return Err(ProductCatalogError::RequestBodySchema);
            }
            if entries
                .insert(contract.projection.operation as i32, contract)
                .is_some()
            {
                return Err(ProductCatalogError::OperationSet);
            }
        }
        if entries.len() != manifest.len()
            || manifest
                .iter()
                .any(|entry| !entries.contains_key(&(entry.operation as i32)))
        {
            return Err(ProductCatalogError::OperationSet);
        }
        Ok(Self { entries })
    }

    /// Return a server-resolved contract for one closed Workspace operation key.
    ///
    /// 返回一个封闭 Workspace operation key 对应的服务端合同。
    pub fn get(
        &self,
        operation: WorkspaceProductApiOperation,
    ) -> Option<&ProductOperationContract> {
        self.entries.get(&(operation as i32))
    }

    /// Return the count of validated canonical Product operations.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns whether the catalog contains no Product operations.
    ///
    /// 返回目录是否不包含任何 Product 操作。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

fn is_supported_method(method: &Method) -> bool {
    method == Method::GET
        || method == Method::POST
        || method == Method::PUT
        || method == Method::PATCH
        || method == Method::DELETE
}

/// Authenticated Workspace API adapter used by the BFF router.
///
/// BFF router 使用的已认证 Workspace API adapter。
#[async_trait]
pub trait WorkspaceProductGateway: Send + Sync + 'static {
    /// Dispatch one typed Product request with the verified web identity context.
    ///
    /// 使用已验证的 Web identity context 派发一个 typed Product request。
    async fn invoke(
        &self,
        principal: &cy_workspace_fabric::VerifiedWebPrincipal,
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

/// Construct the typed Workspace API envelope from already validated BFF values.
///
/// 从已校验的 BFF 值构造 typed Workspace API envelope。
pub(crate) fn workspace_product_request(
    projection: &ProductProjectionEntry,
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
        request: Some(workspace_api_request::Request::ProductApi(
            WorkspaceProductApiRequest {
                owner: projection.owner as i32,
                operation: projection.operation as i32,
                kind: projection.kind as i32,
                resource_id: resource_id.unwrap_or_default().to_owned(),
                json_body: json_body.unwrap_or_default().to_vec(),
                idempotency_key: idempotency_key.unwrap_or_default().to_owned(),
            },
        )),
    }
}

/// Extract a valid Product response from the Workspace oneof.
///
/// 从 Workspace oneof 提取有效 Product response。
pub(crate) fn product_response(
    response: WorkspaceApiResponse,
    expected_request_id: &str,
) -> Result<(u16, &'static str, Vec<u8>), WorkspaceGatewayError> {
    if response.request_id != expected_request_id {
        return Err(WorkspaceGatewayError::InvalidResponse);
    }
    match response.outcome {
        Some(workspace_api_response::Outcome::ProductApi(product)) => {
            let status = u16::try_from(product.status_code)
                .ok()
                .filter(|value| (200..=599).contains(value))
                .ok_or(WorkspaceGatewayError::InvalidResponse)?;
            let content_type = match WorkspaceProductApiContentType::try_from(product.content_type)
                .map_err(|_| WorkspaceGatewayError::InvalidResponse)?
            {
                WorkspaceProductApiContentType::ApplicationJson => "application/json",
                WorkspaceProductApiContentType::ApplicationProblemJson => {
                    "application/problem+json"
                }
                WorkspaceProductApiContentType::Unspecified => {
                    return Err(WorkspaceGatewayError::InvalidResponse);
                }
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
    fn catalog_manifest_has_the_canonical_operation_set() {
        let manifest = product_projection_manifest().expect("manifest should parse");
        assert_eq!(manifest.len(), 13);
        let operations = manifest
            .iter()
            .map(|entry| entry.operation.as_str_name())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(operations.len(), 13);
    }
}
