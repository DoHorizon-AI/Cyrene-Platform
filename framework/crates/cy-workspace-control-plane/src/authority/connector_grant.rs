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

/// Wire schema version signed into every per-operation grant.
pub const CONNECTOR_OPERATION_GRANT_SCHEMA_VERSION: &str =
    "cyrene.workspace.connector-operation-grant.v1";

/// Maximum validity of one exact connector grant.
pub const CONNECTOR_OPERATION_GRANT_TTL_MS: u64 = 15_000;

/// Closed operation identifiers accepted by the connector authorization boundary.
pub const SUPPORTED_CONNECTOR_OPERATION_IDS: [&str; 33] = [
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
    "github.actions.runs.list",
    "github.branches.list",
    "github.ci.status",
    "github.issue.comment.create",
    "github.issue.get",
    "github.issue.state.update",
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
];

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
        Some(("cloud", _)) if operation_id == "cloud.resource.restart" => provider_id == "runpod",
        Some(("cloud", _)) => matches!(provider_id, "azure" | "gcp" | "runpod"),
        _ => false,
    }
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
    use cy_proto::workspace_v1::{UserIdentityRef, WorkspaceConnectionDescriptor};
    use ed25519_dalek::{Signer, SigningKey};
    use tonic::Code;

    use super::*;
    use crate::{
        authority::service::require_directory_connector_operator_role,
        directory::{WorkspaceDirectory, WorkspaceDirectoryError},
    };

    const KEY_ID: &str = "authority-test-key";
    const NOW_MS: u64 = 1_800_000_000_000;

    fn expected<'a>(digest: &'a [u8]) -> ConnectorOperationGrantExpectation<'a> {
        ConnectorOperationGrantExpectation {
            audience: "exchange",
            organization_id: "org-1",
            workspace_id: "workspace-1",
            principal_issuer:
                "https://login.microsoftonline.com/11111111-1111-4111-8111-111111111111/v2.0",
            principal_subject: "d83aeb27-ff11-46d7-b0f7-af8da8d5b8ec",
            connection_ref: "connection-1",
            provider_id: "github",
            profile_ref: "github-main",
            operation_id: "github.repository.list",
            resource_ref: "repositories",
            arguments_digest_sha256: digest,
            request_id: "activity-1",
        }
    }

    fn signed_grant(key: &SigningKey, now_ms: u64, digest: Vec<u8>) -> ConnectorOperationGrantV1 {
        let mut grant = ConnectorOperationGrantV1 {
            schema_version: CONNECTOR_OPERATION_GRANT_SCHEMA_VERSION.to_owned(),
            grant_ref: String::new(),
            audience: "exchange".to_owned(),
            organization_id: "org-1".to_owned(),
            workspace_id: "workspace-1".to_owned(),
            principal_issuer:
                "https://login.microsoftonline.com/11111111-1111-4111-8111-111111111111/v2.0"
                    .to_owned(),
            principal_subject: "d83aeb27-ff11-46d7-b0f7-af8da8d5b8ec".to_owned(),
            connection_ref: "connection-1".to_owned(),
            provider_id: "github".to_owned(),
            profile_ref: "github-main".to_owned(),
            operation_id: "github.repository.list".to_owned(),
            resource_ref: "repositories".to_owned(),
            arguments_digest_sha256: digest,
            request_id: "activity-1".to_owned(),
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
            subject: "d83aeb27-ff11-46d7-b0f7-af8da8d5b8ec".to_owned(),
        };
        let directory = MutableRoleDirectory {
            identity: identity.clone(),
            organization_id: "org-1".to_owned(),
            workspace_id: "workspace-1".to_owned(),
            roles: Mutex::new(BTreeSet::from([
                NAVIGATOR_CONNECTOR_OPERATOR_ROLE.to_owned()
            ])),
        };
        assert!(require_directory_connector_operator_role(
            &directory,
            &identity,
            "org-1",
            "workspace-1"
        )
        .await
        .is_ok());

        directory
            .roles
            .lock()
            .expect("roles lock is healthy")
            .remove(NAVIGATOR_CONNECTOR_OPERATOR_ROLE);
        assert_eq!(
            require_directory_connector_operator_role(
                &directory,
                &identity,
                "org-1",
                "workspace-1"
            )
            .await
            .expect_err("revoked role must deny the next call")
            .code(),
            Code::PermissionDenied
        );
        assert_eq!(
            require_directory_connector_operator_role(
                &directory,
                &identity,
                "org-1",
                "another-workspace"
            )
            .await
            .expect_err("another workspace must remain denied")
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
            operations: Vec<Operation>,
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
        for operation in policy.operations {
            for provider_id in operation.provider_ids {
                assert!(is_supported_connector_operation(
                    &provider_id,
                    &operation.operation_id
                ));
            }
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
}
