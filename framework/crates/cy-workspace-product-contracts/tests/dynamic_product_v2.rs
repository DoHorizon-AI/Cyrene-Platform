//! Dynamic Product v2 contract TCK fixture.
//!
//! This integration test creates a temporary owner Git repository and release
//! lock for an operation absent from the initial 13-row migration seed. It
//! exercises the same bundle loader, policy evaluator, schema validation, and
//! opaque route issuance used by Workspace adapters.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use cy_proto::cyrene::workspace::product::v2::ProductApiInvocationV2;
use cy_workspace_product_contracts::{
    AuthorizationError, ProductBundlePins, ProductContractBundle, ProductInvocationError,
    ProductInvocationResponse, TrustedProductPolicy, TrustedWorkspaceScope,
    VerifiedProductPrincipal, PRODUCT_CONTRACT_API_VERSION,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const OWNER_ID: &str = "example";
const REPOSITORY: &str = "Cyrene-Example";
const OPERATION_ID: &str = "workspacePublishFutureArtifactV2";
const POLICY_VERSION: &str = "cyrene.workspace.product.authorization-policy.v2";
const WORKSPACE_ID: &str = "workspace-42";
const RESOURCE_ID: &str = "artifact-77";

struct Fixture {
    root: PathBuf,
    bundle_root: PathBuf,
    pins: ProductBundlePins,
    commit: String,
}

impl Fixture {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock is after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "cyrene-product-v2-dynamic-{}-{nonce}",
            std::process::id()
        ));
        let repo = root.join(REPOSITORY);
        let bundle_root = root.join("bundle");
        fs::create_dir_all(repo.join("contracts/product/v2")).expect("create owner catalog");
        fs::create_dir_all(repo.join("contracts/product/v1")).expect("create Product OpenAPI");
        fs::create_dir_all(&bundle_root).expect("create bundle directory");

        git(&repo, &["init", "-q"]);
        git(
            &repo,
            &["config", "user.email", "v2-contract-tck@example.invalid"],
        );
        git(&repo, &["config", "user.name", "Workspace v2 TCK"]);
        git(
            &repo,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/DoHorizon-AI/Cyrene-Example.git",
            ],
        );

        let route = "/v2/workspaces/{workspace_id}/artifacts/{artifact_id}";
        let escaped_route = route.replace('/', "~1");
        let request_pointer =
            format!("#/paths/{escaped_route}/patch/requestBody/content/application~1json/schema");
        let response_pointer =
            format!("#/paths/{escaped_route}/patch/responses/200/content/application~1json/schema");
        let catalog = json!({
            "schemaVersion": "cyrene.product.operation-catalog.v2",
            "catalogVersion": "2.0.0",
            "ownerId": OWNER_ID,
            "operations": [{
                "operationId": OPERATION_ID,
                "routeId": OPERATION_ID,
                "openapiPath": "contracts/product/v1/openapi.yaml",
                "kind": "COMMAND",
                "requestSchemaPointer": request_pointer,
                "responseSchemaPointers": [response_pointer],
                "resourceId": {
                    "required": true,
                    "pathParameter": "artifact_id",
                    "minLength": 1,
                    "maxLength": 128,
                    "pattern": "^[A-Za-z0-9-]+$"
                },
                "idempotency": {
                    "required": true,
                    "header": "Idempotency-Key",
                    "minLength": 8,
                    "maxLength": 128
                },
                "scope": {
                    "organizationPathParameter": null,
                    "workspacePathParameter": "workspace_id",
                    "requestBindings": [
                        {"jsonPointer": "/workspaceId", "matches": "WORKSPACE_ID"}
                    ],
                    "responseBindings": [
                        {"jsonPointer": "/workspaceId", "matches": "WORKSPACE_ID"},
                        {"jsonPointer": "/id", "matches": "RESOURCE_ID"}
                    ]
                }
            }]
        });
        fs::write(
            repo.join("contracts/product/v2/catalog.json"),
            serde_json::to_vec_pretty(&catalog).expect("serialize catalog"),
        )
        .expect("write owner catalog");

        let path_item = json!({
            "parameters": [
                {"name": "workspace_id", "in": "path", "required": true, "schema": {"type": "string"}},
                {"name": "artifact_id", "in": "path", "required": true, "schema": {"type": "string"}}
            ],
            "patch": {
                "operationId": OPERATION_ID,
                "requestBody": {
                    "required": true,
                    "content": {
                        "application/json": {"schema": {"$ref": "./schemas.json#/$defs/Request"}}
                    }
                },
                "responses": {
                    "200": {
                        "description": "Updated",
                        "content": {
                            "application/json": {"schema": {"$ref": "./schemas.json#/$defs/Response"}}
                        }
                    },
                    "400": {"description": "Invalid request"}
                }
            }
        });
        let openapi = json!({
            "openapi": "3.1.0",
            "info": {"title": "Example Product", "version": "1.0.0"},
            "paths": serde_json::Map::from_iter([(route.to_owned(), path_item)])
        });
        fs::write(
            repo.join("contracts/product/v1/openapi.yaml"),
            serde_json::to_vec_pretty(&openapi).expect("serialize OpenAPI"),
        )
        .expect("write OpenAPI");
        fs::write(
            repo.join("contracts/product/v1/schemas.json"),
            serde_json::to_vec_pretty(&json!({
                "$defs": {
                    "Request": {
                        "type": "object",
                        "required": ["workspaceId", "name"],
                        "properties": {
                            "workspaceId": {"type": "string"},
                            "name": {"type": "string", "minLength": 1}
                        },
                        "additionalProperties": false
                    },
                    "Response": {
                        "type": "object",
                        "required": ["workspaceId", "id"],
                        "properties": {
                            "workspaceId": {"type": "string"},
                            "id": {"type": "string"}
                        },
                        "additionalProperties": false
                    }
                }
            }))
            .expect("serialize schema"),
        )
        .expect("write schema");

        let policy = json!({
            "schemaVersion": POLICY_VERSION,
            "policyVersion": "2.0.0",
            "grants": [{
                "ownerId": OWNER_ID,
                "operationId": OPERATION_ID,
                "principalKinds": ["DIRECTORY_USER"],
                "requiredRoles": [
                    "workspace.member",
                    "workspace.product.command.example.publish_artifact.v2"
                ],
                "requiresExactWorkspace": true,
                "requiredRequestBindings": [
                    {"jsonPointer": "/workspaceId", "matches": "WORKSPACE_ID"}
                ],
                "requiredResponseBindings": [
                    {"jsonPointer": "/workspaceId", "matches": "WORKSPACE_ID"},
                    {"jsonPointer": "/id", "matches": "RESOURCE_ID"}
                ]
            }]
        });
        let policy_raw = serde_json::to_vec_pretty(&policy).expect("serialize policy");
        let policy_path = bundle_root.join("workspace-product-policy-v2.json");
        fs::write(&policy_path, &policy_raw).expect("write trusted policy");

        git(&repo, &["add", "contracts/product"]);
        git(&repo, &["commit", "-qm", "add future Product v2 operation"]);
        let commit = git_output(&repo, &["rev-parse", "HEAD"])
            .expect("resolve source commit")
            .trim()
            .to_owned();

        let catalog_path = format!("{REPOSITORY}/contracts/product/v2/catalog.json");
        let openapi_path = format!("{REPOSITORY}/contracts/product/v1/openapi.yaml");
        let schemas_path = format!("{REPOSITORY}/contracts/product/v1/schemas.json");
        let owner_files = [
            (
                catalog_path.clone(),
                git_show(&repo, &commit, "contracts/product/v2/catalog.json"),
            ),
            (
                openapi_path.clone(),
                git_show(&repo, &commit, "contracts/product/v1/openapi.yaml"),
            ),
            (
                schemas_path.clone(),
                git_show(&repo, &commit, "contracts/product/v1/schemas.json"),
            ),
        ];
        let mut file_digests = BTreeMap::new();
        for (path, bytes) in owner_files {
            file_digests.insert(path, bytes);
        }
        file_digests.insert(
            "workspace-product-policy-v2.json".to_owned(),
            policy_raw.clone(),
        );
        for (path, bytes) in &file_digests {
            let target = bundle_root.join(path);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).expect("create bundle path");
            }
            fs::write(target, bytes).expect("copy exact release bytes");
        }

        let catalog_sha = sha256(
            file_digests
                .get(&catalog_path)
                .expect("catalog blob is included"),
        );
        let manifest = json!({
            "formatVersion": 2,
            "wireApiVersion": "cyrene.workspace.product.v2",
            "owners": [{
                "ownerId": OWNER_ID,
                "repository": REPOSITORY,
                "sourceSha": commit,
                "catalogPath": catalog_path,
                "catalogSha256": catalog_sha
            }],
            "files": file_digests
                .iter()
                .map(|(path, bytes)| json!({"path": path, "sha256": sha256(bytes)}))
                .collect::<Vec<_>>()
        });
        let mut manifest_raw =
            serde_json::to_vec_pretty(&manifest).expect("serialize release manifest");
        manifest_raw.push(b'\n');
        fs::write(
            bundle_root.join("product-contract-bundle.json"),
            &manifest_raw,
        )
        .expect("write release manifest");
        let manifest_sha = sha256(&manifest_raw);
        let lock = json!({
            "formatVersion": 2,
            "releaseId": "workspace-product-v2-dynamic-tck",
            "wireApiVersion": "cyrene.workspace.product.v2",
            "contractApiVersion": PRODUCT_CONTRACT_API_VERSION,
            "bundle": {
                "manifestPath": "product-contract-bundle.json",
                "manifestSha256": manifest_sha
            },
            "policy": {
                "sourcePath": "contracts/policies/workspace-product-policy-v2.json",
                "bundlePath": "workspace-product-policy-v2.json",
                "schemaVersion": POLICY_VERSION,
                "sha256": sha256(&policy_raw)
            },
            "owners": [{
                "ownerId": OWNER_ID,
                "repository": REPOSITORY,
                "sourceSha": commit,
                "catalogPath": catalog_path,
                "catalogSha256": catalog_sha
            }]
        });
        let lock_path = root.join("workspace-product-v2.lock.json");
        fs::write(
            &lock_path,
            serde_json::to_vec_pretty(&lock).expect("serialize independent lock"),
        )
        .expect("write independent lock");

        let lock: Value = serde_json::from_slice(&fs::read(lock_path).expect("read release lock"))
            .expect("parse release lock");
        let locked_owner = lock["owners"][0].as_object().expect("owner pin is object");
        let owner_sha = locked_owner["sourceSha"]
            .as_str()
            .expect("locked owner source SHA")
            .to_owned();
        let pins = ProductBundlePins::new(
            lock["wireApiVersion"]
                .as_str()
                .expect("locked wire API version"),
            lock["bundle"]["manifestSha256"]
                .as_str()
                .expect("locked manifest digest"),
            BTreeMap::from([(OWNER_ID.to_owned(), owner_sha)]),
            lock["policy"]["schemaVersion"]
                .as_str()
                .expect("locked policy schema version"),
            lock["policy"]["sha256"]
                .as_str()
                .expect("locked policy digest"),
        );
        Self {
            root,
            bundle_root,
            pins,
            commit,
        }
    }

    fn load(&self) -> (ProductContractBundle, TrustedProductPolicy) {
        let bundle =
            ProductContractBundle::load(&self.bundle_root, &self.pins).expect("load pinned bundle");
        let policy = TrustedProductPolicy::load(
            self.bundle_root.join("workspace-product-policy-v2.json"),
            &self.pins,
        )
        .expect("load separately pinned policy");
        policy
            .validate_bundle(&bundle)
            .expect("policy selectors must be supplied by the catalog");
        (bundle, policy)
    }

    fn invocation(&self, body: &[u8], operation_id: &str) -> ProductApiInvocationV2 {
        ProductApiInvocationV2 {
            owner_id: OWNER_ID.to_owned(),
            operation_id: operation_id.to_owned(),
            json_body: body.to_vec(),
            resource_id: RESOURCE_ID.to_owned(),
            idempotency_key: "request-key-123".to_owned(),
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn a_new_catalog_operation_loads_authorizes_and_resolves_without_core_enum_changes() {
    let fixture = Fixture::new();
    let (bundle, policy) = fixture.load();
    assert_eq!(bundle.source_sha(OWNER_ID), Some(fixture.commit.as_str()));
    let principal = VerifiedProductPrincipal::directory_user([
        "workspace.member".to_owned(),
        "workspace.product.command.example.publish_artifact.v2".to_owned(),
    ]);
    let scope = TrustedWorkspaceScope::new("organization-7", WORKSPACE_ID).unwrap();
    let duplicate_scope_body =
        br#"{"workspaceId":"workspace-other","workspaceId":"workspace-42","name":"model card"}"#;
    assert!(matches!(
        policy.authorize(
            &bundle,
            fixture.invocation(duplicate_scope_body, OPERATION_ID),
            &principal,
            scope.clone(),
        ),
        Err(AuthorizationError::InvalidRequest)
    ));

    let original_body = br#"{ "workspaceId":"workspace-42", "name":"model card" }"#;
    let call = policy
        .authorize(
            &bundle,
            fixture.invocation(original_body, OPERATION_ID),
            &principal,
            scope,
        )
        .expect("approved operation should produce an opaque token");

    assert_eq!(call.owner_id(), OWNER_ID);
    assert_eq!(call.operation_id(), OPERATION_ID);
    assert_eq!(call.json_body_bytes(), Some(original_body.as_slice()));
    assert_eq!(call.idempotency_key(), Some("request-key-123"));
    assert_eq!(call.route().method(), "PATCH");
    assert_eq!(
        call.route().path_template(),
        "/v2/workspaces/{workspace_id}/artifacts/{artifact_id}"
    );
    assert_eq!(
        call.route()
            .path_parameters()
            .get("workspace_id")
            .map(String::as_str),
        Some(WORKSPACE_ID)
    );
    assert_eq!(
        call.route()
            .path_parameters()
            .get("artifact_id")
            .map(String::as_str),
        Some(RESOURCE_ID)
    );

    let response = ProductInvocationResponse {
        status_code: 200,
        content_type: "application/json".to_owned(),
        json_body: br#"{"workspaceId":"workspace-42","id":"artifact-77"}"#.to_vec(),
    };
    call.validate_response(&response)
        .expect("schema and scope-bound response should pass");

    let duplicate_scope_response = ProductInvocationResponse {
        json_body:
            br#"{"workspaceId":"workspace-other","workspaceId":"workspace-42","id":"artifact-77"}"#
                .to_vec(),
        ..response.clone()
    };
    assert_eq!(
        call.validate_response(&duplicate_scope_response),
        Err(ProductInvocationError::Internal)
    );

    let cross_workspace_response = ProductInvocationResponse {
        json_body: br#"{"workspaceId":"workspace-else","id":"artifact-77"}"#.to_vec(),
        ..response.clone()
    };
    assert_eq!(
        call.validate_response(&cross_workspace_response),
        Err(ProductInvocationError::Internal)
    );
}

#[test]
fn dynamic_grants_fail_closed_for_missing_grant_unknown_operation_and_bad_pins() {
    let fixture = Fixture::new();
    let (bundle, policy) = fixture.load();
    let principal = VerifiedProductPrincipal::directory_user([
        "workspace.member".to_owned(),
        "workspace.product.command.example.publish_artifact.v2".to_owned(),
    ]);
    let scope = TrustedWorkspaceScope::new("organization-7", WORKSPACE_ID).unwrap();

    let empty_policy_raw = serde_json::to_vec(&json!({
        "schemaVersion": POLICY_VERSION,
        "policyVersion": "2.0.0",
        "grants": []
    }))
    .unwrap();
    let empty_policy_path = fixture.root.join("no-grants.json");
    fs::write(&empty_policy_path, &empty_policy_raw).unwrap();
    let empty_pins = ProductBundlePins::new(
        fixture.pins.wire_api_version(),
        fixture.pins.manifest_sha256(),
        BTreeMap::from([(OWNER_ID.to_owned(), fixture.commit.clone())]),
        POLICY_VERSION,
        sha256(&empty_policy_raw),
    );
    let no_grants = TrustedProductPolicy::load(&empty_policy_path, &empty_pins).unwrap();
    let request = fixture.invocation(
        br#"{"workspaceId":"workspace-42","name":"model card"}"#,
        OPERATION_ID,
    );
    assert!(matches!(
        no_grants.authorize(&bundle, request, &principal, scope.clone()),
        Err(AuthorizationError::UnapprovedOperation)
    ));

    assert!(matches!(
        policy.authorize(
            &bundle,
            fixture.invocation(
                br#"{"workspaceId":"workspace-42","name":"model card"}"#,
                "unknownOperationAddedLater"
            ),
            &principal,
            scope.clone(),
        ),
        Err(AuthorizationError::UnknownOperation)
    ));

    let mut unknown_owner_request = fixture.invocation(
        br#"{"workspaceId":"workspace-42","name":"model card"}"#,
        OPERATION_ID,
    );
    unknown_owner_request.owner_id = "future-owner".to_owned();
    assert!(matches!(
        policy.authorize(&bundle, unknown_owner_request, &principal, scope.clone()),
        Err(AuthorizationError::UnknownOperation)
    ));

    let wrong_pins = ProductBundlePins::new(
        fixture.pins.wire_api_version(),
        fixture.pins.manifest_sha256(),
        BTreeMap::from([(OWNER_ID.to_owned(), "0".repeat(40))]),
        POLICY_VERSION,
        fixture.pins.policy_sha256(),
    );
    assert!(matches!(
        ProductContractBundle::load(&fixture.bundle_root, &wrong_pins),
        Err(cy_workspace_product_contracts::CatalogError::PinMismatch)
    ));

    assert!(matches!(
        policy.authorize(
            &bundle,
            fixture.invocation(
                br#"{"workspaceId":"different-workspace","name":"model card"}"#,
                OPERATION_ID
            ),
            &principal,
            scope,
        ),
        Err(AuthorizationError::InvalidRequest)
    ));
}

#[test]
fn catalog_tampering_is_rejected_even_when_a_different_core_operation_is_requested() {
    let fixture = Fixture::new();
    let catalog_path = fixture
        .bundle_root
        .join(format!("{REPOSITORY}/contracts/product/v2/catalog.json"));
    let mut bytes = fs::read(&catalog_path).unwrap();
    bytes.extend_from_slice(b" ");
    fs::write(catalog_path, bytes).unwrap();
    assert!(matches!(
        ProductContractBundle::load(&fixture.bundle_root, &fixture.pins),
        Err(cy_workspace_product_contracts::CatalogError::FileInvalid)
    ));
}

#[test]
fn an_owner_catalog_cannot_drop_a_platform_required_response_scope_selector() {
    let fixture = Fixture::new();
    let catalog_path = fixture
        .bundle_root
        .join(format!("{REPOSITORY}/contracts/product/v2/catalog.json"));
    let mut catalog: Value = serde_json::from_slice(&fs::read(&catalog_path).unwrap()).unwrap();
    catalog["operations"][0]["scope"]["responseBindings"] = json!([
        {"jsonPointer": "/workspaceId", "matches": "WORKSPACE_ID"}
    ]);
    let catalog_raw = serde_json::to_vec_pretty(&catalog).unwrap();
    fs::write(&catalog_path, &catalog_raw).unwrap();

    let manifest_path = fixture.bundle_root.join("product-contract-bundle.json");
    let mut manifest: Value = serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    let catalog_bundle_path = format!("{REPOSITORY}/contracts/product/v2/catalog.json");
    let catalog_digest = sha256(&catalog_raw);
    for file in manifest["files"].as_array_mut().unwrap() {
        if file["path"] == catalog_bundle_path {
            file["sha256"] = Value::String(catalog_digest.clone());
        }
    }
    manifest["owners"][0]["catalogSha256"] = Value::String(catalog_digest);
    let mut manifest_raw = serde_json::to_vec_pretty(&manifest).unwrap();
    manifest_raw.push(b'\n');
    fs::write(&manifest_path, &manifest_raw).unwrap();

    let pins = ProductBundlePins::new(
        fixture.pins.wire_api_version(),
        sha256(&manifest_raw),
        BTreeMap::from([(OWNER_ID.to_owned(), fixture.commit.clone())]),
        POLICY_VERSION,
        fixture.pins.policy_sha256(),
    );
    let bundle = ProductContractBundle::load(&fixture.bundle_root, &pins)
        .expect("digest-correct owner bundle still loads before policy comparison");
    let policy = TrustedProductPolicy::load(
        fixture.bundle_root.join("workspace-product-policy-v2.json"),
        &pins,
    )
    .unwrap();
    assert_eq!(
        policy.validate_bundle(&bundle),
        Err(cy_workspace_product_contracts::CatalogError::CatalogInvalid)
    );
}

fn git(repository: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .output()
        .expect("git must be installed for the isolated owner fixture");
    assert_success(output);
}

fn git_output(repository: &Path, arguments: &[&str]) -> Result<String, Output> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .output()
        .expect("git must be installed for the isolated owner fixture");
    if output.status.success() {
        Ok(String::from_utf8(output.stdout).expect("git output is UTF-8"))
    } else {
        Err(output)
    }
}

fn git_show(repository: &Path, commit: &str, path: &str) -> Vec<u8> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(["show", &format!("{commit}:{path}")])
        .output()
        .expect("git must be installed for the isolated owner fixture");
    assert_success(output)
}

fn assert_success(output: Output) -> Vec<u8> {
    assert!(
        output.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
