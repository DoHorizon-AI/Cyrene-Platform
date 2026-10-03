//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  Pinned Product contract bundle loader                              │
//! │  Module: cy_workspace_product_contracts::bundle                     │
//! │  Role: Verify owner pins, route closure, and JSON schemas.            │
//! │                                                                     │
//! │  模块职责：校验 owner 来源 pin、OpenAPI 路由闭包与 JSON schema。         │
//! └─────────────────────────────────────────────────────────────────────┘

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path};
use std::sync::Arc;

use jsonschema::{Draft, JSONSchema};
use regex::Regex;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::invocation::{ProductInvocationError, ResolvedProductRoute};
use crate::strict_json::{parse_json_bytes, PRODUCT_JSON_BYTES_LIMIT};

const BUNDLE_MANIFEST_NAME: &str = "product-contract-bundle.json";
const MAX_BUNDLE_FILES: usize = 512;
const MAX_BUNDLE_BYTES: usize = 32 * 1024 * 1024;
const MAX_DOCUMENT_BYTES: usize = 8 * 1024 * 1024;
const MAX_INVOCATION_BYTES: usize = PRODUCT_JSON_BYTES_LIMIT;
const MAX_OWNER_ID_BYTES: usize = 63;
const MAX_SCOPE_ID_BYTES: usize = 512;
const MAX_IDEMPOTENCY_KEY_BYTES: usize = 200;
const WIRE_API_VERSION: &str = "cyrene.workspace.product.v2";

/// Stable failure returned for an invalid pin, bundle, catalog, route, or schema.
///
/// Catalog contents are treated as untrusted until every digest and route
/// relationship has been checked. / 目录内容在完成摘要与路由校验前均视为不可信。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CatalogError {
    /// The bundle manifest is missing or malformed.
    #[error("PRODUCT_BUNDLE_MANIFEST_INVALID")]
    ManifestInvalid,
    /// The embedded manifest, owner, or policy pin does not match the release lock.
    #[error("PRODUCT_BUNDLE_PIN_MISMATCH")]
    PinMismatch,
    /// A listed artifact is missing, unsafe, or has an unexpected digest.
    #[error("PRODUCT_BUNDLE_FILE_INVALID")]
    FileInvalid,
    /// An owner catalog is malformed or does not match its release record.
    #[error("PRODUCT_OWNER_CATALOG_INVALID")]
    CatalogInvalid,
    /// Two entries resolve to the same owner-scoped operation key.
    #[error("PRODUCT_OPERATION_DUPLICATE")]
    DuplicateOperation,
    /// A route or schema pointer does not uniquely resolve in pinned OpenAPI.
    #[error("PRODUCT_OPERATION_ROUTE_INVALID")]
    RouteInvalid,
    /// A JSON Schema is malformed, unpinned, or cannot be compiled.
    #[error("PRODUCT_OPERATION_SCHEMA_INVALID")]
    SchemaInvalid,
    /// A bundled source document contains an unavailable or remote reference.
    #[error("PRODUCT_CONTRACT_REFERENCE_INVALID")]
    ReferenceInvalid,
}

/// Trusted build-time release pins consumed by the runtime bundle loader.
///
/// Values come from the independently reviewed Platform release lock, never
/// from a request or the bundle being verified. / pin 来自独立审阅的 Platform 发布锁。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductBundlePins {
    wire_api_version: String,
    manifest_sha256: String,
    owner_source_shas: BTreeMap<String, String>,
    policy_schema_version: String,
    policy_sha256: String,
}

impl ProductBundlePins {
    /// Builds the pin set embedded by a release-aware host or BFF.
    pub fn new(
        wire_api_version: impl Into<String>,
        manifest_sha256: impl Into<String>,
        owner_source_shas: BTreeMap<String, String>,
        policy_schema_version: impl Into<String>,
        policy_sha256: impl Into<String>,
    ) -> Self {
        Self {
            wire_api_version: wire_api_version.into(),
            manifest_sha256: manifest_sha256.into(),
            owner_source_shas,
            policy_schema_version: policy_schema_version.into(),
            policy_sha256: policy_sha256.into(),
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn empty_for_test() -> Self {
        Self {
            wire_api_version: "cyrene.workspace.product.v2".to_string(),
            manifest_sha256: "0000000000000000000000000000000000000000000000000000000000000000"
                .to_string(),
            owner_source_shas: BTreeMap::new(),
            policy_schema_version: "cyrene.workspace.product.authorization-policy.v2".to_string(),
            policy_sha256: "0000000000000000000000000000000000000000000000000000000000000000"
                .to_string(),
        }
    }

    /// Returns the pinned wire API version.
    pub fn wire_api_version(&self) -> &str {
        &self.wire_api_version
    }

    /// Returns the independently pinned bundle manifest digest.
    pub fn manifest_sha256(&self) -> &str {
        &self.manifest_sha256
    }

    /// Returns the exact owner source SHA map expected from the manifest.
    pub fn owner_source_shas(&self) -> &BTreeMap<String, String> {
        &self.owner_source_shas
    }

    /// Returns the policy schema version required by this release.
    pub fn policy_schema_version(&self) -> &str {
        &self.policy_schema_version
    }

    /// Returns the independently pinned Platform policy digest.
    pub fn policy_sha256(&self) -> &str {
        &self.policy_sha256
    }
}

/// The operation class published by the Product owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ProductOperationKind {
    /// A read operation that does not issue a Product command.
    Read,
    /// A command operation whose grant is separately approved by Platform.
    Command,
}

/// A JSON scope selector and its expected value from verified Platform context.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JsonScopeBinding {
    /// RFC 6901 pointer; `*` matches each item in an array or object.
    pub json_pointer: String,
    /// The immutable value to compare at every selected location.
    pub matches: MatchContextField,
}

/// A field supplied only by verified Platform context or the invocation resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MatchContextField {
    /// Authenticated organization identifier.
    OrganizationId,
    /// Directory-verified Workspace identifier.
    WorkspaceId,
    /// Owner resource identifier selected by the invocation.
    ResourceId,
}

/// Catalog rules for the optional owner resource identifier.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceIdConstraints {
    /// Whether the invocation must provide a resource identifier.
    pub required: bool,
    /// OpenAPI path parameter receiving this resource identifier, if any.
    pub path_parameter: Option<String>,
    /// Minimum UTF-8 byte length accepted by the generic wire contract.
    pub min_length: usize,
    /// Maximum UTF-8 byte length accepted by the generic wire contract.
    pub max_length: usize,
    /// Optional owner-defined regular expression applied to the identifier.
    pub pattern: Option<String>,
}

/// Catalog rules for the standard `Idempotency-Key` request header.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IdempotencyConstraints {
    /// Whether a key must be supplied for this operation.
    pub required: bool,
    /// Header name; when configured it must be `Idempotency-Key`.
    pub header: Option<String>,
    /// Minimum key length accepted by the catalog.
    pub min_length: usize,
    /// Maximum key length accepted by the catalog.
    pub max_length: usize,
}

/// Scope injection and JSON body/response selectors from an owner catalog.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScopeBindings {
    /// OpenAPI path parameter bound to the verified organization, if present.
    pub organization_path_parameter: Option<String>,
    /// OpenAPI path parameter bound to the verified Workspace, if present.
    pub workspace_path_parameter: Option<String>,
    /// Request JSON selectors that must agree with their verified context value.
    pub request_bindings: Vec<JsonScopeBinding>,
    /// Response JSON selectors that must agree with the authorized call context.
    pub response_bindings: Vec<JsonScopeBinding>,
}

/// A digest-verified bundle of generic owner catalogs and resolved operations.
#[derive(Debug)]
pub struct ProductContractBundle {
    manifest_sha256: String,
    owner_source_shas: BTreeMap<String, String>,
    operations: BTreeMap<(String, String), ProductOperation>,
}

impl ProductContractBundle {
    /// Loads the fixed bundle manifest and verifies every independent release pin.
    ///
    /// Args:
    /// * `root` — Server-owned directory containing the bundle manifest and files.
    /// * `pins` — Pins embedded from the Platform release lock.
    ///
    /// Returns:
    /// * A bundle whose operation routes and schemas resolve only from listed, hashed files.
    ///
    /// Errors:
    /// * [`CatalogError`] if any manifest, source, file, route, reference, or schema check fails.
    pub fn load(root: impl AsRef<Path>, pins: &ProductBundlePins) -> Result<Self, CatalogError> {
        let root = root
            .as_ref()
            .canonicalize()
            .map_err(|_| CatalogError::ManifestInvalid)?;
        let manifest_path = root.join(BUNDLE_MANIFEST_NAME);
        let manifest_bytes = read_regular_file(&root, &manifest_path)?;
        let actual_manifest_sha256 = sha256_hex(&manifest_bytes);
        if !constant_time_hex_eq(&actual_manifest_sha256, &pins.manifest_sha256)
            || pins.wire_api_version != WIRE_API_VERSION
            || !valid_sha256(&pins.policy_sha256)
            || pins.policy_schema_version != "cyrene.workspace.product.authorization-policy.v2"
        {
            return Err(CatalogError::PinMismatch);
        }

        let manifest: BundleManifest =
            serde_json::from_slice(&manifest_bytes).map_err(|_| CatalogError::ManifestInvalid)?;
        if manifest.format_version != 2 || manifest.wire_api_version != WIRE_API_VERSION {
            return Err(CatalogError::ManifestInvalid);
        }
        if manifest.files.is_empty()
            || manifest.files.len() > MAX_BUNDLE_FILES
            || manifest.owners.is_empty()
            || manifest.owners.len() != pins.owner_source_shas.len()
        {
            return Err(CatalogError::ManifestInvalid);
        }
        ensure_sorted_unique(manifest.files.iter().map(|entry| entry.path.as_str()))?;
        ensure_sorted_unique(manifest.owners.iter().map(|entry| entry.owner_id.as_str()))?;

        let mut documents = BTreeMap::new();
        let mut total_bytes = 0usize;
        for file in &manifest.files {
            if !valid_bundle_relative_path(&file.path) || !valid_sha256(&file.sha256) {
                return Err(CatalogError::FileInvalid);
            }
            let path = root.join(&file.path);
            let bytes = read_regular_file(&root, &path)?;
            total_bytes = total_bytes
                .checked_add(bytes.len())
                .ok_or(CatalogError::FileInvalid)?;
            if bytes.len() > MAX_DOCUMENT_BYTES
                || total_bytes > MAX_BUNDLE_BYTES
                || !constant_time_hex_eq(&sha256_hex(&bytes), &file.sha256)
            {
                return Err(CatalogError::FileInvalid);
            }
            let document = parse_document(&file.path, &bytes)?;
            documents.insert(file.path.clone(), document);
        }

        let file_digests = manifest
            .files
            .iter()
            .map(|entry| (entry.path.as_str(), entry.sha256.as_str()))
            .collect::<BTreeMap<_, _>>();
        let mut owner_source_shas = BTreeMap::new();
        let mut operations = BTreeMap::new();

        for owner in &manifest.owners {
            if !valid_owner_id(&owner.owner_id)
                || !valid_repository_id(&owner.repository)
                || !valid_commit_sha(&owner.source_sha)
                || owner.catalog_path
                    != format!("{}/contracts/product/v2/catalog.json", owner.repository)
                || !valid_sha256(&owner.catalog_sha256)
                || pins.owner_source_shas.get(&owner.owner_id) != Some(&owner.source_sha)
                || owner_source_shas
                    .insert(owner.owner_id.clone(), owner.source_sha.clone())
                    .is_some()
            {
                return Err(CatalogError::PinMismatch);
            }
            if file_digests.get(owner.catalog_path.as_str()) != Some(&owner.catalog_sha256.as_str())
            {
                return Err(CatalogError::FileInvalid);
            }
            let catalog_document = documents
                .get(&owner.catalog_path)
                .ok_or(CatalogError::CatalogInvalid)?;
            let catalog_bytes = read_regular_file(&root, &root.join(&owner.catalog_path))?;
            if !constant_time_hex_eq(&sha256_hex(&catalog_bytes), &owner.catalog_sha256) {
                return Err(CatalogError::FileInvalid);
            }
            let catalog: OwnerCatalog = serde_json::from_value(catalog_document.clone())
                .map_err(|_| CatalogError::CatalogInvalid)?;
            if catalog.schema_version != "cyrene.product.operation-catalog.v2"
                || catalog.catalog_version.is_empty()
                || catalog.owner_id != owner.owner_id
                || catalog.operations.is_empty()
            {
                return Err(CatalogError::CatalogInvalid);
            }

            let mut local_operation_ids = BTreeSet::new();
            for operation in catalog.operations {
                if operation.operation_id != operation.route_id
                    || !local_operation_ids.insert(operation.operation_id.clone())
                {
                    return Err(CatalogError::DuplicateOperation);
                }
                let document_path = format!("{}/{}", owner.repository, operation.openapi_path);
                if !document_path.starts_with(&format!("{}/contracts/product/v", owner.repository))
                    || !documents.contains_key(&document_path)
                {
                    return Err(CatalogError::FileInvalid);
                }
                let document = documents
                    .get(&document_path)
                    .ok_or(CatalogError::FileInvalid)?;
                validate_local_references(&document_path, document, &documents, &owner.repository)?;
                let (route, documented_status_codes, request_body_required) = find_route(
                    &document_path,
                    document,
                    &operation.operation_id,
                    &operation,
                    &documents,
                )?;
                let request_schema = match operation.request_schema_pointer.as_deref() {
                    Some(pointer) => Some(compile_schema(
                        pointer,
                        &document_path,
                        document,
                        &documents,
                    )?),
                    None => None,
                };
                let mut response_schemas =
                    Vec::with_capacity(operation.response_schema_pointers.len());
                let mut response_schema_pointers = Vec::new();
                for pointer in &operation.response_schema_pointers {
                    let status =
                        pointer_response_status(pointer).ok_or(CatalogError::RouteInvalid)?;
                    let validator = compile_schema(pointer, &document_path, document, &documents)?;
                    response_schemas.push(ResponseSchema { status, validator });
                    response_schema_pointers.push(pointer.clone());
                }
                let key = (owner.owner_id.clone(), operation.operation_id.clone());
                let product_operation = ProductOperation {
                    owner_id: owner.owner_id.clone(),
                    operation_id: operation.operation_id,
                    route_id: operation.route_id,
                    catalog_version: catalog.catalog_version.clone(),
                    kind: operation.kind,
                    route,
                    request_schema,
                    response_schemas,
                    response_schema_pointers,
                    documented_status_codes,
                    request_body_required,
                    resource_id: operation.resource_id,
                    idempotency: operation.idempotency,
                    scope: operation.scope,
                };
                if operations.insert(key, product_operation).is_some() {
                    return Err(CatalogError::DuplicateOperation);
                }
            }
        }
        if owner_source_shas != pins.owner_source_shas {
            return Err(CatalogError::PinMismatch);
        }
        Ok(Self {
            manifest_sha256: actual_manifest_sha256,
            owner_source_shas,
            operations,
        })
    }

    /// Returns the source-pinned SHA-256 of the loaded bundle manifest.
    pub fn manifest_sha256(&self) -> &str {
        &self.manifest_sha256
    }

    /// Returns the exact Git commit SHA pinned for `owner_id`, if present.
    pub fn source_sha(&self, owner_id: &str) -> Option<&str> {
        self.owner_source_shas.get(owner_id).map(String::as_str)
    }

    /// Returns sorted owner identifiers present in the pinned release.
    pub fn owner_ids(&self) -> impl Iterator<Item = &str> {
        self.owner_source_shas.keys().map(String::as_str)
    }

    /// Returns whether a Product owner is present in the pinned release.
    pub fn contains_owner_id(&self, owner_id: &str) -> bool {
        self.owner_source_shas.contains_key(owner_id)
    }

    /// Resolves a generic owner/operation pair without consulting caller route data.
    pub fn operation(&self, owner_id: &str, operation_id: &str) -> Option<&ProductOperation> {
        self.operations
            .get(&(owner_id.to_owned(), operation_id.to_owned()))
    }

    /// Returns an iterator over all resolved Product operations in the bundle.
    pub fn operations(&self) -> impl Iterator<Item = &ProductOperation> {
        self.operations.values()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn empty_for_test() -> Self {
        Self {
            manifest_sha256: "0000000000000000000000000000000000000000000000000000000000000000"
                .to_string(),
            owner_source_shas: BTreeMap::new(),
            operations: BTreeMap::new(),
        }
    }
}

/// A source-pinned, schema-validated Product operation resolved from OpenAPI.
#[derive(Debug, Clone)]
pub struct ProductOperation {
    owner_id: String,
    operation_id: String,
    route_id: String,
    catalog_version: String,
    kind: ProductOperationKind,
    route: ResolvedProductRoute,
    request_schema: Option<CompiledSchema>,
    response_schemas: Vec<ResponseSchema>,
    response_schema_pointers: Vec<String>,
    documented_status_codes: BTreeSet<u16>,
    request_body_required: bool,
    resource_id: ResourceIdConstraints,
    idempotency: IdempotencyConstraints,
    scope: ScopeBindings,
}

impl ProductOperation {
    /// Returns the stable owner ID from the source-pinned catalog.
    pub fn owner_id(&self) -> &str {
        &self.owner_id
    }

    /// Returns the owner-scoped OpenAPI operation identifier.
    pub fn operation_id(&self) -> &str {
        &self.operation_id
    }

    /// Returns the catalog route key, equal to the unique OpenAPI operation ID.
    pub fn route_id(&self) -> &str {
        &self.route_id
    }

    /// Returns the catalog version pinned inside this release.
    pub fn catalog_version(&self) -> &str {
        &self.catalog_version
    }

    /// Returns the owner-declared read or command classification.
    pub fn kind(&self) -> ProductOperationKind {
        self.kind
    }

    /// Returns whether the OpenAPI request body is required.
    pub fn requires_body(&self) -> bool {
        self.request_body_required
    }

    /// Returns the request JSON Schema, if this operation accepts JSON.
    pub fn request_schema(&self) -> Option<&Value> {
        self.request_schema.as_ref().map(|schema| &schema.schema)
    }

    /// Returns all successful JSON response schema pointers for this route.
    pub fn response_schema_pointers(&self) -> &[String] {
        &self.response_schema_pointers
    }

    /// Returns the catalog resource identifier constraints.
    pub fn resource_id(&self) -> &ResourceIdConstraints {
        &self.resource_id
    }

    /// Validates an optional resource identifier against the owner catalog.
    pub fn validate_resource_id(&self, value: Option<&str>) -> Result<(), ProductInvocationError> {
        match value {
            None if self.resource_id.required => Err(ProductInvocationError::InvalidRequest),
            None => Ok(()),
            Some(value)
                if self.resource_id.path_parameter.is_none()
                    || value.len() < self.resource_id.min_length
                    || value.len() > self.resource_id.max_length
                    || !is_safe_path_segment(value) =>
            {
                Err(ProductInvocationError::InvalidRequest)
            }
            Some(value) => {
                if let Some(pattern) = self.resource_id.pattern.as_deref() {
                    let pattern =
                        Regex::new(pattern).map_err(|_| ProductInvocationError::Internal)?;
                    if !pattern.is_match(value) {
                        return Err(ProductInvocationError::InvalidRequest);
                    }
                }
                Ok(())
            }
        }
    }

    /// Returns the catalog idempotency constraints.
    pub fn idempotency(&self) -> &IdempotencyConstraints {
        &self.idempotency
    }

    /// Validates an optional standard idempotency key against the owner catalog.
    pub fn validate_idempotency_key(
        &self,
        value: Option<&str>,
    ) -> Result<(), ProductInvocationError> {
        match value {
            None if self.idempotency.required => Err(ProductInvocationError::InvalidRequest),
            None => Ok(()),
            Some(value)
                if self.idempotency.header.as_deref() != Some("Idempotency-Key")
                    || value.len() < self.idempotency.min_length
                    || value.len() > self.idempotency.max_length
                    || !value.bytes().all(is_http_token_byte) =>
            {
                Err(ProductInvocationError::InvalidRequest)
            }
            Some(_) => Ok(()),
        }
    }

    /// Returns the owner-declared path and JSON scope selectors.
    pub fn scope_bindings(&self) -> &ScopeBindings {
        &self.scope
    }

    /// Parses and validates original request JSON bytes while preserving their bytes.
    pub fn validate_request(
        &self,
        body: Option<&[u8]>,
    ) -> Result<Option<Value>, ProductInvocationError> {
        match (body, self.request_schema.as_ref()) {
            (None, None) if !self.request_body_required => Ok(None),
            (None, None) => Err(ProductInvocationError::InvalidRequest),
            (None, Some(_)) if !self.request_body_required => Ok(None),
            (None, Some(_)) => Err(ProductInvocationError::InvalidRequest),
            (Some(_), None) => Err(ProductInvocationError::InvalidRequest),
            (Some(bytes), Some(schema)) => {
                if bytes.is_empty() || bytes.len() > MAX_INVOCATION_BYTES {
                    return Err(ProductInvocationError::InvalidRequest);
                }
                let body =
                    parse_json_bytes(bytes).map_err(|_| ProductInvocationError::InvalidRequest)?;
                if !schema.validator.is_valid(&body) {
                    return Err(ProductInvocationError::InvalidRequest);
                }
                Ok(Some(body))
            }
        }
    }

    /// Checks the Product response status, media type, body size, and declared schema.
    pub fn validate_response(
        &self,
        status_code: u16,
        content_type: &str,
        body: &[u8],
    ) -> Result<Option<Value>, ProductInvocationError> {
        if body.len() > MAX_INVOCATION_BYTES || !self.documented_status_codes.contains(&status_code)
        {
            return Err(ProductInvocationError::Internal);
        }
        if status_code >= 500 {
            return Err(ProductInvocationError::Unavailable);
        }
        let media_type = content_type
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        if body.is_empty() {
            if self
                .response_schemas
                .iter()
                .any(|schema| schema.status == status_code)
            {
                return Err(ProductInvocationError::Internal);
            }
            return Ok(None);
        }
        if !matches!(
            media_type.as_str(),
            "application/json" | "application/problem+json"
        ) {
            return Err(ProductInvocationError::Internal);
        }
        let value = parse_json_bytes(body).map_err(|_| ProductInvocationError::Internal)?;
        if (200..300).contains(&status_code) {
            let validators = self
                .response_schemas
                .iter()
                .filter(|schema| schema.status == status_code)
                .collect::<Vec<_>>();
            if !validators.is_empty()
                && !validators
                    .iter()
                    .any(|schema| schema.validator.validator.is_valid(&value))
            {
                return Err(ProductInvocationError::Internal);
            }
            if validators.is_empty() && !value.is_null() {
                return Err(ProductInvocationError::Internal);
            }
        } else if !value.is_object() {
            return Err(ProductInvocationError::Internal);
        }
        Ok(Some(value))
    }

    pub(crate) fn route(&self) -> &ResolvedProductRoute {
        &self.route
    }
}

#[derive(Debug, Clone)]
struct CompiledSchema {
    schema: Value,
    validator: Arc<JSONSchema>,
}

#[derive(Debug, Clone)]
struct ResponseSchema {
    status: u16,
    validator: CompiledSchema,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BundleManifest {
    format_version: u32,
    wire_api_version: String,
    owners: Vec<OwnerRelease>,
    files: Vec<BundleFile>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OwnerRelease {
    owner_id: String,
    repository: String,
    source_sha: String,
    catalog_path: String,
    catalog_sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BundleFile {
    path: String,
    sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OwnerCatalog {
    schema_version: String,
    catalog_version: String,
    owner_id: String,
    operations: Vec<OperationSpec>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OperationSpec {
    operation_id: String,
    route_id: String,
    openapi_path: String,
    kind: ProductOperationKind,
    request_schema_pointer: Option<String>,
    response_schema_pointers: Vec<String>,
    resource_id: ResourceIdConstraints,
    idempotency: IdempotencyConstraints,
    scope: ScopeBindings,
}

/// Reads a regular file whose canonical path remains inside the bundle root.
fn read_regular_file(root: &Path, path: &Path) -> Result<Vec<u8>, CatalogError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| CatalogError::FileInvalid)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(CatalogError::FileInvalid);
    }
    let canonical = path.canonicalize().map_err(|_| CatalogError::FileInvalid)?;
    if !canonical.starts_with(root) {
        return Err(CatalogError::FileInvalid);
    }
    fs::read(canonical).map_err(|_| CatalogError::FileInvalid)
}

/// Parses JSON and YAML files into the shared JSON data model.
fn parse_document(path: &str, bytes: &[u8]) -> Result<Value, CatalogError> {
    match Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
    {
        Some("json") => serde_json::from_slice(bytes).map_err(|_| CatalogError::FileInvalid),
        Some("yaml" | "yml") => {
            let yaml: serde_yaml::Value =
                serde_yaml::from_slice(bytes).map_err(|_| CatalogError::FileInvalid)?;
            serde_json::to_value(yaml).map_err(|_| CatalogError::FileInvalid)
        }
        _ => Err(CatalogError::FileInvalid),
    }
}

/// Resolves an operation by its unique OpenAPI operation ID and validates catalog bindings.
fn find_route(
    document_path: &str,
    document: &Value,
    operation_id: &str,
    spec: &OperationSpec,
    documents: &BTreeMap<String, Value>,
) -> Result<(ResolvedProductRoute, BTreeSet<u16>, bool), CatalogError> {
    let paths = document
        .get("paths")
        .and_then(Value::as_object)
        .ok_or(CatalogError::RouteInvalid)?;
    let mut matches = Vec::new();
    for (path, item) in paths {
        let methods = item.as_object().ok_or(CatalogError::RouteInvalid)?;
        for (method, operation) in methods {
            if !matches!(method.as_str(), "get" | "put" | "post" | "delete" | "patch") {
                continue;
            }
            if operation.get("operationId").and_then(Value::as_str) == Some(operation_id) {
                matches.push((path.clone(), method.to_ascii_uppercase(), operation));
            }
        }
    }
    if matches.len() != 1 {
        return Err(CatalogError::RouteInvalid);
    }
    let (path_template, method, operation) = matches.pop().expect("one matched route");
    validate_operation_schema_pointers(
        document_path,
        document,
        operation,
        &path_template,
        &method,
        spec,
    )?;
    validate_catalog_constraints(operation, document_path, documents, spec)?;
    let placeholders = path_placeholders(&path_template)?;
    let mut declared = BTreeSet::new();
    for value in [
        spec.scope.organization_path_parameter.as_deref(),
        spec.scope.workspace_path_parameter.as_deref(),
        spec.resource_id.path_parameter.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        if !declared.insert(value.to_owned()) {
            return Err(CatalogError::RouteInvalid);
        }
    }
    if placeholders != declared {
        return Err(CatalogError::RouteInvalid);
    }
    let documented_status_codes = operation
        .get("responses")
        .and_then(Value::as_object)
        .ok_or(CatalogError::RouteInvalid)?
        .keys()
        .filter_map(|status| status.parse::<u16>().ok())
        .collect::<BTreeSet<_>>();
    if documented_status_codes.is_empty() {
        return Err(CatalogError::RouteInvalid);
    }
    let request_body_required = operation
        .get("requestBody")
        .and_then(Value::as_object)
        .and_then(|body| body.get("required"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Ok((
        ResolvedProductRoute::new(method, path_template),
        documented_status_codes,
        request_body_required,
    ))
}

/// Confirms the owner catalog points to the route's actual OpenAPI request and success schemas.
fn validate_operation_schema_pointers(
    document_path: &str,
    document: &Value,
    operation: &Value,
    route_path: &str,
    method: &str,
    spec: &OperationSpec,
) -> Result<(), CatalogError> {
    let method_key = method.to_ascii_lowercase();
    let route_pointer = format!(
        "/paths/{}/{}/",
        escape_pointer_segment(route_path),
        method_key
    );
    let request_pointer = format!(
        "#{}requestBody/content/application~1json/schema",
        route_pointer
    );
    let request_body = operation.get("requestBody");
    let expects_body = request_body.is_some();
    match (&spec.request_schema_pointer, expects_body) {
        (Some(pointer), true) if *pointer == request_pointer => {
            let schema =
                value_at_uri_pointer(document, pointer).ok_or(CatalogError::SchemaInvalid)?;
            if schema.is_null() {
                return Err(CatalogError::SchemaInvalid);
            }
        }
        (None, false) => {}
        _ => return Err(CatalogError::SchemaInvalid),
    }

    let responses = operation
        .get("responses")
        .and_then(Value::as_object)
        .ok_or(CatalogError::RouteInvalid)?;
    let mut actual_success_schema_pointers = Vec::new();
    for (status, response) in responses {
        if !is_success_status(status) {
            continue;
        }
        let Some(schema) = response.pointer("/content/application~1json/schema") else {
            continue;
        };
        if schema.is_null() {
            return Err(CatalogError::SchemaInvalid);
        }
        actual_success_schema_pointers.push(format!(
            "#{}responses/{}/content/application~1json/schema",
            route_pointer, status
        ));
    }
    actual_success_schema_pointers.sort();
    let mut catalog_schema_pointers = spec.response_schema_pointers.clone();
    catalog_schema_pointers.sort();
    if catalog_schema_pointers != actual_success_schema_pointers {
        return Err(CatalogError::SchemaInvalid);
    }
    for pointer in &catalog_schema_pointers {
        if value_at_uri_pointer(document, pointer).is_none() {
            return Err(CatalogError::SchemaInvalid);
        }
    }
    if let Some(request) = request_body {
        if request
            .get("content")
            .and_then(|content| content.get("application/json"))
            .is_none()
        {
            return Err(CatalogError::SchemaInvalid);
        }
    }
    let _ = document_path;
    Ok(())
}

/// Validates resource and idempotency constraints against their route parameters.
fn validate_catalog_constraints(
    operation: &Value,
    _document_path: &str,
    _documents: &BTreeMap<String, Value>,
    spec: &OperationSpec,
) -> Result<(), CatalogError> {
    if spec.resource_id.min_length == 0
        || spec.resource_id.max_length < spec.resource_id.min_length
        || spec.resource_id.max_length > MAX_SCOPE_ID_BYTES
        || spec.resource_id.required != spec.resource_id.path_parameter.is_some()
        || spec.idempotency.min_length == 0
        || spec.idempotency.max_length < spec.idempotency.min_length
        || spec.idempotency.max_length > MAX_IDEMPOTENCY_KEY_BYTES
        || (spec.idempotency.required
            && spec.idempotency.header.as_deref() != Some("Idempotency-Key"))
        || spec
            .idempotency
            .header
            .as_deref()
            .is_some_and(|header| header != "Idempotency-Key")
    {
        return Err(CatalogError::CatalogInvalid);
    }
    if let Some(pattern) = spec.resource_id.pattern.as_deref() {
        Regex::new(pattern).map_err(|_| CatalogError::CatalogInvalid)?;
    }
    if let Some(parameters) = operation.get("parameters").and_then(Value::as_array) {
        for parameter in parameters {
            if parameter.get("in").and_then(Value::as_str) == Some("query")
                && parameter.get("required").and_then(Value::as_bool) == Some(true)
            {
                return Err(CatalogError::RouteInvalid);
            }
        }
    }
    Ok(())
}

/// Compiles one catalog-selected schema with only digest-pinned local documents.
fn compile_schema(
    pointer: &str,
    document_path: &str,
    document: &Value,
    documents: &BTreeMap<String, Value>,
) -> Result<CompiledSchema, CatalogError> {
    let schema = value_at_uri_pointer(document, pointer)
        .ok_or(CatalogError::SchemaInvalid)?
        .clone();
    if !schema.is_object() {
        return Err(CatalogError::SchemaInvalid);
    }
    let mut schema = schema;
    if let Some(object) = schema.as_object_mut() {
        object
            .entry("$id".to_owned())
            .or_insert_with(|| Value::String(document_uri(document_path)));
    }
    let mut options = JSONSchema::options();
    options.with_draft(Draft::Draft202012);
    for (path, resource) in documents {
        let uri = document_uri(path);
        options.with_document(uri, resource.clone());
        if let Some(id) = resource.get("$id").and_then(Value::as_str) {
            options.with_document(id.to_owned(), resource.clone());
        }
    }
    let validator = options
        .compile(&schema)
        .map_err(|_| CatalogError::SchemaInvalid)?;
    Ok(CompiledSchema {
        schema,
        validator: Arc::new(validator),
    })
}

/// Rejects remote, root-absolute, and unavailable local references.
fn validate_local_references(
    current_path: &str,
    document: &Value,
    documents: &BTreeMap<String, Value>,
    repository: &str,
) -> Result<(), CatalogError> {
    let mut stack = vec![document];
    while let Some(value) = stack.pop() {
        match value {
            Value::Object(object) => {
                if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
                    let (target, _) = reference.split_once('#').unwrap_or((reference, ""));
                    if target.contains("://")
                        || target.starts_with('/')
                        || target.starts_with("data:")
                    {
                        return Err(CatalogError::ReferenceInvalid);
                    }
                    if !target.is_empty() {
                        let parent = Path::new(current_path)
                            .parent()
                            .ok_or(CatalogError::ReferenceInvalid)?;
                        let joined = parent.join(target);
                        let normalized = normalize_relative_path(&joined)
                            .ok_or(CatalogError::ReferenceInvalid)?;
                        if !normalized.starts_with(&format!("{repository}/contracts/product/v"))
                            || !documents.contains_key(&normalized)
                        {
                            return Err(CatalogError::ReferenceInvalid);
                        }
                    }
                }
                stack.extend(object.values());
            }
            Value::Array(items) => stack.extend(items),
            _ => {}
        }
    }
    Ok(())
}

/// Returns whether a byte is valid in an HTTP token value.
fn is_http_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}

/// Rejects path values with traversal or URL-splitting behavior.
fn is_safe_path_segment(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && !value.contains("..")
        && value.chars().all(|character| {
            !character.is_control() && !matches!(character, '/' | '\\' | '%' | '?' | '#')
        })
}

/// Extracts the set of URI-template parameters from an OpenAPI path.
fn path_placeholders(path: &str) -> Result<BTreeSet<String>, CatalogError> {
    let mut names = BTreeSet::new();
    let mut remaining = path;
    while let Some(start) = remaining.find('{') {
        let after_open = &remaining[start + 1..];
        let end = after_open.find('}').ok_or(CatalogError::RouteInvalid)?;
        let name = &after_open[..end];
        if name.is_empty() || name.contains('{') || !names.insert(name.to_owned()) {
            return Err(CatalogError::RouteInvalid);
        }
        remaining = &after_open[end + 1..];
    }
    if remaining.contains('}') || path.contains('?') || path.contains('#') {
        return Err(CatalogError::RouteInvalid);
    }
    Ok(names)
}

/// Reads a JSON Pointer URI fragment from a parsed document.
fn value_at_uri_pointer<'a>(document: &'a Value, pointer: &str) -> Option<&'a Value> {
    let fragment = pointer.strip_prefix('#')?;
    let decoded = percent_decode(fragment)?;
    document.pointer(&decoded)
}

/// Derives an RFC 6901 route pointer segment from one OpenAPI path string.
fn escape_pointer_segment(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

/// Extracts the status code represented by an operation response schema pointer.
fn pointer_response_status(pointer: &str) -> Option<u16> {
    let fragment = pointer.strip_prefix('#')?;
    let decoded = percent_decode(fragment)?;
    let segments = decoded.split('/').collect::<Vec<_>>();
    let response_index = segments
        .iter()
        .position(|segment| *segment == "responses")?;
    segments.get(response_index + 1)?.parse().ok()
}

/// Returns whether an OpenAPI response key denotes a successful HTTP status.
fn is_success_status(status: &str) -> bool {
    status
        .parse::<u16>()
        .is_ok_and(|code| (200..300).contains(&code))
}

/// Converts a bundle-relative path into a stable, non-network schema resource ID.
fn document_uri(path: &str) -> String {
    format!(
        "https://contracts.cyrene.invalid/{}",
        path.replace(' ', "%20")
    )
}

/// Normalizes a relative path without permitting parent traversal.
fn normalize_relative_path(path: &Path) -> Option<String> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => parts.push(value.to_str()?.to_owned()),
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop()?;
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(parts.join("/"))
}

/// Decodes percent escapes in a JSON Pointer fragment without treating `+` specially.
fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = *bytes.get(index + 1)?;
            let low = *bytes.get(index + 2)?;
            decoded.push((hex_value(high)? << 4) | hex_value(low)?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

/// Decodes one hexadecimal digit.
fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

/// Checks that a bundle path is a safe repository-relative file path.
fn valid_bundle_relative_path(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('/')
        && !value.contains('\\')
        && value
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// Checks owner IDs against the public catalog syntax.
fn valid_owner_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_OWNER_ID_BYTES
        && value.as_bytes()[0].is_ascii_lowercase()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// Checks a canonical Product repository directory name.
fn valid_repository_id(value: &str) -> bool {
    value.starts_with("Cyrene-")
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

/// Checks a full lowercase Git commit SHA.
fn valid_commit_sha(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Checks a lowercase hexadecimal SHA-256 digest.
fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Computes a lowercase SHA-256 digest.
fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Compares expected lowercase digests without an early exit on the first mismatch.
fn constant_time_hex_eq(actual: &str, expected: &str) -> bool {
    if actual.len() != expected.len() {
        return false;
    }
    actual
        .bytes()
        .zip(expected.bytes())
        .fold(0u8, |difference, (left, right)| difference | (left ^ right))
        == 0
}

/// Confirms a sequence is sorted and contains no duplicate identifiers.
fn ensure_sorted_unique<'a>(values: impl Iterator<Item = &'a str>) -> Result<(), CatalogError> {
    let values = values.collect::<Vec<_>>();
    if values.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(CatalogError::ManifestInvalid);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{path_placeholders, percent_decode};
    use std::collections::BTreeSet;

    #[test]
    fn parses_openapi_path_bindings_without_accepting_caller_routes() {
        assert_eq!(
            path_placeholders("/internal/workspaces/{workspace_id}/sessions/{session_id}").unwrap(),
            BTreeSet::from(["session_id".to_owned(), "workspace_id".to_owned()])
        );
        assert!(path_placeholders("/workspaces/{workspace_id").is_err());
        assert!(path_placeholders("/workspaces/{workspace_id}?host=caller").is_err());
    }

    #[test]
    fn decodes_json_pointer_fragments_without_plus_rewriting() {
        assert_eq!(
            percent_decode("/paths/~1api~1v1").as_deref(),
            Some("/paths/~1api~1v1")
        );
        assert_eq!(
            percent_decode("/content/application~1json").as_deref(),
            Some("/content/application~1json")
        );
        assert!(percent_decode("/bad%Q1").is_none());
    }
}
