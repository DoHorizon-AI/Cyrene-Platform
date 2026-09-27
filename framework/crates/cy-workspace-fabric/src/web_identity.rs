//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 web_identity.rs                                                 │
//! │  Module: cy_workspace_fabric::web_identity                          │
//! │  Role: Verify Azure AD access tokens and bind identities via Directory.│
//! │                                                                     │
//! │  模块职责：验证 Azure AD access token，并通过 Directory 绑定身份。       │
//! └─────────────────────────────────────────────────────────────────────┘

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use futures_util::StreamExt;
use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use thiserror::Error;
use tokio::{sync::Mutex, time::Instant};
use tonic::async_trait;
use uuid::Uuid;

use cy_proto::workspace_v1::UserIdentityRef;

const MAX_ACCESS_TOKEN_BYTES: usize = 16 * 1024;
const MAX_TOKEN_HEADER_BYTES: usize = 4 * 1024;
const MAX_DISCOVERY_BYTES: usize = 64 * 1024;
const MAX_JWKS_BYTES: usize = 256 * 1024;
const MAX_JWKS_KEYS: usize = 128;
const MAX_KID_BYTES: usize = 128;
const MAX_RSA_MODULUS_BYTES: usize = 512;
const MIN_RSA_MODULUS_BITS: u32 = 2048;
const MAX_RSA_MODULUS_BITS: u32 = 4096;
const MAX_CLOCK_SKEW_SECONDS: u64 = 30;
const JWKS_CACHE_TTL: Duration = Duration::from_secs(15 * 60);
const UNKNOWN_KID_REFRESH_COOLDOWN: Duration = Duration::from_secs(30);
const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const REQUIRED_DELEGATED_SCOPE: &str = "Workspace.Web.Access";
const AZURE_LOGIN_HOST: &str = "login.microsoftonline.com";

/// Safe, stable identity-verification failure categories.
///
/// Error text never contains a bearer token, claim value, key material, or
/// Directory implementation detail. HTTP adapters should use [`Self::http_status`]
/// and return their own closed Problem response.
///
/// 安全且稳定的身份验证错误分类。错误文本不包含 bearer token、claim、密钥材料或 Directory 实现细节。
#[derive(Debug, Error, Clone, Copy, Eq, PartialEq)]
pub enum WebIdentityError {
    /// Token syntax, signature, configured issuer/audience, or required claims are invalid.
    #[error("web identity token is invalid")]
    InvalidToken,
    /// A valid token lacks the required scope or has no unique organization mapping.
    #[error("web identity is not authorized")]
    Forbidden,
    /// Trusted metadata or the authoritative identity Directory is unavailable.
    #[error("web identity dependency is unavailable")]
    Unavailable,
    /// Trusted startup configuration is invalid or the verifier could not initialize.
    #[error("web identity verifier is not configured")]
    Configuration,
}

impl WebIdentityError {
    /// Returns the contract HTTP status for this failure category.
    ///
    /// Invalid credentials map to 401, valid credentials without authority map to 403,
    /// dependency failures map to 503, and startup configuration failures map to 500.
    ///
    /// 返回契约规定的 HTTP 状态码。
    pub const fn http_status(self) -> u16 {
        match self {
            Self::InvalidToken => 401,
            Self::Forbidden => 403,
            Self::Unavailable => 503,
            Self::Configuration => 500,
        }
    }
}

/// Directory failure returned by the narrow OIDC identity binding port.
///
/// Implementations should query the authoritative identity-to-organization relation and
/// collapse storage diagnostics into this generic failure before returning to the verifier.
///
/// 窄 OIDC 身份绑定端口返回的 Directory 故障；存储诊断应在返回前归并为通用错误。
#[derive(Debug, Error, Clone, Copy, Eq, PartialEq)]
pub enum WebIdentityDirectoryError {
    /// No organization is bound to the verified issuer and subject.
    #[error("identity has no organization mapping")]
    MissingMapping,
    /// More than one organization is bound to the verified issuer and subject.
    #[error("identity organization mapping is ambiguous")]
    AmbiguousMapping,
    /// The authoritative mapping store could not answer the lookup.
    #[error("identity Directory is unavailable")]
    Unavailable,
}

/// Authoritative mapping from a verified OIDC issuer and subject to organization candidates.
///
/// The implementation must query canonical Directory state. The verifier accepts only one
/// nonblank organization result; zero or multiple results fail closed. Claims such as `tid`,
/// `oid`, email, and browser-supplied organization values are not mapping inputs.
///
/// 将已验证的 OIDC issuer 与 subject 映射到候选组织的权威端口。只接受唯一且非空的组织结果。
#[async_trait]
pub trait WebIdentityDirectory: Send + Sync {
    /// Looks up organization candidates for the exact verified issuer and subject pair.
    async fn organizations_for_verified_identity(
        &self,
        identity: &UserIdentityRef,
    ) -> Result<Vec<String>, WebIdentityDirectoryError>;
}

/// Immutable principal produced only after signature, claims, scope, and Directory checks pass.
///
/// Read-only getters keep callers from changing the verified identity or organization mapping.
///
/// 仅在签名、claims、scope 与 Directory 检查全部通过后生成的不可变主体。
#[derive(Clone, Debug)]
pub struct VerifiedWebPrincipal {
    identity: UserIdentityRef,
    organization_id: String,
    expires_at_unix_ms: i64,
}

impl VerifiedWebPrincipal {
    /// Returns the OIDC issuer and subject from the verified access token.
    ///
    /// 返回已验证 access token 中的 OIDC issuer 与 subject。
    pub fn identity(&self) -> &UserIdentityRef {
        &self.identity
    }

    /// Returns the unique organization resolved by the server-side Directory.
    ///
    /// 返回服务端 Directory 唯一解析出的组织。
    pub fn organization_id(&self) -> &str {
        &self.organization_id
    }

    /// Returns the signed access-token expiry as Unix milliseconds.
    ///
    /// 返回签名 access token 的 Unix 毫秒过期时间。
    pub const fn expires_at_unix_ms(&self) -> i64 {
        self.expires_at_unix_ms
    }
}

#[cfg(test)]
pub(crate) fn test_principal(
    identity: UserIdentityRef,
    organization_id: impl Into<String>,
    expires_at_unix_ms: i64,
) -> VerifiedWebPrincipal {
    VerifiedWebPrincipal {
        identity,
        organization_id: organization_id.into(),
        expires_at_unix_ms,
    }
}

/// Verifies a private-ingress access token and returns a Directory-bound web principal.
///
/// Implementations must receive only the server-side Easy Auth access-token value. They must
/// never accept an unsigned `X-MS-CLIENT-PRINCIPAL` value as identity evidence.
///
/// 验证私有入口的 access token，并返回绑定 Directory 的 Web 主体；不得信任未签名 principal header。
#[async_trait]
pub trait WebPrincipalVerifier: Send + Sync {
    /// Verifies a bearer token against trusted startup configuration and authoritative Directory.
    async fn verify_access_token(
        &self,
        access_token: &str,
    ) -> Result<VerifiedWebPrincipal, WebIdentityError>;
}

/// Fixed-tenant Azure AD v2 configuration for the web access-token verifier.
///
/// The issuer, discovery URL, and JWKS URL are derived from this trusted tenant UUID. The token
/// cannot select a tenant, issuer, audience, algorithm, discovery endpoint, or key endpoint.
///
/// Web access-token verifier 的固定 tenant Azure AD v2 配置；所有 URL 均由受信任 tenant 派生。
#[derive(Clone, Debug)]
pub struct AzureAdWebIdentityConfig {
    tenant_id: Uuid,
    audience: String,
}

impl AzureAdWebIdentityConfig {
    /// Creates fixed issuer and audience configuration.
    ///
    /// Audience must be a trusted API application ID or resource URI, not a request value.
    ///
    /// 创建固定 issuer 与 audience 配置；audience 必须来自可信 API 配置。
    pub fn new(tenant_id: Uuid, audience: impl Into<String>) -> Result<Self, WebIdentityError> {
        let audience = audience.into();
        if tenant_id.is_nil()
            || audience.is_empty()
            || audience.len() > 512
            || audience.trim() != audience
            || audience
                .chars()
                .any(|character| character.is_whitespace() || character.is_control())
        {
            return Err(WebIdentityError::Configuration);
        }

        Ok(Self {
            tenant_id,
            audience,
        })
    }

    fn tenant_id_string(&self) -> String {
        self.tenant_id.to_string()
    }

    fn issuer(&self) -> String {
        format!("https://{AZURE_LOGIN_HOST}/{}/v2.0", self.tenant_id)
    }

    fn discovery_url(&self) -> String {
        format!(
            "https://{AZURE_LOGIN_HOST}/{}/v2.0/.well-known/openid-configuration",
            self.tenant_id
        )
    }

    fn jwks_url(&self) -> String {
        format!(
            "https://{AZURE_LOGIN_HOST}/{}/discovery/v2.0/keys",
            self.tenant_id
        )
    }
}

/// Azure AD access-token verifier with pinned metadata discovery and bounded JWKS caching.
///
/// Discovery and key requests use fixed HTTPS Microsoft endpoints, refuse redirects, enforce
/// request/body limits, and refresh expired caches before any key can be used.
///
/// 使用固定 HTTPS 元数据发现并对 JWKS 缓存设置上限的 Azure AD access-token verifier。
pub struct AzureAdWebPrincipalVerifier {
    config: AzureAdWebIdentityConfig,
    directory: Arc<dyn WebIdentityDirectory>,
    fetcher: Arc<dyn JwksFetcher>,
    key_cache: Mutex<KeyCache>,
    cache_ttl: Duration,
}

impl AzureAdWebPrincipalVerifier {
    /// Creates a verifier using the fixed Microsoft tenant discovery and JWKS endpoints.
    ///
    /// This constructor creates a no-redirect HTTPS client with strict timeouts. It does not
    /// provision Easy Auth token store or the `Workspace.Web.Access` API scope.
    ///
    /// 使用固定 Microsoft tenant 元数据端点创建 verifier；此构造器不会配置 Easy Auth token store 或 API scope。
    pub fn new(
        config: AzureAdWebIdentityConfig,
        directory: Arc<dyn WebIdentityDirectory>,
    ) -> Result<Self, WebIdentityError> {
        let client = reqwest::Client::builder()
            .https_only(true)
            .connect_timeout(HTTP_CONNECT_TIMEOUT)
            .timeout(HTTP_REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("cyrene-workspace-web-bff/1")
            .build()
            .map_err(|_| WebIdentityError::Configuration)?;

        Ok(Self::with_fetcher(
            config,
            directory,
            Arc::new(HttpJwksFetcher { client }),
            JWKS_CACHE_TTL,
        ))
    }

    fn with_fetcher(
        config: AzureAdWebIdentityConfig,
        directory: Arc<dyn WebIdentityDirectory>,
        fetcher: Arc<dyn JwksFetcher>,
        cache_ttl: Duration,
    ) -> Self {
        Self {
            config,
            directory,
            fetcher,
            key_cache: Mutex::new(KeyCache::default()),
            cache_ttl,
        }
    }

    /// Resolves a token key from the current cache or refreshes the pinned tenant JWKS.
    async fn key_for(&self, kid: &str) -> Result<JwkDocument, WebIdentityError> {
        let issuer = self.config.issuer();
        let discovery_url = self.config.discovery_url();
        let expected_jwks_url = self.config.jwks_url();
        let mut cache = self.key_cache.lock().await;
        let cached_is_fresh = cache
            .keys
            .as_ref()
            .is_some_and(|keys| keys.fetched_at.elapsed() < self.cache_ttl);

        if cached_is_fresh {
            if let Some(key) = cache.keys.as_ref().and_then(|keys| keys.entries.get(kid)) {
                return Ok(key.clone());
            }

            let refresh_is_throttled = cache
                .last_unknown_kid_refresh
                .is_some_and(|time| time.elapsed() < UNKNOWN_KID_REFRESH_COOLDOWN);
            if refresh_is_throttled {
                return Err(WebIdentityError::InvalidToken);
            }

            cache.last_unknown_kid_refresh = Some(Instant::now());
        }

        // Expired key material is never used when refresh fails.
        let jwks = self
            .fetcher
            .fetch_jwks(&discovery_url, &issuer, &expected_jwks_url)
            .await?;
        let entries = parse_jwks(&jwks)?;
        let requested_key = entries.get(kid).cloned();
        cache.keys = Some(CachedKeys {
            fetched_at: Instant::now(),
            entries,
        });

        if requested_key.is_none() {
            cache.last_unknown_kid_refresh = Some(Instant::now());
        } else {
            cache.last_unknown_kid_refresh = None;
        }

        requested_key.ok_or(WebIdentityError::InvalidToken)
    }

    /// Authenticates the signed token before using any identity claims for Directory lookup.
    async fn verify(&self, access_token: &str) -> Result<VerifiedWebPrincipal, WebIdentityError> {
        let header = parse_token_header(access_token)?;
        let jwk = self.key_for(&header.kid).await?;
        let decoding_key = decoding_key_for(&jwk, &self.config.issuer())?;

        let issuer = self.config.issuer();
        let mut validation = Validation::new(Algorithm::RS256);
        validation.algorithms = vec![Algorithm::RS256];
        validation.leeway = MAX_CLOCK_SKEW_SECONDS;
        validation.validate_exp = true;
        validation.validate_nbf = true;
        validation.validate_aud = true;
        validation.set_audience(&[self.config.audience.as_str()]);
        validation.set_issuer(&[issuer.as_str()]);
        validation.required_spec_claims = ["exp", "nbf", "iss", "aud", "sub"]
            .into_iter()
            .map(str::to_owned)
            .collect();

        let claims = decode::<AccessTokenClaims>(access_token, &decoding_key, &validation)
            .map_err(|_| WebIdentityError::InvalidToken)?
            .claims;

        if claims.iss != issuer
            || claims.aud != self.config.audience
            || claims.tid != self.config.tenant_id_string()
            || claims.sub.trim().is_empty()
            || claims.sub.len() > 512
            || claims.sub.chars().any(char::is_control)
        {
            return Err(WebIdentityError::InvalidToken);
        }

        let now =
            u64::try_from(current_unix_seconds()?).map_err(|_| WebIdentityError::InvalidToken)?;
        if claims.exp <= claims.nbf
            || claims.exp <= now.saturating_sub(MAX_CLOCK_SKEW_SECONDS)
            || claims.nbf > now.saturating_add(MAX_CLOCK_SKEW_SECONDS)
        {
            return Err(WebIdentityError::InvalidToken);
        }

        if !claims.scp.as_deref().is_some_and(|scope_list| {
            scope_list
                .split(' ')
                .any(|scope| scope == REQUIRED_DELEGATED_SCOPE)
        }) {
            return Err(WebIdentityError::Forbidden);
        }

        let expires_at_unix_ms = i64::try_from(claims.exp)
            .ok()
            .and_then(|seconds| seconds.checked_mul(1000))
            .ok_or(WebIdentityError::InvalidToken)?;
        let identity = UserIdentityRef {
            issuer: claims.iss,
            subject: claims.sub,
        };

        let organizations = match self
            .directory
            .organizations_for_verified_identity(&identity)
            .await
        {
            Ok(organizations) => organizations,
            Err(WebIdentityDirectoryError::MissingMapping)
            | Err(WebIdentityDirectoryError::AmbiguousMapping) => {
                return Err(WebIdentityError::Forbidden);
            }
            Err(WebIdentityDirectoryError::Unavailable) => {
                return Err(WebIdentityError::Unavailable);
            }
        };
        if organizations.len() != 1
            || organizations[0].trim().is_empty()
            || organizations[0].len() > 200
            || organizations[0].trim() != organizations[0]
            || organizations[0].chars().any(char::is_control)
        {
            return Err(WebIdentityError::Forbidden);
        }

        Ok(VerifiedWebPrincipal {
            identity,
            organization_id: organizations
                .into_iter()
                .next()
                .ok_or(WebIdentityError::Forbidden)?,
            expires_at_unix_ms,
        })
    }
}

#[async_trait]
impl WebPrincipalVerifier for AzureAdWebPrincipalVerifier {
    async fn verify_access_token(
        &self,
        access_token: &str,
    ) -> Result<VerifiedWebPrincipal, WebIdentityError> {
        self.verify(access_token).await
    }
}

#[async_trait]
trait JwksFetcher: Send + Sync {
    async fn fetch_jwks(
        &self,
        discovery_url: &str,
        expected_issuer: &str,
        expected_jwks_url: &str,
    ) -> Result<Vec<u8>, WebIdentityError>;
}

struct HttpJwksFetcher {
    client: reqwest::Client,
}

#[async_trait]
impl JwksFetcher for HttpJwksFetcher {
    async fn fetch_jwks(
        &self,
        discovery_url: &str,
        expected_issuer: &str,
        expected_jwks_url: &str,
    ) -> Result<Vec<u8>, WebIdentityError> {
        let discovery_response = self
            .client
            .get(discovery_url)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|_| WebIdentityError::Unavailable)?;
        let discovery_body = read_json_response(discovery_response, MAX_DISCOVERY_BYTES).await?;
        let jwks_url = validate_discovery(&discovery_body, expected_issuer, expected_jwks_url)?;

        let jwks_response = self
            .client
            .get(jwks_url)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|_| WebIdentityError::Unavailable)?;
        read_json_response(jwks_response, MAX_JWKS_BYTES).await
    }
}

async fn read_json_response(
    response: reqwest::Response,
    max_bytes: usize,
) -> Result<Vec<u8>, WebIdentityError> {
    if response.status() != reqwest::StatusCode::OK {
        return Err(WebIdentityError::Unavailable);
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .unwrap_or_default();
    if content_type != "application/json" && content_type != "application/jwk-set+json" {
        return Err(WebIdentityError::Unavailable);
    }
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        return Err(WebIdentityError::Unavailable);
    }

    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| WebIdentityError::Unavailable)?;
        if body.len().saturating_add(chunk.len()) > max_bytes {
            return Err(WebIdentityError::Unavailable);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[derive(Deserialize)]
struct DiscoveryDocument {
    issuer: String,
    jwks_uri: String,
}

fn validate_discovery(
    body: &[u8],
    expected_issuer: &str,
    expected_jwks_url: &str,
) -> Result<String, WebIdentityError> {
    let document: DiscoveryDocument =
        serde_json::from_slice(body).map_err(|_| WebIdentityError::Unavailable)?;
    if document.issuer != expected_issuer || document.jwks_uri != expected_jwks_url {
        return Err(WebIdentityError::Unavailable);
    }
    if !document.jwks_uri.starts_with("https://") {
        return Err(WebIdentityError::Unavailable);
    }
    Ok(document.jwks_uri)
}

#[derive(Default)]
struct KeyCache {
    keys: Option<CachedKeys>,
    last_unknown_kid_refresh: Option<Instant>,
}

struct CachedKeys {
    fetched_at: Instant,
    entries: HashMap<String, JwkDocument>,
}

#[derive(Deserialize)]
struct JwkSetDocument {
    keys: Vec<JwkDocument>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct JwkDocument {
    kty: String,
    kid: String,
    #[serde(default, rename = "use")]
    key_use: Option<String>,
    #[serde(default)]
    alg: Option<String>,
    #[serde(default)]
    n: Option<String>,
    #[serde(default)]
    e: Option<String>,
    #[serde(default)]
    key_ops: Option<Vec<String>>,
    #[serde(default)]
    issuer: Option<String>,
    #[serde(default)]
    nbf: Option<i64>,
    #[serde(default)]
    d: Option<String>,
    #[serde(default)]
    p: Option<String>,
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    dp: Option<String>,
    #[serde(default)]
    dq: Option<String>,
    #[serde(default)]
    qi: Option<String>,
    #[serde(default)]
    oth: Option<Vec<Value>>,
    #[serde(default, rename = "x5t")]
    _x5t: Option<String>,
    #[serde(default, rename = "x5t#S256")]
    _x5t_sha256: Option<String>,
    #[serde(default, rename = "x5c")]
    _x5c: Option<Vec<String>>,
    #[serde(default, rename = "crv")]
    _crv: Option<String>,
    #[serde(default, rename = "x")]
    _x: Option<String>,
    #[serde(default, rename = "y")]
    _y: Option<String>,
}

fn parse_jwks(body: &[u8]) -> Result<HashMap<String, JwkDocument>, WebIdentityError> {
    let document: JwkSetDocument =
        serde_json::from_slice(body).map_err(|_| WebIdentityError::Unavailable)?;
    if document.keys.is_empty() || document.keys.len() > MAX_JWKS_KEYS {
        return Err(WebIdentityError::Unavailable);
    }

    let mut entries = HashMap::with_capacity(document.keys.len());
    for key in document.keys {
        if key.kid.is_empty()
            || key.kid.len() > MAX_KID_BYTES
            || !key.kid.is_ascii()
            || key.kid.chars().any(char::is_control)
            || key.kty.is_empty()
            || key.d.is_some()
            || key.p.is_some()
            || key.q.is_some()
            || key.dp.is_some()
            || key.dq.is_some()
            || key.qi.is_some()
            || key.oth.is_some()
        {
            return Err(WebIdentityError::Unavailable);
        }
        if entries.insert(key.kid.clone(), key).is_some() {
            return Err(WebIdentityError::Unavailable);
        }
    }
    Ok(entries)
}

fn decoding_key_for(
    jwk: &JwkDocument,
    expected_issuer: &str,
) -> Result<DecodingKey, WebIdentityError> {
    if jwk.kty != "RSA"
        || jwk.key_use.as_deref() != Some("sig")
        || jwk.alg.as_deref().is_some_and(|alg| alg != "RS256")
    {
        return Err(WebIdentityError::InvalidToken);
    }
    if jwk
        .issuer
        .as_deref()
        .is_some_and(|issuer| !signing_key_issuer_matches(issuer, expected_issuer))
        || jwk
            .key_ops
            .as_ref()
            .is_some_and(|operations| operations.len() != 1 || operations[0] != "verify")
    {
        return Err(WebIdentityError::InvalidToken);
    }
    if jwk.nbf.is_some_and(|nbf| {
        current_unix_seconds()
            .map(|now| nbf > now.saturating_add(MAX_CLOCK_SKEW_SECONDS as i64))
            .unwrap_or(true)
    }) {
        return Err(WebIdentityError::InvalidToken);
    }

    let modulus = jwk.n.as_deref().ok_or(WebIdentityError::InvalidToken)?;
    let exponent = jwk.e.as_deref().ok_or(WebIdentityError::InvalidToken)?;
    let modulus_bytes = URL_SAFE_NO_PAD
        .decode(modulus)
        .map_err(|_| WebIdentityError::InvalidToken)?;
    let exponent_bytes = URL_SAFE_NO_PAD
        .decode(exponent)
        .map_err(|_| WebIdentityError::InvalidToken)?;
    if modulus_bytes.is_empty()
        || modulus_bytes.len() > MAX_RSA_MODULUS_BYTES
        || exponent_bytes != [0x01, 0x00, 0x01]
    {
        return Err(WebIdentityError::InvalidToken);
    }
    let modulus_bits =
        (modulus_bytes.len() as u32 - 1) * 8 + (8 - modulus_bytes[0].leading_zeros());
    if !(MIN_RSA_MODULUS_BITS..=MAX_RSA_MODULUS_BITS).contains(&modulus_bits) {
        return Err(WebIdentityError::InvalidToken);
    }

    DecodingKey::from_rsa_components(modulus, exponent).map_err(|_| WebIdentityError::InvalidToken)
}

fn signing_key_issuer_matches(key_issuer: &str, expected_issuer: &str) -> bool {
    let tenant_id = expected_issuer
        .strip_prefix("https://login.microsoftonline.com/")
        .and_then(|issuer| issuer.strip_suffix("/v2.0"))
        .unwrap_or_default();
    if key_issuer == expected_issuer {
        return true;
    }

    let marker = "{tenantid}";
    let lower = key_issuer.to_ascii_lowercase();
    let Some(position) = lower.find(marker) else {
        return false;
    };
    let mut tenant_specific_issuer = key_issuer.to_owned();
    tenant_specific_issuer.replace_range(position..position + marker.len(), tenant_id);
    tenant_specific_issuer == expected_issuer
}

fn current_unix_seconds() -> Result<i64, WebIdentityError> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| WebIdentityError::Unavailable)?
        .as_secs();
    i64::try_from(seconds).map_err(|_| WebIdentityError::Unavailable)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictTokenHeader {
    alg: String,
    kid: String,
    typ: String,
    #[serde(default, rename = "x5t")]
    _x5t: Option<String>,
}

fn parse_token_header(access_token: &str) -> Result<StrictTokenHeader, WebIdentityError> {
    if access_token.len() > MAX_ACCESS_TOKEN_BYTES || !access_token.is_ascii() {
        return Err(WebIdentityError::InvalidToken);
    }
    let mut segments = access_token.split('.');
    let header_segment = segments.next().ok_or(WebIdentityError::InvalidToken)?;
    let payload_segment = segments.next().ok_or(WebIdentityError::InvalidToken)?;
    let signature_segment = segments.next().ok_or(WebIdentityError::InvalidToken)?;
    if segments.next().is_some()
        || header_segment.is_empty()
        || payload_segment.is_empty()
        || signature_segment.is_empty()
        || access_token.bytes().any(|byte| {
            !(byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' || byte == b'.')
        })
    {
        return Err(WebIdentityError::InvalidToken);
    }

    let header_bytes = URL_SAFE_NO_PAD
        .decode(header_segment)
        .map_err(|_| WebIdentityError::InvalidToken)?;
    if header_bytes.len() > MAX_TOKEN_HEADER_BYTES {
        return Err(WebIdentityError::InvalidToken);
    }
    let header: StrictTokenHeader =
        serde_json::from_slice(&header_bytes).map_err(|_| WebIdentityError::InvalidToken)?;
    if header.alg != "RS256"
        || header.typ != "JWT"
        || header.kid.is_empty()
        || header.kid.len() > MAX_KID_BYTES
        || !header.kid.is_ascii()
        || header.kid.chars().any(char::is_control)
    {
        return Err(WebIdentityError::InvalidToken);
    }
    Ok(header)
}

#[derive(Deserialize)]
struct AccessTokenClaims {
    iss: String,
    sub: String,
    aud: String,
    tid: String,
    exp: u64,
    nbf: u64,
    #[serde(default)]
    scp: Option<String>,
    #[serde(flatten)]
    _other_claims: HashMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{encode, EncodingKey, Header};
    use serde::Serialize;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    const TEST_TENANT: &str = "d1270a88-7832-466f-a653-b9950a3ea0fa";
    const TEST_AUDIENCE: &str = "api://workspace-web-test";
    const TEST_KID: &str = "workspace-web-local-test-key";

    #[derive(Clone, Serialize)]
    struct TestClaims {
        iss: String,
        sub: String,
        aud: String,
        tid: String,
        exp: u64,
        nbf: u64,
        scp: String,
        organization_id: String,
    }

    struct TestDirectory {
        result: Result<Vec<String>, WebIdentityDirectoryError>,
        observed_identity: Mutex<Option<UserIdentityRef>>,
    }

    #[async_trait]
    impl WebIdentityDirectory for TestDirectory {
        async fn organizations_for_verified_identity(
            &self,
            identity: &UserIdentityRef,
        ) -> Result<Vec<String>, WebIdentityDirectoryError> {
            *self.observed_identity.lock().await = Some(identity.clone());
            self.result.clone()
        }
    }

    struct TestJwksFetcher {
        jwks: tokio::sync::RwLock<Vec<u8>>,
        fail: AtomicBool,
        calls: AtomicUsize,
    }

    impl TestJwksFetcher {
        fn new(jwks: Vec<u8>) -> Self {
            Self {
                jwks: tokio::sync::RwLock::new(jwks),
                fail: AtomicBool::new(false),
                calls: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl JwksFetcher for TestJwksFetcher {
        async fn fetch_jwks(
            &self,
            _discovery_url: &str,
            _expected_issuer: &str,
            _expected_jwks_url: &str,
        ) -> Result<Vec<u8>, WebIdentityError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail.load(Ordering::SeqCst) {
                return Err(WebIdentityError::Unavailable);
            }
            Ok(self.jwks.read().await.clone())
        }
    }

    fn config() -> AzureAdWebIdentityConfig {
        AzureAdWebIdentityConfig::new(TEST_TENANT.parse().unwrap(), TEST_AUDIENCE).unwrap()
    }

    fn directory(result: Result<Vec<String>, WebIdentityDirectoryError>) -> Arc<TestDirectory> {
        Arc::new(TestDirectory {
            result,
            observed_identity: Mutex::new(None),
        })
    }

    fn rsa_modulus() -> String {
        URL_SAFE_NO_PAD
            .encode(hex_bytes(
                "EE76153DE58E83D4F4C86ECF2DCE4C83C250076104B99E20064374D535540AA85D547536FCF9EA6488CE592EC97CDFFA6CC338471A5B94E4EA9C6F85B47D57CF769DD515444B9DFC0D86BD8C3ED77B1D7F757B66B96E2D06A5FEA71FCA724935BCDE8BF1B8A3F05EC96EC99B01B762C6A44E1EFC69991FE2EE7392726C5C0D87BA20DC491B10D18BE342BCB3B72598EF97508246D8CE3C03CFF526182AF474B5FE480A2C7BA48010628D9DA262490326D2575821E11A567D94B7523C976F8F35FBE118F5739A22DF82FA71DB28A7EB0196EFCAFA6FA9D32BD72A13BA7DCD4336E590FF6D7B9D7FAB2D98FC1F9E0AD94DF4EDDEDD71DDD8C0B76606616D652D0D",
            ))
    }

    fn hex_bytes(input: &str) -> Vec<u8> {
        input
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                let digit = |byte: u8| match byte {
                    b'0'..=b'9' => byte - b'0',
                    b'A'..=b'F' => byte - b'A' + 10,
                    _ => unreachable!(),
                };
                (digit(pair[0]) << 4) | digit(pair[1])
            })
            .collect()
    }

    fn jwks(kid: &str, alg: Option<&str>) -> Vec<u8> {
        let mut key = serde_json::json!({
            "kty": "RSA",
            "kid": kid,
            "use": "sig",
            "n": rsa_modulus(),
            "e": "AQAB"
        });
        if let Some(alg) = alg {
            key["alg"] = Value::String(alg.to_owned());
        }
        serde_json::to_vec(&serde_json::json!({ "keys": [key] })).unwrap()
    }

    fn claims() -> TestClaims {
        let now = current_unix_seconds().unwrap() as u64;
        TestClaims {
            iss: format!("https://{AZURE_LOGIN_HOST}/{TEST_TENANT}/v2.0"),
            sub: "opaque-user-subject-37".to_owned(),
            aud: TEST_AUDIENCE.to_owned(),
            tid: TEST_TENANT.to_owned(),
            exp: now + 3600,
            nbf: now - 10,
            scp: "openid Workspace.Web.Access profile".to_owned(),
            organization_id: "untrusted-token-org-is-ignored".to_owned(),
        }
    }

    fn signed_token(kid: &str, claims: &TestClaims) -> String {
        let private_key = include_bytes!("../testdata/workspace-web-test-private.pem");
        let encoding_key = EncodingKey::from_rsa_pem(private_key).unwrap();
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(kid.to_owned());
        encode(&header, claims, &encoding_key).unwrap()
    }

    fn make_verifier(
        fetcher: Arc<TestJwksFetcher>,
        directory: Arc<TestDirectory>,
        cache_ttl: Duration,
    ) -> AzureAdWebPrincipalVerifier {
        AzureAdWebPrincipalVerifier::with_fetcher(config(), directory, fetcher, cache_ttl)
    }

    #[tokio::test]
    async fn signed_token_resolves_only_directory_organization() {
        let fetcher = Arc::new(TestJwksFetcher::new(jwks(TEST_KID, Some("RS256"))));
        let directory = directory(Ok(vec!["org-from-directory".to_owned()]));
        let verifier = make_verifier(fetcher, Arc::clone(&directory), JWKS_CACHE_TTL);
        let token = signed_token(TEST_KID, &claims());

        let principal = verifier.verify_access_token(&token).await.unwrap();
        assert_eq!(principal.identity().issuer, config().issuer());
        assert_eq!(principal.identity().subject, "opaque-user-subject-37");
        assert_eq!(principal.organization_id(), "org-from-directory");
        assert!(principal.expires_at_unix_ms() > 0);
        assert_eq!(
            directory.observed_identity.lock().await.as_ref(),
            Some(principal.identity())
        );
    }

    #[tokio::test]
    async fn invalid_signature_wrong_claims_and_untrusted_headers_are_rejected() {
        let fetcher = Arc::new(TestJwksFetcher::new(jwks(TEST_KID, Some("RS256"))));
        let verifier = make_verifier(
            Arc::clone(&fetcher),
            directory(Ok(vec!["org-1".to_owned()])),
            JWKS_CACHE_TTL,
        );

        let valid = signed_token(TEST_KID, &claims());
        let mut signature_parts = valid.split('.').map(str::to_owned).collect::<Vec<_>>();
        let first = signature_parts[2].as_bytes()[0];
        signature_parts[2].replace_range(0..1, if first == b'A' { "B" } else { "A" });
        assert_eq!(
            verifier
                .verify_access_token(&signature_parts.join("."))
                .await
                .unwrap_err(),
            WebIdentityError::InvalidToken
        );

        let mut wrong_tenant = claims();
        wrong_tenant.tid = "00000000-0000-0000-0000-000000000000".to_owned();
        assert_eq!(
            verifier
                .verify_access_token(&signed_token(TEST_KID, &wrong_tenant))
                .await
                .unwrap_err(),
            WebIdentityError::InvalidToken
        );

        let mut wrong_issuer = claims();
        wrong_issuer.iss = "https://issuer.invalid/tenant/v2.0".to_owned();
        assert_eq!(
            verifier
                .verify_access_token(&signed_token(TEST_KID, &wrong_issuer))
                .await
                .unwrap_err(),
            WebIdentityError::InvalidToken
        );

        let mut wrong_audience = claims();
        wrong_audience.aud = "api://another-api".to_owned();
        assert_eq!(
            verifier
                .verify_access_token(&signed_token(TEST_KID, &wrong_audience))
                .await
                .unwrap_err(),
            WebIdentityError::InvalidToken
        );

        let mut wrong_times = claims();
        wrong_times.nbf += 120;
        assert_eq!(
            verifier
                .verify_access_token(&signed_token(TEST_KID, &wrong_times))
                .await
                .unwrap_err(),
            WebIdentityError::InvalidToken
        );

        let mut expired = claims();
        expired.exp = current_unix_seconds().unwrap() as u64 - 120;
        assert_eq!(
            verifier
                .verify_access_token(&signed_token(TEST_KID, &expired))
                .await
                .unwrap_err(),
            WebIdentityError::InvalidToken
        );

        let untrusted_header = URL_SAFE_NO_PAD.encode(
            br#"{"alg":"RS256","kid":"workspace-web-local-test-key","typ":"JWT","jku":"https://attacker.invalid/jwks"}"#,
        );
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims()).unwrap());
        let token_with_jku = format!("{untrusted_header}.{payload}.AA");
        assert_eq!(
            verifier
                .verify_access_token(&token_with_jku)
                .await
                .unwrap_err(),
            WebIdentityError::InvalidToken
        );
    }

    #[tokio::test]
    async fn scope_and_organization_mapping_fail_closed() {
        let fetcher = Arc::new(TestJwksFetcher::new(jwks(TEST_KID, Some("RS256"))));
        let verifier = make_verifier(
            Arc::clone(&fetcher),
            directory(Ok(vec!["org-1".to_owned()])),
            JWKS_CACHE_TTL,
        );
        let mut no_scope = claims();
        no_scope.scp = "Workspace.Web.Access.Admin".to_owned();
        assert_eq!(
            verifier
                .verify_access_token(&signed_token(TEST_KID, &no_scope))
                .await
                .unwrap_err(),
            WebIdentityError::Forbidden
        );

        for mapping in [
            Vec::new(),
            vec!["org-a".to_owned(), "org-b".to_owned()],
            vec![" ".to_owned()],
        ] {
            let verifier =
                make_verifier(Arc::clone(&fetcher), directory(Ok(mapping)), JWKS_CACHE_TTL);
            assert_eq!(
                verifier
                    .verify_access_token(&signed_token(TEST_KID, &claims()))
                    .await
                    .unwrap_err(),
                WebIdentityError::Forbidden
            );
        }

        let unavailable_directory = make_verifier(
            Arc::clone(&fetcher),
            directory(Err(WebIdentityDirectoryError::Unavailable)),
            JWKS_CACHE_TTL,
        );
        assert_eq!(
            unavailable_directory
                .verify_access_token(&signed_token(TEST_KID, &claims()))
                .await
                .unwrap_err(),
            WebIdentityError::Unavailable
        );

        for error in [
            WebIdentityDirectoryError::MissingMapping,
            WebIdentityDirectoryError::AmbiguousMapping,
        ] {
            let directory_error =
                make_verifier(Arc::clone(&fetcher), directory(Err(error)), JWKS_CACHE_TTL);
            assert_eq!(
                directory_error
                    .verify_access_token(&signed_token(TEST_KID, &claims()))
                    .await
                    .unwrap_err(),
                WebIdentityError::Forbidden
            );
        }
    }

    #[tokio::test]
    async fn unknown_kid_refreshes_once_and_cache_expiry_never_uses_stale_keys() {
        let fetcher = Arc::new(TestJwksFetcher::new(jwks("old-kid", Some("RS256"))));
        let identity_directory = directory(Ok(vec!["org-1".to_owned()]));
        let verifier = make_verifier(Arc::clone(&fetcher), identity_directory, JWKS_CACHE_TTL);

        let old_token = signed_token("old-kid", &claims());
        assert!(verifier.verify_access_token(&old_token).await.is_ok());
        assert_eq!(fetcher.calls.load(Ordering::SeqCst), 1);
        *fetcher.jwks.write().await = jwks("rotated-kid", Some("RS256"));
        let token = signed_token("rotated-kid", &claims());
        assert_eq!(
            verifier
                .verify_access_token(&token)
                .await
                .unwrap()
                .organization_id(),
            "org-1"
        );
        assert_eq!(fetcher.calls.load(Ordering::SeqCst), 2);

        let expiring_fetcher = Arc::new(TestJwksFetcher::new(jwks(TEST_KID, Some("RS256"))));
        let expiring_verifier = make_verifier(
            Arc::clone(&expiring_fetcher),
            directory(Ok(vec!["org-1".to_owned()])),
            Duration::ZERO,
        );
        let token = signed_token(TEST_KID, &claims());
        assert!(expiring_verifier.verify_access_token(&token).await.is_ok());
        expiring_fetcher.fail.store(true, Ordering::SeqCst);
        assert_eq!(
            expiring_verifier
                .verify_access_token(&token)
                .await
                .unwrap_err(),
            WebIdentityError::Unavailable
        );
        assert_eq!(expiring_fetcher.calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn metadata_and_jwks_reject_untrusted_or_ambiguous_key_sources() {
        let issuer = config().issuer();
        let jwks_url = config().jwks_url();
        let discovery = serde_json::to_vec(&serde_json::json!({
            "issuer": issuer,
            "jwks_uri": jwks_url
        }))
        .unwrap();
        assert!(validate_discovery(&discovery, &config().issuer(), &config().jwks_url()).is_ok());
        assert_eq!(
            validate_discovery(&discovery, "https://issuer.invalid", &config().jwks_url())
                .unwrap_err(),
            WebIdentityError::Unavailable
        );

        let duplicate_kids = serde_json::to_vec(&serde_json::json!({
            "keys": [
                {"kty":"RSA","kid":"same","use":"sig","n":rsa_modulus(),"e":"AQAB"},
                {"kty":"RSA","kid":"same","use":"sig","n":rsa_modulus(),"e":"AQAB"}
            ]
        }))
        .unwrap();
        assert_eq!(
            parse_jwks(&duplicate_kids).unwrap_err(),
            WebIdentityError::Unavailable
        );

        assert!(signing_key_issuer_matches(
            "https://login.microsoftonline.com/{tenantid}/v2.0",
            &config().issuer()
        ));
        assert!(signing_key_issuer_matches(
            "https://login.microsoftonline.com/{tenantId}/v2.0",
            &config().issuer()
        ));

        let wrong_algorithm = parse_jwks(&jwks(TEST_KID, Some("PS256"))).unwrap();
        assert_eq!(
            decoding_key_for(wrong_algorithm.get(TEST_KID).unwrap(), &config().issuer())
                .unwrap_err(),
            WebIdentityError::InvalidToken
        );

        let wrong_key_type = serde_json::to_vec(&serde_json::json!({
            "keys": [{"kty":"EC","kid":TEST_KID,"use":"sig","crv":"P-256","x":"AA","y":"AA"}]
        }))
        .unwrap();
        let wrong_key_type = parse_jwks(&wrong_key_type).unwrap();
        assert_eq!(
            decoding_key_for(wrong_key_type.get(TEST_KID).unwrap(), &config().issuer())
                .unwrap_err(),
            WebIdentityError::InvalidToken
        );

        let duplicate_header = br#"{"alg":"RS256","kid":"first","kid":"second","typ":"JWT"}"#;
        assert_eq!(
            parse_token_header(&format!(
                "{}.e30.AA",
                URL_SAFE_NO_PAD.encode(duplicate_header)
            ))
            .unwrap_err(),
            WebIdentityError::InvalidToken
        );
    }

    #[test]
    fn config_and_error_classes_are_explicit() {
        assert_eq!(
            AzureAdWebIdentityConfig::new(Uuid::nil(), TEST_AUDIENCE).unwrap_err(),
            WebIdentityError::Configuration
        );
        assert_eq!(WebIdentityError::InvalidToken.http_status(), 401);
        assert_eq!(WebIdentityError::Forbidden.http_status(), 403);
        assert_eq!(WebIdentityError::Unavailable.http_status(), 503);
        assert_eq!(WebIdentityError::Configuration.http_status(), 500);
    }
}
