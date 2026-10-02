//! Bounded generic HTTP transport for Platform-authorized Product calls.
//!
//! Method and path come only from an opaque invocation resolved against a
//! pinned owner OpenAPI contract. Organization/Workspace/resource path values
//! are already bound by Platform and cannot be replaced by the caller. / HTTP
//! 传输不做授权，只执行 Platform 已批准的固定路由绑定。

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cy_workspace_product_contracts::{
    parse_json_bytes_with_limit, AuthorizedProductInvocation, ProductContractBundle,
    ProductInvocationAdapter, ProductInvocationError, ProductInvocationResponse,
    PRODUCT_JSON_BYTES_LIMIT,
};
use reqwest::header::{HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use reqwest::{Client, Method, StatusCode, Url};

use crate::endpoint::ProductEndpoint;
use crate::{validate_product_endpoint_configs, ProductEndpointConfig};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_JSON_BODY_BYTES: usize = PRODUCT_JSON_BYTES_LIMIT;
const MAX_IDEMPOTENCY_KEY_BYTES: usize = 256;
const JSON_ACCEPT: &str = "application/json, application/problem+json";

/// HTTP adapter for all operations present in a validated owner release catalog.
///
/// It receives an opaque Platform-issued call and exact-scoped endpoint
/// configuration. It never sees caller roles, chooses a business route, or
/// accepts caller-supplied network configuration.
pub struct ProductHttpApiAdapter {
    endpoints: Vec<ScopedEndpoint>,
    transport: Arc<dyn ProductHttpTransport>,
}

impl ProductHttpApiAdapter {
    /// Loads server-owned HTTPS endpoints and creates the production transport.
    pub fn from_private_config(
        configs: Vec<ProductEndpointConfig>,
        catalog: &ProductContractBundle,
    ) -> Result<Self, ProductInvocationError> {
        validate_product_endpoint_configs(&configs)
            .map_err(|_| ProductInvocationError::Unavailable)?;
        validate_endpoint_owners(&configs, |owner_id| catalog.contains_owner_id(owner_id))?;
        let endpoints = configs
            .iter()
            .map(|config| {
                Ok(ScopedEndpoint {
                    owner_id: config.owner_id().to_owned(),
                    organization_id: config.organization_id().to_owned(),
                    workspace_id: config.workspace_id().to_owned(),
                    endpoint: ProductEndpoint::try_from(config)
                        .map_err(|_| ProductInvocationError::Unavailable)?,
                })
            })
            .collect::<Result<Vec<_>, ProductInvocationError>>()?;
        let client = build_http_client(CONNECT_TIMEOUT, REQUEST_TIMEOUT, true)?;
        Ok(Self {
            endpoints,
            transport: Arc::new(ReqwestProductHttpTransport { client }),
        })
    }

    fn endpoint_for(
        &self,
        request: &AuthorizedProductInvocation,
    ) -> Result<&ProductEndpoint, ProductInvocationError> {
        self.endpoints
            .iter()
            .find(|configured| {
                configured.owner_id == request.owner_id()
                    && configured.organization_id == request.organization_id()
                    && configured.workspace_id == request.workspace_id()
            })
            .map(|configured| &configured.endpoint)
            .ok_or(ProductInvocationError::Unavailable)
    }

    async fn send(
        &self,
        request: &AuthorizedProductInvocation,
    ) -> Result<ProductInvocationResponse, ProductInvocationError> {
        let route = request.route();
        let method = parse_method(route.method())?;
        let url = expand_path_template(
            &self.endpoint_for(request)?.base_url,
            route.path_template(),
            route.path_parameters(),
        )?;
        let body = request.json_body_bytes().map(|body| body.to_vec());
        validate_request_body(body.as_deref())?;
        let idempotency_key = request
            .idempotency_key()
            .map(parse_idempotency_key)
            .transpose()?;
        let response = self
            .transport
            .send(ProductHttpRequest {
                owner_id: request.owner_id().to_owned(),
                method,
                url,
                authorization: self.endpoint_for(request)?.authorization.clone(),
                body,
                idempotency_key,
            })
            .await?;

        validate_http_response(response.status, &response.content_type, &response.body)?;

        Ok(ProductInvocationResponse {
            status_code: response.status.as_u16(),
            content_type: response.content_type,
            json_body: response.body,
        })
    }
}

#[async_trait]
impl ProductInvocationAdapter for ProductHttpApiAdapter {
    async fn invoke(
        &self,
        request: &AuthorizedProductInvocation,
    ) -> Result<ProductInvocationResponse, ProductInvocationError> {
        self.send(request).await
    }
}

struct ScopedEndpoint {
    owner_id: String,
    organization_id: String,
    workspace_id: String,
    endpoint: ProductEndpoint,
}

/// A fully assembled request; its Debug output never exposes URL or credentials.
struct ProductHttpRequest {
    owner_id: String,
    method: Method,
    url: Url,
    authorization: HeaderValue,
    body: Option<Vec<u8>>,
    idempotency_key: Option<HeaderValue>,
}

impl fmt::Debug for ProductHttpRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProductHttpRequest")
            .field("owner_id", &self.owner_id)
            .field("method", &self.method)
            .field("url", &"<redacted>")
            .field("authorization", &"<redacted>")
            .field("body_bytes", &self.body.as_ref().map_or(0, Vec::len))
            .field("has_idempotency_key", &self.idempotency_key.is_some())
            .finish()
    }
}

struct ProductHttpResponse {
    status: StatusCode,
    content_type: String,
    body: Vec<u8>,
}

#[async_trait]
trait ProductHttpTransport: Send + Sync + 'static {
    async fn send(
        &self,
        request: ProductHttpRequest,
    ) -> Result<ProductHttpResponse, ProductInvocationError>;
}

struct ReqwestProductHttpTransport {
    client: Client,
}

fn build_http_client(
    connect_timeout: Duration,
    request_timeout: Duration,
    https_only: bool,
) -> Result<Client, ProductInvocationError> {
    Client::builder()
        .https_only(https_only)
        .no_proxy()
        .connect_timeout(connect_timeout)
        .timeout(request_timeout)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| ProductInvocationError::Unavailable)
}

fn validate_endpoint_owners(
    configs: &[ProductEndpointConfig],
    is_pinned: impl Fn(&str) -> bool,
) -> Result<(), ProductInvocationError> {
    if configs.iter().any(|config| !is_pinned(config.owner_id())) {
        return Err(ProductInvocationError::Unavailable);
    }
    Ok(())
}

fn validate_request_body(body: Option<&[u8]>) -> Result<(), ProductInvocationError> {
    if body.is_some_and(|body| body.len() > MAX_JSON_BODY_BYTES) {
        return Err(ProductInvocationError::InvalidRequest);
    }
    Ok(())
}

fn validate_http_response(
    status: StatusCode,
    content_type: &str,
    body: &[u8],
) -> Result<(), ProductInvocationError> {
    if !valid_status(status)
        || body.len() > MAX_JSON_BODY_BYTES
        || (!body.is_empty() && content_type.is_empty())
        || (!content_type.is_empty() && !valid_content_type(content_type))
        || (!body.is_empty() && parse_json_bytes_with_limit(body, MAX_JSON_BODY_BYTES).is_err())
    {
        return Err(ProductInvocationError::Internal);
    }
    Ok(())
}

#[async_trait]
impl ProductHttpTransport for ReqwestProductHttpTransport {
    async fn send(
        &self,
        request: ProductHttpRequest,
    ) -> Result<ProductHttpResponse, ProductInvocationError> {
        let mut builder = self
            .client
            .request(request.method, request.url)
            .header(AUTHORIZATION, request.authorization)
            .header(ACCEPT, HeaderValue::from_static(JSON_ACCEPT));
        if let Some(body) = request.body {
            builder = builder.header(CONTENT_TYPE, "application/json").body(body);
        }
        if let Some(idempotency_key) = request.idempotency_key {
            builder = builder.header("Idempotency-Key", idempotency_key);
        }

        let mut response = builder
            .send()
            .await
            .map_err(|_| ProductInvocationError::Unavailable)?;
        let status = response.status();
        if !valid_status(status)
            || response
                .content_length()
                .is_some_and(|length| length > MAX_JSON_BODY_BYTES as u64)
        {
            return Err(ProductInvocationError::Internal);
        }
        let content_type = match response.headers().get(CONTENT_TYPE) {
            Some(value) => value
                .to_str()
                .ok()
                .and_then(parse_content_type)
                .ok_or(ProductInvocationError::Internal)?,
            None => String::new(),
        };
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| ProductInvocationError::Unavailable)?
        {
            if body.len().saturating_add(chunk.len()) > MAX_JSON_BODY_BYTES {
                return Err(ProductInvocationError::Internal);
            }
            body.extend_from_slice(&chunk);
        }
        Ok(ProductHttpResponse {
            status,
            content_type,
            body,
        })
    }
}

fn parse_method(value: &str) -> Result<Method, ProductInvocationError> {
    match value {
        "GET" => Ok(Method::GET),
        "POST" => Ok(Method::POST),
        "PUT" => Ok(Method::PUT),
        "PATCH" => Ok(Method::PATCH),
        "DELETE" => Ok(Method::DELETE),
        _ => Err(ProductInvocationError::InvalidRequest),
    }
}

fn expand_path_template(
    base_url: &Url,
    template: &str,
    path_parameters: &BTreeMap<String, String>,
) -> Result<Url, ProductInvocationError> {
    if !template.starts_with('/')
        || template.starts_with("//")
        || template
            .chars()
            .any(|character| matches!(character, '?' | '#' | '\\'))
        || template.len() > 4096
    {
        return Err(ProductInvocationError::InvalidRequest);
    }

    let mut values = path_parameters.clone();
    let mut url = base_url.clone();
    let mut segments = url
        .path_segments_mut()
        .map_err(|_| ProductInvocationError::Unavailable)?;
    segments.clear();
    for template_segment in template[1..].split('/') {
        if template_segment.is_empty() || template_segment == "." || template_segment == ".." {
            return Err(ProductInvocationError::InvalidRequest);
        }
        if let Some(parameter) = template_segment
            .strip_prefix('{')
            .and_then(|segment| segment.strip_suffix('}'))
        {
            if parameter.is_empty() || parameter.contains(['{', '}']) {
                return Err(ProductInvocationError::InvalidRequest);
            }
            let value = values
                .remove(parameter)
                .ok_or(ProductInvocationError::InvalidRequest)?;
            if value.is_empty()
                || value.len() > 512
                || value.chars().any(char::is_control)
                || value == "."
                || value == ".."
                || value
                    .chars()
                    .any(|character| matches!(character, '/' | '\\' | '%' | '?' | '#'))
            {
                return Err(ProductInvocationError::InvalidRequest);
            }
            segments.push(&value);
        } else {
            if template_segment
                .chars()
                .any(|character| matches!(character, '{' | '}' | '%'))
                || template_segment.chars().any(char::is_control)
            {
                return Err(ProductInvocationError::InvalidRequest);
            }
            segments.push(template_segment);
        }
    }
    drop(segments);
    if !values.is_empty() {
        return Err(ProductInvocationError::InvalidRequest);
    }
    Ok(url)
}

fn parse_idempotency_key(value: &str) -> Result<HeaderValue, ProductInvocationError> {
    if value.is_empty()
        || value.len() > MAX_IDEMPOTENCY_KEY_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(ProductInvocationError::InvalidRequest);
    }
    let mut header =
        HeaderValue::from_str(value).map_err(|_| ProductInvocationError::InvalidRequest)?;
    header.set_sensitive(true);
    Ok(header)
}

fn valid_status(status: StatusCode) -> bool {
    status.is_success() || status.is_client_error() || status.is_server_error()
}

fn valid_content_type(value: &str) -> bool {
    parse_content_type(value).is_some()
}

fn parse_content_type(value: &str) -> Option<String> {
    let media_type = value.split(';').next()?.trim();
    if media_type.eq_ignore_ascii_case("application/json") {
        Some("application/json".to_owned())
    } else if media_type.eq_ignore_ascii_case("application/problem+json") {
        Some("application/problem+json".to_owned())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::net::TcpListener;
    use std::thread;

    use super::*;

    fn endpoint(owner_id: &str) -> ProductEndpointConfig {
        ProductEndpointConfig::new(
            owner_id,
            "org-1",
            "workspace-1",
            "https://product.example.test/",
            "product-service-private-token-value-0123456789",
        )
    }

    #[test]
    fn endpoint_owner_must_exist_in_the_pinned_release_catalog() {
        let configs = vec![endpoint("catalyst")];
        assert_eq!(
            validate_endpoint_owners(&configs, |owner| owner == "catalyst"),
            Ok(())
        );
        assert_eq!(
            validate_endpoint_owners(&configs, |owner| owner == "echo"),
            Err(ProductInvocationError::Unavailable)
        );
    }

    #[test]
    fn request_and_response_bodies_enforce_the_adapter_size_limit() {
        let oversized = vec![b'x'; MAX_JSON_BODY_BYTES + 1];
        assert_eq!(validate_request_body(None), Ok(()));
        assert_eq!(validate_request_body(Some(b"{}")), Ok(()));
        assert_eq!(
            validate_request_body(Some(oversized.as_slice())),
            Err(ProductInvocationError::InvalidRequest)
        );
        assert_eq!(
            validate_http_response(StatusCode::OK, "application/json", &oversized),
            Err(ProductInvocationError::Internal)
        );
        assert_eq!(
            validate_http_response(StatusCode::OK, "application/json", b"{}"),
            Ok(())
        );
        assert_eq!(
            validate_http_response(
                StatusCode::OK,
                "application/json",
                br#"{"scope":{"workspaceId":"workspace-1","workspaceId":"workspace-2"}}"#,
            ),
            Err(ProductInvocationError::Internal)
        );
    }

    #[tokio::test]
    async fn request_timeout_is_mapped_to_unavailable() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local HTTP fixture");
        let address = listener.local_addr().expect("read local fixture address");
        let server = thread::spawn(move || {
            let (mut connection, _) = listener.accept().expect("accept local HTTP request");
            thread::sleep(Duration::from_millis(150));
            let _ = connection.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
            );
        });
        let client = build_http_client(Duration::from_secs(1), Duration::from_millis(30), false)
            .expect("create bounded test client");
        let transport = ReqwestProductHttpTransport { client };
        let result = transport
            .send(ProductHttpRequest {
                owner_id: "catalyst".to_owned(),
                method: Method::GET,
                url: Url::parse(&format!("http://{address}/")).unwrap(),
                authorization: HeaderValue::from_static("Bearer fixture-token"),
                body: None,
                idempotency_key: None,
            })
            .await;
        server.join().expect("finish local HTTP fixture");
        assert!(matches!(result, Err(ProductInvocationError::Unavailable)));
    }

    #[test]
    fn catalog_route_template_uses_only_platform_bound_path_parameters() {
        let base = Url::parse("https://product.example.test/").unwrap();
        let mut parameters = BTreeMap::new();
        parameters.insert("organizationId".to_owned(), "org-1".to_owned());
        parameters.insert("workspaceId".to_owned(), "workspace-1".to_owned());
        parameters.insert("sessionId".to_owned(), "session-1".to_owned());

        let url = expand_path_template(
            &base,
            "/internal/{organizationId}/workspaces/{workspaceId}/sessions/{sessionId}",
            &parameters,
        )
        .expect("all catalog path parameters are bound");

        assert_eq!(
            url.as_str(),
            "https://product.example.test/internal/org-1/workspaces/workspace-1/sessions/session-1"
        );
    }

    #[test]
    fn catalog_route_cannot_select_a_host_or_leave_parameters_unbound() {
        let base = Url::parse("https://product.example.test/").unwrap();
        let mut parameters = BTreeMap::new();
        parameters.insert("resourceId".to_owned(), "resource-1".to_owned());

        assert_eq!(
            expand_path_template(&base, "https://attacker.test/{resourceId}", &parameters),
            Err(ProductInvocationError::InvalidRequest)
        );
        assert_eq!(
            expand_path_template(&base, "/internal/{workspaceId}", &parameters),
            Err(ProductInvocationError::InvalidRequest)
        );
        let resource_route = expand_path_template(&base, "/internal/{resourceId}", &parameters)
            .expect("catalog-declared resource ID is a bound route parameter");
        assert_eq!(
            resource_route.as_str(),
            "https://product.example.test/internal/resource-1"
        );
        assert_eq!(
            expand_path_template(&base, "/internal", &parameters),
            Err(ProductInvocationError::InvalidRequest)
        );
    }

    #[test]
    fn resource_path_parameters_cannot_escape_their_single_segment() {
        let base = Url::parse("https://product.example.test/").unwrap();
        let mut parameters = BTreeMap::new();
        parameters.insert("resourceId".to_owned(), "../other-resource".to_owned());

        assert_eq!(
            expand_path_template(&base, "/internal/{resourceId}", &parameters),
            Err(ProductInvocationError::InvalidRequest)
        );
    }
}
