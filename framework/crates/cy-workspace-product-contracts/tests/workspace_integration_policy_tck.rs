//! Workspace integration authorization-policy TCK.
//!
//! Loads the tracked Platform policy and exercises its grants through the
//! production policy evaluator using a digest-pinned owner-catalog fixture.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use cy_proto::cyrene::workspace::product::v2::ProductApiInvocationV2;
use cy_workspace_product_contracts::{
    AuthorizationError, AuthorizedProductInvocation, ProductBundlePins, ProductContractBundle,
    TrustedProductPolicy, TrustedWorkspaceScope, VerifiedProductPrincipal,
};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

const OWNER_ID: &str = "exchange";
const OWNER_REPOSITORY: &str = "Cyrene-IntegrationFixture";
const POLICY_SCHEMA_VERSION: &str = "cyrene.workspace.product.authorization-policy.v2";
const WORKSPACE_MEMBER: &str = "workspace.member";
const NATIVE_AUTHORIZATION_REF: &str = "wecom-cli:corp-a:authorization-a";
const SESSION_UUID: &str = "550e8400-e29b-41d4-a716-446655440000";
const CONNECTION_UUID: &str = "550e8400-e29b-41d4-a716-446655440001";
const NOTIFICATION_UUID: &str = "550e8400-e29b-41d4-a716-446655440002";

#[derive(Clone, Copy)]
struct OperationCase {
    operation_id: &'static str,
    route: &'static str,
    method: &'static str,
    is_command: bool,
    resource_parameter: Option<&'static str>,
    command_role: Option<&'static str>,
}

const OPERATIONS: [OperationCase; 8] = [
    OperationCase {
        operation_id: "accountBeginRenewalSession",
        route: "/api/v1/accounts/authorizations/{authorization_id}/renewal-sessions",
        method: "post",
        is_command: true,
        resource_parameter: Some("authorization_id"),
        command_role: Some("workspace.product.command.exchange.integration.renewal.v1"),
    },
    OperationCase {
        operation_id: "accountGetRenewalSession",
        route: "/api/v1/accounts/renewal-sessions/{session_id}",
        method: "get",
        is_command: false,
        resource_parameter: Some("session_id"),
        command_role: None,
    },
    OperationCase {
        operation_id: "integrationGetAuthorizationHealth",
        route: "/api/v1/integrations/connections/{connection_id}/authorization-health",
        method: "get",
        is_command: false,
        resource_parameter: Some("connection_id"),
        command_role: None,
    },
    OperationCase {
        operation_id: "integrationListConnectionResources",
        route: "/api/v1/integrations/connections/{connection_id}/resources",
        method: "get",
        is_command: false,
        resource_parameter: Some("connection_id"),
        command_role: None,
    },
    OperationCase {
        operation_id: "integrationRefreshConnectionResources",
        route: "/api/v1/integrations/connections/{connection_id}/actions/refresh",
        method: "post",
        is_command: true,
        resource_parameter: Some("connection_id"),
        command_role: Some("workspace.product.command.exchange.integration.refresh.v1"),
    },
    OperationCase {
        operation_id: "integrationListNotifications",
        route: "/api/v1/integrations/notifications",
        method: "get",
        is_command: false,
        resource_parameter: None,
        command_role: None,
    },
    OperationCase {
        operation_id: "integrationAcknowledgeNotification",
        route: "/api/v1/integrations/notifications/{notification_id}/ack",
        method: "post",
        is_command: true,
        resource_parameter: Some("notification_id"),
        command_role: Some("workspace.product.command.exchange.notification.ack.v1"),
    },
    OperationCase {
        operation_id: "integrationDeliverNotification",
        route: "/api/v1/integrations/notifications/{notification_id}/actions/deliver",
        method: "post",
        is_command: true,
        resource_parameter: Some("notification_id"),
        command_role: Some("workspace.product.command.exchange.notification.v1"),
    },
];

struct Fixture {
    root: PathBuf,
    bundle_root: PathBuf,
    pins: ProductBundlePins,
    policy_path: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock is after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "cyrene-exchange-policy-tck-{}-{nonce}",
            std::process::id()
        ));
        let bundle_root = root.join("bundle");
        let repository_root = bundle_root.join(OWNER_REPOSITORY);
        fs::create_dir_all(repository_root.join("contracts/product/v2"))
            .expect("create synthetic owner catalog directory");
        fs::create_dir_all(repository_root.join("contracts/product/v1"))
            .expect("create synthetic OpenAPI directory");

        let catalog_path = format!("{OWNER_REPOSITORY}/contracts/product/v2/catalog.json");
        let openapi_path = format!("{OWNER_REPOSITORY}/contracts/product/v1/openapi.yaml");
        let catalog = build_catalog();
        let openapi = build_openapi();
        let catalog_raw = serde_json::to_vec_pretty(&catalog).expect("serialize catalog");
        let openapi_raw = serde_json::to_vec_pretty(&openapi).expect("serialize OpenAPI");
        fs::write(bundle_root.join(&catalog_path), &catalog_raw).expect("write catalog");
        fs::write(bundle_root.join(&openapi_path), &openapi_raw).expect("write OpenAPI");

        let source_sha = "1111111111111111111111111111111111111111";
        let catalog_sha = sha256(&catalog_raw);
        let manifest = json!({
            "formatVersion": 2,
            "wireApiVersion": "cyrene.workspace.product.v2",
            "owners": [{
                "ownerId": OWNER_ID,
                "repository": OWNER_REPOSITORY,
                "sourceSha": source_sha,
                "catalogPath": catalog_path,
                "catalogSha256": catalog_sha
            }],
            "files": [
                {"path": openapi_path, "sha256": sha256(&openapi_raw)},
                {"path": catalog_path, "sha256": catalog_sha}
            ]
        });
        let manifest_raw = serde_json::to_vec_pretty(&manifest).expect("serialize manifest");
        fs::write(
            bundle_root.join("product-contract-bundle.json"),
            &manifest_raw,
        )
        .expect("write manifest");

        let policy_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../contracts/policies/workspace-product-policy-v2.json");
        let policy_raw = fs::read(&policy_path).expect("read tracked Platform policy");
        let pins = ProductBundlePins::new(
            "cyrene.workspace.product.v2",
            sha256(&manifest_raw),
            BTreeMap::from([(OWNER_ID.to_owned(), source_sha.to_owned())]),
            POLICY_SCHEMA_VERSION,
            sha256(&policy_raw),
        );

        Self {
            root,
            bundle_root,
            pins,
            policy_path,
        }
    }

    fn load(&self) -> (ProductContractBundle, TrustedProductPolicy) {
        let bundle = ProductContractBundle::load(&self.bundle_root, &self.pins)
            .expect("load digest-pinned synthetic owner bundle");
        let policy = TrustedProductPolicy::load(&self.policy_path, &self.pins)
            .expect("load actual tracked Platform policy bytes");
        (bundle, policy)
    }

    fn invocation(&self, operation: &OperationCase) -> ProductApiInvocationV2 {
        ProductApiInvocationV2 {
            owner_id: OWNER_ID.to_owned(),
            operation_id: operation.operation_id.to_owned(),
            json_body: Vec::new(),
            resource_id: operation
                .resource_parameter
                .map(|_| resource_id_for(operation.operation_id).to_owned())
                .unwrap_or_default(),
            idempotency_key: if operation.is_command {
                "policy-tck-key-123".to_owned()
            } else {
                String::new()
            },
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn workspace_integration_policy_accepts_exact_grants_and_denies_escalation() {
    let fixture = Fixture::new();
    let (bundle, policy) = fixture.load();
    assert_eq!(policy.policy_version(), "2.1.0");
    let scope = TrustedWorkspaceScope::new("organization-tck", "workspace-tck")
        .expect("verified exact Workspace scope");

    let member = VerifiedProductPrincipal::directory_user([WORKSPACE_MEMBER.to_owned()]);
    for operation in OPERATIONS.iter().filter(|operation| !operation.is_command) {
        assert!(
            policy
                .authorize(
                    &bundle,
                    fixture.invocation(operation),
                    &member,
                    scope.clone(),
                )
                .is_ok(),
            "Workspace member should read {}",
            operation.operation_id
        );
    }

    let renewal_operator = VerifiedProductPrincipal::directory_user([
        WORKSPACE_MEMBER.to_owned(),
        "workspace.product.command.exchange.integration.renewal.v1".to_owned(),
    ]);
    let renewal_call = policy
        .authorize(
            &bundle,
            fixture.invocation(&OPERATIONS[0]),
            &renewal_operator,
            scope.clone(),
        )
        .expect("opaque provider authorization reference remains a valid resource ID");
    assert_eq!(renewal_call.resource_id(), Some(NATIVE_AUTHORIZATION_REF));
    assert_eq!(
        renewal_call
            .route()
            .path_parameters()
            .get("authorization_id")
            .map(String::as_str),
        Some(NATIVE_AUTHORIZATION_REF),
        "the generic route binding must preserve the provider-native opaque reference"
    );
    let mut unsafe_opaque_reference = fixture.invocation(&OPERATIONS[0]);
    unsafe_opaque_reference.resource_id = "wecom-cli:corp-a/authorization-a".to_owned();
    assert_denied(
        policy.authorize(
            &bundle,
            unsafe_opaque_reference,
            &renewal_operator,
            scope.clone(),
        ),
        AuthorizationError::InvalidRequest,
        "opaque references cannot escape their single route segment",
    );

    let no_roles = VerifiedProductPrincipal::directory_user(std::iter::empty::<String>());
    for operation in &OPERATIONS {
        assert_denied(
            policy.authorize(
                &bundle,
                fixture.invocation(operation),
                &no_roles,
                scope.clone(),
            ),
            AuthorizationError::MissingRole,
            &format!(
                "missing current membership denies {}",
                operation.operation_id
            ),
        );
    }

    for operation in OPERATIONS.iter().filter(|operation| operation.is_command) {
        assert_denied(
            policy.authorize(
                &bundle,
                fixture.invocation(operation),
                &member,
                scope.clone(),
            ),
            AuthorizationError::MissingRole,
            &format!(
                "member-only principal must not command {}",
                operation.operation_id
            ),
        );

        let command_role = operation.command_role.expect("command has its exact role");
        let exact_roles = VerifiedProductPrincipal::directory_user([
            WORKSPACE_MEMBER.to_owned(),
            command_role.to_owned(),
        ]);
        assert!(
            policy
                .authorize(
                    &bundle,
                    fixture.invocation(operation),
                    &exact_roles,
                    scope.clone(),
                )
                .is_ok(),
            "exact role should authorize {}",
            operation.operation_id
        );

        for other in OPERATIONS
            .iter()
            .filter_map(|candidate| candidate.command_role)
        {
            if other == command_role {
                continue;
            }
            let substituted = VerifiedProductPrincipal::directory_user([
                WORKSPACE_MEMBER.to_owned(),
                other.to_owned(),
            ]);
            assert_denied(
                policy.authorize(
                    &bundle,
                    fixture.invocation(operation),
                    &substituted,
                    scope.clone(),
                ),
                AuthorizationError::MissingRole,
                &format!("{other} must not substitute for {command_role}"),
            );
        }
    }

    assert_denied(
        policy.authorize(
            &bundle,
            fixture.invocation(&OPERATIONS[7]),
            &member,
            scope.clone(),
        ),
        AuthorizationError::MissingRole,
        "notification-feed membership alone cannot deliver a notification",
    );

    let revoked_member = VerifiedProductPrincipal::directory_user([
        "workspace.product.command.exchange.notification.v1".to_owned(),
    ]);
    assert_denied(
        policy.authorize(
            &bundle,
            fixture.invocation(&OPERATIONS[7]),
            &revoked_member,
            scope.clone(),
        ),
        AuthorizationError::MissingRole,
        "revoked Workspace membership denies even a still-present command role",
    );

    let trusted_workload = VerifiedProductPrincipal::trusted_navigator_workload();
    assert_denied(
        policy.authorize(
            &bundle,
            fixture.invocation(&OPERATIONS[5]),
            &trusted_workload,
            scope.clone(),
        ),
        AuthorizationError::PrincipalNotAllowed,
        "the new integration grants are Directory-user only",
    );

    assert!(
        matches!(
            TrustedWorkspaceScope::new("organization-tck", ""),
            Err(AuthorizationError::ScopeMismatch)
        ),
        "an absent exact Workspace must fail closed before authorization"
    );
    assert!(matches!(
        TrustedWorkspaceScope::new("organization-tck", "workspace/other"),
        Err(AuthorizationError::ScopeMismatch)
    ));

    let mut unknown = fixture.invocation(&OPERATIONS[5]);
    unknown.operation_id = "integrationUnknownOperation".to_owned();
    assert_denied(
        policy.authorize(&bundle, unknown, &member, scope.clone()),
        AuthorizationError::UnknownOperation,
        "unknown operation must deny",
    );

    let unapproved = OperationCase {
        operation_id: "integrationUnapprovedFixture",
        route: "/api/v1/integrations/tck-unapproved",
        method: "get",
        is_command: false,
        resource_parameter: None,
        command_role: None,
    };
    assert_denied(
        policy.authorize(&bundle, fixture.invocation(&unapproved), &member, scope),
        AuthorizationError::UnapprovedOperation,
        "known but unapproved operation must deny",
    );
}

fn assert_denied(
    result: Result<AuthorizedProductInvocation, AuthorizationError>,
    expected: AuthorizationError,
    message: &str,
) {
    assert!(
        matches!(result, Err(actual) if actual == expected),
        "{message}"
    );
}

fn build_catalog() -> Value {
    let mut operations = OPERATIONS
        .iter()
        .map(operation_catalog_entry)
        .collect::<Vec<_>>();
    operations.push(operation_catalog_entry(&OperationCase {
        operation_id: "integrationUnapprovedFixture",
        route: "/api/v1/integrations/tck-unapproved",
        method: "get",
        is_command: false,
        resource_parameter: None,
        command_role: None,
    }));
    json!({
        "schemaVersion": "cyrene.product.operation-catalog.v2",
        "catalogVersion": "2.0.0",
        "ownerId": OWNER_ID,
        "operations": operations
    })
}

fn operation_catalog_entry(operation: &OperationCase) -> Value {
    let escaped_route = operation.route.replace('~', "~0").replace('/', "~1");
    let status = if operation.is_command { "202" } else { "200" };
    json!({
        "operationId": operation.operation_id,
        "routeId": operation.operation_id,
        "openapiPath": "contracts/product/v1/openapi.yaml",
        "kind": if operation.is_command { "COMMAND" } else { "READ" },
        "requestSchemaPointer": null,
        "responseSchemaPointers": [format!(
            "#/paths/{escaped_route}/{}/responses/{status}/content/application~1json/schema",
            operation.method
        )],
        "resourceId": {
            "required": operation.resource_parameter.is_some(),
            "pathParameter": operation.resource_parameter,
            "minLength": 1,
            "maxLength": 300,
            "pattern": null
        },
        "idempotency": {
            "required": operation.is_command,
            "header": operation.is_command.then_some("Idempotency-Key"),
            "minLength": 1,
            "maxLength": 200
        },
        "scope": {
            "organizationPathParameter": null,
            "workspacePathParameter": null,
            "requestBindings": [],
            "responseBindings": []
        }
    })
}

fn resource_id_for(operation_id: &str) -> &'static str {
    match operation_id {
        "accountBeginRenewalSession" => NATIVE_AUTHORIZATION_REF,
        "accountGetRenewalSession" => SESSION_UUID,
        "integrationGetAuthorizationHealth"
        | "integrationListConnectionResources"
        | "integrationRefreshConnectionResources" => CONNECTION_UUID,
        "integrationAcknowledgeNotification" | "integrationDeliverNotification" => {
            NOTIFICATION_UUID
        }
        _ => "",
    }
}

fn build_openapi() -> Value {
    let mut paths = Map::new();
    let unapproved = OperationCase {
        operation_id: "integrationUnapprovedFixture",
        route: "/api/v1/integrations/tck-unapproved",
        method: "get",
        is_command: false,
        resource_parameter: None,
        command_role: None,
    };
    for operation in OPERATIONS.iter().chain(std::iter::once(&unapproved)) {
        let status = if operation.is_command { "202" } else { "200" };
        let mut path_item = Map::new();
        if let Some(parameter) = operation.resource_parameter {
            path_item.insert(
                "parameters".to_owned(),
                json!([{
                    "name": parameter,
                    "in": "path",
                    "required": true,
                    "schema": {"type": "string"}
                }]),
            );
        }
        path_item.insert(
            operation.method.to_owned(),
            json!({
                "operationId": operation.operation_id,
                "responses": {
                    (status): {
                        "description": "TCK response",
                        "content": {
                            "application/json": {
                                "schema": {
                                    "type": "object",
                                    "additionalProperties": true
                                }
                            }
                        }
                    }
                }
            }),
        );
        paths.insert(operation.route.to_owned(), Value::Object(path_item));
    }
    json!({
        "openapi": "3.1.0",
        "info": {"title": "Workspace authorization TCK", "version": "1.0.0"},
        "paths": paths
    })
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
