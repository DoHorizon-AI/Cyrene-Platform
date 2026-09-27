// ╔══════════════════════════════════════════════════════════════════════╗
// ║ File: framework/crates/cy-workspace-web-bff/src/catalog_loader.rs  ║
// ║ Module: cy_workspace_web_bff::catalog_loader                       ║
// ║ Role: Load a provenance-pinned Product OpenAPI release bundle.      ║
// ║                                                                    ║
// ║ 模块：cy_workspace_web_bff::catalog_loader                         ║
// ║ 职责：加载带来源锁定的 Product OpenAPI 发布 bundle。                 ║
// ╚══════════════════════════════════════════════════════════════════════╝

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use cy_workspace_fabric::workspace_v1::{
    WorkspaceProductApiOperation, WorkspaceProductApiOwner, WorkspaceProductApiRequestKind,
};
use http::Method;
use jsonschema::{Draft, JSONSchema};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::manifest::{product_projection_manifest, product_projection_tck_sha256};
use crate::product::{
    ProductCatalogError, ProductJsonSchema, ProductOperationCatalog, ProductOperationContract,
    ProductPathParameter, ProductRequestBodyContract, ProductResourceReferenceField,
    ProductResponseContract,
};

/// Name of the required release-bundle provenance manifest.
///
/// 受信任合同 bundle 必须包含的来源清单文件名。
pub const CONTRACT_BUNDLE_MANIFEST_FILENAME: &str = "product-contract-bundle.json";

/// Environment variable naming the read-only Product contract bundle mount.
///
/// 指向只读 Product 合同 bundle 挂载路径的环境变量名称。
pub const PRODUCT_CONTRACT_ROOT_ENV: &str = "CYRENE_WORKSPACE_WEB_BFF_PRODUCT_CONTRACT_ROOT";
const MAX_BUNDLE_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_CONTRACT_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_CONTRACT_BUNDLE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_CONTRACT_FILES: usize = 4096;
const MAX_REF_BYTES: usize = 4096;
const REQUIRED_REPOSITORIES: [&str; 6] = [
    "Cyrene-Catalyst",
    "Cyrene-Echo",
    "Cyrene-Exchange",
    "Cyrene-Navigator",
    "Cyrene-Reactor",
    "Cyrene-Yield",
];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct BundleManifest {
    format_version: u32,
    canonical_tck_sha256: String,
    repositories: Vec<BundleRepository>,
    files: Vec<BundleFile>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BundleRepository {
    name: String,
    commit: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BundleFile {
    path: String,
    sha256: String,
}

struct ProductContractBundle {
    files: BTreeMap<String, Value>,
    declared_paths: BTreeSet<String>,
}

#[derive(Clone)]
struct CompiledProductSchema {
    request_schema: Option<Arc<JSONSchema>>,
    response_schemas: BTreeMap<u16, Arc<JSONSchema>>,
    path_parameter_schemas: BTreeMap<String, Arc<JSONSchema>>,
    idempotency_key_schema: Option<Arc<JSONSchema>>,
}

impl ProductJsonSchema for CompiledProductSchema {
    fn validate(&self, value: &Value) -> bool {
        self.request_schema
            .as_ref()
            .is_some_and(|schema| schema.is_valid(value))
            || self
                .idempotency_key_schema
                .as_ref()
                .is_some_and(|schema| schema.is_valid(value))
    }

    fn validate_response(&self, status: u16, value: &Value) -> bool {
        self.response_schemas
            .get(&status)
            .is_some_and(|schema| schema.is_valid(value))
    }

    fn validate_path_parameter(&self, name: &str, value: &str) -> bool {
        self.path_parameter_schemas
            .get(name)
            .is_some_and(|schema| schema.is_valid(&Value::String(value.to_owned())))
    }
}

/// Load and compile the complete canonical Product operation catalog.
///
/// The bundle root must be a read-only trusted release artifact with the
/// original `Cyrene-*` repository paths. Its TCK digest must match this build,
/// every included file is content-hash checked, and the bundle may not contain
/// unresolved network references or files outside the static TCK closure.
///
/// 加载并编译完整规范 Product operation catalog。根目录必须来自只读可信发布产物，保留 `Cyrene-*` 仓库路径；
/// TCK digest 必须匹配当前构建，并校验每个文件的内容 hash、静态闭包及本地引用。
pub fn load_product_operation_catalog(
    contract_root: impl AsRef<Path>,
) -> Result<ProductOperationCatalog, ProductCatalogError> {
    let manifest = product_projection_manifest().map_err(|_| ProductCatalogError::Manifest)?;
    let bundle = ProductContractBundle::load(contract_root.as_ref(), &manifest)?;
    let mut contracts = Vec::with_capacity(manifest.len());

    for projection in manifest {
        let source_path = projection.product_contract;
        let source = bundle
            .files
            .get(source_path)
            .ok_or(ProductCatalogError::ContractBundle)?;
        let resolved = bundle.resolve_document(source_path, source)?;
        contracts.push(compile_operation(&projection, &resolved)?);
    }

    ProductOperationCatalog::new(contracts)
}

/// Load the required Product contract bundle path from trusted process configuration.
///
/// 从可信进程配置中读取必需的 Product 合同 bundle 路径。
pub fn load_product_operation_catalog_from_environment(
) -> Result<ProductOperationCatalog, ProductCatalogError> {
    let root = std::env::var_os(PRODUCT_CONTRACT_ROOT_ENV)
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or(ProductCatalogError::ContractBundle)?;
    load_product_operation_catalog(root)
}

impl ProductContractBundle {
    fn load(
        root: &Path,
        manifest_rows: &[crate::ProductProjectionEntry],
    ) -> Result<Self, ProductCatalogError> {
        let root = root
            .canonicalize()
            .map_err(|_| ProductCatalogError::ContractBundle)?;
        if !root.is_dir() {
            return Err(ProductCatalogError::ContractBundle);
        }

        let manifest_bytes = read_regular_file(
            &root,
            Path::new(CONTRACT_BUNDLE_MANIFEST_FILENAME),
            MAX_BUNDLE_MANIFEST_BYTES,
        )?;
        if manifest_bytes.is_empty() {
            return Err(ProductCatalogError::ContractBundle);
        }
        let manifest: BundleManifest = serde_json::from_slice(&manifest_bytes)
            .map_err(|_| ProductCatalogError::ContractBundle)?;
        if manifest.format_version != 1
            || manifest.canonical_tck_sha256 != product_projection_tck_sha256()
            || manifest.canonical_tck_sha256.len() != 64
            || !is_lower_hex(&manifest.canonical_tck_sha256)
            || manifest.repositories.len() != REQUIRED_REPOSITORIES.len()
            || manifest.files.is_empty()
            || manifest.files.len() > MAX_CONTRACT_FILES
        {
            return Err(ProductCatalogError::ContractBundle);
        }

        for (repository, expected_name) in manifest.repositories.iter().zip(REQUIRED_REPOSITORIES) {
            if repository.name != expected_name
                || repository.commit.len() != 40
                || !is_lower_hex(&repository.commit)
            {
                return Err(ProductCatalogError::ContractBundle);
            }
        }

        let mut declared_paths = BTreeSet::new();
        let mut files = BTreeMap::new();
        let mut previous_path: Option<&str> = None;
        let mut total_bytes = 0_u64;
        for bundle_file in &manifest.files {
            if previous_path.is_some_and(|previous| previous >= bundle_file.path.as_str())
                || !is_valid_bundle_relative_path(&bundle_file.path)
                || !is_lower_hex_digest(&bundle_file.sha256)
            {
                return Err(ProductCatalogError::ContractBundle);
            }
            previous_path = Some(&bundle_file.path);

            let relative_path = Path::new(&bundle_file.path);
            let bytes = read_regular_file(&root, relative_path, MAX_CONTRACT_FILE_BYTES)?;
            total_bytes = total_bytes
                .checked_add(
                    u64::try_from(bytes.len()).map_err(|_| ProductCatalogError::ContractBundle)?,
                )
                .ok_or(ProductCatalogError::ContractBundle)?;
            if total_bytes > MAX_CONTRACT_BUNDLE_BYTES || sha256_hex(&bytes) != bundle_file.sha256 {
                return Err(ProductCatalogError::ContractBundle);
            }
            let value = parse_contract_document(relative_path, &bytes)?;
            declared_paths.insert(bundle_file.path.clone());
            files.insert(bundle_file.path.clone(), value);
        }

        for row in manifest_rows {
            if !declared_paths.contains(row.product_contract) {
                return Err(ProductCatalogError::ContractBundle);
            }
        }

        let bundle = Self {
            files,
            declared_paths,
        };
        bundle.validate_reference_closure(manifest_rows)?;
        Ok(bundle)
    }

    fn validate_reference_closure(
        &self,
        rows: &[crate::ProductProjectionEntry],
    ) -> Result<(), ProductCatalogError> {
        let mut closure = BTreeSet::new();
        let mut pending = BTreeSet::new();
        for row in rows {
            closure.insert(row.product_contract.to_owned());
            pending.insert(row.product_contract.to_owned());
        }

        while let Some(path) = pending.pop_first() {
            let document = self
                .files
                .get(&path)
                .ok_or(ProductCatalogError::ContractBundle)?;
            let mut refs = Vec::new();
            collect_reference_strings(document, &mut refs)?;
            for reference in refs {
                let target = self.resolve_reference_target(&path, &reference)?;
                self.pointer_target(&target.0, &target.1)?;
                if closure.insert(target.0.clone()) {
                    pending.insert(target.0);
                }
            }
        }

        if closure != self.declared_paths {
            return Err(ProductCatalogError::ContractBundle);
        }
        Ok(())
    }

    fn resolve_document(&self, path: &str, value: &Value) -> Result<Value, ProductCatalogError> {
        resolve_refs(self, path, value, &mut Vec::new())
    }

    fn resolve_reference_target(
        &self,
        source_path: &str,
        reference: &str,
    ) -> Result<(String, String), ProductCatalogError> {
        if reference.is_empty()
            || reference.len() > MAX_REF_BYTES
            || reference.starts_with("//")
            || reference.contains("://")
            || reference.contains('\\')
            || reference.contains('?')
            || reference.contains('%')
        {
            return Err(ProductCatalogError::ContractBundle);
        }
        let (path_part, fragment) = reference.split_once('#').unwrap_or((reference, ""));
        let source = Path::new(source_path);
        let owner = source
            .components()
            .next()
            .and_then(|component| component.as_os_str().to_str())
            .ok_or(ProductCatalogError::ContractBundle)?;
        let target_path = if path_part.is_empty() {
            source_path.to_owned()
        } else {
            if path_part.starts_with('/') || path_part.contains(':') {
                return Err(ProductCatalogError::ContractBundle);
            }
            normalize_reference_path(
                source.parent().ok_or(ProductCatalogError::ContractBundle)?,
                path_part,
                owner,
            )?
        };
        if !target_path.starts_with(&format!("{owner}/contracts/product/v1/")) {
            return Err(ProductCatalogError::ContractBundle);
        }
        let pointer = if fragment.is_empty() {
            String::new()
        } else if fragment.starts_with('/') {
            fragment.to_owned()
        } else {
            return Err(ProductCatalogError::ContractBundle);
        };
        Ok((target_path, pointer))
    }

    fn pointer_target<'a>(
        &'a self,
        path: &str,
        pointer: &str,
    ) -> Result<&'a Value, ProductCatalogError> {
        let mut value = self
            .files
            .get(path)
            .ok_or(ProductCatalogError::ContractBundle)?;
        if pointer.is_empty() {
            return Ok(value);
        }
        for token in pointer
            .strip_prefix('/')
            .ok_or(ProductCatalogError::ContractBundle)?
            .split('/')
        {
            let token = decode_pointer_token(token)?;
            value = match value {
                Value::Object(object) => object.get(&token),
                Value::Array(array) => token
                    .parse::<usize>()
                    .ok()
                    .and_then(|index| array.get(index)),
                _ => None,
            }
            .ok_or(ProductCatalogError::ContractBundle)?;
        }
        Ok(value)
    }
}

fn compile_operation(
    projection: &crate::ProductProjectionEntry,
    document: &Value,
) -> Result<ProductOperationContract, ProductCatalogError> {
    if document
        .get("openapi")
        .and_then(Value::as_str)
        .is_none_or(|version| !version.starts_with("3.1."))
        || document
            .get("x-cyrene-contract-profile")
            .and_then(Value::as_str)
            != Some("product-http-v1")
    {
        return Err(ProductCatalogError::ContractBundle);
    }
    let paths = document
        .get("paths")
        .and_then(Value::as_object)
        .ok_or(ProductCatalogError::ContractBundle)?;
    let mut matches = Vec::new();
    let mut operation_ids = HashSet::new();
    for (path, path_item) in paths {
        let path_item = path_item
            .as_object()
            .ok_or(ProductCatalogError::ContractBundle)?;
        for (method_name, operation) in path_item {
            if !is_http_method_name(method_name) {
                continue;
            }
            let operation = operation
                .as_object()
                .ok_or(ProductCatalogError::ContractBundle)?;
            if let Some(operation_id) = operation.get("operationId").and_then(Value::as_str) {
                if !operation_ids.insert(operation_id.to_owned()) {
                    return Err(ProductCatalogError::ProjectionMismatch);
                }
                if operation_id == projection.product_operation_id {
                    matches.push((path.as_str(), method_name.as_str(), path_item, operation));
                }
            }
        }
    }
    if matches.len() != 1 {
        return Err(ProductCatalogError::ProjectionMismatch);
    }
    let (path, method_name, path_item, operation) = matches
        .pop()
        .ok_or(ProductCatalogError::ProjectionMismatch)?;
    let upstream_method = Method::from_bytes(method_name.as_bytes())
        .map_err(|_| ProductCatalogError::UpstreamMethod)?;

    if is_navigator_append(projection.operation) {
        if projection.owner != WorkspaceProductApiOwner::Navigator
            || projection.kind != WorkspaceProductApiRequestKind::Command
            || upstream_method != Method::POST
        {
            return Err(ProductCatalogError::ProjectionMismatch);
        }
        return Ok(deny_only_contract(projection));
    }
    validate_service_bearer_binding(document, projection, path, operation)?;

    let (path_parameters, path_schema, unsupported_parameters, idempotency) =
        compile_parameters(path_item, operation, path)?;
    let projection_manifest =
        product_projection_manifest().map_err(|_| ProductCatalogError::Manifest)?;
    let (request_body_required, request_schema) =
        compile_request_body(operation, &projection_manifest)?;
    let (response_schema, resource_reference_fields) = compile_response(operation)?;

    Ok(ProductOperationContract {
        projection: projection.clone(),
        upstream_method,
        path_parameters,
        path_schema: path_schema.map(|schema| Arc::new(schema) as Arc<dyn ProductJsonSchema>),
        has_unsupported_query_or_header_parameters: unsupported_parameters,
        request_body: ProductRequestBodyContract {
            allowed: request_schema.is_some(),
            required: request_body_required,
            schema: request_schema.map(|schema| {
                Arc::new(CompiledProductSchema {
                    request_schema: Some(schema),
                    response_schemas: BTreeMap::new(),
                    path_parameter_schemas: BTreeMap::new(),
                    idempotency_key_schema: None,
                }) as Arc<dyn ProductJsonSchema>
            }),
        },
        allows_idempotency_key: idempotency.allowed,
        requires_idempotency_key: idempotency.required,
        idempotency_key_schema: idempotency.schema.map(|schema| {
            Arc::new(CompiledProductSchema {
                request_schema: None,
                response_schemas: BTreeMap::new(),
                path_parameter_schemas: BTreeMap::new(),
                idempotency_key_schema: Some(schema),
            }) as Arc<dyn ProductJsonSchema>
        }),
        response: ProductResponseContract {
            schema: Some(Arc::new(response_schema) as Arc<dyn ProductJsonSchema>),
            resource_reference_fields: Some(resource_reference_fields),
        },
    })
}

fn deny_only_contract(projection: &crate::ProductProjectionEntry) -> ProductOperationContract {
    ProductOperationContract {
        projection: projection.clone(),
        upstream_method: Method::POST,
        path_parameters: Vec::new(),
        path_schema: None,
        has_unsupported_query_or_header_parameters: true,
        request_body: ProductRequestBodyContract {
            allowed: false,
            required: false,
            schema: None,
        },
        allows_idempotency_key: false,
        requires_idempotency_key: false,
        idempotency_key_schema: None,
        response: ProductResponseContract {
            schema: None,
            resource_reference_fields: None,
        },
    }
}

fn is_navigator_append(operation: WorkspaceProductApiOperation) -> bool {
    operation == WorkspaceProductApiOperation::WorkspaceProductApiOperation13
}

fn validate_service_bearer_binding(
    document: &Value,
    projection: &crate::ProductProjectionEntry,
    path: &str,
    operation: &Map<String, Value>,
) -> Result<(), ProductCatalogError> {
    let (path_prefix, expected_scheme, exact_path) = match (projection.owner, projection.operation)
    {
        (
            WorkspaceProductApiOwner::Catalyst
            | WorkspaceProductApiOwner::Yield
            | WorkspaceProductApiOwner::Reactor
            | WorkspaceProductApiOwner::Echo,
            _,
        ) => ("/internal/workspace/v1/", "WorkspaceServiceBearer", false),
        (WorkspaceProductApiOwner::Exchange, _) => {
            ("/api/v1/workspace/", "WorkspaceControlBearer", false)
        }
        (
            WorkspaceProductApiOwner::Navigator,
            WorkspaceProductApiOperation::WorkspaceProductApiOperation11,
        ) => (
            "/api/v1/workspace-snapshots",
            "NavigatorProductBearer",
            true,
        ),
        (
            WorkspaceProductApiOwner::Navigator,
            WorkspaceProductApiOperation::WorkspaceProductApiOperation12,
        ) => (
            "/internal/workspace/v1/workspaces/",
            "NavigatorWorkspaceBearer",
            false,
        ),
        _ => return Err(ProductCatalogError::ProjectionMismatch),
    };
    if (exact_path && path != path_prefix) || (!exact_path && !path.starts_with(path_prefix)) {
        return Err(ProductCatalogError::ProjectionMismatch);
    }

    operation
        .get("security")
        .and_then(Value::as_array)
        .filter(|security| security.len() == 1)
        .and_then(|security| security.first())
        .and_then(Value::as_object)
        .filter(|requirement| requirement.len() == 1)
        .and_then(|requirement| requirement.get(expected_scheme))
        .and_then(Value::as_array)
        .filter(|scopes| scopes.is_empty())
        .ok_or(ProductCatalogError::ProjectionMismatch)?;

    let bearer_scheme = document
        .get("components")
        .and_then(|components| components.get("securitySchemes"))
        .and_then(|schemes| schemes.get(expected_scheme))
        .and_then(Value::as_object)
        .ok_or(ProductCatalogError::ProjectionMismatch)?;
    if bearer_scheme.get("type").and_then(Value::as_str) != Some("http")
        || bearer_scheme.get("scheme").and_then(Value::as_str) != Some("bearer")
    {
        return Err(ProductCatalogError::ProjectionMismatch);
    }
    Ok(())
}

fn is_http_method_name(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "get" | "post" | "put" | "patch" | "delete" | "head" | "options" | "trace"
    )
}

struct IdempotencyKeyContract {
    allowed: bool,
    required: bool,
    schema: Option<Arc<JSONSchema>>,
}

fn compile_parameters(
    path_item: &Map<String, Value>,
    operation: &Map<String, Value>,
    path: &str,
) -> Result<
    (
        Vec<ProductPathParameter>,
        Option<CompiledProductSchema>,
        bool,
        IdempotencyKeyContract,
    ),
    ProductCatalogError,
> {
    let mut parameters = Vec::new();
    for holder in [path_item, operation] {
        if let Some(value) = holder.get("parameters") {
            let values = value
                .as_array()
                .ok_or(ProductCatalogError::ContractBundle)?;
            parameters.extend(values.iter());
        }
    }

    let mut names = BTreeSet::new();
    let mut path_parameters = Vec::new();
    let mut path_schemas = BTreeMap::new();
    let mut has_unsupported = false;
    let mut idempotency = IdempotencyKeyContract {
        allowed: false,
        required: false,
        schema: None,
    };

    for parameter in parameters {
        let parameter = parameter
            .as_object()
            .ok_or(ProductCatalogError::ContractBundle)?;
        let name = parameter
            .get("name")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty() && value.trim() == *value)
            .ok_or(ProductCatalogError::ContractBundle)?;
        let location = parameter
            .get("in")
            .and_then(Value::as_str)
            .ok_or(ProductCatalogError::ContractBundle)?;
        let required = parameter
            .get("required")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let normalized_name = if location == "header" {
            name.to_ascii_lowercase()
        } else {
            name.to_owned()
        };
        if !names.insert((location.to_owned(), normalized_name)) {
            return Err(ProductCatalogError::ContractBundle);
        }

        match location {
            "path" => {
                if !required {
                    return Err(ProductCatalogError::ProjectionMismatch);
                }
                let schema = parameter
                    .get("schema")
                    .ok_or(ProductCatalogError::ContractBundle)?;
                let schema = compile_json_schema(schema.clone())?;
                if path_schemas.insert(name.to_owned(), schema).is_some() {
                    return Err(ProductCatalogError::ContractBundle);
                }
                path_parameters.push(ProductPathParameter {
                    name: name.to_owned(),
                    required,
                });
            }
            "header" if name.eq_ignore_ascii_case("Idempotency-Key") => {
                if idempotency.allowed {
                    return Err(ProductCatalogError::ContractBundle);
                }
                let schema = parameter
                    .get("schema")
                    .ok_or(ProductCatalogError::ContractBundle)?;
                idempotency.allowed = true;
                idempotency.required = required;
                idempotency.schema = Some(compile_json_schema(schema.clone())?);
            }
            "header" | "query" | "cookie" => has_unsupported |= required,
            _ => has_unsupported |= required,
        }
    }

    let declared_path_names = path_parameter_names(path)?;
    let actual_path_names = path_parameters
        .iter()
        .map(|parameter| parameter.name.clone())
        .collect::<BTreeSet<_>>();
    if declared_path_names != actual_path_names {
        return Err(ProductCatalogError::ProjectionMismatch);
    }
    let workspace_parameters = path_parameters
        .iter()
        .filter(|parameter| matches!(parameter.name.as_str(), "workspace_id" | "workspaceId"))
        .count();
    let resource_parameters = path_parameters.len().saturating_sub(workspace_parameters);
    if workspace_parameters > 1 || resource_parameters > 1 {
        return Err(ProductCatalogError::ProjectionMismatch);
    }

    let path_schema = (!path_schemas.is_empty()).then_some(CompiledProductSchema {
        request_schema: None,
        response_schemas: BTreeMap::new(),
        path_parameter_schemas: path_schemas,
        idempotency_key_schema: None,
    });
    Ok((path_parameters, path_schema, has_unsupported, idempotency))
}

fn path_parameter_names(path: &str) -> Result<BTreeSet<String>, ProductCatalogError> {
    let mut names = BTreeSet::new();
    let mut remainder = path;
    while let Some(open) = remainder.find('{') {
        let after_open = &remainder[open + 1..];
        let close = after_open
            .find('}')
            .ok_or(ProductCatalogError::ContractBundle)?;
        let name = &after_open[..close];
        if name.is_empty()
            || name.contains('{')
            || name.contains('/')
            || !names.insert(name.to_owned())
        {
            return Err(ProductCatalogError::ContractBundle);
        }
        remainder = &after_open[close + 1..];
    }
    if remainder.contains('}') {
        return Err(ProductCatalogError::ContractBundle);
    }
    Ok(names)
}

fn compile_request_body(
    operation: &Map<String, Value>,
    manifest: &[crate::ProductProjectionEntry],
) -> Result<(bool, Option<Arc<JSONSchema>>), ProductCatalogError> {
    let Some(body) = operation.get("requestBody") else {
        return Ok((false, None));
    };
    let body = body
        .as_object()
        .ok_or(ProductCatalogError::RequestBodySchema)?;
    let required = body
        .get("required")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let content = body
        .get("content")
        .and_then(Value::as_object)
        .ok_or(ProductCatalogError::RequestBodySchema)?;
    let mut json_schema = None;
    for (media_type, media) in content {
        if media_type != "application/json" {
            return Err(ProductCatalogError::RequestBodySchema);
        }
        let schema = media
            .get("schema")
            .ok_or(ProductCatalogError::RequestBodySchema)?;
        validate_request_resource_links(schema, manifest)?;
        json_schema = Some(compile_json_schema(schema.clone())?);
    }
    if required && json_schema.is_none() {
        return Err(ProductCatalogError::RequestBodySchema);
    }
    Ok((required, json_schema))
}

fn validate_request_resource_links(
    schema: &Value,
    manifest: &[crate::ProductProjectionEntry],
) -> Result<(), ProductCatalogError> {
    if !is_closed_product_resource_reference(schema, manifest)
        && schema_contains_external_link_semantics(schema)
    {
        return Err(ProductCatalogError::UnsafeRequestSchema);
    }
    match schema {
        Value::Object(object) => {
            if let Some(properties) = object.get("properties").and_then(Value::as_object) {
                for (name, child) in properties {
                    if !is_closed_product_resource_reference(child, manifest)
                        && is_resource_link_field(name, child)
                    {
                        return Err(ProductCatalogError::UnsafeRequestSchema);
                    }
                    validate_request_resource_links(child, manifest)?;
                }
            }
            for key in [
                "items",
                "allOf",
                "anyOf",
                "oneOf",
                "not",
                "if",
                "then",
                "else",
                "additionalProperties",
                "propertyNames",
            ] {
                if let Some(child) = object.get(key) {
                    validate_request_resource_links(child, manifest)?;
                }
            }
            if let Some(pattern_properties) =
                object.get("patternProperties").and_then(Value::as_object)
            {
                for child in pattern_properties.values() {
                    validate_request_resource_links(child, manifest)?;
                }
            }
        }
        Value::Array(items) => {
            for child in items {
                validate_request_resource_links(child, manifest)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn compile_response(
    operation: &Map<String, Value>,
) -> Result<(CompiledProductSchema, Vec<ProductResourceReferenceField>), ProductCatalogError> {
    let responses = operation
        .get("responses")
        .and_then(Value::as_object)
        .ok_or(ProductCatalogError::ContractBundle)?;
    let mut raw_schemas = BTreeMap::<u16, Vec<Value>>::new();
    let mut resource_fields = BTreeMap::<String, ProductResourceReferenceField>::new();
    let manifest = product_projection_manifest().map_err(|_| ProductCatalogError::Manifest)?;

    for (status_name, response) in responses {
        let status = status_name
            .parse::<u16>()
            .ok()
            .filter(|status| (100..=599).contains(status))
            .ok_or(ProductCatalogError::ContractBundle)?;
        let response = response
            .as_object()
            .ok_or(ProductCatalogError::ContractBundle)?;
        let Some(content) = response.get("content") else {
            if (200..=299).contains(&status) {
                return Err(ProductCatalogError::ContractBundle);
            }
            continue;
        };
        let content = content
            .as_object()
            .ok_or(ProductCatalogError::ContractBundle)?;
        for (media_type, media) in content {
            if !matches!(
                media_type.as_str(),
                "application/json" | "application/problem+json"
            ) {
                return Err(ProductCatalogError::ContractBundle);
            }
            let schema = media
                .get("schema")
                .ok_or(ProductCatalogError::ContractBundle)?;
            if media_type == "application/problem+json" {
                if status < 400 || !is_standard_problem_details_schema(schema) {
                    return Err(ProductCatalogError::UnsafeResponseSchema);
                }
            } else {
                // Product data schemas, including JSON error DTOs, must be closed and must not
                // expose undeclared URLs or service endpoints.
                let mut found_fields = Vec::new();
                collect_resource_references(schema, "", &manifest, &mut found_fields, true, true)?;
                for field in found_fields {
                    resource_fields
                        .entry(field.json_pointer.clone())
                        .or_insert(field);
                }
            }
            raw_schemas.entry(status).or_default().push(schema.clone());
        }
    }

    if !raw_schemas
        .keys()
        .any(|status| (200..=299).contains(status))
    {
        return Err(ProductCatalogError::ContractBundle);
    }

    let mut response_schemas = BTreeMap::new();
    for (status, schemas) in raw_schemas {
        let schema = if schemas.len() == 1 {
            schemas
                .into_iter()
                .next()
                .ok_or(ProductCatalogError::ContractBundle)?
        } else {
            json!({ "anyOf": schemas })
        };
        response_schemas.insert(status, compile_json_schema(schema)?);
    }

    Ok((
        CompiledProductSchema {
            request_schema: None,
            response_schemas,
            path_parameter_schemas: BTreeMap::new(),
            idempotency_key_schema: None,
        },
        resource_fields.into_values().collect(),
    ))
}

fn is_standard_problem_details_schema(schema: &Value) -> bool {
    let Some(object) = schema.as_object() else {
        return false;
    };
    if object.get("type").and_then(Value::as_str) != Some("object")
        || object.get("additionalProperties").and_then(Value::as_bool) != Some(false)
    {
        return false;
    }
    let Some(properties) = object.get("properties").and_then(Value::as_object) else {
        return false;
    };
    let allowed = BTreeSet::from([
        "type",
        "title",
        "status",
        "detail",
        "instance",
        "code",
        "retryable",
        "traceId",
        "resourceRef",
    ]);
    if properties
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>()
        != allowed
    {
        return false;
    }
    let Some(required) = object.get("required").and_then(Value::as_array) else {
        return false;
    };
    let required = required
        .iter()
        .filter_map(Value::as_str)
        .collect::<BTreeSet<_>>();
    let required_count = object["required"]
        .as_array()
        .map(Vec::len)
        .unwrap_or_default();
    required
        == BTreeSet::from([
            "type",
            "title",
            "status",
            "detail",
            "instance",
            "code",
            "retryable",
            "traceId",
        ])
        && required_count == required.len()
        && properties["type"].get("type").and_then(Value::as_str) == Some("string")
        && properties["type"].get("format").and_then(Value::as_str) == Some("uri")
        && ["title", "detail", "instance", "code"]
            .iter()
            .all(|name| properties[*name].get("type").and_then(Value::as_str) == Some("string"))
        && properties["status"].get("type").and_then(Value::as_str) == Some("integer")
        && properties["retryable"].get("type").and_then(Value::as_str) == Some("boolean")
        && properties["traceId"].get("type").and_then(Value::as_str) == Some("string")
        && properties["resourceRef"]
            .get("type")
            .and_then(Value::as_str)
            == Some("string")
}

fn collect_resource_references(
    schema: &Value,
    pointer: &str,
    manifest: &[crate::ProductProjectionEntry],
    output: &mut Vec<ProductResourceReferenceField>,
    instance_pointer: bool,
    enforce_closed_objects: bool,
) -> Result<(), ProductCatalogError> {
    match schema {
        Value::Object(object) => {
            if enforce_closed_objects
                && schema_declares_object(object)
                && object.get("additionalProperties").and_then(Value::as_bool) != Some(false)
            {
                return Err(ProductCatalogError::UnsafeResponseSchema);
            }
            if let Some(properties) = object.get("properties").and_then(Value::as_object) {
                for (name, child) in properties {
                    let child_pointer = format!("{pointer}/{}", escape_pointer_token(name));
                    if is_closed_product_resource_reference(child, manifest) {
                        if instance_pointer {
                            output.push(ProductResourceReferenceField {
                                json_pointer: child_pointer.clone(),
                                // This pointer list spans response status variants. The owner
                                // schema validates requiredness for the exact status; this adds
                                // a runtime value check only when the field is present.
                                required: false,
                            });
                        }
                    } else if is_resource_link_field(name, child) {
                        return Err(ProductCatalogError::UnsafeResourceReference);
                    }
                    collect_resource_references(
                        child,
                        &child_pointer,
                        manifest,
                        output,
                        instance_pointer,
                        enforce_closed_objects,
                    )?;
                }
            }
            for key in ["items", "allOf", "anyOf", "oneOf", "not"] {
                if let Some(child) = object.get(key) {
                    let child_pointer = if key == "items" {
                        format!("{pointer}/items")
                    } else {
                        format!("{pointer}/{key}")
                    };
                    collect_resource_references(
                        child,
                        &child_pointer,
                        manifest,
                        output,
                        false,
                        enforce_closed_objects,
                    )?;
                }
            }
            for key in ["if", "then", "else"] {
                if let Some(child) = object.get(key) {
                    let child_pointer = format!("{pointer}/{key}");
                    collect_resource_references(
                        child,
                        &child_pointer,
                        manifest,
                        output,
                        false,
                        false,
                    )?;
                }
            }
            for key in ["additionalProperties", "propertyNames"] {
                if let Some(child) = object.get(key) {
                    collect_resource_references(child, pointer, manifest, output, false, true)?;
                }
            }
            if let Some(pattern_properties) =
                object.get("patternProperties").and_then(Value::as_object)
            {
                for child in pattern_properties.values() {
                    collect_resource_references(child, pointer, manifest, output, false, true)?;
                }
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                collect_resource_references(
                    child,
                    &format!("{pointer}/{index}"),
                    manifest,
                    output,
                    instance_pointer,
                    enforce_closed_objects,
                )?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn schema_declares_object(object: &Map<String, Value>) -> bool {
    object.get("type").is_some_and(|value| match value {
        Value::String(value) => value == "object",
        Value::Array(values) => values.iter().any(|value| value.as_str() == Some("object")),
        _ => false,
    }) || ["properties", "patternProperties", "additionalProperties"]
        .iter()
        .any(|key| object.contains_key(*key))
}

fn is_resource_link_field(name: &str, schema: &Value) -> bool {
    let normalized_name = name.to_ascii_lowercase();
    let explicit_name = [
        "url",
        "uri",
        "href",
        "hyperlink",
        "resource-link",
        "resourceref",
        "reference",
    ]
    .iter()
    .any(|marker| normalized_name.contains(marker));
    explicit_name || schema_contains_external_link_semantics(schema)
}

fn schema_contains_external_link_semantics(schema: &Value) -> bool {
    match schema {
        Value::Object(object) => {
            let format = object
                .get("format")
                .and_then(Value::as_str)
                .is_some_and(|format| matches!(format, "uri" | "uri-reference" | "url"));
            let pattern = object
                .get("pattern")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_ascii_lowercase();
            format
                || pattern.contains("https?://")
                || pattern.contains("http://")
                || pattern.contains("https://")
                || object.values().any(schema_contains_external_link_semantics)
        }
        Value::Array(values) => values.iter().any(schema_contains_external_link_semantics),
        _ => false,
    }
}

fn is_closed_product_resource_reference(
    schema: &Value,
    manifest: &[crate::ProductProjectionEntry],
) -> bool {
    let Some(object) = schema.as_object() else {
        return false;
    };
    if object.get("type").and_then(Value::as_str) != Some("object")
        || object.get("additionalProperties").and_then(Value::as_bool) != Some(false)
    {
        return false;
    }
    let Some(required) = object.get("required").and_then(Value::as_array) else {
        return false;
    };
    let required = required
        .iter()
        .filter_map(Value::as_str)
        .collect::<BTreeSet<_>>();
    if required != BTreeSet::from(["operation", "resourceId"]) {
        return false;
    }
    let Some(properties) = object.get("properties").and_then(Value::as_object) else {
        return false;
    };
    if properties.len() != 2 {
        return false;
    }
    let Some(operation_values) = properties
        .get("operation")
        .and_then(|value| value.get("enum"))
        .and_then(Value::as_array)
    else {
        return false;
    };
    let expected_operations = manifest
        .iter()
        .filter(|entry| entry.kind == WorkspaceProductApiRequestKind::Read)
        .map(|entry| entry.operation.as_str_name())
        .collect::<BTreeSet<_>>();
    let actual_operations = operation_values
        .iter()
        .filter_map(Value::as_str)
        .collect::<BTreeSet<_>>();
    let resource_id = &properties["resourceId"];
    let resource_id_ok = resource_id.get("type").and_then(Value::as_str) == Some("string")
        && resource_id
            .get("minLength")
            .and_then(Value::as_u64)
            .is_some_and(|minimum| minimum >= 1)
        && resource_id
            .get("maxLength")
            .and_then(Value::as_u64)
            .is_some_and(|maximum| maximum <= 512)
        && !schema_contains_external_link_semantics(resource_id);
    actual_operations == expected_operations
        && operation_values.len() == actual_operations.len()
        && resource_id_ok
}

fn read_regular_file(
    root: &Path,
    relative_path: &Path,
    maximum_bytes: u64,
) -> Result<Vec<u8>, ProductCatalogError> {
    if relative_path.is_absolute()
        || relative_path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(ProductCatalogError::ContractBundle);
    }
    let mut current = root.to_path_buf();
    for component in relative_path.components() {
        let Component::Normal(component) = component else {
            return Err(ProductCatalogError::ContractBundle);
        };
        current.push(component);
        let metadata =
            fs::symlink_metadata(&current).map_err(|_| ProductCatalogError::ContractBundle)?;
        if metadata.file_type().is_symlink() {
            return Err(ProductCatalogError::ContractBundle);
        }
    }
    let metadata = fs::metadata(&current).map_err(|_| ProductCatalogError::ContractBundle)?;
    if !metadata.is_file() || metadata.len() > maximum_bytes {
        return Err(ProductCatalogError::ContractBundle);
    }
    fs::read(current).map_err(|_| ProductCatalogError::ContractBundle)
}

fn is_valid_bundle_relative_path(path: &str) -> bool {
    if path.is_empty() || path.contains('\\') || path.starts_with('/') || path.contains('\0') {
        return false;
    }
    let components = path.split('/').collect::<Vec<_>>();
    components.len() >= 5
        && REQUIRED_REPOSITORIES.contains(&components[0])
        && components[1..4] == ["contracts", "product", "v1"]
        && components
            .iter()
            .all(|component| !component.is_empty() && *component != "." && *component != "..")
}

fn normalize_reference_path(
    source_parent: &Path,
    reference_path: &str,
    owner: &str,
) -> Result<String, ProductCatalogError> {
    let mut components = source_parent
        .components()
        .map(|component| {
            component
                .as_os_str()
                .to_str()
                .ok_or(ProductCatalogError::ContractBundle)
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if components.first().map(String::as_str) != Some(owner) {
        return Err(ProductCatalogError::ContractBundle);
    }
    for component in reference_path.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                if components.len() <= 1 {
                    return Err(ProductCatalogError::ContractBundle);
                }
                components.pop();
            }
            value if value.contains(':') => return Err(ProductCatalogError::ContractBundle),
            value => components.push(value.to_owned()),
        }
    }
    if components.len() < 2 || components[0] != owner {
        return Err(ProductCatalogError::ContractBundle);
    }
    Ok(components.join("/"))
}

fn parse_contract_document(path: &Path, bytes: &[u8]) -> Result<Value, ProductCatalogError> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .ok_or(ProductCatalogError::ContractBundle)?;
    let value = match extension {
        "yaml" | "yml" => {
            let yaml: serde_yaml::Value =
                serde_yaml::from_slice(bytes).map_err(|_| ProductCatalogError::ContractBundle)?;
            serde_json::to_value(yaml).map_err(|_| ProductCatalogError::ContractBundle)?
        }
        "json" => serde_json::from_slice(bytes).map_err(|_| ProductCatalogError::ContractBundle)?,
        _ => return Err(ProductCatalogError::ContractBundle),
    };
    if value.is_object() || value.is_array() {
        Ok(value)
    } else {
        Err(ProductCatalogError::ContractBundle)
    }
}

fn collect_reference_strings(
    value: &Value,
    output: &mut Vec<String>,
) -> Result<(), ProductCatalogError> {
    match value {
        Value::Object(object) => {
            if let Some(reference) = object.get("$ref") {
                output.push(
                    reference
                        .as_str()
                        .filter(|reference| !reference.is_empty())
                        .ok_or(ProductCatalogError::ContractBundle)?
                        .to_owned(),
                );
            }
            for child in object.values() {
                collect_reference_strings(child, output)?;
            }
        }
        Value::Array(items) => {
            for child in items {
                collect_reference_strings(child, output)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn resolve_refs(
    bundle: &ProductContractBundle,
    source_path: &str,
    value: &Value,
    stack: &mut Vec<(String, String)>,
) -> Result<Value, ProductCatalogError> {
    match value {
        Value::Object(object) => {
            if let Some(reference) = object.get("$ref") {
                let reference = reference
                    .as_str()
                    .ok_or(ProductCatalogError::ContractBundle)?;
                let target = bundle.resolve_reference_target(source_path, reference)?;
                let key = target.clone();
                if stack.contains(&key) {
                    return Err(ProductCatalogError::ContractBundle);
                }
                stack.push(key);
                let target_value = bundle.pointer_target(&target.0, &target.1)?;
                let mut resolved = resolve_refs(bundle, &target.0, target_value, stack)?;
                stack.pop();

                let mut nullable = false;
                let mut default_value = None;
                for (name, child) in object.iter().filter(|(name, _)| name.as_str() != "$ref") {
                    match name.as_str() {
                        "nullable" => {
                            nullable =
                                child.as_bool().ok_or(ProductCatalogError::ContractBundle)?;
                        }
                        "default" => default_value = Some(child.clone()),
                        _ => return Err(ProductCatalogError::ContractBundle),
                    }
                }
                if nullable {
                    resolved = json!({ "anyOf": [resolved, { "type": "null" }] });
                }
                if let Some(default_value) = default_value {
                    if let Some(object) = resolved.as_object_mut() {
                        object.insert("default".to_owned(), default_value);
                    } else {
                        resolved = json!({ "allOf": [resolved], "default": default_value });
                    }
                }
                Ok(resolved)
            } else {
                let mut resolved = Map::new();
                let nullable = object
                    .get("nullable")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if object.contains_key("nullable")
                    && object.get("nullable").and_then(Value::as_bool).is_none()
                {
                    return Err(ProductCatalogError::ContractBundle);
                }
                for (name, child) in object {
                    if name == "nullable" {
                        continue;
                    }
                    resolved.insert(
                        name.clone(),
                        resolve_refs(bundle, source_path, child, stack)?,
                    );
                }
                let resolved = Value::Object(resolved);
                if nullable {
                    Ok(json!({ "anyOf": [resolved, { "type": "null" }] }))
                } else {
                    Ok(resolved)
                }
            }
        }
        Value::Array(items) => Ok(Value::Array(
            items
                .iter()
                .map(|child| resolve_refs(bundle, source_path, child, stack))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        _ => Ok(value.clone()),
    }
}

fn compile_json_schema(schema: Value) -> Result<Arc<JSONSchema>, ProductCatalogError> {
    let mut references = Vec::new();
    collect_reference_strings(&schema, &mut references)?;
    if !references.is_empty() {
        return Err(ProductCatalogError::ContractBundle);
    }
    JSONSchema::options()
        .with_draft(Draft::Draft202012)
        .compile(&schema)
        .map(Arc::new)
        .map_err(|_| ProductCatalogError::ContractBundle)
}

fn decode_pointer_token(token: &str) -> Result<String, ProductCatalogError> {
    let mut decoded = String::with_capacity(token.len());
    let mut chars = token.chars();
    while let Some(character) = chars.next() {
        if character == '~' {
            match chars.next() {
                Some('0') => decoded.push('~'),
                Some('1') => decoded.push('/'),
                _ => return Err(ProductCatalogError::ContractBundle),
            }
        } else {
            decoded.push(character);
        }
    }
    Ok(decoded)
}

fn escape_pointer_token(token: &str) -> String {
    token.replace('~', "~0").replace('/', "~1")
}

fn is_lower_hex_digest(value: &str) -> bool {
    value.len() == 64 && is_lower_hex(value)
}

fn is_lower_hex(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
