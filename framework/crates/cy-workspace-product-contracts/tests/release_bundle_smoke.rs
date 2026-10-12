//! Product v2 release bundle loader smoke test.
//!
//! This test loads the external bundle context against the tracked Platform
//! lock when `CYRENE_PRODUCT_V2_BUNDLE_ROOT` points to a generated release.
//! / 指向生成的发布包时，按 Platform 跟踪的 lock 做真实 loader 冒烟验证。

use std::collections::BTreeMap;
use std::path::PathBuf;

use cy_proto::cyrene::workspace::product::v2::ProductApiInvocationV2;
use cy_workspace_product_contracts::{
    AuthorizationError, ProductBundlePins, ProductContractBundle, TrustedProductPolicy,
    TrustedWorkspaceScope, VerifiedProductPrincipal, CONTRACT_API_VERSION,
};
use serde_json::Value;

const RELEASE_LOCK: &str = include_str!(
    "../../../../tooling/workspace-product-contract-bundle/releases/workspace-product-v2.lock.json"
);

#[test]
#[ignore = "requires a generated bundle in CYRENE_PRODUCT_V2_BUNDLE_ROOT"]
fn generated_six_owner_bundle_loads_with_the_tracked_platform_lock() {
    let bundle_root = PathBuf::from(
        std::env::var_os("CYRENE_PRODUCT_V2_BUNDLE_ROOT")
            .expect("set CYRENE_PRODUCT_V2_BUNDLE_ROOT to the generated bundle context"),
    );
    let lock: Value = serde_json::from_str(RELEASE_LOCK).expect("tracked release lock is valid");
    assert_eq!(
        lock["contractApiVersion"].as_str(),
        Some(CONTRACT_API_VERSION)
    );
    let owner_source_shas = lock["owners"]
        .as_array()
        .expect("release lock has owner pins")
        .iter()
        .map(|owner| {
            (
                owner["ownerId"]
                    .as_str()
                    .expect("owner ID is a string")
                    .to_owned(),
                owner["sourceSha"]
                    .as_str()
                    .expect("source SHA is a string")
                    .to_owned(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let pins = ProductBundlePins::new(
        lock["wireApiVersion"]
            .as_str()
            .expect("wire API version is a string"),
        lock["bundle"]["manifestSha256"]
            .as_str()
            .expect("manifest SHA-256 is a string"),
        owner_source_shas,
        lock["policy"]["schemaVersion"]
            .as_str()
            .expect("policy schema version is a string"),
        lock["policy"]["sha256"]
            .as_str()
            .expect("policy SHA-256 is a string"),
    );

    let bundle = ProductContractBundle::load(&bundle_root, &pins)
        .expect("generated bundle matches the tracked Platform release lock");
    let policy_path = bundle_root.join(
        lock["policy"]["bundlePath"]
            .as_str()
            .expect("policy bundle path is a string"),
    );
    let policy = TrustedProductPolicy::load(policy_path, &pins)
        .expect("generated Platform policy matches the tracked release lock");
    policy
        .validate_bundle(&bundle)
        .expect("approved policy selectors exist in owner catalogs");

    let member_reads = [
        "accountBeginLink",
        "accountListLinks",
        "accountConfirmWeComLink",
        "accountUnlink",
        "integrationAttachActorBinding",
        "integrationListActorBindings",
        "integrationGetIssue",
        "integrationQueryIssue",
        "integrationGetOperation",
        "integrationReconcile",
        "integrationExecuteIssueQuery",
    ];
    for operation_id in member_reads {
        assert!(
            policy.has_grant("exchange", operation_id),
            "missing grant: {operation_id}"
        );
        assert_exchange_policy_rejects_roles(
            &bundle,
            &policy,
            operation_id,
            std::iter::empty::<String>(),
            AuthorizationError::MissingRole,
        );
    }

    let configure_operations = [
        "integrationListConnections",
        "integrationCreateConnection",
        "integrationListBindings",
        "integrationCreateBinding",
    ];
    for operation_id in configure_operations {
        assert!(
            policy.has_grant("exchange", operation_id),
            "missing grant: {operation_id}"
        );
        assert_exchange_policy_rejects_roles(
            &bundle,
            &policy,
            operation_id,
            ["workspace.member"],
            AuthorizationError::MissingRole,
        );
        assert_exchange_policy_rejects_roles(
            &bundle,
            &policy,
            operation_id,
            ["workspace.product.command.exchange.integration.configure.v1"],
            AuthorizationError::MissingRole,
        );
    }

    for operation_id in [
        "integrationCreateIssueComment",
        "integrationExecuteIssueComment",
    ] {
        assert!(
            policy.has_grant("exchange", operation_id),
            "missing grant: {operation_id}"
        );
        assert_exchange_policy_rejects_roles(
            &bundle,
            &policy,
            operation_id,
            ["workspace.member"],
            AuthorizationError::MissingRole,
        );
    }
    for operation_id in [
        "integrationUpdateIssueState",
        "integrationExecuteIssueState",
    ] {
        assert!(
            policy.has_grant("exchange", operation_id),
            "missing grant: {operation_id}"
        );
        assert_exchange_policy_rejects_roles(
            &bundle,
            &policy,
            operation_id,
            ["workspace.member"],
            AuthorizationError::MissingRole,
        );
    }

    assert!(!policy.has_grant("exchange", "accountCompleteGitHubLink"));
    assert!(!policy.has_grant("exchange", "integrationReceiveGitHubWebhook"));
    let trusted_workload = VerifiedProductPrincipal::trusted_navigator_workload();
    let account_link_request = ProductApiInvocationV2 {
        owner_id: "exchange".to_owned(),
        operation_id: "accountBeginLink".to_owned(),
        ..ProductApiInvocationV2::default()
    };
    let scope = TrustedWorkspaceScope::new("organization-release-smoke", "workspace-release-smoke")
        .expect("test Workspace scope is valid");
    assert!(matches!(
        policy.authorize(&bundle, account_link_request, &trusted_workload, scope),
        Err(AuthorizationError::PrincipalNotAllowed)
    ));

    assert_eq!(
        bundle.owner_ids().count(),
        lock["owners"].as_array().unwrap().len()
    );
    assert!(bundle
        .operation("navigator", "observeWorkspaceSnapshot")
        .is_some());
    assert!(!policy.has_grant(
        "navigator",
        "append_events_api_v1_harness_workspaces__workspace_id__sessions__session_id__append_post"
    ));
}

fn assert_exchange_policy_rejects_roles(
    bundle: &ProductContractBundle,
    policy: &TrustedProductPolicy,
    operation_id: &str,
    roles: impl IntoIterator<Item = impl Into<String>>,
    expected: AuthorizationError,
) {
    let request = ProductApiInvocationV2 {
        owner_id: "exchange".to_owned(),
        operation_id: operation_id.to_owned(),
        ..ProductApiInvocationV2::default()
    };
    let principal = VerifiedProductPrincipal::directory_user(roles.into_iter().map(Into::into));
    let scope = TrustedWorkspaceScope::new("organization-release-smoke", "workspace-release-smoke")
        .expect("test Workspace scope is valid");
    let result = policy.authorize(bundle, request, &principal, scope);
    assert!(
        matches!(result, Err(error) if error == expected),
        "expected {expected:?} for Exchange operation {operation_id}, got {result:?}"
    );
}
