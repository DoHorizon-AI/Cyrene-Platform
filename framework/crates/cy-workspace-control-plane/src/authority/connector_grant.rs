//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 connector_grant.rs                                              │
//! │  Module: cy_workspace_control_plane::authority::connector_grant    │
//! │  Role: Canonical exact-scope connector grant signing and validation.│
//! │                                                                     │
//! │  模块职责：精确 Connector scope grant 的规范化、签发与校验。         │
//! └─────────────────────────────────────────────────────────────────────┘

use cy_proto::cyrene::workspace::authority::v2::ConnectorOperationGrantV1;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use prost::Message;
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Directory role required before Authority can issue a connector operation grant.
pub const NAVIGATOR_CONNECTOR_OPERATOR_ROLE: &str = "navigator.connector.operator.v1";

/// Directory role allowed to request only the explicitly listed automation reads.
pub const NAVIGATOR_AUTOMATION_VIEWER_ROLE: &str = "navigator.automation.viewer.v1";

/// Directory role allowed to read the Graph pilot set and write Planner tasks.
pub const NAVIGATOR_OFFICE_TASK_WRITER_ROLE: &str = "navigator.office.task_writer.v1";

/// Wire schema version signed into every per-operation grant.
pub const CONNECTOR_OPERATION_GRANT_SCHEMA_VERSION: &str =
    "cyrene.workspace.connector-operation-grant.v1";

/// Maximum validity of one exact connector grant.
pub const CONNECTOR_OPERATION_GRANT_TTL_MS: u64 = 15_000;

/// Closed operation identifiers accepted by the connector authorization boundary.
pub const SUPPORTED_CONNECTOR_OPERATION_IDS: [&str; 81] = [
    "azure_devops.observations.poll",
    "cloud.operation.cancel",
    "cloud.operation.logs.read",
    "cloud.operation.status.read",
    "cloud.provider.health.read",
    "cloud.resource.create",
    "cloud.resource.delete",
    "cloud.resource.deploy",
    "cloud.resource.get",
    "cloud.resource.restart",
    "cloud.resource.start",
    "cloud.resource.stop",
    "cloud.resource.update",
    "cloud.resources.list",
    "cloud.vendor.cli.execute",
    "computer.command.execute",
    "computer.directory.list",
    "computer.file.read",
    "computer.file.write",
    "github.actions.job.logs.read",
    "github.actions.jobs.list",
    "github.actions.rerun_failed",
    "github.actions.runs.list",
    "github.branch.create",
    "github.branch.delete",
    "github.branch.get",
    "github.branches.list",
    "github.ci.status",
    "github.file.commit",
    "github.file.get",
    "github.issue.comment.create",
    "github.issue.get",
    "github.issue.state.update",
    "github.issues.search",
    "github.pull_request.draft.create",
    "github.pull_request.draft.find",
    "github.pull_request.get",
    "github.pull_requests.list",
    "github.repository.get",
    "github.repository.list",
    "github.vendor.cli.execute",
    "huggingface.datasets.list",
    "huggingface.file.download",
    "huggingface.file.upload",
    "huggingface.models.list",
    "huggingface.repo.get",
    "huggingface.spaces.list",
    "huggingface.vendor.cli.execute",
    "msteams.message.reply",
    "microsoft_graph.calendar.calendars.list",
    "microsoft_graph.calendar.event.create",
    "microsoft_graph.calendar.event.delete",
    "microsoft_graph.calendar.event.get",
    "microsoft_graph.calendar.event.update",
    "microsoft_graph.calendar.events.list",
    "microsoft_graph.calendar.freebusy.get",
    "microsoft_graph.mail.draft.create",
    "microsoft_graph.mail.draft.delete",
    "microsoft_graph.mail.draft.update",
    "microsoft_graph.mail.drafts.list",
    "microsoft_graph.mail.folders.list",
    "microsoft_graph.mail.message.get",
    "microsoft_graph.mail.message.send",
    "microsoft_graph.mail.messages.list",
    "microsoft_graph.planner.plan.get",
    "microsoft_graph.planner.plans.list",
    "microsoft_graph.planner.task.create",
    "microsoft_graph.planner.task.get",
    "microsoft_graph.planner.task.update",
    "microsoft_graph.planner.tasks.list",
    "microsoft_graph.sharepoint.drive.items.list",
    "microsoft_graph.sharepoint.drives.list",
    "microsoft_graph.sharepoint.list.get",
    "microsoft_graph.sharepoint.list.items.delta",
    "microsoft_graph.sharepoint.site.get",
    "microsoft_graph.todo.lists.list",
    "microsoft_graph.todo.task.create",
    "microsoft_graph.todo.task.delete",
    "microsoft_graph.todo.task.get",
    "microsoft_graph.todo.task.update",
    "microsoft_graph.todo.tasks.list",
];

/// Exact read operations currently available to the automation viewer role.
///
/// This deliberately excludes existing provider reads until their connection and source
/// authority are checked for the same actor by the provider path.
pub const AUTOMATION_VIEWER_CONNECTOR_OPERATION_IDS: [&str; 6] = [
    "microsoft_graph.planner.plan.get",
    "microsoft_graph.planner.tasks.list",
    "microsoft_graph.planner.task.get",
    "microsoft_graph.sharepoint.site.get",
    "microsoft_graph.sharepoint.list.get",
    "microsoft_graph.sharepoint.list.items.delta",
];

/// Planner, To Do, and selected SharePoint metadata operations for Office task writers.
pub const OFFICE_TASK_WRITER_CONNECTOR_OPERATION_IDS: [&str; 17] = [
    "microsoft_graph.planner.plan.get",
    "microsoft_graph.planner.plans.list",
    "microsoft_graph.planner.tasks.list",
    "microsoft_graph.planner.task.get",
    "microsoft_graph.planner.task.create",
    "microsoft_graph.planner.task.update",
    "microsoft_graph.sharepoint.site.get",
    "microsoft_graph.sharepoint.list.get",
    "microsoft_graph.sharepoint.list.items.delta",
    "microsoft_graph.sharepoint.drives.list",
    "microsoft_graph.sharepoint.drive.items.list",
    "microsoft_graph.todo.lists.list",
    "microsoft_graph.todo.tasks.list",
    "microsoft_graph.todo.task.get",
    "microsoft_graph.todo.task.create",
    "microsoft_graph.todo.task.update",
    "microsoft_graph.todo.task.delete",
];

/// Calendar operations remain limited to the existing connector operator role.
pub const OFFICE_CALENDAR_OPERATOR_ONLY_CONNECTOR_OPERATION_IDS: [&str; 7] = [
    "microsoft_graph.calendar.calendars.list",
    "microsoft_graph.calendar.events.list",
    "microsoft_graph.calendar.event.get",
    "microsoft_graph.calendar.freebusy.get",
    "microsoft_graph.calendar.event.create",
    "microsoft_graph.calendar.event.update",
    "microsoft_graph.calendar.event.delete",
];

/// Mail operations require the connector operator role and a separately opted-in profile.
///
/// This catalog grants no Microsoft permissions and does not enable mail on existing profiles.
/// 邮箱操作不会自动向普通任务成员开放，也不会替已有连接申请邮件权限。
pub const OFFICE_MAIL_OPERATOR_ONLY_CONNECTOR_OPERATION_IDS: [&str; 8] = [
    "microsoft_graph.mail.folders.list",
    "microsoft_graph.mail.messages.list",
    "microsoft_graph.mail.message.get",
    "microsoft_graph.mail.drafts.list",
    "microsoft_graph.mail.draft.create",
    "microsoft_graph.mail.draft.update",
    "microsoft_graph.mail.draft.delete",
    "microsoft_graph.mail.message.send",
];

/// Exact Teams transport operation available to the two automation roles.
///
/// This allows a member to receive a reply for their own authorized Work task. It does not
/// authorize arbitrary Teams messages or expand either role's source-operation catalog.
pub const AUTOMATION_TRANSPORT_CONNECTOR_OPERATION_IDS: [&str; 1] = ["msteams.message.reply"];

const SIGNATURE_DOMAIN: &[u8] = b"cyrene.workspace.authority.v2/connector-operation-grant\0";

/// Expected exact scope for verifying one connector operation grant.
#[derive(Debug, Clone, Copy)]
pub struct ConnectorOperationGrantExpectation<'a> {
    /// Authority configured to receive this grant.
    pub audience: &'a str,
    /// Exact Directory organization bound to the caller.
    pub organization_id: &'a str,
    /// Exact Directory workspace bound to the caller.
    pub workspace_id: &'a str,
    /// Exact verified identity issuer.
    pub principal_issuer: &'a str,
    /// Exact verified identity subject.
    pub principal_subject: &'a str,
    /// Opaque provider connection reference.
    pub connection_ref: &'a str,
    /// Canonical provider identifier.
    pub provider_id: &'a str,
    /// Server-owned provider profile reference.
    pub profile_ref: &'a str,
    /// Closed connector operation identifier.
    pub operation_id: &'a str,
    /// Canonical provider resource or collection reference.
    pub resource_ref: &'a str,
    /// SHA-256 over canonical operation arguments.
    pub arguments_digest_sha256: &'a [u8],
    /// Stable request identifier used for replay and idempotency handling.
    pub request_id: &'a str,
}

/// Sanitized failures from validating an Authority-signed connector grant.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum ConnectorOperationGrantError {
    /// The grant structure, schema, or canonical digest is invalid.
    #[error("CONNECTOR_OPERATION_GRANT_INVALID")]
    Invalid,
    /// One or more grant scope values differ from the requested operation.
    #[error("CONNECTOR_OPERATION_GRANT_SCOPE_MISMATCH")]
    ScopeMismatch,
    /// The grant is expired, not yet valid, or exceeds the short validity window.
    #[error("CONNECTOR_OPERATION_GRANT_EXPIRED")]
    Expired,
    /// The signature does not verify under the configured Authority key.
    #[error("CONNECTOR_OPERATION_GRANT_SIGNATURE_INVALID")]
    SignatureInvalid,
}

/// Return whether a provider and operation pair exists in the closed catalog.
pub fn is_supported_connector_operation(provider_id: &str, operation_id: &str) -> bool {
    if !SUPPORTED_CONNECTOR_OPERATION_IDS.contains(&operation_id) {
        return false;
    }
    match operation_id.split_once('.') {
        Some(("github", _)) => provider_id == "github",
        Some(("huggingface", _)) => provider_id == "huggingface",
        Some(("msteams", _)) => provider_id == "msteams",
        Some(("computer", _)) => provider_id == "computer",
        Some(("azure_devops", _)) => provider_id == "azure_devops",
        Some(("microsoft_graph", _)) => provider_id == "microsoft_graph",
        Some(("cloud", _)) if operation_id == "cloud.resource.restart" => provider_id == "runpod",
        Some(("cloud", _)) => matches!(provider_id, "azure" | "gcp" | "runpod"),
        _ => false,
    }
}

/// Return whether the exact provider/operation pair is in the viewer's read-only catalog.
pub fn is_automation_viewer_connector_operation(provider_id: &str, operation_id: &str) -> bool {
    AUTOMATION_VIEWER_CONNECTOR_OPERATION_IDS.contains(&operation_id)
        && is_supported_connector_operation(provider_id, operation_id)
}

/// Return whether the exact provider/operation pair is in the Office task writer catalog.
pub fn is_office_task_writer_connector_operation(provider_id: &str, operation_id: &str) -> bool {
    provider_id == "microsoft_graph"
        && OFFICE_TASK_WRITER_CONNECTOR_OPERATION_IDS.contains(&operation_id)
        && is_supported_connector_operation(provider_id, operation_id)
}

/// Return whether an exact provider/operation pair is in the automation transport catalog.
pub fn is_automation_transport_connector_operation(provider_id: &str, operation_id: &str) -> bool {
    provider_id == "msteams"
        && AUTOMATION_TRANSPORT_CONNECTOR_OPERATION_IDS.contains(&operation_id)
        && is_supported_connector_operation(provider_id, operation_id)
}

/// Verify the Authority signature, short lifetime, and every exact invocation binding.
///
/// The digest is SHA-256 of deterministic protobuf bytes after clearing only `grant_ref` and
/// `authority_signature`. The signature covers a domain-separated key ID and that digest.
/// Callers should invoke this for each action and must not reuse a grant for a later request.
pub fn verify_connector_operation_grant(
    grant: &ConnectorOperationGrantV1,
    verifying_key: &VerifyingKey,
    expected_key_id: &str,
    expected: ConnectorOperationGrantExpectation<'_>,
    now_unix_ms: u64,
) -> Result<(), ConnectorOperationGrantError> {
    if grant.schema_version != CONNECTOR_OPERATION_GRANT_SCHEMA_VERSION
        || grant.signing_key_id != expected_key_id
        || grant.arguments_digest_sha256.len() != 32
        || expected.arguments_digest_sha256.len() != 32
        || grant.authority_signature.len() != 64
    {
        return Err(ConnectorOperationGrantError::Invalid);
    }
    if grant.audience != expected.audience
        || grant.organization_id != expected.organization_id
        || grant.workspace_id != expected.workspace_id
        || grant.principal_issuer != expected.principal_issuer
        || grant.principal_subject != expected.principal_subject
        || grant.connection_ref != expected.connection_ref
        || grant.provider_id != expected.provider_id
        || grant.profile_ref != expected.profile_ref
        || grant.operation_id != expected.operation_id
        || grant.resource_ref != expected.resource_ref
        || grant.arguments_digest_sha256 != expected.arguments_digest_sha256
        || grant.request_id != expected.request_id
        || !is_supported_connector_operation(&grant.provider_id, &grant.operation_id)
    {
        return Err(ConnectorOperationGrantError::ScopeMismatch);
    }

    let issued_at = timestamp_to_unix_ms(grant.issued_at.as_ref())?;
    let expires_at = timestamp_to_unix_ms(grant.expires_at.as_ref())?;
    if issued_at == 0
        || expires_at <= issued_at
        || expires_at.saturating_sub(issued_at) > CONNECTOR_OPERATION_GRANT_TTL_MS
        || now_unix_ms >= expires_at
        || now_unix_ms.saturating_add(1_000) < issued_at
    {
        return Err(ConnectorOperationGrantError::Expired);
    }

    let mut unsigned = grant.clone();
    unsigned.grant_ref.clear();
    unsigned.authority_signature.clear();
    let digest = Sha256::digest(unsigned.encode_to_vec());
    if grant.grant_ref != format!("sha256:{}", encode_hex(&digest)) {
        return Err(ConnectorOperationGrantError::Invalid);
    }
    let message = signature_message(expected_key_id, &digest)?;
    let signature = Signature::from_slice(&grant.authority_signature)
        .map_err(|_| ConnectorOperationGrantError::SignatureInvalid)?;
    verifying_key
        .verify(&message, &signature)
        .map_err(|_| ConnectorOperationGrantError::SignatureInvalid)
}

/// Build the domain-separated signature input used by Authority and connector verifiers.
pub fn connector_operation_grant_signature_message(
    key_id: &str,
    digest: &[u8],
) -> Result<Vec<u8>, ConnectorOperationGrantError> {
    signature_message(key_id, digest)
}

pub(crate) fn grant_digest(grant: &ConnectorOperationGrantV1) -> [u8; 32] {
    let mut unsigned = grant.clone();
    unsigned.grant_ref.clear();
    unsigned.authority_signature.clear();
    Sha256::digest(unsigned.encode_to_vec()).into()
}

pub(crate) fn signature_message(
    key_id: &str,
    digest: &[u8],
) -> Result<Vec<u8>, ConnectorOperationGrantError> {
    if key_id.is_empty() || digest.len() != 32 {
        return Err(ConnectorOperationGrantError::Invalid);
    }
    let key_id_length =
        u32::try_from(key_id.len()).map_err(|_| ConnectorOperationGrantError::Invalid)?;
    let mut message = Vec::with_capacity(SIGNATURE_DOMAIN.len() + 4 + key_id.len() + digest.len());
    message.extend_from_slice(SIGNATURE_DOMAIN);
    message.extend_from_slice(&key_id_length.to_be_bytes());
    message.extend_from_slice(key_id.as_bytes());
    message.extend_from_slice(digest);
    Ok(message)
}

pub(crate) fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn timestamp_to_unix_ms(
    timestamp: Option<&prost_types::Timestamp>,
) -> Result<u64, ConnectorOperationGrantError> {
    let timestamp = timestamp.ok_or(ConnectorOperationGrantError::Invalid)?;
    if timestamp.seconds < 0 || !(0..1_000_000_000).contains(&timestamp.nanos) {
        return Err(ConnectorOperationGrantError::Invalid);
    }
    u64::try_from(timestamp.seconds)
        .ok()
        .and_then(|seconds| seconds.checked_mul(1_000))
        .and_then(|millis| millis.checked_add(u64::try_from(timestamp.nanos).ok()? / 1_000_000))
        .ok_or(ConnectorOperationGrantError::Invalid)
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, sync::Mutex};

    use async_trait::async_trait;
    use cy_proto::cyrene::workspace::authority::v2::AuthorizeConnectorOperationRequest;
    use cy_proto::workspace_v1::{UserIdentityRef, WorkspaceConnectionDescriptor};
    use ed25519_dalek::{Signer, SigningKey};
    use tonic::Code;

    use super::*;
    use crate::{
        authority::service::{
            issue_connector_operation_grant, require_directory_connector_operation_role,
            AuthorityCredentialSigner, AuthorityManagedWorkload,
        },
        directory::{WorkspaceDirectory, WorkspaceDirectoryError},
    };

    const KEY_ID: &str = "authority-test-key";
    const NOW_MS: u64 = 1_800_000_000_000;

    #[derive(Clone, Copy)]
    struct TestConnectorScope<'a> {
        connection_ref: &'a str,
        provider_id: &'a str,
        profile_ref: &'a str,
        operation_id: &'a str,
        resource_ref: &'a str,
        request_id: &'a str,
    }

    fn expected<'a>(digest: &'a [u8]) -> ConnectorOperationGrantExpectation<'a> {
        ConnectorOperationGrantExpectation {
            audience: "exchange",
            organization_id: "org-1",
            workspace_id: "workspace-1",
            principal_issuer:
                "https://login.microsoftonline.com/11111111-1111-4111-8111-111111111111/v2.0",
            principal_subject: "11111111-1111-4111-8111-111111111111",
            connection_ref: "connection-1",
            provider_id: "github",
            profile_ref: "github-main",
            operation_id: "github.repository.list",
            resource_ref: "repositories",
            arguments_digest_sha256: digest,
            request_id: "activity-1",
        }
    }

    fn request_for(provider_id: &str, operation_id: &str) -> AuthorizeConnectorOperationRequest {
        AuthorizeConnectorOperationRequest {
            workspace_id: "workspace-1".to_owned(),
            principal_issuer:
                "https://login.microsoftonline.com/11111111-1111-4111-8111-111111111111/v2.0"
                    .to_owned(),
            principal_subject: "11111111-1111-4111-8111-111111111111".to_owned(),
            connection_ref: format!("{provider_id}-connection"),
            provider_id: provider_id.to_owned(),
            profile_ref: format!("{provider_id}-profile"),
            operation_id: operation_id.to_owned(),
            resource_ref: "source:target-1".to_owned(),
            arguments_digest_sha256: vec![0x27; 32],
            request_id: format!("request-{operation_id}"),
        }
    }

    fn viewer_request(operation_id: &str) -> AuthorizeConnectorOperationRequest {
        request_for("microsoft_graph", operation_id)
    }

    fn signed_grant(key: &SigningKey, now_ms: u64, digest: Vec<u8>) -> ConnectorOperationGrantV1 {
        signed_grant_for(
            key,
            now_ms,
            digest,
            TestConnectorScope {
                connection_ref: "connection-1",
                provider_id: "github",
                profile_ref: "github-main",
                operation_id: "github.repository.list",
                resource_ref: "repositories",
                request_id: "activity-1",
            },
        )
    }

    fn signed_grant_for(
        key: &SigningKey,
        now_ms: u64,
        digest: Vec<u8>,
        scope: TestConnectorScope<'_>,
    ) -> ConnectorOperationGrantV1 {
        let mut grant = ConnectorOperationGrantV1 {
            schema_version: CONNECTOR_OPERATION_GRANT_SCHEMA_VERSION.to_owned(),
            grant_ref: String::new(),
            audience: "exchange".to_owned(),
            organization_id: "org-1".to_owned(),
            workspace_id: "workspace-1".to_owned(),
            principal_issuer:
                "https://login.microsoftonline.com/11111111-1111-4111-8111-111111111111/v2.0"
                    .to_owned(),
            principal_subject: "11111111-1111-4111-8111-111111111111".to_owned(),
            connection_ref: scope.connection_ref.to_owned(),
            provider_id: scope.provider_id.to_owned(),
            profile_ref: scope.profile_ref.to_owned(),
            operation_id: scope.operation_id.to_owned(),
            resource_ref: scope.resource_ref.to_owned(),
            arguments_digest_sha256: digest,
            request_id: scope.request_id.to_owned(),
            issued_at: Some(timestamp_for_test(now_ms)),
            expires_at: Some(timestamp_for_test(
                now_ms + CONNECTOR_OPERATION_GRANT_TTL_MS,
            )),
            signing_key_id: KEY_ID.to_owned(),
            authority_signature: Vec::new(),
        };
        let digest = grant_digest(&grant);
        grant.grant_ref = format!("sha256:{}", encode_hex(&digest));
        let message = signature_message(KEY_ID, &digest).expect("signature message is valid");
        grant.authority_signature = key.sign(&message).to_bytes().to_vec();
        grant
    }

    fn timestamp_for_test(unix_ms: u64) -> prost_types::Timestamp {
        prost_types::Timestamp {
            seconds: (unix_ms / 1_000) as i64,
            nanos: ((unix_ms % 1_000) * 1_000_000) as i32,
        }
    }

    struct MutableRoleDirectory {
        identity: UserIdentityRef,
        organization_id: String,
        workspace_id: String,
        roles: Mutex<BTreeSet<String>>,
    }

    #[async_trait]
    impl WorkspaceDirectory for MutableRoleDirectory {
        async fn discover(
            &self,
            _user: &UserIdentityRef,
            _organization_id: &str,
            _now_unix_ms: u64,
        ) -> Result<Vec<WorkspaceConnectionDescriptor>, WorkspaceDirectoryError> {
            Ok(Vec::new())
        }

        async fn is_member(
            &self,
            user: &UserIdentityRef,
            organization_id: &str,
            workspace_id: &str,
        ) -> Result<bool, WorkspaceDirectoryError> {
            Ok(user == &self.identity
                && organization_id == self.organization_id
                && workspace_id == self.workspace_id)
        }

        async fn roles_for_member(
            &self,
            user: &UserIdentityRef,
            organization_id: &str,
            workspace_id: &str,
        ) -> Result<Option<BTreeSet<String>>, WorkspaceDirectoryError> {
            if !self.is_member(user, organization_id, workspace_id).await? {
                return Ok(None);
            }
            Ok(Some(
                self.roles.lock().expect("roles lock is healthy").clone(),
            ))
        }
    }

    #[tokio::test]
    async fn directory_operator_revocation_is_visible_on_the_next_lookup() {
        let identity = UserIdentityRef {
            issuer: "https://login.microsoftonline.com/11111111-1111-4111-8111-111111111111/v2.0"
                .to_owned(),
            subject: "11111111-1111-4111-8111-111111111111".to_owned(),
        };
        let directory = MutableRoleDirectory {
            identity: identity.clone(),
            organization_id: "org-1".to_owned(),
            workspace_id: "workspace-1".to_owned(),
            roles: Mutex::new(BTreeSet::from([
                NAVIGATOR_CONNECTOR_OPERATOR_ROLE.to_owned()
            ])),
        };
        assert!(require_directory_connector_operation_role(
            &directory,
            &identity,
            "org-1",
            "workspace-1",
            "github",
            "github.repository.list",
        )
        .await
        .is_ok());

        directory
            .roles
            .lock()
            .expect("roles lock is healthy")
            .remove(NAVIGATOR_CONNECTOR_OPERATOR_ROLE);
        assert_eq!(
            require_directory_connector_operation_role(
                &directory,
                &identity,
                "org-1",
                "workspace-1",
                "github",
                "github.repository.list",
            )
            .await
            .expect_err("revoked role must deny the next call")
            .code(),
            Code::PermissionDenied
        );
        assert_eq!(
            require_directory_connector_operation_role(
                &directory,
                &identity,
                "org-1",
                "another-workspace",
                "github",
                "github.repository.list",
            )
            .await
            .expect_err("another workspace must remain denied")
            .code(),
            Code::PermissionDenied
        );
    }

    #[tokio::test]
    async fn directory_operator_role_issues_signed_existing_and_new_provider_grants() {
        let identity = UserIdentityRef {
            issuer: "https://login.microsoftonline.com/11111111-1111-4111-8111-111111111111/v2.0"
                .to_owned(),
            subject: "11111111-1111-4111-8111-111111111111".to_owned(),
        };
        let directory = MutableRoleDirectory {
            identity: identity.clone(),
            organization_id: "org-1".to_owned(),
            workspace_id: "workspace-1".to_owned(),
            roles: Mutex::new(BTreeSet::from([
                NAVIGATOR_CONNECTOR_OPERATOR_ROLE.to_owned()
            ])),
        };
        let signer = AuthorityCredentialSigner::new(
            KEY_ID,
            SigningKey::from_bytes(&[5; 32]),
            std::time::Duration::from_secs(30),
        )
        .expect("Authority signing key is valid");
        let workload = AuthorityManagedWorkload {
            workload_id: "exchange-test".into(),
            audience: "exchange".into(),
            certificate_fingerprints_sha256: Vec::new(),
        };
        let mut requests = vec![
            AuthorizeConnectorOperationRequest {
                workspace_id: "workspace-1".into(),
                principal_issuer: identity.issuer.clone(),
                principal_subject: identity.subject.clone(),
                connection_ref: "github-connection".into(),
                provider_id: "github".into(),
                profile_ref: "github-main".into(),
                operation_id: "github.actions.rerun_failed".into(),
                resource_ref: "repository:123/run:45".into(),
                arguments_digest_sha256: vec![7; 32],
                request_id: "github-rerun-request".into(),
            },
            AuthorizeConnectorOperationRequest {
                workspace_id: "workspace-1".into(),
                principal_issuer: identity.issuer.clone(),
                principal_subject: identity.subject.clone(),
                connection_ref: "computer-connection".into(),
                provider_id: "computer".into(),
                profile_ref: "computer-workspace".into(),
                operation_id: "computer.command.execute".into(),
                resource_ref: "workspace:workspace-1".into(),
                arguments_digest_sha256: vec![8; 32],
                request_id: "computer-command-request".into(),
            },
        ];
        requests.extend(
            SUPPORTED_CONNECTOR_OPERATION_IDS
                .iter()
                .filter(|operation_id| {
                    operation_id.starts_with("microsoft_graph.")
                        || **operation_id == "azure_devops.observations.poll"
                })
                .map(|operation_id| {
                    let provider_id = operation_id
                        .split_once('.')
                        .expect("connector operation ID is namespaced")
                        .0;
                    request_for(provider_id, operation_id)
                }),
        );

        for request in &requests {
            let grant = issue_connector_operation_grant(
                &directory,
                &signer,
                "org-1",
                "workspace-1",
                &identity.issuer,
                &workload,
                request,
            )
            .await
            .expect("current Directory connector operator can receive the exact grant");
            let now_unix_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock is after Unix epoch")
                .as_millis() as u64;
            verify_connector_operation_grant(
                &grant,
                &signer.verifying_key(),
                KEY_ID,
                ConnectorOperationGrantExpectation {
                    audience: "exchange",
                    organization_id: "org-1",
                    workspace_id: "workspace-1",
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
            .expect("signature and exact provider/operation scope verify");
        }

        directory
            .roles
            .lock()
            .expect("roles lock is healthy")
            .remove(NAVIGATOR_CONNECTOR_OPERATOR_ROLE);
        assert_eq!(
            issue_connector_operation_grant(
                &directory,
                &signer,
                "org-1",
                "workspace-1",
                &identity.issuer,
                &workload,
                &requests[1],
            )
            .await
            .expect_err("revoked Directory role cannot receive another operation grant")
            .code(),
            Code::PermissionDenied
        );
    }

    #[test]
    fn grant_verification_binds_identity_workspace_profile_resource_and_arguments() {
        let key = SigningKey::from_bytes(&[9; 32]);
        let arguments_digest = [7; 32];
        let grant = signed_grant(&key, NOW_MS, arguments_digest.to_vec());
        assert_eq!(
            verify_connector_operation_grant(
                &grant,
                &key.verifying_key(),
                KEY_ID,
                expected(&arguments_digest),
                NOW_MS + 1,
            ),
            Ok(())
        );

        let mut altered = grant.clone();
        altered.profile_ref = "other-profile".to_owned();
        assert_eq!(
            verify_connector_operation_grant(
                &altered,
                &key.verifying_key(),
                KEY_ID,
                expected(&arguments_digest),
                NOW_MS + 1,
            ),
            Err(ConnectorOperationGrantError::ScopeMismatch)
        );

        let mut wrong_resource = expected(&arguments_digest);
        wrong_resource.resource_ref = "another-repository";
        assert_eq!(
            verify_connector_operation_grant(
                &grant,
                &key.verifying_key(),
                KEY_ID,
                wrong_resource,
                NOW_MS + 1,
            ),
            Err(ConnectorOperationGrantError::ScopeMismatch)
        );

        let mut wrong_workspace = expected(&arguments_digest);
        wrong_workspace.workspace_id = "workspace-2";
        assert_eq!(
            verify_connector_operation_grant(
                &grant,
                &key.verifying_key(),
                KEY_ID,
                wrong_workspace,
                NOW_MS + 1,
            ),
            Err(ConnectorOperationGrantError::ScopeMismatch)
        );

        let mut wrong_actor = expected(&arguments_digest);
        wrong_actor.principal_subject = "00000000-0000-4000-8000-000000000001";
        assert_eq!(
            verify_connector_operation_grant(
                &grant,
                &key.verifying_key(),
                KEY_ID,
                wrong_actor,
                NOW_MS + 1,
            ),
            Err(ConnectorOperationGrantError::ScopeMismatch)
        );

        let mut altered = grant.clone();
        altered.arguments_digest_sha256[0] ^= 1;
        assert_eq!(
            verify_connector_operation_grant(
                &altered,
                &key.verifying_key(),
                KEY_ID,
                expected(&arguments_digest),
                NOW_MS + 1,
            ),
            Err(ConnectorOperationGrantError::ScopeMismatch)
        );
    }

    #[test]
    fn grant_expiry_and_signature_are_checked_and_catalog_rejects_shell() {
        let key = SigningKey::from_bytes(&[9; 32]);
        let arguments_digest = [7; 32];
        let grant = signed_grant(&key, NOW_MS, arguments_digest.to_vec());
        assert_eq!(
            verify_connector_operation_grant(
                &grant,
                &key.verifying_key(),
                KEY_ID,
                expected(&arguments_digest),
                NOW_MS + CONNECTOR_OPERATION_GRANT_TTL_MS,
            ),
            Err(ConnectorOperationGrantError::Expired)
        );

        assert_eq!(
            verify_connector_operation_grant(
                &grant,
                &key.verifying_key(),
                KEY_ID,
                expected(&arguments_digest),
                NOW_MS.saturating_sub(2_000),
            ),
            Err(ConnectorOperationGrantError::Expired)
        );

        let other_key = SigningKey::from_bytes(&[8; 32]);
        assert_eq!(
            verify_connector_operation_grant(
                &grant,
                &other_key.verifying_key(),
                KEY_ID,
                expected(&arguments_digest),
                NOW_MS + 1,
            ),
            Err(ConnectorOperationGrantError::SignatureInvalid)
        );
        assert!(!is_supported_connector_operation("github", "shell.execute"));
        assert!(!is_supported_connector_operation(
            "azure",
            "github.repository.list"
        ));
        assert!(is_supported_connector_operation(
            "runpod",
            "cloud.resource.restart"
        ));
        assert!(!is_supported_connector_operation(
            "azure",
            "cloud.resource.restart"
        ));
        assert!(is_supported_connector_operation(
            "msteams",
            "msteams.message.reply"
        ));
    }

    #[test]
    fn connector_policy_matches_rpc_and_operation_catalog() {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Policy {
            grant: Grant,
            operator_role: String,
            operations: Vec<Operation>,
            read_only_role: String,
            read_only_operations: Vec<Operation>,
            task_writer_role: String,
            task_writer_operations: Vec<Operation>,
            automation_transport_operations: Vec<Operation>,
        }
        #[derive(serde::Deserialize)]
        struct Grant {
            rpc: String,
        }
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Operation {
            operation_id: String,
            provider_ids: Vec<String>,
        }
        let policy: Policy = serde_json::from_str(include_str!(
            "../../../../../contracts/policies/workspace-connector-operator-policy-v1.json"
        ))
        .expect("connector operator policy is valid JSON");
        assert_eq!(
            policy.grant.rpc,
            "cyrene.workspace.authority.v2.WorkspaceConnectorAuthorizationService/AuthorizeConnectorOperation"
        );
        assert_eq!(policy.operator_role, NAVIGATOR_CONNECTOR_OPERATOR_ROLE);
        assert_eq!(policy.read_only_role, NAVIGATOR_AUTOMATION_VIEWER_ROLE);
        assert_eq!(policy.task_writer_role, NAVIGATOR_OFFICE_TASK_WRITER_ROLE);
        let policy_ids = policy
            .operations
            .iter()
            .map(|entry| entry.operation_id.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(policy_ids.len(), policy.operations.len());
        let code_ids = SUPPORTED_CONNECTOR_OPERATION_IDS
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(policy_ids, code_ids);
        let catalog_pairs = policy
            .operations
            .iter()
            .flat_map(|operation| {
                operation
                    .provider_ids
                    .iter()
                    .map(|provider_id| (operation.operation_id.clone(), provider_id.clone()))
            })
            .collect::<std::collections::BTreeSet<_>>();
        for operation in policy.operations {
            for provider_id in operation.provider_ids {
                assert!(is_supported_connector_operation(
                    &provider_id,
                    &operation.operation_id
                ));
            }
        }
        let viewer_ids = policy
            .read_only_operations
            .iter()
            .map(|entry| entry.operation_id.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(viewer_ids.len(), policy.read_only_operations.len());
        assert_eq!(
            viewer_ids,
            AUTOMATION_VIEWER_CONNECTOR_OPERATION_IDS
                .into_iter()
                .collect()
        );
        for operation in policy.read_only_operations {
            for provider_id in operation.provider_ids {
                assert!(is_automation_viewer_connector_operation(
                    &provider_id,
                    &operation.operation_id
                ));
                assert!(
                    catalog_pairs.contains(&(operation.operation_id.clone(), provider_id.clone()))
                );
            }
        }
        let task_writer_ids = policy
            .task_writer_operations
            .iter()
            .map(|entry| entry.operation_id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            task_writer_ids,
            OFFICE_TASK_WRITER_CONNECTOR_OPERATION_IDS
                .into_iter()
                .map(str::to_owned)
                .collect()
        );
        assert_eq!(task_writer_ids.len(), policy.task_writer_operations.len());
        for operation in policy.task_writer_operations {
            for provider_id in operation.provider_ids {
                assert!(is_office_task_writer_connector_operation(
                    &provider_id,
                    &operation.operation_id
                ));
                assert!(
                    catalog_pairs.contains(&(operation.operation_id.clone(), provider_id.clone()))
                );
            }
        }
        let transport_ids = policy
            .automation_transport_operations
            .iter()
            .map(|entry| entry.operation_id.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            transport_ids,
            AUTOMATION_TRANSPORT_CONNECTOR_OPERATION_IDS
                .into_iter()
                .collect()
        );
        for operation in policy.automation_transport_operations {
            for provider_id in operation.provider_ids {
                assert!(is_automation_transport_connector_operation(
                    &provider_id,
                    &operation.operation_id
                ));
                assert!(
                    catalog_pairs.contains(&(operation.operation_id.clone(), provider_id.clone()))
                );
            }
        }
    }

    #[test]
    fn office_operations_are_closed_and_have_exact_provider_pairs() {
        for operation_id in OFFICE_TASK_WRITER_CONNECTOR_OPERATION_IDS
            .into_iter()
            .chain(OFFICE_CALENDAR_OPERATOR_ONLY_CONNECTOR_OPERATION_IDS)
            .chain(OFFICE_MAIL_OPERATOR_ONLY_CONNECTOR_OPERATION_IDS)
        {
            assert!(is_supported_connector_operation(
                "microsoft_graph",
                operation_id
            ));
            assert!(!is_supported_connector_operation(
                "azure_devops",
                operation_id
            ));
        }
        for operation_id in OFFICE_CALENDAR_OPERATOR_ONLY_CONNECTOR_OPERATION_IDS
            .into_iter()
            .chain(OFFICE_MAIL_OPERATOR_ONLY_CONNECTOR_OPERATION_IDS)
        {
            assert!(!is_automation_viewer_connector_operation(
                "microsoft_graph",
                operation_id
            ));
            assert!(!is_office_task_writer_connector_operation(
                "microsoft_graph",
                operation_id
            ));
        }
        assert!(is_supported_connector_operation(
            "azure_devops",
            "azure_devops.observations.poll"
        ));
        assert!(!is_supported_connector_operation(
            "microsoft_graph",
            "azure_devops.observations.poll"
        ));
        assert!(!is_supported_connector_operation(
            "microsoft_graph",
            "microsoft_graph.unknown.read"
        ));
        assert!(!is_supported_connector_operation(
            "azure_devops",
            "azure_devops.pipeline_run.delete"
        ));
    }

    #[tokio::test]
    async fn automation_viewer_role_is_limited_to_six_graph_reads() {
        let identity = UserIdentityRef {
            issuer: "https://login.microsoftonline.com/11111111-1111-4111-8111-111111111111/v2.0"
                .to_owned(),
            subject: "11111111-1111-4111-8111-111111111111".to_owned(),
        };
        let directory = MutableRoleDirectory {
            identity: identity.clone(),
            organization_id: "org-1".to_owned(),
            workspace_id: "workspace-1".to_owned(),
            roles: Mutex::new(BTreeSet::from(
                [NAVIGATOR_AUTOMATION_VIEWER_ROLE.to_owned()],
            )),
        };
        let signer = AuthorityCredentialSigner::new(
            KEY_ID,
            SigningKey::from_bytes(&[4; 32]),
            std::time::Duration::from_secs(30),
        )
        .expect("Authority signing key is valid");
        let workload = AuthorityManagedWorkload {
            workload_id: "exchange-test".into(),
            audience: "exchange".into(),
            certificate_fingerprints_sha256: Vec::new(),
        };

        for operation_id in AUTOMATION_VIEWER_CONNECTOR_OPERATION_IDS {
            let request = viewer_request(operation_id);
            let grant = issue_connector_operation_grant(
                &directory,
                &signer,
                "org-1",
                "workspace-1",
                &identity.issuer,
                &workload,
                &request,
            )
            .await
            .expect("viewer role can receive only its exact Graph read grant");
            let now_unix_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock is after Unix epoch")
                .as_millis() as u64;
            let exact_scope = ConnectorOperationGrantExpectation {
                audience: "exchange",
                organization_id: "org-1",
                workspace_id: &request.workspace_id,
                principal_issuer: &request.principal_issuer,
                principal_subject: &request.principal_subject,
                connection_ref: &request.connection_ref,
                provider_id: &request.provider_id,
                profile_ref: &request.profile_ref,
                operation_id: &request.operation_id,
                resource_ref: &request.resource_ref,
                arguments_digest_sha256: &request.arguments_digest_sha256,
                request_id: &request.request_id,
            };
            verify_connector_operation_grant(
                &grant,
                &signer.verifying_key(),
                KEY_ID,
                exact_scope,
                now_unix_ms,
            )
            .expect("signature keeps the actor, target, and canonical arguments bound");

            let changed_arguments = [0x28; 32];
            let mismatched_scopes = [
                ConnectorOperationGrantExpectation {
                    principal_subject: "00000000-0000-4000-8000-000000000001",
                    ..exact_scope
                },
                ConnectorOperationGrantExpectation {
                    workspace_id: "workspace-2",
                    ..exact_scope
                },
                ConnectorOperationGrantExpectation {
                    resource_ref: "source:other-target",
                    ..exact_scope
                },
                ConnectorOperationGrantExpectation {
                    arguments_digest_sha256: &changed_arguments,
                    ..exact_scope
                },
                ConnectorOperationGrantExpectation {
                    profile_ref: "different-profile",
                    ..exact_scope
                },
            ];
            for mismatched_scope in mismatched_scopes {
                assert_eq!(
                    verify_connector_operation_grant(
                        &grant,
                        &signer.verifying_key(),
                        KEY_ID,
                        mismatched_scope,
                        now_unix_ms,
                    ),
                    Err(ConnectorOperationGrantError::ScopeMismatch)
                );
            }
        }

        for operation_id in OFFICE_TASK_WRITER_CONNECTOR_OPERATION_IDS
            .into_iter()
            .chain(OFFICE_CALENDAR_OPERATOR_ONLY_CONNECTOR_OPERATION_IDS)
            .chain(OFFICE_MAIL_OPERATOR_ONLY_CONNECTOR_OPERATION_IDS)
        {
            if AUTOMATION_VIEWER_CONNECTOR_OPERATION_IDS.contains(&operation_id) {
                continue;
            }
            let request = request_for("microsoft_graph", operation_id);
            assert_eq!(
                issue_connector_operation_grant(
                    &directory,
                    &signer,
                    "org-1",
                    "workspace-1",
                    &identity.issuer,
                    &workload,
                    &request,
                )
                .await
                .expect_err("viewer cannot receive an Office operation grant")
                .code(),
                Code::PermissionDenied,
                "viewer must not receive {operation_id}"
            );
        }

        for (provider_id, operation_id, expected_code) in [
            (
                "microsoft_graph",
                "microsoft_graph.planner.task.create",
                Code::PermissionDenied,
            ),
            (
                "microsoft_graph",
                "microsoft_graph.planner.task.update",
                Code::PermissionDenied,
            ),
            (
                "azure_devops",
                "azure_devops.observations.poll",
                Code::PermissionDenied,
            ),
            (
                "azure",
                "cloud.provider.health.read",
                Code::PermissionDenied,
            ),
            ("gcp", "cloud.resources.list", Code::PermissionDenied),
            ("github", "github.repository.list", Code::PermissionDenied),
            (
                "github",
                "github.vendor.cli.execute",
                Code::PermissionDenied,
            ),
            (
                "computer",
                "computer.directory.list",
                Code::PermissionDenied,
            ),
            (
                "microsoft_graph",
                "microsoft_graph.unknown.read",
                Code::InvalidArgument,
            ),
            (
                "microsoft_graph",
                "microsoft_graph.credentials.export",
                Code::InvalidArgument,
            ),
            (
                "microsoft_graph",
                "github.vendor.cli.execute",
                Code::InvalidArgument,
            ),
        ] {
            let request = request_for(provider_id, operation_id);
            assert_eq!(
                issue_connector_operation_grant(
                    &directory,
                    &signer,
                    "org-1",
                    "workspace-1",
                    &identity.issuer,
                    &workload,
                    &request,
                )
                .await
                .expect_err("viewer role must deny operations outside its exact read list")
                .code(),
                expected_code
            );
        }
    }

    #[tokio::test]
    async fn office_task_writer_role_can_grant_only_non_mail_office_graph_operations() {
        let identity = UserIdentityRef {
            issuer: "https://login.microsoftonline.com/11111111-1111-4111-8111-111111111111/v2.0"
                .to_owned(),
            subject: "11111111-1111-4111-8111-111111111111".to_owned(),
        };
        let directory = MutableRoleDirectory {
            identity: identity.clone(),
            organization_id: "org-1".to_owned(),
            workspace_id: "workspace-1".to_owned(),
            roles: Mutex::new(BTreeSet::from([
                NAVIGATOR_OFFICE_TASK_WRITER_ROLE.to_owned()
            ])),
        };
        let signer = AuthorityCredentialSigner::new(
            KEY_ID,
            SigningKey::from_bytes(&[12; 32]),
            std::time::Duration::from_secs(30),
        )
        .expect("Authority signing key is valid");
        let workload = AuthorityManagedWorkload {
            workload_id: "exchange-test".into(),
            audience: "exchange".into(),
            certificate_fingerprints_sha256: Vec::new(),
        };

        for operation_id in OFFICE_TASK_WRITER_CONNECTOR_OPERATION_IDS {
            let request = request_for("microsoft_graph", operation_id);
            let grant = issue_connector_operation_grant(
                &directory,
                &signer,
                "org-1",
                "workspace-1",
                &identity.issuer,
                &workload,
                &request,
            )
            .await
            .expect("task writer role can grant the exact non-mail Office operation set");
            let now_unix_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock is after Unix epoch")
                .as_millis() as u64;
            verify_connector_operation_grant(
                &grant,
                &signer.verifying_key(),
                KEY_ID,
                ConnectorOperationGrantExpectation {
                    audience: "exchange",
                    organization_id: "org-1",
                    workspace_id: &request.workspace_id,
                    principal_issuer: &request.principal_issuer,
                    principal_subject: &request.principal_subject,
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
            .expect("task writer grant verifies against the exact operation scope");
        }

        for operation_id in OFFICE_CALENDAR_OPERATOR_ONLY_CONNECTOR_OPERATION_IDS
            .into_iter()
            .chain(OFFICE_MAIL_OPERATOR_ONLY_CONNECTOR_OPERATION_IDS)
        {
            let request = request_for("microsoft_graph", operation_id);
            assert_eq!(
                issue_connector_operation_grant(
                    &directory,
                    &signer,
                    "org-1",
                    "workspace-1",
                    &identity.issuer,
                    &workload,
                    &request,
                )
                .await
                .expect_err("Calendar grants remain limited to connector operators")
                .code(),
                Code::PermissionDenied,
                "task writer must not receive {operation_id}"
            );
        }

        for (provider_id, operation_id, expected_code) in [
            (
                "azure_devops",
                "azure_devops.observations.poll",
                Code::PermissionDenied,
            ),
            (
                "azure",
                "cloud.provider.health.read",
                Code::PermissionDenied,
            ),
            ("gcp", "cloud.resources.list", Code::PermissionDenied),
            ("github", "github.repository.list", Code::PermissionDenied),
            (
                "github",
                "github.vendor.cli.execute",
                Code::PermissionDenied,
            ),
            (
                "computer",
                "computer.directory.list",
                Code::PermissionDenied,
            ),
            (
                "microsoft_graph",
                "microsoft_graph.credentials.export",
                Code::InvalidArgument,
            ),
        ] {
            let request = request_for(provider_id, operation_id);
            assert_eq!(
                issue_connector_operation_grant(
                    &directory,
                    &signer,
                    "org-1",
                    "workspace-1",
                    &identity.issuer,
                    &workload,
                    &request,
                )
                .await
                .expect_err("task writer role must not extend beyond the Graph catalog")
                .code(),
                expected_code
            );
        }
    }

    #[tokio::test]
    async fn automation_roles_can_only_receive_exact_actor_bound_teams_replies() {
        let identity = UserIdentityRef {
            issuer: "https://login.microsoftonline.com/11111111-1111-4111-8111-111111111111/v2.0"
                .to_owned(),
            subject: "11111111-1111-4111-8111-111111111111".to_owned(),
        };
        let signer = AuthorityCredentialSigner::new(
            KEY_ID,
            SigningKey::from_bytes(&[14; 32]),
            std::time::Duration::from_secs(30),
        )
        .expect("Authority signing key is valid");
        let workload = AuthorityManagedWorkload {
            workload_id: "exchange-test".into(),
            audience: "exchange".into(),
            certificate_fingerprints_sha256: Vec::new(),
        };

        for role in [
            NAVIGATOR_AUTOMATION_VIEWER_ROLE,
            NAVIGATOR_OFFICE_TASK_WRITER_ROLE,
        ] {
            let directory = MutableRoleDirectory {
                identity: identity.clone(),
                organization_id: "org-1".to_owned(),
                workspace_id: "workspace-1".to_owned(),
                roles: Mutex::new(BTreeSet::from([role.to_owned()])),
            };
            let request = request_for("msteams", "msteams.message.reply");
            let grant = issue_connector_operation_grant(
                &directory,
                &signer,
                "org-1",
                "workspace-1",
                &identity.issuer,
                &workload,
                &request,
            )
            .await
            .expect("both automation roles can receive the existing Teams reply operation");

            let now_unix_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock is after Unix epoch")
                .as_millis() as u64;
            let exact_scope = ConnectorOperationGrantExpectation {
                audience: "exchange",
                organization_id: "org-1",
                workspace_id: &request.workspace_id,
                principal_issuer: &request.principal_issuer,
                principal_subject: &request.principal_subject,
                connection_ref: &request.connection_ref,
                provider_id: &request.provider_id,
                profile_ref: &request.profile_ref,
                operation_id: &request.operation_id,
                resource_ref: &request.resource_ref,
                arguments_digest_sha256: &request.arguments_digest_sha256,
                request_id: &request.request_id,
            };
            verify_connector_operation_grant(
                &grant,
                &signer.verifying_key(),
                KEY_ID,
                exact_scope,
                now_unix_ms,
            )
            .expect("Teams reply grant is bound to the exact original actor and target");

            let changed_arguments = [0x72; 32];
            for changed_scope in [
                ConnectorOperationGrantExpectation {
                    principal_subject: "00000000-0000-4000-8000-000000000002",
                    ..exact_scope
                },
                ConnectorOperationGrantExpectation {
                    provider_id: "microsoft_graph",
                    ..exact_scope
                },
                ConnectorOperationGrantExpectation {
                    operation_id: "microsoft_graph.planner.tasks.list",
                    ..exact_scope
                },
                ConnectorOperationGrantExpectation {
                    resource_ref: "source:another-target",
                    ..exact_scope
                },
                ConnectorOperationGrantExpectation {
                    arguments_digest_sha256: &changed_arguments,
                    ..exact_scope
                },
            ] {
                assert_eq!(
                    verify_connector_operation_grant(
                        &grant,
                        &signer.verifying_key(),
                        KEY_ID,
                        changed_scope,
                        now_unix_ms,
                    ),
                    Err(ConnectorOperationGrantError::ScopeMismatch)
                );
            }

            directory
                .roles
                .lock()
                .expect("roles lock is healthy")
                .remove(role);
            assert_eq!(
                issue_connector_operation_grant(
                    &directory,
                    &signer,
                    "org-1",
                    "workspace-1",
                    &identity.issuer,
                    &workload,
                    &request,
                )
                .await
                .expect_err("the next lookup must see role revocation")
                .code(),
                Code::PermissionDenied
            );
        }

        assert!(is_automation_transport_connector_operation(
            "msteams",
            "msteams.message.reply"
        ));
        assert!(!is_automation_transport_connector_operation(
            "microsoft_graph",
            "msteams.message.reply"
        ));
        assert!(!is_automation_transport_connector_operation(
            "msteams",
            "microsoft_graph.planner.tasks.list"
        ));
        assert!(!is_automation_transport_connector_operation(
            "msteams",
            "msteams.message.send"
        ));

        let directory = MutableRoleDirectory {
            identity: identity.clone(),
            organization_id: "org-1".to_owned(),
            workspace_id: "workspace-1".to_owned(),
            roles: Mutex::new(BTreeSet::from(
                [NAVIGATOR_AUTOMATION_VIEWER_ROLE.to_owned()],
            )),
        };
        for invalid_request in [
            request_for("microsoft_graph", "msteams.message.reply"),
            request_for("msteams", "msteams.message.send"),
        ] {
            assert_eq!(
                issue_connector_operation_grant(
                    &directory,
                    &signer,
                    "org-1",
                    "workspace-1",
                    &identity.issuer,
                    &workload,
                    &invalid_request,
                )
                .await
                .expect_err("wrong provider or generic send is not a supported action")
                .code(),
                Code::InvalidArgument
            );
        }
    }

    #[test]
    fn provider_cli_operations_are_bound_to_their_provider() {
        assert!(is_supported_connector_operation(
            "github",
            "github.vendor.cli.execute"
        ));
        assert!(!is_supported_connector_operation(
            "huggingface",
            "github.vendor.cli.execute"
        ));
        assert!(is_supported_connector_operation(
            "huggingface",
            "huggingface.vendor.cli.execute"
        ));
        assert!(!is_supported_connector_operation(
            "github",
            "huggingface.vendor.cli.execute"
        ));
    }

    #[test]
    fn new_provider_grants_verify_with_exact_signed_scope() {
        let key = SigningKey::from_bytes(&[3; 32]);
        let digest = [0x31; 32];
        let scopes = [
            TestConnectorScope {
                connection_ref: "graph-connection",
                provider_id: "microsoft_graph",
                profile_ref: "graph-profile",
                operation_id: "microsoft_graph.planner.plan.get",
                resource_ref: "planner:plan-7",
                request_id: "graph-read-1",
            },
            TestConnectorScope {
                connection_ref: "graph-connection",
                provider_id: "microsoft_graph",
                profile_ref: "graph-profile",
                operation_id: "microsoft_graph.planner.task.create",
                resource_ref: "planner:plan-7/task:new",
                request_id: "graph-write-1",
            },
            TestConnectorScope {
                connection_ref: "ado-connection",
                provider_id: "azure_devops",
                profile_ref: "ado-profile",
                operation_id: "azure_devops.observations.poll",
                resource_ref: "azure_devops:project-7/pipeline-2",
                request_id: "ado-poll-1",
            },
        ];

        for scope in scopes {
            let grant = signed_grant_for(&key, NOW_MS, digest.to_vec(), scope);
            let exact_scope = ConnectorOperationGrantExpectation {
                connection_ref: scope.connection_ref,
                provider_id: scope.provider_id,
                profile_ref: scope.profile_ref,
                operation_id: scope.operation_id,
                resource_ref: scope.resource_ref,
                request_id: scope.request_id,
                ..expected(&digest)
            };
            verify_connector_operation_grant(
                &grant,
                &key.verifying_key(),
                KEY_ID,
                exact_scope,
                NOW_MS + 1,
            )
            .expect("new provider grant verifies only for the exact signed scope");
        }
    }

    #[test]
    fn expanded_github_and_computer_operations_remain_provider_bound() {
        for operation_id in [
            "github.actions.job.logs.read",
            "github.actions.jobs.list",
            "github.actions.rerun_failed",
            "github.branch.create",
            "github.branch.delete",
            "github.branch.get",
            "github.file.commit",
            "github.file.get",
            "github.issues.search",
            "github.pull_request.draft.create",
            "github.pull_request.draft.find",
        ] {
            assert!(is_supported_connector_operation("github", operation_id));
            assert!(!is_supported_connector_operation("computer", operation_id));
        }
        for operation_id in [
            "computer.command.execute",
            "computer.directory.list",
            "computer.file.read",
            "computer.file.write",
        ] {
            assert!(is_supported_connector_operation("computer", operation_id));
            assert!(!is_supported_connector_operation("github", operation_id));
        }
        assert!(!is_supported_connector_operation(
            "computer",
            "computer.shell.execute"
        ));
        assert!(!is_supported_connector_operation(
            "github",
            "github.actions.run.get"
        ));
    }

    #[test]
    fn newly_cataloged_github_and_computer_grants_verify_with_exact_provider_scope() {
        let key = SigningKey::from_bytes(&[6; 32]);
        let digest = [0x5a; 32];
        let scopes = [
            TestConnectorScope {
                connection_ref: "github-connection",
                provider_id: "github",
                profile_ref: "github-main",
                operation_id: "github.actions.rerun_failed",
                resource_ref: "repository:123/run:45",
                request_id: "github-rerun-1",
            },
            TestConnectorScope {
                connection_ref: "computer-connection",
                provider_id: "computer",
                profile_ref: "computer-workspace",
                operation_id: "computer.command.execute",
                resource_ref: "workspace:workspace-1",
                request_id: "computer-command-1",
            },
        ];

        for scope in scopes {
            let grant = signed_grant_for(&key, NOW_MS, digest.to_vec(), scope);
            let exact_scope = ConnectorOperationGrantExpectation {
                connection_ref: scope.connection_ref,
                provider_id: scope.provider_id,
                profile_ref: scope.profile_ref,
                operation_id: scope.operation_id,
                resource_ref: scope.resource_ref,
                request_id: scope.request_id,
                ..expected(&digest)
            };

            verify_connector_operation_grant(
                &grant,
                &key.verifying_key(),
                KEY_ID,
                exact_scope,
                NOW_MS + 1,
            )
            .expect("new operation IDs receive only exact provider-bound signatures");
        }
    }
}
