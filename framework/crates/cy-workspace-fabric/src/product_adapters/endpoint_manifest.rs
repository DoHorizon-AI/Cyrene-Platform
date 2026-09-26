//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 endpoint_manifest.rs                                            │
//! │  Module: cy_workspace_fabric::product_adapters::endpoint_manifest  │
//! │  Role: Load private scoped Product endpoints from server files.     │
//! │                                                                     │
//! │  模块职责：从服务端文件加载私有且精确 scope 的 Product endpoint。       │
//! └─────────────────────────────────────────────────────────────────────┘

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::path::{Component, Path};

use cy_proto::workspace_v1::WorkspaceProductApiOwner as Owner;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::http::{validate_product_endpoint_configs, ProductEndpointConfig};

const MAX_MANIFEST_BYTES: usize = 256 * 1024;
const MAX_ENDPOINTS: usize = 256;
const MAX_SECRET_FILE_NAME_BYTES: usize = 128;
const MAX_CREDENTIAL_BYTES: usize = 4096;
const MIN_CREDENTIAL_BYTES: usize = 32;

/// Stable errors for startup configuration loading. No path, URL, or secret is included.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ProductEndpointManifestError {
    /// The server-owned manifest is missing, malformed, or inconsistent.
    #[error("PRODUCT_ENDPOINT_MANIFEST_INVALID")]
    ManifestInvalid,
    /// A mounted endpoint credential is missing or does not meet file policy.
    #[error("PRODUCT_ENDPOINT_SECRET_UNAVAILABLE")]
    SecretUnavailable,
    /// The platform cannot enforce this file policy on the current operating system.
    #[error("PRODUCT_ENDPOINT_FILE_POLICY_UNSUPPORTED")]
    UnsupportedPlatform,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EndpointManifest {
    version: u32,
    endpoints: Vec<EndpointEntry>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EndpointEntry {
    owner: String,
    organization_id: String,
    workspace_id: String,
    base_url: String,
    credential_file: String,
}

/// Load scoped Product endpoints from a server-owned manifest and private secret files.
///
/// The manifest path must be absolute, canonical, regular, and owned by its
/// parent directory owner. The secret root must be a private directory owned
/// by the same service identity as its credential files. Secret references are
/// basenames only; symlinks, duplicate scopes, reused credentials, malformed
/// endpoints, and missing files fail startup. The credential file contains
/// exactly one printable ASCII bearer value with no trailing newline.
///
/// ACA Key Vault volume files may be symlinks or have broader permissions.
/// Copy required values into a service-owned private directory before calling
/// this loader; this function deliberately does not read the volume directly.
///
/// # Errors
/// Returns a fixed error that does not reveal paths, endpoint URLs, or secrets.
#[cfg(unix)]
pub fn load_product_endpoint_configs(
    manifest_path: &Path,
    private_secret_root: &Path,
) -> Result<Vec<ProductEndpointConfig>, ProductEndpointManifestError> {
    use std::os::unix::fs::MetadataExt;

    let manifest_parent = manifest_path
        .parent()
        .ok_or(ProductEndpointManifestError::ManifestInvalid)?;
    let manifest_name = manifest_path
        .file_name()
        .ok_or(ProductEndpointManifestError::ManifestInvalid)?;
    let manifest_directory = open_canonical_directory(manifest_parent)
        .map_err(|_| ProductEndpointManifestError::ManifestInvalid)?;
    let manifest_directory_metadata = manifest_directory
        .metadata()
        .map_err(|_| ProductEndpointManifestError::ManifestInvalid)?;
    let manifest_bytes = read_checked_file(
        &manifest_directory,
        manifest_parent,
        manifest_name,
        MAX_MANIFEST_BYTES,
        manifest_directory_metadata.uid(),
        false,
    )
    .map_err(|_| ProductEndpointManifestError::ManifestInvalid)?;
    let manifest: EndpointManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|_| ProductEndpointManifestError::ManifestInvalid)?;
    if manifest.version != 1
        || manifest.endpoints.is_empty()
        || manifest.endpoints.len() > MAX_ENDPOINTS
    {
        return Err(ProductEndpointManifestError::ManifestInvalid);
    }

    let secret_directory = open_canonical_directory(private_secret_root)
        .map_err(|_| ProductEndpointManifestError::SecretUnavailable)?;
    let secret_directory_metadata = secret_directory
        .metadata()
        .map_err(|_| ProductEndpointManifestError::SecretUnavailable)?;
    let secret_owner = secret_directory_metadata.uid();
    if secret_directory_metadata.mode() & 0o7777 != 0o700
        || secret_owner != manifest_directory_metadata.uid()
    {
        return Err(ProductEndpointManifestError::SecretUnavailable);
    }

    let mut configs = Vec::with_capacity(manifest.endpoints.len());
    let mut secret_digests = HashSet::with_capacity(manifest.endpoints.len());
    for entry in manifest.endpoints {
        let owner =
            parse_owner(&entry.owner).ok_or(ProductEndpointManifestError::ManifestInvalid)?;
        if !valid_scope_id(&entry.organization_id)
            || !valid_scope_id(&entry.workspace_id)
            || !valid_secret_file_name(&entry.credential_file)
        {
            return Err(ProductEndpointManifestError::ManifestInvalid);
        }

        let credential = read_checked_file(
            &secret_directory,
            private_secret_root,
            std::ffi::OsStr::new(&entry.credential_file),
            MAX_CREDENTIAL_BYTES,
            secret_owner,
            true,
        )
        .map_err(|_| ProductEndpointManifestError::SecretUnavailable)?;
        if !valid_credential(&credential) {
            return Err(ProductEndpointManifestError::SecretUnavailable);
        }
        let digest: [u8; 32] = Sha256::digest(&credential).into();
        if !secret_digests.insert(digest) {
            return Err(ProductEndpointManifestError::ManifestInvalid);
        }
        let credential = String::from_utf8(credential)
            .map_err(|_| ProductEndpointManifestError::SecretUnavailable)?;

        configs.push(ProductEndpointConfig::new(
            owner,
            entry.organization_id,
            entry.workspace_id,
            entry.base_url,
            credential,
        ));
    }

    validate_product_endpoint_configs(&configs)
        .map_err(|_| ProductEndpointManifestError::ManifestInvalid)?;
    Ok(configs)
}

/// Non-Unix targets fail closed because the loader cannot verify Unix owner and mode policy.
#[cfg(not(unix))]
pub fn load_product_endpoint_configs(
    _manifest_path: &Path,
    _private_secret_root: &Path,
) -> Result<Vec<ProductEndpointConfig>, ProductEndpointManifestError> {
    Err(ProductEndpointManifestError::UnsupportedPlatform)
}

#[cfg(unix)]
fn open_canonical_directory(path: &Path) -> Result<File, ()> {
    use std::os::unix::fs::MetadataExt;

    if !path.is_absolute()
        || fs::canonicalize(path).map_err(|_| ())? != path
        || !fs::symlink_metadata(path).map_err(|_| ())?.is_dir()
    {
        return Err(());
    }
    let before = fs::symlink_metadata(path).map_err(|_| ())?;
    if before.file_type().is_symlink() || !before.is_dir() {
        return Err(());
    }
    let directory = File::open(path).map_err(|_| ())?;
    let opened = directory.metadata().map_err(|_| ())?;
    let after = fs::symlink_metadata(path).map_err(|_| ())?;
    if !same_file(&before, &opened)
        || !same_file(&opened, &after)
        || opened.uid() != before.uid()
        || opened.mode() & 0o022 != 0
    {
        return Err(());
    }
    Ok(directory)
}

#[cfg(unix)]
fn read_checked_file(
    directory: &File,
    directory_path: &Path,
    file_name: &std::ffi::OsStr,
    maximum_bytes: usize,
    expected_owner: u32,
    require_private: bool,
) -> Result<Vec<u8>, ()> {
    use std::os::unix::fs::MetadataExt;

    let relative_path = Path::new(file_name);
    if relative_path.components().count() != 1
        || !matches!(
            relative_path.components().next(),
            Some(Component::Normal(_))
        )
    {
        return Err(());
    }
    let directory_before = fs::symlink_metadata(directory_path).map_err(|_| ())?;
    let directory_opened = directory.metadata().map_err(|_| ())?;
    if directory_before.file_type().is_symlink()
        || !directory_before.is_dir()
        || !same_file(&directory_before, &directory_opened)
    {
        return Err(());
    }

    let path = directory_path.join(relative_path);
    let before = fs::symlink_metadata(&path).map_err(|_| ())?;
    if before.file_type().is_symlink() || !before.is_file() {
        return Err(());
    }

    let opened_file = OpenOptions::new().read(true).open(&path).map_err(|_| ())?;
    let opened = opened_file.metadata().map_err(|_| ())?;
    let after = fs::symlink_metadata(&path).map_err(|_| ())?;
    let directory_after = fs::symlink_metadata(directory_path).map_err(|_| ())?;
    if !opened.is_file()
        || !same_file(&before, &opened)
        || !same_file(&opened, &after)
        || !same_file(&directory_opened, &directory_after)
        || opened.uid() != expected_owner
        || opened.len() == 0
        || opened.len() > maximum_bytes as u64
        || (require_private && opened.mode() & 0o7777 != 0o600)
        || (!require_private && opened.mode() & 0o022 != 0)
    {
        return Err(());
    }

    let mut bytes = Vec::with_capacity(opened.len() as usize);
    opened_file
        .take((maximum_bytes as u64).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| ())?;
    if bytes.len() > maximum_bytes {
        return Err(());
    }
    Ok(bytes)
}

#[cfg(unix)]
fn same_file(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(unix)]
fn valid_scope_id(value: &str) -> bool {
    !value.is_empty()
        && value.trim() == value
        && value.len() <= 512
        && !value.chars().any(char::is_control)
}

#[cfg(unix)]
fn valid_secret_file_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_SECRET_FILE_NAME_BYTES
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

#[cfg(unix)]
fn valid_credential(value: &[u8]) -> bool {
    (MIN_CREDENTIAL_BYTES..=MAX_CREDENTIAL_BYTES).contains(&value.len())
        && value.iter().all(|byte| (b'!'..=b'~').contains(byte))
}

#[cfg(unix)]
fn parse_owner(value: &str) -> Option<Owner> {
    match value {
        "CATALYST" => Some(Owner::Catalyst),
        "YIELD" => Some(Owner::Yield),
        "REACTOR" => Some(Owner::Reactor),
        "EXCHANGE" => Some(Owner::Exchange),
        "ECHO" => Some(Owner::Echo),
        "NAVIGATOR" => Some(Owner::Navigator),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    use std::path::PathBuf;

    use tempfile::TempDir;

    use super::*;
    use crate::ProductHttpClient;

    const LONG_CREDENTIAL: &str = "test-workspace-private-bearer-credential-0123456789";

    struct Fixture {
        root: TempDir,
        manifest: PathBuf,
        secrets: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().expect("temporary directory");
            fs::set_permissions(root.path(), fs::Permissions::from_mode(0o755))
                .expect("manifest parent permissions");
            let manifest = root.path().join("product-endpoints.json");
            let secrets = root.path().join("secrets");
            fs::create_dir(&secrets).expect("secret directory");
            fs::set_permissions(&secrets, fs::Permissions::from_mode(0o700))
                .expect("private directory mode");
            write_secret(
                &secrets.join("catalyst-token"),
                LONG_CREDENTIAL.as_bytes(),
                0o600,
            );
            write_manifest(&manifest, &document("catalyst-token"));
            Self {
                root,
                manifest,
                secrets,
            }
        }
    }

    fn document(secret_file: &str) -> String {
        format!(
            r#"{{"version":1,"endpoints":[{{"owner":"CATALYST","organizationId":"org-1","workspaceId":"workspace-1","baseUrl":"https://catalyst.example.test/","credentialFile":"{secret_file}"}}]}}"#
        )
    }

    fn write_manifest(path: &Path, value: &str) {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .expect("create private manifest");
        file.write_all(value.as_bytes()).expect("write manifest");
    }

    fn write_secret(path: &Path, value: &[u8], mode: u32) {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(path)
            .expect("create private secret");
        file.write_all(value).expect("write secret");
    }

    #[test]
    fn loads_exact_scoped_endpoints_from_private_server_files() {
        let fixture = Fixture::new();
        let configs = load_product_endpoint_configs(&fixture.manifest, &fixture.secrets)
            .expect("valid manifest and private secret");

        assert_eq!(configs.len(), 1);
        assert!(!format!("{:?}", configs[0]).contains(LONG_CREDENTIAL));
        assert!(ProductHttpClient::from_private_config(configs).is_ok());
        let _keep_root_alive = fixture.root;
    }

    #[test]
    fn missing_malformed_and_empty_configuration_fail_closed() {
        let fixture = Fixture::new();
        let missing = fixture.root.path().join("missing.json");
        assert!(matches!(
            load_product_endpoint_configs(&missing, &fixture.secrets),
            Err(ProductEndpointManifestError::ManifestInvalid)
        ));

        let malformed = fixture.root.path().join("malformed.json");
        write_manifest(
            &malformed,
            r#"{"version":1,"endpoints":[{"owner":"OTHER","organizationId":"org-1","workspaceId":"workspace-1","baseUrl":"https://catalyst.example.test/","credentialFile":"catalyst-token"}]}"#,
        );
        assert!(load_product_endpoint_configs(&malformed, &fixture.secrets).is_err());

        let unknown_field = fixture.root.path().join("unknown-field.json");
        write_manifest(
            &unknown_field,
            &document("catalyst-token").replace("\"version\":1", "\"version\":1,\"unknown\":true"),
        );
        assert!(load_product_endpoint_configs(&unknown_field, &fixture.secrets).is_err());

        let invalid_url = fixture.root.path().join("invalid-url.json");
        write_manifest(
            &invalid_url,
            &document("catalyst-token").replace("https://", "http://"),
        );
        assert!(load_product_endpoint_configs(&invalid_url, &fixture.secrets).is_err());

        let empty = fixture.root.path().join("empty.json");
        write_manifest(&empty, r#"{"version":1,"endpoints":[]}"#);
        assert!(load_product_endpoint_configs(&empty, &fixture.secrets).is_err());
    }

    #[test]
    fn path_traversal_symlinks_and_broad_secret_permissions_are_rejected() {
        let fixture = Fixture::new();
        let traversal = fixture.root.path().join("traversal.json");
        write_manifest(&traversal, &document("../catalyst-token"));
        assert!(load_product_endpoint_configs(&traversal, &fixture.secrets).is_err());

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let linked = fixture.secrets.join("linked-token");
            symlink(fixture.secrets.join("catalyst-token"), &linked).expect("create symlink");
            let linked_manifest = fixture.root.path().join("linked.json");
            write_manifest(&linked_manifest, &document("linked-token"));
            assert!(load_product_endpoint_configs(&linked_manifest, &fixture.secrets).is_err());

            let linked_manifest_path = fixture.root.path().join("manifest-link.json");
            symlink(&fixture.manifest, &linked_manifest_path).expect("link manifest file");
            assert!(
                load_product_endpoint_configs(&linked_manifest_path, &fixture.secrets).is_err()
            );
        }

        let broad = fixture.secrets.join("broad-token");
        write_secret(&broad, LONG_CREDENTIAL.as_bytes(), 0o640);
        let broad_manifest = fixture.root.path().join("broad.json");
        write_manifest(&broad_manifest, &document("broad-token"));
        assert!(load_product_endpoint_configs(&broad_manifest, &fixture.secrets).is_err());
    }

    #[test]
    fn short_reused_and_non_ascii_credentials_are_rejected() {
        let fixture = Fixture::new();
        let short = fixture.secrets.join("short-token");
        write_secret(&short, b"short-token", 0o600);
        let short_manifest = fixture.root.path().join("short.json");
        write_manifest(&short_manifest, &document("short-token"));
        assert!(load_product_endpoint_configs(&short_manifest, &fixture.secrets).is_err());

        let duplicate_manifest = fixture.root.path().join("duplicate.json");
        let duplicate = r#"{"version":1,"endpoints":[{"owner":"CATALYST","organizationId":"org-1","workspaceId":"workspace-1","baseUrl":"https://catalyst.example.test/","credentialFile":"catalyst-token"},{"owner":"ECHO","organizationId":"org-1","workspaceId":"workspace-1","baseUrl":"https://echo.example.test/","credentialFile":"catalyst-token"}]}"#;
        write_manifest(&duplicate_manifest, duplicate);
        assert!(load_product_endpoint_configs(&duplicate_manifest, &fixture.secrets).is_err());

        let non_ascii = fixture.secrets.join("non-ascii-token");
        write_secret(
            &non_ascii,
            "token-with-µ-non-ascii-value-123456".as_bytes(),
            0o600,
        );
        let non_ascii_manifest = fixture.root.path().join("non-ascii.json");
        write_manifest(&non_ascii_manifest, &document("non-ascii-token"));
        assert!(load_product_endpoint_configs(&non_ascii_manifest, &fixture.secrets).is_err());
    }
}
