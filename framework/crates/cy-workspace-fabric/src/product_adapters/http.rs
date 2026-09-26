//! Product-owned HTTP transport for Workspace Product projections.
//!
//! This module accepts only server-configured HTTPS endpoints and typed route
//! targets. Product credentials, request bodies, and response bodies are never
//! included in adapter diagnostics. / 本模块仅接受服务端配置的 HTTPS 端点与类型化路由。

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use cy_proto::workspace_v1::{WorkspaceProductApiContentType, WorkspaceProductApiOwner};
use reqwest::header::{HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use reqwest::{Client, Method, StatusCode, Url};

use crate::{
    product_projection::validate_product_response, ProductInvocationError,
    ProductInvocationRequest, ProductInvocationResponse, PRODUCT_JSON_BODY_MAX_BYTES,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_IDEMPOTENCY_KEY_CHARS: usize = 200;
const MAX_PATH_SEGMENT_BYTES: usize = 512;
const MAX_SERVICE_CREDENTIAL_BYTES: usize = 4096;
const JSON_ACCEPT: &str = "application/json, application/problem+json";

/// HTTP verb selected by an internal Product operation mapping.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ProductHttpMethod {
    Get,
    Post,
}

impl ProductHttpMethod {
    fn as_reqwest(self) -> Method {
        match self {
            Self::Get => Method::GET,
            Self::Post => Method::POST,
        }
    }

    fn matches_target(
        self,
        target: &ProductHttpTarget,
        request: &ProductInvocationRequest,
    ) -> bool {
        use cy_proto::workspace_v1::WorkspaceProductApiRequestKind as Kind;
        use cy_proto::workspace_v1::{
            WorkspaceProductApiOperation as Operation, WorkspaceProductApiOwner as Owner,
        };

        matches!(
            (self, request.kind),
            (Self::Get, Kind::Read) | (Self::Post, Kind::Command)
        ) || (self == Self::Post
            && request.kind == Kind::Read
            && request.owner == Owner::Navigator
            && request.operation == Operation::WorkspaceProductApiOperation11
            && matches!(
                target.path_segments.as_slice(),
                [
                    ProductHttpPathSegment::Static("api"),
                    ProductHttpPathSegment::Static("v1"),
                    ProductHttpPathSegment::Static("workspace-snapshots")
                ]
            ))
    }
}

impl fmt::Debug for ProductHttpMethod {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Get => "GET",
            Self::Post => "POST",
        })
    }
}

/// One segment in a route written by an owner-specific adapter.
#[derive(Clone, PartialEq, Eq)]
pub(super) enum ProductHttpPathSegment {
    /// A fixed segment from the Product OpenAPI path.
    Static(&'static str),
    /// A validated identifier occupying exactly one path segment.
    Resource(String),
}

impl ProductHttpPathSegment {
    /// Creates a resource segment after rejecting URL delimiters and controls.
    pub(super) fn resource(value: &str) -> Result<Self, ProductInvocationError> {
        if !valid_segment(value) {
            return Err(ProductInvocationError::InvalidRequest);
        }
        Ok(Self::Resource(value.to_owned()))
    }

    fn value(&self) -> &str {
        match self {
            Self::Static(value) => value,
            Self::Resource(value) => value,
        }
    }

    #[cfg(test)]
    pub(super) fn test_value(&self) -> &str {
        self.value()
    }
}

impl fmt::Debug for ProductHttpPathSegment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Static(_) => "Static(<fixed>)",
            Self::Resource(_) => "Resource(<redacted>)",
        })
    }
}

/// A private, allowlisted owner route with no caller-selectable URL or method.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct ProductHttpTarget {
    owner: WorkspaceProductApiOwner,
    method: ProductHttpMethod,
    path_segments: Vec<ProductHttpPathSegment>,
}

impl ProductHttpTarget {
    /// Builds a route only from a fixed `/api/v1` Product path and safe segments.
    pub(super) fn new(
        owner: WorkspaceProductApiOwner,
        method: ProductHttpMethod,
        path_segments: Vec<ProductHttpPathSegment>,
    ) -> Result<Self, ProductInvocationError> {
        if !allowlisted_owner(owner)
            || path_segments.len() < 3
            || !matches!(
                path_segments.first(),
                Some(ProductHttpPathSegment::Static("api"))
            )
            || !matches!(
                path_segments.get(1),
                Some(ProductHttpPathSegment::Static("v1"))
            )
            || path_segments
                .iter()
                .any(|segment| !valid_segment(segment.value()))
        {
            return Err(ProductInvocationError::InvalidRequest);
        }

        Ok(Self {
            owner,
            method,
            path_segments,
        })
    }

    #[cfg(test)]
    pub(super) fn owner(&self) -> WorkspaceProductApiOwner {
        self.owner
    }

    #[cfg(test)]
    pub(super) fn method(&self) -> ProductHttpMethod {
        self.method
    }

    #[cfg(test)]
    pub(super) fn path_segments(&self) -> &[ProductHttpPathSegment] {
        &self.path_segments
    }

    fn build_url(&self, base_url: &Url) -> Result<Url, ProductInvocationError> {
        let mut url = base_url.clone();
        let mut path_segments = url
            .path_segments_mut()
            .map_err(|_| ProductInvocationError::Unavailable)?;
        path_segments.clear();
        for segment in &self.path_segments {
            path_segments.push(segment.value());
        }
        drop(path_segments);
        Ok(url)
    }
}

impl fmt::Debug for ProductHttpTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProductHttpTarget")
            .field("owner", &self.owner)
            .field("method", &self.method)
            .field("path_segment_count", &self.path_segments.len())
            .finish()
    }
}

/// Private endpoint input supplied by server startup configuration.
///
/// The credential is consumed into a sensitive header during resolver setup;
/// it is not serializable and its Debug output is redacted.
pub struct ProductEndpointConfig {
    owner: WorkspaceProductApiOwner,
    base_url: String,
    service_credential: String,
}

impl ProductEndpointConfig {
    /// Creates a private server-side Product endpoint configuration.
    ///
    /// Configure the URL to an HTTPS hosting gateway that validates this
    /// service credential. Current Product OpenAPI and server routes do not
    /// establish bearer verification or Workspace-user attribution themselves.
    pub fn new(
        owner: WorkspaceProductApiOwner,
        base_url: impl Into<String>,
        service_credential: impl Into<String>,
    ) -> Self {
        Self {
            owner,
            base_url: base_url.into(),
            service_credential: service_credential.into(),
        }
    }
}

impl fmt::Debug for ProductEndpointConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProductEndpointConfig")
            .field("owner", &self.owner)
            .field("base_url", &"<redacted>")
            .field("service_credential", &"<redacted>")
            .finish()
    }
}

#[derive(Clone)]
pub(super) struct ProductEndpoint {
    base_url: Url,
    authorization: HeaderValue,
}

impl ProductEndpoint {
    fn from_config(config: ProductEndpointConfig) -> Result<Self, ProductInvocationError> {
        let base_url =
            Url::parse(&config.base_url).map_err(|_| ProductInvocationError::Unavailable)?;
        if base_url.scheme() != "https"
            || base_url.host_str().is_none()
            || base_url.username() != ""
            || base_url.password().is_some()
            || base_url.path() != "/"
            || base_url.query().is_some()
            || base_url.fragment().is_some()
            || config.service_credential.is_empty()
            || config.service_credential.len() > MAX_SERVICE_CREDENTIAL_BYTES
            || config.service_credential.trim() != config.service_credential
        {
            return Err(ProductInvocationError::Unavailable);
        }

        let mut authorization =
            HeaderValue::from_str(&format!("Bearer {}", config.service_credential))
                .map_err(|_| ProductInvocationError::Unavailable)?;
        authorization.set_sensitive(true);

        Ok(Self {
            base_url,
            authorization,
        })
    }
}

impl fmt::Debug for ProductEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProductEndpoint")
            .field("base_url", &"<redacted>")
            .field("authorization", &"<redacted>")
            .finish()
    }
}

/// Resolves only typed Product owners to server-configured endpoints.
#[tonic::async_trait]
pub(super) trait ProductEndpointResolver: Send + Sync + 'static {
    async fn resolve(
        &self,
        owner: WorkspaceProductApiOwner,
    ) -> Result<ProductEndpoint, ProductInvocationError>;
}

pub(super) struct ConfiguredProductEndpointResolver {
    endpoints: Vec<(WorkspaceProductApiOwner, ProductEndpoint)>,
}

impl ConfiguredProductEndpointResolver {
    pub(super) fn new(configs: Vec<ProductEndpointConfig>) -> Result<Self, ProductInvocationError> {
        let mut endpoints = Vec::with_capacity(configs.len());
        for config in configs {
            if !allowlisted_owner(config.owner)
                || endpoints.iter().any(|(owner, _)| *owner == config.owner)
            {
                return Err(ProductInvocationError::Unavailable);
            }
            let owner = config.owner;
            endpoints.push((owner, ProductEndpoint::from_config(config)?));
        }
        Ok(Self { endpoints })
    }
}

#[tonic::async_trait]
impl ProductEndpointResolver for ConfiguredProductEndpointResolver {
    async fn resolve(
        &self,
        owner: WorkspaceProductApiOwner,
    ) -> Result<ProductEndpoint, ProductInvocationError> {
        self.endpoints
            .iter()
            .find(|(configured_owner, _)| *configured_owner == owner)
            .map(|(_, endpoint)| endpoint.clone())
            .ok_or(ProductInvocationError::Unavailable)
    }
}

/// Fully assembled HTTP request passed to the injected transport.
pub(super) struct ProductHttpRequest {
    owner: WorkspaceProductApiOwner,
    method: ProductHttpMethod,
    url: Url,
    authorization: HeaderValue,
    body: Vec<u8>,
    idempotency_key: Option<HeaderValue>,
}

impl ProductHttpRequest {
    #[cfg(test)]
    pub(super) fn owner(&self) -> WorkspaceProductApiOwner {
        self.owner
    }

    #[cfg(test)]
    pub(super) fn method(&self) -> ProductHttpMethod {
        self.method
    }

    #[cfg(test)]
    pub(super) fn url(&self) -> &Url {
        &self.url
    }

    #[cfg(test)]
    pub(super) fn body(&self) -> &[u8] {
        &self.body
    }

    #[cfg(test)]
    pub(super) fn idempotency_key(&self) -> Option<&str> {
        self.idempotency_key
            .as_ref()
            .and_then(|value| value.to_str().ok())
    }

    fn authorization(&self) -> &HeaderValue {
        &self.authorization
    }
}

impl fmt::Debug for ProductHttpRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProductHttpRequest")
            .field("owner", &self.owner)
            .field("method", &self.method)
            .field("url", &"<redacted>")
            .field("authorization", &"<redacted>")
            .field("body_bytes", &self.body.len())
            .field("has_idempotency_key", &self.idempotency_key.is_some())
            .finish()
    }
}

/// Bounded JSON response read by the injected transport.
pub(super) struct ProductHttpResponse {
    status_code: u16,
    content_type: WorkspaceProductApiContentType,
    body: Vec<u8>,
}

impl ProductHttpResponse {
    #[cfg(test)]
    pub(super) fn new(
        status_code: u16,
        content_type: WorkspaceProductApiContentType,
        body: Vec<u8>,
    ) -> Self {
        Self {
            status_code,
            content_type,
            body,
        }
    }
}

/// HTTP I/O boundary, injectable for deterministic adapter tests.
#[tonic::async_trait]
pub(super) trait ProductHttpTransport: Send + Sync + 'static {
    async fn send(
        &self,
        request: ProductHttpRequest,
    ) -> Result<ProductHttpResponse, ProductInvocationError>;
}

struct ReqwestProductHttpTransport {
    client: Client,
}

impl ReqwestProductHttpTransport {
    fn new() -> Result<Self, ProductInvocationError> {
        let client = Client::builder()
            .https_only(true)
            .no_proxy()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| ProductInvocationError::Unavailable)?;
        Ok(Self { client })
    }
}

#[tonic::async_trait]
impl ProductHttpTransport for ReqwestProductHttpTransport {
    async fn send(
        &self,
        request: ProductHttpRequest,
    ) -> Result<ProductHttpResponse, ProductInvocationError> {
        let mut builder = self
            .client
            .request(request.method.as_reqwest(), request.url.clone())
            .header(AUTHORIZATION, request.authorization().clone())
            .header(ACCEPT, HeaderValue::from_static(JSON_ACCEPT))
            .body(request.body.clone());
        if !request.body.is_empty() {
            builder = builder.header(CONTENT_TYPE, "application/json");
        }
        if let Some(idempotency_key) = &request.idempotency_key {
            builder = builder.header("Idempotency-Key", idempotency_key.clone());
        }

        let mut response = builder
            .send()
            .await
            .map_err(|_| ProductInvocationError::Unavailable)?;
        let status = response.status();
        if !valid_status(status) {
            return Err(ProductInvocationError::Internal);
        }
        let content_type = parse_content_type(response.headers())?;
        if response
            .content_length()
            .is_some_and(|length| length > PRODUCT_JSON_BODY_MAX_BYTES as u64)
        {
            return Err(ProductInvocationError::Internal);
        }

        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| ProductInvocationError::Unavailable)?
        {
            if body.len().saturating_add(chunk.len()) > PRODUCT_JSON_BODY_MAX_BYTES {
                return Err(ProductInvocationError::Internal);
            }
            body.extend_from_slice(&chunk);
        }

        Ok(ProductHttpResponse {
            status_code: status.as_u16(),
            content_type,
            body,
        })
    }
}

/// Shared HTTP client for owner-specific Product operation adapters.
#[derive(Clone)]
pub struct ProductHttpClient {
    resolver: Arc<dyn ProductEndpointResolver>,
    transport: Arc<dyn ProductHttpTransport>,
}

impl ProductHttpClient {
    /// Builds a production client from private, server-side endpoint secrets.
    pub fn from_private_config(
        configs: Vec<ProductEndpointConfig>,
    ) -> Result<Self, ProductInvocationError> {
        let resolver = ConfiguredProductEndpointResolver::new(configs)?;
        let transport = ReqwestProductHttpTransport::new()?;
        Ok(Self {
            resolver: Arc::new(resolver),
            transport: Arc::new(transport),
        })
    }

    /// Creates a client with injected resolver and transport for in-process tests.
    #[cfg(test)]
    pub(super) fn with_transport(
        resolver: Arc<dyn ProductEndpointResolver>,
        transport: Arc<dyn ProductHttpTransport>,
    ) -> Self {
        Self {
            resolver,
            transport,
        }
    }

    /// Dispatches one mapped Product request through its fixed owner endpoint.
    pub(super) async fn send(
        &self,
        target: ProductHttpTarget,
        request: &ProductInvocationRequest,
    ) -> Result<ProductInvocationResponse, ProductInvocationError> {
        validate_request(&target, request)?;
        let endpoint = self.resolver.resolve(target.owner).await?;
        let url = target.build_url(&endpoint.base_url)?;
        let idempotency_key = request
            .idempotency_key
            .as_deref()
            .map(HeaderValue::from_str)
            .transpose()
            .map_err(|_| ProductInvocationError::InvalidRequest)?;
        let response = self
            .transport
            .send(ProductHttpRequest {
                owner: target.owner,
                method: target.method,
                url,
                authorization: endpoint.authorization,
                body: request.json_body.clone(),
                idempotency_key,
            })
            .await?;

        let validated = validate_product_response(ProductInvocationResponse {
            status_code: response.status_code,
            json_body: response.body,
            content_type: response.content_type,
        })?;
        let status_code =
            u16::try_from(validated.status_code).map_err(|_| ProductInvocationError::Internal)?;
        let content_type = WorkspaceProductApiContentType::try_from(validated.content_type)
            .map_err(|_| ProductInvocationError::Internal)?;
        Ok(ProductInvocationResponse {
            status_code,
            json_body: validated.json_body,
            content_type,
        })
    }
}

fn validate_request(
    target: &ProductHttpTarget,
    request: &ProductInvocationRequest,
) -> Result<(), ProductInvocationError> {
    if target.owner != request.owner || !target.method.matches_target(target, request) {
        return Err(ProductInvocationError::InvalidRequest);
    }
    if request.json_body.len() > PRODUCT_JSON_BODY_MAX_BYTES
        || (!request.json_body.is_empty()
            && serde_json::from_slice::<serde_json::Value>(&request.json_body).is_err())
    {
        return Err(ProductInvocationError::InvalidRequest);
    }
    if request.idempotency_key.as_ref().is_some_and(|key| {
        key.is_empty()
            || key.chars().count() > MAX_IDEMPOTENCY_KEY_CHARS
            || key.chars().any(char::is_control)
    }) {
        return Err(ProductInvocationError::InvalidRequest);
    }
    if target.method == ProductHttpMethod::Get
        && (!request.json_body.is_empty() || request.idempotency_key.is_some())
    {
        return Err(ProductInvocationError::InvalidRequest);
    }
    Ok(())
}

fn allowlisted_owner(owner: WorkspaceProductApiOwner) -> bool {
    use WorkspaceProductApiOwner as Owner;

    matches!(
        owner,
        Owner::Catalyst
            | Owner::Yield
            | Owner::Reactor
            | Owner::Exchange
            | Owner::Echo
            | Owner::Navigator
    )
}

fn valid_segment(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_PATH_SEGMENT_BYTES
        && value != "."
        && value != ".."
        && !value.chars().any(|character| {
            character.is_control() || matches!(character, '/' | '\\' | '%' | '?' | '#')
        })
}

fn valid_status(status: StatusCode) -> bool {
    (status.is_success() && !matches!(status, StatusCode::NO_CONTENT | StatusCode::RESET_CONTENT))
        || status.is_client_error()
        || status.is_server_error()
}

fn parse_content_type(
    headers: &reqwest::header::HeaderMap,
) -> Result<WorkspaceProductApiContentType, ProductInvocationError> {
    let header = headers
        .get(CONTENT_TYPE)
        .ok_or(ProductInvocationError::Internal)?
        .to_str()
        .map_err(|_| ProductInvocationError::Internal)?;
    let media_type = header.split(';').next().unwrap_or_default().trim();
    if media_type.eq_ignore_ascii_case("application/json") {
        Ok(WorkspaceProductApiContentType::ApplicationJson)
    } else if media_type.eq_ignore_ascii_case("application/problem+json") {
        Ok(WorkspaceProductApiContentType::ApplicationProblemJson)
    } else {
        Err(ProductInvocationError::Internal)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use cy_proto::workspace_v1::{
        WorkspaceProductApiOperation as Operation, WorkspaceProductApiRequestKind as Kind,
    };

    use super::*;

    struct RecordingTransport {
        requests: Mutex<Vec<ProductHttpRequest>>,
        response: Mutex<Option<ProductHttpResponse>>,
    }

    #[tonic::async_trait]
    impl ProductHttpTransport for RecordingTransport {
        async fn send(
            &self,
            request: ProductHttpRequest,
        ) -> Result<ProductHttpResponse, ProductInvocationError> {
            self.requests.lock().unwrap().push(request);
            self.response
                .lock()
                .unwrap()
                .take()
                .ok_or(ProductInvocationError::Unavailable)
        }
    }

    fn test_client(
        owner: WorkspaceProductApiOwner,
        response: ProductHttpResponse,
    ) -> (ProductHttpClient, Arc<RecordingTransport>) {
        let resolver = ConfiguredProductEndpointResolver::new(vec![ProductEndpointConfig::new(
            owner,
            "https://product.example.test/",
            "test-service-credential",
        )])
        .unwrap();
        let transport = Arc::new(RecordingTransport {
            requests: Mutex::new(Vec::new()),
            response: Mutex::new(Some(response)),
        });
        let client = ProductHttpClient::with_transport(Arc::new(resolver), transport.clone());
        (client, transport)
    }

    fn invocation(
        owner: WorkspaceProductApiOwner,
        operation: Operation,
        kind: Kind,
        resource_id: Option<&str>,
        json_body: &[u8],
        idempotency_key: Option<&str>,
    ) -> ProductInvocationRequest {
        ProductInvocationRequest {
            owner,
            operation,
            kind,
            resource_id: resource_id.map(str::to_owned),
            json_body: json_body.to_vec(),
            idempotency_key: idempotency_key.map(str::to_owned),
        }
    }

    fn datasets_target(
        owner: WorkspaceProductApiOwner,
        method: ProductHttpMethod,
    ) -> ProductHttpTarget {
        ProductHttpTarget::new(
            owner,
            method,
            vec![
                ProductHttpPathSegment::Static("api"),
                ProductHttpPathSegment::Static("v1"),
                ProductHttpPathSegment::Static("datasets"),
            ],
        )
        .unwrap()
    }

    #[tokio::test]
    async fn post_mapping_preserves_json_and_owner_idempotency_key() {
        let body = br#"{"name":"dataset","description":"owner data"}"#;
        let (client, transport) = test_client(
            WorkspaceProductApiOwner::Catalyst,
            ProductHttpResponse::new(
                201,
                WorkspaceProductApiContentType::ApplicationJson,
                br#"{"id":"dataset-1"}"#.to_vec(),
            ),
        );
        let request = invocation(
            WorkspaceProductApiOwner::Catalyst,
            Operation::WorkspaceProductApiOperation02,
            Kind::Command,
            None,
            body,
            Some("replay-1"),
        );

        let response = client
            .send(
                datasets_target(WorkspaceProductApiOwner::Catalyst, ProductHttpMethod::Post),
                &request,
            )
            .await
            .unwrap();

        assert_eq!(response.status_code, 201);
        let requests = transport.requests.lock().unwrap();
        let outbound = &requests[0];
        assert_eq!(outbound.owner(), WorkspaceProductApiOwner::Catalyst);
        assert_eq!(
            outbound.url().as_str(),
            "https://product.example.test/api/v1/datasets"
        );
        assert_eq!(outbound.method(), ProductHttpMethod::Post);
        assert_eq!(outbound.body(), body);
        assert_eq!(outbound.idempotency_key(), Some("replay-1"));
        let debug = format!("{outbound:?}");
        assert!(!debug.contains("test-service-credential"));
        assert!(!debug.contains("owner data"));
    }

    #[tokio::test]
    async fn navigator_snapshot_post_is_the_only_read_post_exception() {
        let (client, transport) = test_client(
            WorkspaceProductApiOwner::Navigator,
            ProductHttpResponse::new(
                200,
                WorkspaceProductApiContentType::ApplicationJson,
                br#"{"status":"COMPLETE"}"#.to_vec(),
            ),
        );
        let target = ProductHttpTarget::new(
            WorkspaceProductApiOwner::Navigator,
            ProductHttpMethod::Post,
            vec![
                ProductHttpPathSegment::Static("api"),
                ProductHttpPathSegment::Static("v1"),
                ProductHttpPathSegment::Static("workspace-snapshots"),
            ],
        )
        .unwrap();
        let request = invocation(
            WorkspaceProductApiOwner::Navigator,
            Operation::WorkspaceProductApiOperation11,
            Kind::Read,
            None,
            br#"{"workspaceId":"workspace-1","reads":[]}"#,
            None,
        );

        let response = client.send(target, &request).await.unwrap();

        assert_eq!(response.status_code, 200);
        assert_eq!(
            transport.requests.lock().unwrap()[0].url().as_str(),
            "https://product.example.test/api/v1/workspace-snapshots"
        );
    }

    #[tokio::test]
    async fn resource_segments_and_base_urls_cannot_select_another_route() {
        assert!(ProductHttpPathSegment::resource("dataset/other").is_err());
        assert!(ProductHttpPathSegment::resource("%2e%2e").is_err());
        assert!(ProductHttpTarget::new(
            WorkspaceProductApiOwner::Catalyst,
            ProductHttpMethod::Get,
            vec![
                ProductHttpPathSegment::Static("api"),
                ProductHttpPathSegment::Static("v1"),
                ProductHttpPathSegment::Static("datasets?admin=true"),
            ],
        )
        .is_err());

        assert!(
            ConfiguredProductEndpointResolver::new(vec![ProductEndpointConfig::new(
                WorkspaceProductApiOwner::Catalyst,
                "http://product.example.test/",
                "test-secret",
            )])
            .is_err()
        );
        assert!(
            ConfiguredProductEndpointResolver::new(vec![ProductEndpointConfig::new(
                WorkspaceProductApiOwner::Catalyst,
                "https://user:password@product.example.test/",
                "test-secret",
            )])
            .is_err()
        );
    }

    #[tokio::test]
    async fn oversized_request_or_response_fails_before_projection() {
        let (client, transport) = test_client(
            WorkspaceProductApiOwner::Catalyst,
            ProductHttpResponse::new(
                200,
                WorkspaceProductApiContentType::ApplicationJson,
                b"[]".to_vec(),
            ),
        );
        let large_request = vec![b' '; PRODUCT_JSON_BODY_MAX_BYTES + 1];
        let request = invocation(
            WorkspaceProductApiOwner::Catalyst,
            Operation::WorkspaceProductApiOperation02,
            Kind::Command,
            None,
            &large_request,
            None,
        );
        let target = datasets_target(WorkspaceProductApiOwner::Catalyst, ProductHttpMethod::Post);
        assert_eq!(
            client.send(target, &request).await.unwrap_err(),
            ProductInvocationError::InvalidRequest
        );
        assert!(transport.requests.lock().unwrap().is_empty());

        let (client, _) = test_client(
            WorkspaceProductApiOwner::Catalyst,
            ProductHttpResponse::new(
                200,
                WorkspaceProductApiContentType::ApplicationJson,
                vec![b' '; PRODUCT_JSON_BODY_MAX_BYTES + 1],
            ),
        );
        let request = invocation(
            WorkspaceProductApiOwner::Catalyst,
            Operation::WorkspaceProductApiOperation01,
            Kind::Read,
            None,
            b"",
            None,
        );
        assert_eq!(
            client
                .send(
                    datasets_target(WorkspaceProductApiOwner::Catalyst, ProductHttpMethod::Get),
                    &request,
                )
                .await
                .unwrap_err(),
            ProductInvocationError::Internal
        );
    }

    #[test]
    fn private_endpoint_debug_never_contains_service_credential() {
        let config = ProductEndpointConfig::new(
            WorkspaceProductApiOwner::Echo,
            "https://echo.example.test/",
            "distinctive-secret-value",
        );
        assert!(!format!("{config:?}").contains("distinctive-secret-value"));
    }
}
