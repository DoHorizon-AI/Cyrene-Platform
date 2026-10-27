//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 directory_connector_operator.rs                                 │
//! │  Module: cy_workspace_postgres_storage::tests                       │
//! │  Role: Verify live Directory maintainer grants and revocation.      │
//! │                                                                     │
//! │  模块职责：验证真实 Directory maintainer 授权及撤销。                 │
//! └─────────────────────────────────────────────────────────────────────┘

use std::{
    env,
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use cy_proto::cyrene::workspace::authority::v2::AuthorizeConnectorOperationRequest;
use cy_proto::workspace_v1::UserIdentityRef;
use cy_workspace_control_plane::authority::{
    issue_connector_operation_grant, verify_connector_operation_grant, AuthorityCredentialSigner,
    AuthorityManagedWorkload, ConnectorOperationGrantExpectation, NAVIGATOR_AUTOMATION_VIEWER_ROLE,
    NAVIGATOR_CONNECTOR_OPERATOR_ROLE, NAVIGATOR_OFFICE_TASK_WRITER_ROLE,
};
use cy_workspace_postgres_storage::{DirectoryOperatorProvisioner, PostgresWorkspaceDirectory};
use ed25519_dalek::SigningKey;
use tonic::Code;
use uuid::Uuid;

const MIGRATION_DATABASE_URL_ENV: &str = "CYRENE_WORKSPACE_DIRECTORY_MIGRATION_DATABASE_URL";
const OPERATOR_DATABASE_URL_ENV: &str = "CYRENE_WORKSPACE_DIRECTORY_OPERATOR_DATABASE_URL";
const READER_DATABASE_URL_ENV: &str = "CYRENE_WORKSPACE_DIRECTORY_TEST_DATABASE_URL";
const OPERATOR_ID_ENV: &str = "CYRENE_WORKSPACE_DIRECTORY_OPERATOR_ID";

#[tokio::test]
#[ignore = "requires an isolated PostgreSQL database and Directory migration/operator credentials"]
async fn maintainer_role_grant_and_revoke_are_immediately_visible_to_directory_reads() {
    let migration_url =
        env::var(MIGRATION_DATABASE_URL_ENV).expect("isolated Directory migration database URL");
    let operator_url =
        env::var(OPERATOR_DATABASE_URL_ENV).expect("restricted Directory operator database URL");
    let reader_url = env::var(READER_DATABASE_URL_ENV).expect("Directory reader database URL");
    set_test_env(MIGRATION_DATABASE_URL_ENV, &migration_url);

    DirectoryOperatorProvisioner::migrate_from_environment()
        .await
        .expect("Directory migrations apply to isolated test database");
    let directory = PostgresWorkspaceDirectory::connect(&reader_url)
        .await
        .expect("Directory reader connects");

    let identity = UserIdentityRef {
        issuer: "https://login.microsoftonline.com/11111111-1111-4111-8111-111111111111/v2.0"
            .to_owned(),
        subject: Uuid::new_v4().to_string(),
    };
    let organization_id = format!("connector-test-org-{}", Uuid::new_v4());
    let workspace_id = format!("connector-test-workspace-{}", Uuid::new_v4());
    let signer = AuthorityCredentialSigner::new(
        "directory-integration-test-key",
        SigningKey::from_bytes(&[5; 32]),
        Duration::from_secs(30),
    )
    .expect("Authority test signing key is valid");
    let exchange = AuthorityManagedWorkload {
        workload_id: "cy-workspace-exchange-test".to_owned(),
        audience: "exchange".to_owned(),
        certificate_fingerprints_sha256: Vec::new(),
    };
    let request = AuthorizeConnectorOperationRequest {
        workspace_id: workspace_id.clone(),
        principal_issuer: identity.issuer.clone(),
        principal_subject: identity.subject.clone(),
        connection_ref: "github-test-connection".to_owned(),
        provider_id: "github".to_owned(),
        profile_ref: "github-test-profile".to_owned(),
        operation_id: "github.repository.list".to_owned(),
        resource_ref: "repositories".to_owned(),
        arguments_digest_sha256: vec![7; 32],
        request_id: format!("test-activity-{}", Uuid::new_v4()),
    };

    let add_status = Command::new(env!("CARGO_BIN_EXE_cy-workspace-directory-admin"))
        .args([
            "grant-membership",
            "--issuer",
            &identity.issuer,
            "--subject",
            &identity.subject,
            "--organization",
            &organization_id,
            "--workspace",
            &workspace_id,
            "--role",
            NAVIGATOR_AUTOMATION_VIEWER_ROLE,
            "--reason",
            "automation viewer integration grant",
        ])
        .env(OPERATOR_DATABASE_URL_ENV, &operator_url)
        .env(OPERATOR_ID_ENV, "directory-connector-integration-test")
        .status()
        .expect("generic membership grant CLI starts");
    assert!(
        add_status.success(),
        "generic viewer membership grant succeeds"
    );

    let operator_grant_status = Command::new(env!("CARGO_BIN_EXE_cy-workspace-directory-admin"))
        .args([
            "maintainer",
            "add",
            "--issuer",
            &identity.issuer,
            "--subject",
            &identity.subject,
            "--organization",
            &organization_id,
            "--workspace",
            &workspace_id,
            "--reason",
            "connector maintainer integration grant",
        ])
        .env(OPERATOR_DATABASE_URL_ENV, &operator_url)
        .env(OPERATOR_ID_ENV, "directory-connector-integration-test")
        .status()
        .expect("maintainer add CLI starts");
    assert!(
        operator_grant_status.success(),
        "maintainer add CLI succeeds"
    );

    let after_grant = directory
        .roles_for_member(&identity, &organization_id, &workspace_id)
        .await
        .expect("Directory query succeeds")
        .expect("new membership is visible");
    assert!(after_grant.contains(NAVIGATOR_CONNECTOR_OPERATOR_ROLE));
    assert!(after_grant.contains(NAVIGATOR_AUTOMATION_VIEWER_ROLE));

    let readback = Command::new(env!("CARGO_BIN_EXE_cy-workspace-directory-admin"))
        .args([
            "membership",
            "get",
            "--issuer",
            &identity.issuer,
            "--subject",
            &identity.subject,
            "--organization",
            &organization_id,
            "--workspace",
            &workspace_id,
        ])
        .env("CYRENE_WORKSPACE_DIRECTORY_DATABASE_URL", &reader_url)
        .output()
        .expect("read-only Directory membership CLI starts");
    assert!(readback.status.success(), "membership readback succeeds");
    let readback: serde_json::Value =
        serde_json::from_slice(&readback.stdout).expect("membership readback is one JSON object");
    assert_eq!(readback["isMember"], true);
    assert_eq!(readback["issuer"], identity.issuer);
    assert_eq!(readback["subject"], identity.subject);
    assert_eq!(readback["organization"], organization_id);
    assert_eq!(readback["workspace"], workspace_id);
    assert!(readback["roles"]
        .as_array()
        .expect("roles is an explicit array")
        .iter()
        .any(|role| role == NAVIGATOR_AUTOMATION_VIEWER_ROLE));

    let writer_grant_status = Command::new(env!("CARGO_BIN_EXE_cy-workspace-directory-admin"))
        .args([
            "grant-role",
            "--issuer",
            &identity.issuer,
            "--subject",
            &identity.subject,
            "--organization",
            &organization_id,
            "--workspace",
            &workspace_id,
            "--role",
            NAVIGATOR_OFFICE_TASK_WRITER_ROLE,
            "--reason",
            "Office task writer integration grant",
        ])
        .env(OPERATOR_DATABASE_URL_ENV, &operator_url)
        .env(OPERATOR_ID_ENV, "directory-connector-integration-test")
        .status()
        .expect("generic Office task writer role grant CLI starts");
    assert!(writer_grant_status.success());
    let after_writer_grant = directory
        .roles_for_member(&identity, &organization_id, &workspace_id)
        .await
        .expect("Directory query succeeds after writer role grant")
        .expect("membership remains visible after writer role grant");
    assert!(after_writer_grant.contains(NAVIGATOR_OFFICE_TASK_WRITER_ROLE));

    let grant = issue_connector_operation_grant(
        &directory,
        &signer,
        &organization_id,
        &workspace_id,
        &identity.issuer,
        &exchange,
        &request,
    )
    .await
    .expect("current Directory operator role can receive an exact signed grant");
    let now_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after Unix epoch")
        .as_millis() as u64;
    verify_connector_operation_grant(
        &grant,
        &signer.verifying_key(),
        "directory-integration-test-key",
        ConnectorOperationGrantExpectation {
            audience: "exchange",
            organization_id: &organization_id,
            workspace_id: &workspace_id,
            principal_issuer: &identity.issuer,
            principal_subject: &identity.subject,
            connection_ref: &request.connection_ref,
            provider_id: &request.provider_id,
            profile_ref: &request.profile_ref,
            operation_id: &request.operation_id,
            resource_ref: &request.resource_ref,
            arguments_digest_sha256: &request.arguments_digest_sha256,
            request_id: &request.request_id,
        },
        now_unix_ms,
    )
    .expect("grant verifies against its exact operation binding");

    let github_mutating_request = AuthorizeConnectorOperationRequest {
        connection_ref: "github-test-connection".to_owned(),
        provider_id: "github".to_owned(),
        profile_ref: "github-test-profile".to_owned(),
        operation_id: "github.actions.rerun_failed".to_owned(),
        resource_ref: "repository:123/run:45".to_owned(),
        arguments_digest_sha256: vec![8; 32],
        request_id: format!("test-github-rerun-{}", Uuid::new_v4()),
        ..request.clone()
    };
    let computer_request = AuthorizeConnectorOperationRequest {
        connection_ref: "computer-test-connection".to_owned(),
        provider_id: "computer".to_owned(),
        profile_ref: "computer-test-profile".to_owned(),
        operation_id: "computer.command.execute".to_owned(),
        resource_ref: "workspace:workspace-1".to_owned(),
        arguments_digest_sha256: vec![9; 32],
        request_id: format!("test-computer-command-{}", Uuid::new_v4()),
        ..request.clone()
    };
    for scoped_request in [&github_mutating_request, &computer_request] {
        let scoped_grant = issue_connector_operation_grant(
            &directory,
            &signer,
            &organization_id,
            &workspace_id,
            &identity.issuer,
            &exchange,
            scoped_request,
        )
        .await
        .expect("current Directory operator role can receive each newly cataloged grant");
        verify_connector_operation_grant(
            &scoped_grant,
            &signer.verifying_key(),
            "directory-integration-test-key",
            ConnectorOperationGrantExpectation {
                audience: "exchange",
                organization_id: &organization_id,
                workspace_id: &workspace_id,
                principal_issuer: &identity.issuer,
                principal_subject: &identity.subject,
                connection_ref: &scoped_request.connection_ref,
                provider_id: &scoped_request.provider_id,
                profile_ref: &scoped_request.profile_ref,
                operation_id: &scoped_request.operation_id,
                resource_ref: &scoped_request.resource_ref,
                arguments_digest_sha256: &scoped_request.arguments_digest_sha256,
                request_id: &scoped_request.request_id,
            },
            now_unix_ms,
        )
        .expect("new GitHub and computer grants verify against exact signed scope");
    }

    let revoke_status = Command::new(env!("CARGO_BIN_EXE_cy-workspace-directory-admin"))
        .args([
            "maintainer",
            "revoke",
            "--issuer",
            &identity.issuer,
            "--subject",
            &identity.subject,
            "--organization",
            &organization_id,
            "--workspace",
            &workspace_id,
            "--reason",
            "connector maintainer integration revoke",
        ])
        .env(OPERATOR_DATABASE_URL_ENV, &operator_url)
        .env(OPERATOR_ID_ENV, "directory-connector-integration-test")
        .status()
        .expect("maintainer revoke CLI starts");
    assert!(revoke_status.success(), "maintainer revoke CLI succeeds");

    let writer_revoke_status = Command::new(env!("CARGO_BIN_EXE_cy-workspace-directory-admin"))
        .args([
            "revoke-role",
            "--issuer",
            &identity.issuer,
            "--subject",
            &identity.subject,
            "--organization",
            &organization_id,
            "--workspace",
            &workspace_id,
            "--role",
            NAVIGATOR_OFFICE_TASK_WRITER_ROLE,
            "--reason",
            "Office task writer integration revoke",
        ])
        .env(OPERATOR_DATABASE_URL_ENV, &operator_url)
        .env(OPERATOR_ID_ENV, "directory-connector-integration-test")
        .status()
        .expect("generic Office task writer role revoke CLI starts");
    assert!(writer_revoke_status.success());

    let viewer_revoke_status = Command::new(env!("CARGO_BIN_EXE_cy-workspace-directory-admin"))
        .args([
            "revoke-role",
            "--issuer",
            &identity.issuer,
            "--subject",
            &identity.subject,
            "--organization",
            &organization_id,
            "--workspace",
            &workspace_id,
            "--role",
            NAVIGATOR_AUTOMATION_VIEWER_ROLE,
            "--reason",
            "automation viewer integration revoke",
        ])
        .env(OPERATOR_DATABASE_URL_ENV, &operator_url)
        .env(OPERATOR_ID_ENV, "directory-connector-integration-test")
        .status()
        .expect("generic role revoke CLI starts");
    assert!(
        viewer_revoke_status.success(),
        "generic viewer role revoke succeeds"
    );

    let after_revoke = directory
        .roles_for_member(&identity, &organization_id, &workspace_id)
        .await
        .expect("Directory query succeeds after revoke")
        .expect("membership remains after role revoke");
    assert!(!after_revoke.contains(NAVIGATOR_CONNECTOR_OPERATOR_ROLE));
    assert!(!after_revoke.contains(NAVIGATOR_AUTOMATION_VIEWER_ROLE));
    assert!(!after_revoke.contains(NAVIGATOR_OFFICE_TASK_WRITER_ROLE));
    let retained_membership_readback =
        Command::new(env!("CARGO_BIN_EXE_cy-workspace-directory-admin"))
            .args([
                "membership",
                "get",
                "--issuer",
                &identity.issuer,
                "--subject",
                &identity.subject,
                "--organization",
                &organization_id,
                "--workspace",
                &workspace_id,
            ])
            .env("CYRENE_WORKSPACE_DIRECTORY_DATABASE_URL", &reader_url)
            .output()
            .expect("membership remains readable after role revokes");
    assert!(retained_membership_readback.status.success());
    let retained_membership_readback: serde_json::Value =
        serde_json::from_slice(&retained_membership_readback.stdout)
            .expect("membership readback is JSON");
    assert_eq!(retained_membership_readback["isMember"], true);
    assert_eq!(retained_membership_readback["roles"], serde_json::json!([]));
    assert_eq!(
        issue_connector_operation_grant(
            &directory,
            &signer,
            &organization_id,
            &workspace_id,
            &identity.issuer,
            &exchange,
            &request,
        )
        .await
        .expect_err("the revoked role must not receive another grant")
        .code(),
        Code::PermissionDenied
    );

    let cleanup_status = Command::new(env!("CARGO_BIN_EXE_cy-workspace-directory-admin"))
        .args([
            "revoke-membership",
            "--issuer",
            &identity.issuer,
            "--subject",
            &identity.subject,
            "--organization",
            &organization_id,
            "--workspace",
            &workspace_id,
            "--reason",
            "connector maintainer integration cleanup",
        ])
        .env(OPERATOR_DATABASE_URL_ENV, &operator_url)
        .env(OPERATOR_ID_ENV, "directory-connector-integration-test")
        .status()
        .expect("membership cleanup CLI starts");
    assert!(cleanup_status.success(), "membership cleanup CLI succeeds");
    let absent_membership_readback =
        Command::new(env!("CARGO_BIN_EXE_cy-workspace-directory-admin"))
            .args([
                "membership",
                "get",
                "--issuer",
                &identity.issuer,
                "--subject",
                &identity.subject,
                "--organization",
                &organization_id,
                "--workspace",
                &workspace_id,
            ])
            .env("CYRENE_WORKSPACE_DIRECTORY_DATABASE_URL", &reader_url)
            .output()
            .expect("membership absence readback starts");
    assert!(absent_membership_readback.status.success());
    let absent_membership_readback: serde_json::Value =
        serde_json::from_slice(&absent_membership_readback.stdout)
            .expect("membership readback is JSON");
    assert_eq!(absent_membership_readback["isMember"], false);
    assert_eq!(absent_membership_readback["roles"], serde_json::json!([]));
}

fn set_test_env(name: &str, value: &str) {
    // This isolated integration test runs in its own process and owns these Directory config keys.
    env::set_var(name, value);
}
