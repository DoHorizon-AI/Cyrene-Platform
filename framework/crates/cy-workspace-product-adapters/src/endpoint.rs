//! Server-owned Product endpoint configuration for the generic adapter.
//!
//! Endpoint identity is bound to one owner release and one immutable
//! organization/Workspace pair. Credentials never enter catalog or caller
//! request data. / 端点仅绑定服务端配置的 owner 与精确租户 scope。

use std::collections::HashSet;
use std::fmt;

use reqwest::Url;
use sha2::{Digest, Sha256};

const MAX_SCOPE_ID_BYTES: usize = 512;
const MAX_OWNER_ID_BYTES: usize = 63;
const MAX_SERVICE_CREDENTIAL_BYTES: usize = 4096;
const MIN_SERVICE_CREDENTIAL_BYTES: usize = 32;

/// Private server configuration for one Product owner's HTTPS endpoint.
///
/// The base URL must be a bare HTTPS origin. The pinned operation catalog
/// supplies the route; caller data cannot choose a URL, host, or path.
pub struct ProductEndpointConfig {
    owner_id: String,
    organization_id: String,
    workspace_id: String,
    base_url: String,
    service_credential: String,
}

impl ProductEndpointConfig {
    /// Creates a server-owned endpoint binding for one exact owner and scope.
    pub fn new(
        owner_id: impl Into<String>,
        organization_id: impl Into<String>,
        workspace_id: impl Into<String>,
        base_url: impl Into<String>,
        service_credential: impl Into<String>,
    ) -> Self {
        Self {
            owner_id: owner_id.into(),
            organization_id: organization_id.into(),
            workspace_id: workspace_id.into(),
            base_url: base_url.into(),
            service_credential: service_credential.into(),
        }
    }

    /// Returns the stable owner ID used by the pinned release catalog.
    pub fn owner_id(&self) -> &str {
        &self.owner_id
    }

    /// Returns the organization bound to this private endpoint.
    pub fn organization_id(&self) -> &str {
        &self.organization_id
    }

    /// Returns the Workspace bound to this private endpoint.
    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }

    pub(crate) fn base_url(&self) -> &str {
        &self.base_url
    }

    pub(crate) fn service_credential(&self) -> &str {
        &self.service_credential
    }
}

impl fmt::Debug for ProductEndpointConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProductEndpointConfig")
            .field("owner_id", &self.owner_id)
            .field("organization_id", &"<redacted>")
            .field("workspace_id", &"<redacted>")
            .field("base_url", &"<redacted>")
            .field("service_credential", &"<redacted>")
            .finish()
    }
}

/// Validates all endpoint bindings before the Connector starts serving.
///
/// Duplicate owner/scope entries and credential reuse fail closed. Owner IDs
/// are syntax-checked here and must also be present in the loaded release
/// catalog during host composition.
pub fn validate_product_endpoint_configs(
    configs: &[ProductEndpointConfig],
) -> Result<(), ProductEndpointConfigError> {
    let mut credential_digests = HashSet::with_capacity(configs.len());
    for (index, config) in configs.iter().enumerate() {
        if !valid_owner_id(&config.owner_id)
            || !valid_scope_id(&config.organization_id)
            || !valid_scope_id(&config.workspace_id)
            || configs[..index].iter().any(|configured| {
                configured.owner_id == config.owner_id
                    && configured.organization_id == config.organization_id
                    && configured.workspace_id == config.workspace_id
            })
        {
            return Err(ProductEndpointConfigError::Invalid);
        }
        ProductEndpoint::try_from(config)?;
        let digest: [u8; 32] = Sha256::digest(config.service_credential.as_bytes()).into();
        if !credential_digests.insert(digest) {
            return Err(ProductEndpointConfigError::Invalid);
        }
    }
    if configs.is_empty() {
        return Err(ProductEndpointConfigError::Invalid);
    }
    Ok(())
}

/// Safe public configuration failure without endpoint or credential details.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ProductEndpointConfigError {
    /// A private endpoint entry is missing, invalid, duplicated, or unsafe.
    #[error("PRODUCT_ENDPOINT_CONFIGURATION_INVALID")]
    Invalid,
}

pub(crate) struct ProductEndpoint {
    pub(crate) base_url: Url,
    pub(crate) authorization: reqwest::header::HeaderValue,
}

impl TryFrom<&ProductEndpointConfig> for ProductEndpoint {
    type Error = ProductEndpointConfigError;

    fn try_from(config: &ProductEndpointConfig) -> Result<Self, Self::Error> {
        let base_url =
            Url::parse(config.base_url()).map_err(|_| ProductEndpointConfigError::Invalid)?;
        let credential = config.service_credential();
        if base_url.scheme() != "https"
            || base_url.host_str().is_none()
            || !base_url.username().is_empty()
            || base_url.password().is_some()
            || base_url.path() != "/"
            || base_url.query().is_some()
            || base_url.fragment().is_some()
            || !(MIN_SERVICE_CREDENTIAL_BYTES..=MAX_SERVICE_CREDENTIAL_BYTES)
                .contains(&credential.len())
            || !credential.bytes().all(|byte| (b'!'..=b'~').contains(&byte))
        {
            return Err(ProductEndpointConfigError::Invalid);
        }

        let mut authorization =
            reqwest::header::HeaderValue::from_str(&format!("Bearer {credential}"))
                .map_err(|_| ProductEndpointConfigError::Invalid)?;
        authorization.set_sensitive(true);
        Ok(Self {
            base_url,
            authorization,
        })
    }
}

fn valid_owner_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_OWNER_ID_BYTES
        && value.as_bytes()[0].is_ascii_lowercase()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn valid_scope_id(value: &str) -> bool {
    !value.is_empty()
        && value.trim() == value
        && value.len() <= MAX_SCOPE_ID_BYTES
        && !value.chars().any(|character| {
            character.is_control() || matches!(character, '/' | '\\' | '%' | '?' | '#')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "product-service-private-bearer-token-0123456789";

    fn endpoint(owner_id: &str, url: &str, credential: &str) -> ProductEndpointConfig {
        ProductEndpointConfig::new(owner_id, "org-1", "workspace-1", url, credential)
    }

    #[test]
    fn validates_exact_scoped_https_endpoint_and_redacts_secrets() {
        let config = endpoint("catalyst", "https://catalyst.example.test/", TOKEN);
        validate_product_endpoint_configs(std::slice::from_ref(&config))
            .expect("private HTTPS origin and scoped bearer token");

        let debug = format!("{config:?}");
        assert!(!debug.contains(TOKEN));
        assert!(!debug.contains("catalyst.example.test"));
        assert!(!debug.contains("workspace-1"));
    }

    #[test]
    fn rejects_non_https_urls_bad_owner_ids_and_unsafe_scope_ids() {
        for config in [
            endpoint("sample-owner", "https://api.example.test/", TOKEN),
            endpoint("catalyst", "http://catalyst.example.test/", TOKEN),
            endpoint(
                "catalyst",
                "https://user:pass@catalyst.example.test/",
                TOKEN,
            ),
            endpoint("catalyst", "https://catalyst.example.test/private", TOKEN),
            ProductEndpointConfig::new(
                "catalyst",
                "org/other",
                "workspace-1",
                "https://catalyst.example.test/",
                TOKEN,
            ),
        ] {
            assert_eq!(
                validate_product_endpoint_configs(&[config]),
                Err(ProductEndpointConfigError::Invalid)
            );
        }
    }

    #[test]
    fn rejects_duplicate_owner_bindings_and_credential_reuse() {
        let first = endpoint("catalyst", "https://catalyst.example.test/", TOKEN);
        let duplicate_binding = endpoint(
            "catalyst",
            "https://catalyst.example.test/",
            "other-product-service-bearer-token-0123456789",
        );
        assert_eq!(
            validate_product_endpoint_configs(&[first, duplicate_binding]),
            Err(ProductEndpointConfigError::Invalid)
        );

        let first = endpoint("catalyst", "https://catalyst.example.test/", TOKEN);
        let reused_credential = ProductEndpointConfig::new(
            "echo",
            "org-1",
            "workspace-1",
            "https://echo.example.test/",
            TOKEN,
        );
        assert_eq!(
            validate_product_endpoint_configs(&[first, reused_credential]),
            Err(ProductEndpointConfigError::Invalid)
        );
    }
}
