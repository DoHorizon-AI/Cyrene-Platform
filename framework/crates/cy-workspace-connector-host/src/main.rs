//! Production Workspace Connector process entrypoint.
//!
//! This binary reads only service-owned files at fixed runtime locations. It
//! constructs the Connector identity internally and serves the Workspace API
//! over the existing outbound Relay transport. / 本入口只读取固定位置的服务端私有文件，并内部构造 Connector 身份。

use std::collections::BTreeMap;
use std::error::Error;
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cy_observability::{init_observability, ObservabilityConfig};
use cy_proto::workspace_v1::{DeviceEnrollmentRef, RelayHello, RelayParticipantRole};
use cy_workspace_control_plane::{
    WorkspaceApi, WorkspaceControlPlane, WorkspaceDispatchError, WorkspaceOperationProjection,
    WorkspaceProductRequest, WorkspaceRequestDispatcher,
};
use cy_workspace_fabric::{run_workspace_connector_session, RelayClientConfig};
use cy_workspace_product_adapters::{
    load_product_endpoint_configs_for_workspace, ProductHttpApiAdapter,
};
use cy_workspace_product_contracts::{
    ProductBundlePins, ProductContractBundle, TrustedProductPolicy,
};
use openssl::{pkey::PKey, x509::X509};
use serde::Deserialize;
use thiserror::Error as ThisError;
use tokio::time::sleep;
use url::Url;
use x509_parser::parse_x509_certificate;

const PRIVATE_ROOT: &str = "/run/cyrene/workspace-connector";
const PRIVATE_SECRET_ROOT: &str = "/run/cyrene/workspace-connector/secrets";
const PRODUCT_ENDPOINT_MANIFEST: &str = "/run/cyrene/workspace-connector/product-endpoints.json";
const PRODUCT_CONTRACT_ROOT_ENV: &str = "CYRENE_WORKSPACE_CONNECTOR_PRODUCT_CONTRACT_ROOT_V2";
const DEFAULT_PRODUCT_CONTRACT_ROOT: &str = "/run/cyrene-workspace-product-contracts";
const PRODUCT_POLICY_BUNDLE_FILENAME: &str = "workspace-product-policy-v2.json";
const PRODUCT_RELEASE_LOCK: &[u8] = include_bytes!(
    "../../../../tooling/workspace-product-contract-bundle/releases/workspace-product-v2.lock.json"
);
const RELAY_CA_FILE: &str = "relay-server-ca.pem";
const DEVICE_CERTIFICATE_FILE: &str = "device-enrollment-cert.pem";
const DEVICE_PRIVATE_KEY_FILE: &str = "device-enrollment-key.pem";
const MAX_CONNECTOR_MANIFEST_BYTES: usize = 32 * 1024;
const MAX_RELAY_CA_BYTES: usize = 1024 * 1024;
const MAX_DEVICE_CERTIFICATE_BYTES: usize = 64 * 1024;
const MAX_DEVICE_PRIVATE_KEY_BYTES: usize = 64 * 1024;
const RECONNECT_INITIAL_DELAY: Duration = Duration::from_secs(1);
const RECONNECT_MAX_DELAY: Duration = Duration::from_secs(30);
const SHUTDOWN_FLUSH_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, ThisError)]
enum HostConfigurationError {
    #[cfg(not(unix))]
    #[error("WORKSPACE_CONNECTOR_FILE_POLICY_UNSUPPORTED")]
    UnsupportedPlatform,
    #[error("WORKSPACE_CONNECTOR_PRIVATE_FILE_UNAVAILABLE")]
    PrivateFileUnavailable,
    #[error("WORKSPACE_CONNECTOR_MANIFEST_INVALID")]
    ManifestInvalid,
    #[error("WORKSPACE_CONNECTOR_TLS_MATERIAL_INVALID")]
    TlsMaterialInvalid,
    #[error("WORKSPACE_CONNECTOR_PRODUCT_MANIFEST_INVALID")]
    ProductManifestInvalid,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ConnectorManifest {
    version: u32,
    relay_endpoint: String,
    relay_server_name: String,
    organization_id: String,
    workspace_id: String,
    device_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct WorkspaceProductLock {
    format_version: u32,
    release_id: String,
    wire_api_version: String,
    contract_api_version: String,
    bundle: ProductBundlePin,
    policy: ProductPolicyPin,
    owners: Vec<ProductOwnerPin>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ProductBundlePin {
    manifest_path: String,
    manifest_sha256: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ProductPolicyPin {
    source_path: String,
    bundle_path: String,
    schema_version: String,
    sha256: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ProductOwnerPin {
    owner_id: String,
    repository: String,
    source_sha: String,
    catalog_path: String,
    catalog_sha256: String,
}

struct LoadedHostConfiguration {
    relay: RelayClientConfig,
    hello: RelayHello,
    organization_id: String,
    workspace_id: String,
}

/// Product-neutral operation routes remain unavailable until their separate
/// Product authority adapter is composed. Product API requests use the
/// configured `ProductHttpApiAdapter` below.
struct UnavailableOperationDispatcher;

#[tonic::async_trait]
impl WorkspaceRequestDispatcher for UnavailableOperationDispatcher {
    async fn dispatch(
        &self,
        _workspace_id: &str,
        _request: WorkspaceProductRequest,
    ) -> Result<WorkspaceOperationProjection, WorkspaceDispatchError> {
        Err(WorkspaceDispatchError::Unavailable)
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    if std::env::args_os().len() != 1 {
        return Err("cy-workspace-connector-host accepts no command-line configuration".into());
    }

    let observability =
        init_observability(ObservabilityConfig::managed("cy-workspace-connector-host"))?;
    let result = run_host().await;
    if let Err(error) = &result {
        tracing::error!(
            event.name = "platform.workspace_connector.host_failed",
            error = %error,
            message = "Workspace Connector host stopped with a configuration or runtime error",
        );
    }
    observability.shutdown_with_timeout(SHUTDOWN_FLUSH_TIMEOUT);
    result
}

async fn run_host() -> Result<(), Box<dyn Error>> {
    let configuration = load_host_configuration()?;
    let pins = load_product_bundle_pins()?;
    let product_contract_root = load_product_contract_root()?;
    let product_contracts = Arc::new(
        ProductContractBundle::load(&product_contract_root, &pins)
            .map_err(|_| HostConfigurationError::ProductManifestInvalid)?,
    );
    let product_policy = Arc::new(
        TrustedProductPolicy::load(
            product_contract_root.join(PRODUCT_POLICY_BUNDLE_FILENAME),
            &pins,
        )
        .map_err(|_| HostConfigurationError::ProductManifestInvalid)?,
    );
    let endpoint_configs = load_product_endpoint_configs_for_workspace(
        Path::new(PRODUCT_ENDPOINT_MANIFEST),
        Path::new(PRIVATE_SECRET_ROOT),
        &configuration.organization_id,
        &configuration.workspace_id,
    )
    .map_err(|_| HostConfigurationError::ProductManifestInvalid)?;
    let product_api = Arc::new(
        ProductHttpApiAdapter::from_private_config(endpoint_configs, &product_contracts)
            .map_err(|_| HostConfigurationError::ProductManifestInvalid)?,
    );
    let dispatcher = Arc::new(UnavailableOperationDispatcher);
    let control_plane = WorkspaceControlPlane::new(
        configuration.organization_id.clone(),
        configuration.workspace_id.clone(),
        uuid::Uuid::new_v4().to_string(),
        dispatcher,
    )
    .map_err(|_| HostConfigurationError::ManifestInvalid)?
    .with_product_v2(product_contracts, product_policy, product_api);
    let api: Arc<dyn WorkspaceApi> = Arc::new(control_plane);

    tracing::info!(
        event.name = "platform.workspace_connector.started",
        message = "Workspace Connector loaded private configuration and is connecting to Relay",
    );
    serve_until_shutdown(configuration, api).await?;
    Ok(())
}

/// Loads the server-owned Product contract directory override, if installed.
/// The embedded release lock remains the trust root; this path selects only
/// where its pinned files are read from. / 仅从 Connector 进程环境读取安装器提供的文件目录。
fn load_product_contract_root() -> Result<PathBuf, HostConfigurationError> {
    let root = std::env::var_os(PRODUCT_CONTRACT_ROOT_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_PRODUCT_CONTRACT_ROOT));
    validate_product_contract_root(&root)?;
    Ok(root)
}

fn validate_product_contract_root(root: &Path) -> Result<(), HostConfigurationError> {
    if root.as_os_str().is_empty()
        || !root.is_absolute()
        || root == Path::new("/")
        || root
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(HostConfigurationError::ProductManifestInvalid);
    }
    Ok(())
}

/// Validate the compile-time Platform release lock and derive runtime loader pins.
///
/// The lock is the trust root; runtime mounts and environment variables cannot replace it.
/// 校验编译时固定的 Platform release lock，并生成 bundle/policy loader pins。
fn load_product_bundle_pins() -> Result<ProductBundlePins, HostConfigurationError> {
    let lock: WorkspaceProductLock = serde_json::from_slice(PRODUCT_RELEASE_LOCK)
        .map_err(|_| HostConfigurationError::ProductManifestInvalid)?;
    if lock.format_version != 2
        || lock.release_id.trim().is_empty()
        || lock.wire_api_version != "cyrene.workspace.product.v2"
        || lock.contract_api_version != "0.1.0"
        || lock.bundle.manifest_path != "product-contract-bundle.json"
        || !is_lower_hex(&lock.bundle.manifest_sha256, 64)
        || lock.policy.source_path != "contracts/policies/workspace-product-policy-v2.json"
        || lock.policy.bundle_path != PRODUCT_POLICY_BUNDLE_FILENAME
        || lock.policy.schema_version != "cyrene.workspace.product.authorization-policy.v2"
        || !is_lower_hex(&lock.policy.sha256, 64)
        || lock.owners.is_empty()
    {
        return Err(HostConfigurationError::ProductManifestInvalid);
    }

    let mut owner_source_shas = BTreeMap::new();
    let mut previous_owner = None;
    for owner in lock.owners {
        if !valid_product_owner_id(&owner.owner_id)
            || owner.repository.trim().is_empty()
            || owner.repository.chars().any(char::is_control)
            || !owner
                .catalog_path
                .starts_with(&format!("{}/", owner.repository))
            || owner.catalog_path.contains("..")
            || !is_lower_hex(&owner.source_sha, 40)
            || !is_lower_hex(&owner.catalog_sha256, 64)
            || previous_owner
                .as_deref()
                .is_some_and(|previous: &str| previous >= owner.owner_id.as_str())
            || owner_source_shas
                .insert(owner.owner_id.clone(), owner.source_sha)
                .is_some()
        {
            return Err(HostConfigurationError::ProductManifestInvalid);
        }
        previous_owner = Some(owner.owner_id);
    }

    Ok(ProductBundlePins::new(
        lock.wire_api_version,
        lock.bundle.manifest_sha256,
        owner_source_shas,
        lock.policy.schema_version,
        lock.policy.sha256,
    ))
}

fn valid_product_owner_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value.as_bytes()[0].is_ascii_lowercase()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

async fn serve_until_shutdown(
    configuration: LoadedHostConfiguration,
    api: Arc<dyn WorkspaceApi>,
) -> Result<(), Box<dyn Error>> {
    let mut shutdown = Box::pin(tokio::signal::ctrl_c());
    let mut delay = RECONNECT_INITIAL_DELAY;

    loop {
        tokio::select! {
            signal = &mut shutdown => {
                signal?;
                tracing::info!(
                    event.name = "platform.workspace_connector.shutdown",
                    message = "Workspace Connector received a shutdown signal",
                );
                return Ok(());
            }
            result = run_workspace_connector_session(
                &configuration.relay,
                configuration.hello.clone(),
                Arc::clone(&api),
            ) => {
                let failure_kind = match result {
                    Ok(_) => "relay_stream_closed",
                    Err(_) => "relay_session_failed",
                };
                tracing::warn!(
                    event.name = "platform.workspace_connector.relay_session_ended",
                    failure_kind,
                    retry_delay_seconds = delay.as_secs(),
                    message = "Workspace Connector Relay session ended; retrying with bounded backoff",
                );
            }
        }

        tokio::select! {
            signal = &mut shutdown => {
                signal?;
                tracing::info!(
                    event.name = "platform.workspace_connector.shutdown",
                    message = "Workspace Connector received a shutdown signal",
                );
                return Ok(());
            }
            _ = sleep(delay) => {}
        }
        delay = delay.saturating_mul(2).min(RECONNECT_MAX_DELAY);
    }
}

#[cfg(unix)]
fn load_host_configuration() -> Result<LoadedHostConfiguration, HostConfigurationError> {
    let private_owner = validate_private_directory(Path::new(PRIVATE_ROOT), None)?;
    validate_private_directory(Path::new(PRIVATE_SECRET_ROOT), Some(private_owner))?;
    let manifest_bytes = read_private_file(
        Path::new(PRIVATE_ROOT),
        "connector.json",
        private_owner,
        MAX_CONNECTOR_MANIFEST_BYTES,
    )?;
    let manifest: ConnectorManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|_| HostConfigurationError::ManifestInvalid)?;
    if manifest.version != 1
        || !valid_scope_value(&manifest.organization_id)
        || !valid_scope_value(&manifest.workspace_id)
        || !valid_scope_value(&manifest.device_id)
        || !valid_relay_server_name(&manifest.relay_server_name)
        || !valid_relay_endpoint(&manifest.relay_endpoint, &manifest.relay_server_name)
    {
        return Err(HostConfigurationError::ManifestInvalid);
    }

    let ca_certificate_pem = read_private_file(
        Path::new(PRIVATE_SECRET_ROOT),
        RELAY_CA_FILE,
        private_owner,
        MAX_RELAY_CA_BYTES,
    )?;
    let client_certificate_pem = read_private_file(
        Path::new(PRIVATE_SECRET_ROOT),
        DEVICE_CERTIFICATE_FILE,
        private_owner,
        MAX_DEVICE_CERTIFICATE_BYTES,
    )?;
    let client_key_pem = read_private_file(
        Path::new(PRIVATE_SECRET_ROOT),
        DEVICE_PRIVATE_KEY_FILE,
        private_owner,
        MAX_DEVICE_PRIVATE_KEY_BYTES,
    )?;
    validate_tls_materials(
        &ca_certificate_pem,
        &client_certificate_pem,
        &client_key_pem,
    )?;

    let hello = RelayHello {
        role: RelayParticipantRole::WorkspaceConnector as i32,
        session_credential: String::new(),
        user: None,
        organization_id: manifest.organization_id.clone(),
        workspace_id: manifest.workspace_id.clone(),
        device: Some(DeviceEnrollmentRef {
            device_id: manifest.device_id,
            workspace_id: manifest.workspace_id.clone(),
            enrollment_state: String::new(),
        }),
    };
    Ok(LoadedHostConfiguration {
        relay: RelayClientConfig {
            control_endpoint: manifest.relay_endpoint,
            server_name: manifest.relay_server_name,
            ca_certificate_pem,
            client_certificate_pem,
            client_key_pem,
        },
        hello,
        organization_id: manifest.organization_id,
        workspace_id: manifest.workspace_id,
    })
}

#[cfg(not(unix))]
fn load_host_configuration() -> Result<LoadedHostConfiguration, HostConfigurationError> {
    Err(HostConfigurationError::UnsupportedPlatform)
}

#[cfg(unix)]
fn validate_private_directory(
    path: &Path,
    expected_owner: Option<u32>,
) -> Result<u32, HostConfigurationError> {
    use std::os::unix::fs::MetadataExt;

    if !path.is_absolute() || fs::canonicalize(path).ok().as_deref() != Some(path) {
        return Err(HostConfigurationError::PrivateFileUnavailable);
    }
    let before =
        fs::symlink_metadata(path).map_err(|_| HostConfigurationError::PrivateFileUnavailable)?;
    if before.file_type().is_symlink() || !before.is_dir() || before.mode() & 0o7777 != 0o700 {
        return Err(HostConfigurationError::PrivateFileUnavailable);
    }
    let opened = File::open(path).map_err(|_| HostConfigurationError::PrivateFileUnavailable)?;
    let opened_metadata = opened
        .metadata()
        .map_err(|_| HostConfigurationError::PrivateFileUnavailable)?;
    let after =
        fs::symlink_metadata(path).map_err(|_| HostConfigurationError::PrivateFileUnavailable)?;
    if !same_file(&before, &opened_metadata)
        || !same_file(&opened_metadata, &after)
        || after.file_type().is_symlink()
        || opened_metadata.mode() & 0o7777 != 0o700
        || after.mode() & 0o7777 != 0o700
        || expected_owner.is_some_and(|owner| owner != opened_metadata.uid())
        || after.uid() != opened_metadata.uid()
    {
        return Err(HostConfigurationError::PrivateFileUnavailable);
    }
    Ok(opened_metadata.uid())
}

#[cfg(unix)]
fn read_private_file(
    directory: &Path,
    file_name: &str,
    expected_owner: u32,
    maximum_bytes: usize,
) -> Result<Vec<u8>, HostConfigurationError> {
    use std::os::unix::fs::MetadataExt;

    let name = Path::new(file_name);
    if name.components().count() != 1
        || !matches!(name.components().next(), Some(Component::Normal(_)))
    {
        return Err(HostConfigurationError::PrivateFileUnavailable);
    }
    let path = directory.join(name);
    let before =
        fs::symlink_metadata(&path).map_err(|_| HostConfigurationError::PrivateFileUnavailable)?;
    if before.file_type().is_symlink() || !before.is_file() {
        return Err(HostConfigurationError::PrivateFileUnavailable);
    }
    let opened = OpenOptions::new()
        .read(true)
        .open(&path)
        .map_err(|_| HostConfigurationError::PrivateFileUnavailable)?;
    let opened_metadata = opened
        .metadata()
        .map_err(|_| HostConfigurationError::PrivateFileUnavailable)?;
    let after =
        fs::symlink_metadata(&path).map_err(|_| HostConfigurationError::PrivateFileUnavailable)?;
    if !opened_metadata.is_file()
        || !same_file(&before, &opened_metadata)
        || !same_file(&opened_metadata, &after)
        || after.file_type().is_symlink()
        || opened_metadata.uid() != expected_owner
        || before.uid() != expected_owner
        || after.uid() != expected_owner
        || opened_metadata.mode() & 0o7777 != 0o600
        || before.mode() & 0o7777 != 0o600
        || after.mode() & 0o7777 != 0o600
        || opened_metadata.len() == 0
        || opened_metadata.len() > maximum_bytes as u64
    {
        return Err(HostConfigurationError::PrivateFileUnavailable);
    }

    let mut bytes = Vec::with_capacity(opened_metadata.len() as usize);
    opened
        .take(maximum_bytes.saturating_add(1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| HostConfigurationError::PrivateFileUnavailable)?;
    if bytes.len() > maximum_bytes {
        return Err(HostConfigurationError::PrivateFileUnavailable);
    }
    Ok(bytes)
}

#[cfg(not(unix))]
fn validate_private_directory(
    _path: &Path,
    _expected_owner: Option<u32>,
) -> Result<u32, HostConfigurationError> {
    Err(HostConfigurationError::UnsupportedPlatform)
}

#[cfg(not(unix))]
fn read_private_file(
    _directory: &Path,
    _file_name: &str,
    _expected_owner: u32,
    _maximum_bytes: usize,
) -> Result<Vec<u8>, HostConfigurationError> {
    Err(HostConfigurationError::UnsupportedPlatform)
}

#[cfg(unix)]
fn same_file(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    left.dev() == right.dev() && left.ino() == right.ino()
}

fn valid_scope_value(value: &str) -> bool {
    !value.is_empty()
        && value.trim() == value
        && value.len() <= 512
        && !value.chars().any(char::is_control)
}

fn valid_relay_server_name(value: &str) -> bool {
    !value.is_empty()
        && value.trim() == value
        && value.len() <= 253
        && !value.chars().any(char::is_control)
        && rustls::pki_types::ServerName::try_from(value.to_owned()).is_ok()
}

fn valid_relay_endpoint(endpoint: &str, server_name: &str) -> bool {
    let Ok(parsed) = Url::parse(endpoint) else {
        return false;
    };
    parsed.scheme() == "https"
        && parsed.host_str().is_some()
        && parsed.username().is_empty()
        && parsed.password().is_none()
        && matches!(parsed.path(), "" | "/")
        && parsed.query().is_none()
        && parsed.fragment().is_none()
        && !server_name.is_empty()
}

fn validate_tls_materials(
    relay_ca_pem: &[u8],
    device_certificate_pem: &[u8],
    device_key_pem: &[u8],
) -> Result<(), HostConfigurationError> {
    let authorities = X509::stack_from_pem(relay_ca_pem)
        .map_err(|_| HostConfigurationError::TlsMaterialInvalid)?;
    let device_certificates = X509::stack_from_pem(device_certificate_pem)
        .map_err(|_| HostConfigurationError::TlsMaterialInvalid)?;
    let device_key = PKey::private_key_from_pem(device_key_pem)
        .map_err(|_| HostConfigurationError::TlsMaterialInvalid)?;
    if authorities.is_empty() || authorities.len() > 32 || device_certificates.len() != 1 {
        return Err(HostConfigurationError::TlsMaterialInvalid);
    }
    for authority in &authorities {
        let der = authority
            .to_der()
            .map_err(|_| HostConfigurationError::TlsMaterialInvalid)?;
        let (remaining, parsed) =
            parse_x509_certificate(&der).map_err(|_| HostConfigurationError::TlsMaterialInvalid)?;
        let constraints = parsed
            .basic_constraints()
            .map_err(|_| HostConfigurationError::TlsMaterialInvalid)?
            .ok_or(HostConfigurationError::TlsMaterialInvalid)?;
        if !remaining.is_empty() || !constraints.value.ca {
            return Err(HostConfigurationError::TlsMaterialInvalid);
        }
    }
    let now_unix_seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| HostConfigurationError::TlsMaterialInvalid)?
        .as_secs();
    let now_unix_seconds =
        i64::try_from(now_unix_seconds).map_err(|_| HostConfigurationError::TlsMaterialInvalid)?;
    let leaf_der = device_certificates[0]
        .to_der()
        .map_err(|_| HostConfigurationError::TlsMaterialInvalid)?;
    let (remaining, leaf) = parse_x509_certificate(&leaf_der)
        .map_err(|_| HostConfigurationError::TlsMaterialInvalid)?;
    if !remaining.is_empty()
        || leaf.validity().not_before.timestamp() > now_unix_seconds
        || leaf.validity().not_after.timestamp() <= now_unix_seconds
    {
        return Err(HostConfigurationError::TlsMaterialInvalid);
    }
    let leaf_key = device_certificates[0]
        .public_key()
        .map_err(|_| HostConfigurationError::TlsMaterialInvalid)?;
    if !leaf_key.public_eq(&device_key) {
        return Err(HostConfigurationError::TlsMaterialInvalid);
    }
    Ok(())
}

#[cfg(test)]
mod product_contract_root_tests {
    use super::{
        validate_product_contract_root, HostConfigurationError, DEFAULT_PRODUCT_CONTRACT_ROOT,
    };
    use std::path::Path;

    #[test]
    fn accepts_the_docker_default_and_an_absolute_native_release_root() {
        assert!(validate_product_contract_root(Path::new(DEFAULT_PRODUCT_CONTRACT_ROOT)).is_ok());
        assert!(validate_product_contract_root(Path::new(
            "/opt/cyrene/releases/connector-2/share/cyrene/product-contracts"
        ))
        .is_ok());
    }

    #[test]
    fn rejects_relative_broad_and_parent_traversing_roots() {
        for root in [
            Path::new("share/cyrene/product-contracts"),
            Path::new("/"),
            Path::new("/opt/cyrene/releases/../share/cyrene/product-contracts"),
        ] {
            assert!(matches!(
                validate_product_contract_root(root),
                Err(HostConfigurationError::ProductManifestInvalid)
            ));
        }
    }
}
