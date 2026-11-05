//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 connector_catalog.rs                                            │
//! │  Module: cy_workspace_control_plane::authority::connector_catalog  │
//! │  Role: The connector operations this Authority process can grant.  │
//! │                                                                     │
//! │  模块职责：本 Authority 进程可以授权的 Connector 操作。               │
//! └─────────────────────────────────────────────────────────────────────┘

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::connector_grant::{
    encode_hex, is_automation_transport_connector_operation,
    is_automation_viewer_connector_operation, is_office_task_writer_connector_operation,
    is_supported_connector_operation, AUTOMATION_TRANSPORT_CONNECTOR_OPERATION_IDS,
    AUTOMATION_VIEWER_CONNECTOR_OPERATION_IDS, NAVIGATOR_AUTOMATION_VIEWER_ROLE,
    NAVIGATOR_CONNECTOR_OPERATOR_ROLE, NAVIGATOR_OFFICE_TASK_WRITER_ROLE,
    OFFICE_TASK_WRITER_CONNECTOR_OPERATION_IDS,
};

/// Largest manifest Authority accepts.
pub const CONNECTOR_OPERATIONS_MANIFEST_MAX_BYTES: usize = 1024 * 1024;

const MANIFEST_SCHEMA_VERSION: &str = "cyrene.connector-operations.v1";
const ROLE_OPERATOR: &str = "operator";
const ROLE_AUTOMATION_VIEWER: &str = "automationViewer";
const ROLE_OFFICE_TASK_WRITER: &str = "officeTaskWriter";
const CLOUD_NAMESPACE: &str = "cloud";
const CLOUD_PROVIDER_IDS: [&str; 3] = ["azure", "gcp", "runpod"];
const MAX_OPERATION_ID_BYTES: usize = 256;

/// Why Authority refused an operation manifest. Authority must not start on any of these.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ConnectorCatalogError {
    /// The configured digest is not `sha256:` followed by 64 lowercase hex characters.
    #[error("the approved digest is not sha256:<64 lowercase hex characters>")]
    ApprovedDigestInvalid,
    /// The manifest is larger than [`CONNECTOR_OPERATIONS_MANIFEST_MAX_BYTES`].
    #[error("the manifest is too large")]
    TooLarge,
    /// The manifest bytes are not the version this deployment approved.
    #[error("the manifest digest differs from the approved digest")]
    DigestMismatch,
    /// The manifest is not JSON of the expected shape.
    #[error("the manifest is not valid")]
    Malformed,
    /// The manifest uses a schema this Platform does not read.
    #[error("the manifest schema version is not supported")]
    UnsupportedSchema,
    /// The manifest maps a short role name to a Directory role other than Platform's.
    #[error("the manifest maps a role to a different Directory role")]
    RoleNameChanged,
    /// The manifest lists no operations.
    #[error("the manifest lists no operations")]
    Empty,
    /// One row breaks a rule that holds for every deployment.
    #[error("the manifest row {operation_id:?} is refused: {reason}")]
    OperationRefused {
        /// The row's operation ID, exactly as written in the manifest.
        operation_id: String,
        /// The rule that row breaks.
        reason: &'static str,
    },
}

/// The connector operations this Authority process can grant, fixed when the process starts.
///
/// By default this applies Platform's built-in lists. With a manifest approved by digest, the
/// manifest decides which operations exist and which providers they apply to. An operation it
/// doesn't list is refused, even for the connector operator.
///
/// A manifest can't give the automation viewer or Office task writer roles anything beyond
/// Platform's built-in lists, because Directory gives those roles to ordinary members. It can
/// take access away from them, and it can add operations for the connector operator.
#[derive(Debug, Clone, Default)]
pub struct ConnectorOperationCatalog {
    approved: Option<ApprovedManifest>,
}

#[derive(Debug, Clone)]
struct ApprovedManifest {
    digest: String,
    operations: BTreeMap<String, ManifestOperation>,
}

#[derive(Debug, Clone)]
struct ManifestOperation {
    provider_ids: BTreeSet<String>,
    automation_viewer: bool,
    office_task_writer: bool,
}

// Only the facts Platform uses. Other repositories' facts in the same rows are ignored.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawManifest {
    schema_version: String,
    roles: RawRoleNames,
    operations: Vec<RawOperation>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRoleNames {
    automation_viewer: String,
    office_task_writer: String,
    operator: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawOperation {
    operation_id: String,
    provider_ids: Vec<String>,
    roles: Vec<String>,
    automation_transport: bool,
    mutating: Option<bool>,
}

impl ConnectorOperationCatalog {
    /// The catalog of a deployment that hasn't approved a manifest.
    pub fn builtin() -> Self {
        Self::default()
    }

    /// Accept manifest bytes only if they are the approved version and every row is allowed.
    ///
    /// The digest covers the exact bytes, so the caller must pass the bytes it read rather than
    /// reading the file again.
    pub fn from_approved_manifest(
        bytes: &[u8],
        approved_digest: &str,
    ) -> Result<Self, ConnectorCatalogError> {
        if !valid_digest(approved_digest) {
            return Err(ConnectorCatalogError::ApprovedDigestInvalid);
        }
        if bytes.len() > CONNECTOR_OPERATIONS_MANIFEST_MAX_BYTES {
            return Err(ConnectorCatalogError::TooLarge);
        }
        let digest = format!("sha256:{}", encode_hex(&Sha256::digest(bytes)));
        if digest != approved_digest {
            return Err(ConnectorCatalogError::DigestMismatch);
        }

        let manifest: RawManifest =
            serde_json::from_slice(bytes).map_err(|_| ConnectorCatalogError::Malformed)?;
        if manifest.schema_version != MANIFEST_SCHEMA_VERSION {
            return Err(ConnectorCatalogError::UnsupportedSchema);
        }
        if manifest.roles.operator != NAVIGATOR_CONNECTOR_OPERATOR_ROLE
            || manifest.roles.automation_viewer != NAVIGATOR_AUTOMATION_VIEWER_ROLE
            || manifest.roles.office_task_writer != NAVIGATOR_OFFICE_TASK_WRITER_ROLE
        {
            return Err(ConnectorCatalogError::RoleNameChanged);
        }
        if manifest.operations.is_empty() {
            return Err(ConnectorCatalogError::Empty);
        }

        let mut operations = BTreeMap::new();
        for row in manifest.operations {
            let refused = |reason| ConnectorCatalogError::OperationRefused {
                operation_id: row.operation_id.clone(),
                reason,
            };
            let operation = accept_operation(&row).map_err(refused)?;
            if operations.contains_key(&row.operation_id) {
                return Err(refused("the operation is listed twice"));
            }
            operations.insert(row.operation_id, operation);
        }
        Ok(Self {
            approved: Some(ApprovedManifest { digest, operations }),
        })
    }

    /// The approved manifest's digest, or `None` when the built-in lists apply.
    pub fn approved_digest(&self) -> Option<&str> {
        self.approved
            .as_ref()
            .map(|approved| approved.digest.as_str())
    }

    /// How many operations the approved manifest lists, or `None` when the built-in lists apply.
    pub fn approved_operation_count(&self) -> Option<usize> {
        self.approved
            .as_ref()
            .map(|approved| approved.operations.len())
    }

    /// Return whether this provider and operation pair exists in the catalog.
    pub fn is_supported(&self, provider_id: &str, operation_id: &str) -> bool {
        match &self.approved {
            None => is_supported_connector_operation(provider_id, operation_id),
            Some(approved) => approved.operation(provider_id, operation_id).is_some(),
        }
    }

    /// Return whether any of these Directory roles may request this pair.
    ///
    /// Check [`Self::is_supported`] first, as request validation does. For a pair outside the
    /// catalog, the built-in lists answer `true` for the connector operator and a manifest answers
    /// `false`.
    pub fn role_allows(
        &self,
        roles: &BTreeSet<String>,
        provider_id: &str,
        operation_id: &str,
    ) -> bool {
        let Some(approved) = &self.approved else {
            return roles.contains(NAVIGATOR_CONNECTOR_OPERATOR_ROLE)
                || (is_automation_viewer_connector_operation(provider_id, operation_id)
                    && roles.contains(NAVIGATOR_AUTOMATION_VIEWER_ROLE))
                || (is_office_task_writer_connector_operation(provider_id, operation_id)
                    && roles.contains(NAVIGATOR_OFFICE_TASK_WRITER_ROLE))
                || (is_automation_transport_connector_operation(provider_id, operation_id)
                    && (roles.contains(NAVIGATOR_AUTOMATION_VIEWER_ROLE)
                        || roles.contains(NAVIGATOR_OFFICE_TASK_WRITER_ROLE)));
        };
        approved
            .operation(provider_id, operation_id)
            .is_some_and(|operation| {
                roles.contains(NAVIGATOR_CONNECTOR_OPERATOR_ROLE)
                    || (operation.automation_viewer
                        && roles.contains(NAVIGATOR_AUTOMATION_VIEWER_ROLE))
                    || (operation.office_task_writer
                        && roles.contains(NAVIGATOR_OFFICE_TASK_WRITER_ROLE))
            })
    }
}

impl ApprovedManifest {
    fn operation(&self, provider_id: &str, operation_id: &str) -> Option<&ManifestOperation> {
        self.operations
            .get(operation_id)
            .filter(|operation| operation.provider_ids.contains(provider_id))
    }
}

fn accept_operation(row: &RawOperation) -> Result<ManifestOperation, &'static str> {
    let operation_id = row.operation_id.as_str();
    if !valid_operation_id(operation_id) {
        return Err("the operation ID is malformed");
    }

    let provider_ids: BTreeSet<String> = row.provider_ids.iter().cloned().collect();
    if provider_ids.is_empty() || provider_ids.len() != row.provider_ids.len() {
        return Err("the provider list is empty or repeats a provider");
    }
    let namespace = operation_id.split('.').next().unwrap_or_default();
    if !provider_ids
        .iter()
        .all(|provider_id| provider_serves_namespace(provider_id, namespace))
    {
        return Err("a provider is outside the operation's namespace");
    }

    let mut roles = BTreeSet::new();
    for role in &row.roles {
        if ![
            ROLE_OPERATOR,
            ROLE_AUTOMATION_VIEWER,
            ROLE_OFFICE_TASK_WRITER,
        ]
        .contains(&role.as_str())
        {
            return Err("it names an unknown role");
        }
        if !roles.insert(role.as_str()) {
            return Err("it repeats a role");
        }
    }
    if !roles.contains(ROLE_OPERATOR) {
        return Err("the connector operator role is missing");
    }

    let is_transport = AUTOMATION_TRANSPORT_CONNECTOR_OPERATION_IDS.contains(&operation_id);
    if row.automation_transport != is_transport {
        return Err("its transport flag differs from Platform's list");
    }
    let automation_viewer = roles.contains(ROLE_AUTOMATION_VIEWER);
    let office_task_writer = roles.contains(ROLE_OFFICE_TASK_WRITER);
    if !is_transport {
        if automation_viewer && !AUTOMATION_VIEWER_CONNECTOR_OPERATION_IDS.contains(&operation_id) {
            return Err("it widens the automation viewer role");
        }
        if automation_viewer && row.mutating != Some(false) {
            return Err("it gives the automation viewer role an operation that isn't read-only");
        }
        if office_task_writer && !OFFICE_TASK_WRITER_CONNECTOR_OPERATION_IDS.contains(&operation_id)
        {
            return Err("it widens the office task writer role");
        }
    }

    Ok(ManifestOperation {
        provider_ids,
        automation_viewer,
        office_task_writer,
    })
}

fn provider_serves_namespace(provider_id: &str, namespace: &str) -> bool {
    if namespace == CLOUD_NAMESPACE {
        CLOUD_PROVIDER_IDS.contains(&provider_id)
    } else {
        provider_id == namespace
    }
}

fn valid_operation_id(value: &str) -> bool {
    value.len() <= MAX_OPERATION_ID_BYTES
        && value.starts_with(|first: char| first.is_ascii_lowercase())
        && value.contains('.')
        && value.split('.').all(|segment| {
            !segment.is_empty()
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        })
}

fn valid_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use cy_proto::cyrene::workspace::authority::v2::AuthorizeConnectorOperationRequest;
    use cy_proto::workspace_v1::{UserIdentityRef, WorkspaceConnectionDescriptor};
    use ed25519_dalek::SigningKey;
    use serde_json::{json, Value};
    use tonic::Code;

    use super::*;
    use crate::{
        authority::{
            connector_grant::SUPPORTED_CONNECTOR_OPERATION_IDS,
            service::{
                issue_connector_operation_grant, issue_connector_operation_grant_in_catalog,
                AuthorityCredentialSigner, AuthorityManagedWorkload,
            },
        },
        directory::{WorkspaceDirectory, WorkspaceDirectoryError},
    };

    const PROVIDER_IDS: [&str; 10] = [
        "azure",
        "azure_devops",
        "bilibili",
        "computer",
        "gcp",
        "github",
        "huggingface",
        "microsoft_graph",
        "msteams",
        "runpod",
    ];
    const ISSUER: &str =
        "https://login.microsoftonline.com/11111111-1111-4111-8111-111111111111/v2.0";
    const SUBJECT: &str = "11111111-1111-4111-8111-111111111111";

    /// A manifest stating exactly what the built-in lists say.
    fn manifest() -> Value {
        let operations: Vec<Value> = SUPPORTED_CONNECTOR_OPERATION_IDS
            .iter()
            .map(|operation_id| {
                let provider_ids: Vec<&str> = PROVIDER_IDS
                    .into_iter()
                    .filter(|provider_id| {
                        is_supported_connector_operation(provider_id, operation_id)
                    })
                    .collect();
                let transport = AUTOMATION_TRANSPORT_CONNECTOR_OPERATION_IDS.contains(operation_id);
                let viewer = AUTOMATION_VIEWER_CONNECTOR_OPERATION_IDS.contains(operation_id);
                let writer = OFFICE_TASK_WRITER_CONNECTOR_OPERATION_IDS.contains(operation_id);
                let mut roles = vec![ROLE_OPERATOR];
                if viewer || transport {
                    roles.push(ROLE_AUTOMATION_VIEWER);
                }
                if writer || transport {
                    roles.push(ROLE_OFFICE_TASK_WRITER);
                }
                json!({
                    "operationId": operation_id,
                    "providerIds": provider_ids,
                    "roles": roles,
                    "automationTransport": transport,
                    "mutating": if transport { Value::Null } else { json!(!viewer) },
                })
            })
            .collect();
        json!({
            "schemaVersion": MANIFEST_SCHEMA_VERSION,
            "roles": {
                "operator": NAVIGATOR_CONNECTOR_OPERATOR_ROLE,
                "automationViewer": NAVIGATOR_AUTOMATION_VIEWER_ROLE,
                "officeTaskWriter": NAVIGATOR_OFFICE_TASK_WRITER_ROLE,
            },
            "operations": operations,
        })
    }

    fn digest_of(bytes: &[u8]) -> String {
        format!("sha256:{}", encode_hex(&Sha256::digest(bytes)))
    }

    fn load(manifest: &Value) -> Result<ConnectorOperationCatalog, ConnectorCatalogError> {
        let bytes = serde_json::to_vec(manifest).expect("manifest serializes");
        ConnectorOperationCatalog::from_approved_manifest(&bytes, &digest_of(&bytes))
    }

    fn row<'a>(manifest: &'a mut Value, operation_id: &str) -> &'a mut Value {
        manifest["operations"]
            .as_array_mut()
            .expect("operations is an array")
            .iter_mut()
            .find(|row| row["operationId"] == operation_id)
            .expect("the operation is in the manifest")
    }

    fn operations(manifest: &mut Value) -> &mut Vec<Value> {
        manifest["operations"]
            .as_array_mut()
            .expect("operations is an array")
    }

    fn demo_row() -> Value {
        json!({
            "operationId": "demo.items.list",
            "providerIds": ["demo"],
            "roles": [ROLE_OPERATOR],
            "automationTransport": false,
            "mutating": false,
        })
    }

    fn assert_row_refused(manifest: &Value, operation_id: &str, reason: &str) {
        match load(manifest) {
            Err(ConnectorCatalogError::OperationRefused {
                operation_id: refused_id,
                reason: refused_reason,
            }) => {
                assert_eq!(refused_id, operation_id);
                assert_eq!(refused_reason, reason);
            }
            other => panic!("expected {operation_id} to be refused for {reason}: {other:?}"),
        }
    }

    fn role_subsets() -> Vec<BTreeSet<String>> {
        let roles = [
            NAVIGATOR_CONNECTOR_OPERATOR_ROLE,
            NAVIGATOR_AUTOMATION_VIEWER_ROLE,
            NAVIGATOR_OFFICE_TASK_WRITER_ROLE,
            "navigator.other.v1",
        ];
        (0..1_u32 << roles.len())
            .map(|mask| {
                roles
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| mask & (1 << index) != 0)
                    .map(|(_, role)| (*role).to_owned())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn manifest_stating_the_builtin_lists_grants_exactly_what_they_grant() {
        let builtin = ConnectorOperationCatalog::builtin();
        let approved = load(&manifest()).expect("manifest is accepted");
        assert_eq!(
            approved.approved_operation_count(),
            Some(SUPPORTED_CONNECTOR_OPERATION_IDS.len())
        );

        let mut operation_ids: Vec<&str> = SUPPORTED_CONNECTOR_OPERATION_IDS.to_vec();
        operation_ids.extend(["github.commits.compare", "demo.items.list", "", "github"]);
        let mut provider_ids: Vec<&str> = PROVIDER_IDS.to_vec();
        provider_ids.extend(["demo", "oracle", ""]);
        let role_subsets = role_subsets();
        let mut supported_pairs = 0;
        for operation_id in &operation_ids {
            for provider_id in &provider_ids {
                let supported = builtin.is_supported(provider_id, operation_id);
                assert_eq!(
                    approved.is_supported(provider_id, operation_id),
                    supported,
                    "{provider_id} {operation_id}"
                );
                supported_pairs += usize::from(supported);
                for roles in &role_subsets {
                    let allowed = approved.role_allows(roles, provider_id, operation_id);
                    if supported {
                        assert_eq!(
                            allowed,
                            builtin.role_allows(roles, provider_id, operation_id),
                            "{provider_id} {operation_id} {roles:?}"
                        );
                    } else {
                        assert!(!allowed, "{provider_id} {operation_id} {roles:?}");
                    }
                }
            }
        }
        assert!(supported_pairs >= SUPPORTED_CONNECTOR_OPERATION_IDS.len());
    }

    #[test]
    fn builtin_catalog_reports_no_approved_manifest() {
        let builtin = ConnectorOperationCatalog::builtin();
        assert_eq!(builtin.approved_digest(), None);
        assert_eq!(builtin.approved_operation_count(), None);
    }

    #[test]
    fn approved_manifest_reports_its_digest() {
        let bytes = serde_json::to_vec(&manifest()).expect("manifest serializes");
        let digest = digest_of(&bytes);
        let approved = ConnectorOperationCatalog::from_approved_manifest(&bytes, &digest)
            .expect("manifest is accepted");
        assert_eq!(approved.approved_digest(), Some(digest.as_str()));
    }

    #[test]
    fn only_the_approved_bytes_are_accepted() {
        let bytes = serde_json::to_vec(&manifest()).expect("manifest serializes");
        let digest = digest_of(&bytes);

        let mut changed = bytes.clone();
        changed.push(b'\n');
        assert_eq!(
            ConnectorOperationCatalog::from_approved_manifest(&changed, &digest).unwrap_err(),
            ConnectorCatalogError::DigestMismatch
        );

        let other_digest = format!("sha256:{}", "0".repeat(64));
        assert_eq!(
            ConnectorOperationCatalog::from_approved_manifest(&bytes, &other_digest).unwrap_err(),
            ConnectorCatalogError::DigestMismatch
        );

        for invalid in [
            String::new(),
            digest.trim_start_matches("sha256:").to_owned(),
            digest.to_uppercase(),
            format!("sha256:{}", "A".repeat(64)),
            format!("sha256:{}", "0".repeat(63)),
            format!("sha256:{}", "g".repeat(64)),
            format!("{digest} "),
        ] {
            assert_eq!(
                ConnectorOperationCatalog::from_approved_manifest(&bytes, &invalid).unwrap_err(),
                ConnectorCatalogError::ApprovedDigestInvalid,
                "{invalid:?}"
            );
        }
    }

    #[test]
    fn oversized_manifest_is_refused_even_with_its_own_digest() {
        let bytes = vec![b' '; CONNECTOR_OPERATIONS_MANIFEST_MAX_BYTES + 1];
        assert_eq!(
            ConnectorOperationCatalog::from_approved_manifest(&bytes, &digest_of(&bytes))
                .unwrap_err(),
            ConnectorCatalogError::TooLarge
        );
    }

    #[test]
    fn malformed_manifests_are_refused() {
        let duplicated_field = br#"{"schemaVersion":"cyrene.connector-operations.v1","schemaVersion":"cyrene.connector-operations.v1","roles":{"operator":"a","automationViewer":"b","officeTaskWriter":"c"},"operations":[]}"#;
        for bytes in [
            &b""[..],
            &b"not json"[..],
            &b"[]"[..],
            &b"{}"[..],
            &duplicated_field[..],
        ] {
            assert_eq!(
                ConnectorOperationCatalog::from_approved_manifest(bytes, &digest_of(bytes))
                    .unwrap_err(),
                ConnectorCatalogError::Malformed,
                "{}",
                String::from_utf8_lossy(bytes)
            );
        }

        for field in ["operationId", "providerIds", "roles", "automationTransport"] {
            let mut missing = manifest();
            row(&mut missing, "github.repository.list")
                .as_object_mut()
                .expect("row is an object")
                .remove(field);
            assert_eq!(
                load(&missing).unwrap_err(),
                ConnectorCatalogError::Malformed,
                "{field}"
            );
        }
    }

    #[test]
    fn manifest_level_rules_are_enforced() {
        let mut wrong_schema = manifest();
        wrong_schema["schemaVersion"] = json!("cyrene.connector-operations.v2");
        assert_eq!(
            load(&wrong_schema).unwrap_err(),
            ConnectorCatalogError::UnsupportedSchema
        );

        for role in ["operator", "automationViewer", "officeTaskWriter"] {
            let mut renamed = manifest();
            renamed["roles"][role] = json!("navigator.super.v1");
            assert_eq!(
                load(&renamed).unwrap_err(),
                ConnectorCatalogError::RoleNameChanged,
                "{role}"
            );
        }

        let mut empty = manifest();
        operations(&mut empty).clear();
        assert_eq!(load(&empty).unwrap_err(), ConnectorCatalogError::Empty);
    }

    #[test]
    fn rows_cannot_misstate_identifiers_or_providers() {
        for operation_id in [
            "",
            "github",
            ".github",
            "github.",
            "github..issue",
            "GitHub.issue.get",
            "github.issue get",
            "1github.issue.get",
            "github.issue.get\n",
            &format!("github.{}", "a".repeat(MAX_OPERATION_ID_BYTES)),
        ] {
            let mut bad = manifest();
            row(&mut bad, "github.repository.list")["operationId"] = json!(operation_id);
            assert_row_refused(&bad, operation_id, "the operation ID is malformed");
        }

        let mut repeated = manifest();
        operations(&mut repeated).push(demo_row());
        operations(&mut repeated).push(demo_row());
        assert_row_refused(
            &repeated,
            "demo.items.list",
            "the operation is listed twice",
        );

        for provider_ids in [json!([]), json!(["github", "github"])] {
            let mut bad = manifest();
            row(&mut bad, "github.repository.list")["providerIds"] = provider_ids;
            assert_row_refused(
                &bad,
                "github.repository.list",
                "the provider list is empty or repeats a provider",
            );
        }

        // A GitHub operation can't be pointed at another provider's connection.
        let mut crossed = manifest();
        row(&mut crossed, "github.repository.list")["providerIds"] = json!(["github", "gcp"]);
        assert_row_refused(
            &crossed,
            "github.repository.list",
            "a provider is outside the operation's namespace",
        );

        let mut unknown_cloud = manifest();
        row(&mut unknown_cloud, "cloud.resource.get")["providerIds"] = json!(["azure", "oracle"]);
        assert_row_refused(
            &unknown_cloud,
            "cloud.resource.get",
            "a provider is outside the operation's namespace",
        );
    }

    #[test]
    fn rows_cannot_misstate_roles() {
        let mut unknown = manifest();
        row(&mut unknown, "github.repository.list")["roles"] = json!(["operator", "superuser"]);
        assert_row_refused(
            &unknown,
            "github.repository.list",
            "it names an unknown role",
        );

        let mut repeated = manifest();
        row(&mut repeated, "github.repository.list")["roles"] = json!(["operator", "operator"]);
        assert_row_refused(&repeated, "github.repository.list", "it repeats a role");

        let mut no_operator = manifest();
        row(&mut no_operator, "microsoft_graph.planner.plan.get")["roles"] =
            json!(["automationViewer", "officeTaskWriter"]);
        assert_row_refused(
            &no_operator,
            "microsoft_graph.planner.plan.get",
            "the connector operator role is missing",
        );
    }

    #[test]
    fn manifest_cannot_widen_what_ordinary_members_can_reach() {
        for (operation_id, role, reason) in [
            // Reads outside the viewer's six: another Graph read, and another provider.
            (
                "microsoft_graph.mail.messages.list",
                ROLE_AUTOMATION_VIEWER,
                "it widens the automation viewer role",
            ),
            (
                "microsoft_graph.todo.lists.list",
                ROLE_AUTOMATION_VIEWER,
                "it widens the automation viewer role",
            ),
            (
                "github.repository.list",
                ROLE_AUTOMATION_VIEWER,
                "it widens the automation viewer role",
            ),
            // Writes outside the task writer's 17.
            (
                "microsoft_graph.mail.message.send",
                ROLE_OFFICE_TASK_WRITER,
                "it widens the office task writer role",
            ),
            (
                "microsoft_graph.calendar.event.create",
                ROLE_OFFICE_TASK_WRITER,
                "it widens the office task writer role",
            ),
            (
                "github.repository.list",
                ROLE_OFFICE_TASK_WRITER,
                "it widens the office task writer role",
            ),
        ] {
            let mut widened = manifest();
            row(&mut widened, operation_id)["roles"]
                .as_array_mut()
                .expect("roles is an array")
                .push(json!(role));
            row(&mut widened, operation_id)["mutating"] = json!(false);
            assert_row_refused(&widened, operation_id, reason);
        }

        let mut new_provider = manifest();
        let mut demo = demo_row();
        demo["roles"] = json!([ROLE_OPERATOR, ROLE_AUTOMATION_VIEWER]);
        operations(&mut new_provider).push(demo);
        assert_row_refused(
            &new_provider,
            "demo.items.list",
            "it widens the automation viewer role",
        );

        for mutating in [json!(true), Value::Null] {
            let mut not_read_only = manifest();
            row(&mut not_read_only, "microsoft_graph.planner.plan.get")["mutating"] = mutating;
            assert_row_refused(
                &not_read_only,
                "microsoft_graph.planner.plan.get",
                "it gives the automation viewer role an operation that isn't read-only",
            );
        }

        // The transport exception stays with the one exact operation.
        let mut other_transport = manifest();
        row(&mut other_transport, "microsoft_graph.mail.message.send")["automationTransport"] =
            json!(true);
        assert_row_refused(
            &other_transport,
            "microsoft_graph.mail.message.send",
            "its transport flag differs from Platform's list",
        );
        let mut reply_not_transport = manifest();
        row(&mut reply_not_transport, "msteams.message.reply")["automationTransport"] =
            json!(false);
        assert_row_refused(
            &reply_not_transport,
            "msteams.message.reply",
            "its transport flag differs from Platform's list",
        );
    }

    #[test]
    fn manifest_can_narrow_ordinary_members_and_ignores_other_repositories_facts() {
        let mut narrowed = manifest();
        row(&mut narrowed, "microsoft_graph.planner.plan.get")["roles"] =
            json!([ROLE_OPERATOR, ROLE_OFFICE_TASK_WRITER]);
        row(&mut narrowed, "msteams.message.reply")["roles"] =
            json!([ROLE_OPERATOR, ROLE_OFFICE_TASK_WRITER]);
        operations(&mut narrowed)
            .retain(|row| row["operationId"] != "microsoft_graph.sharepoint.site.get");
        row(&mut narrowed, "github.repository.list")["navigatorToolId"] = json!("anything");
        narrowed["exchangeOnlyOperations"] = json!([{ "operationId": "github.commits.compare" }]);

        let catalog = load(&narrowed).expect("a narrower manifest is accepted");
        let viewer = BTreeSet::from([NAVIGATOR_AUTOMATION_VIEWER_ROLE.to_owned()]);
        let operator = BTreeSet::from([NAVIGATOR_CONNECTOR_OPERATOR_ROLE.to_owned()]);
        assert!(!catalog.role_allows(
            &viewer,
            "microsoft_graph",
            "microsoft_graph.planner.plan.get"
        ));
        assert!(!catalog.role_allows(&viewer, "msteams", "msteams.message.reply"));
        assert!(catalog.role_allows(
            &viewer,
            "microsoft_graph",
            "microsoft_graph.planner.task.get"
        ));
        assert!(!catalog.is_supported("microsoft_graph", "microsoft_graph.sharepoint.site.get"));
        assert!(!catalog.role_allows(
            &operator,
            "microsoft_graph",
            "microsoft_graph.sharepoint.site.get"
        ));
        assert!(!catalog.is_supported("github", "github.commits.compare"));
    }

    struct FixedRolesDirectory {
        roles: BTreeSet<String>,
    }

    #[async_trait]
    impl WorkspaceDirectory for FixedRolesDirectory {
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
            _user: &UserIdentityRef,
            _organization_id: &str,
            _workspace_id: &str,
        ) -> Result<bool, WorkspaceDirectoryError> {
            Ok(true)
        }

        async fn roles_for_member(
            &self,
            _user: &UserIdentityRef,
            _organization_id: &str,
            _workspace_id: &str,
        ) -> Result<Option<BTreeSet<String>>, WorkspaceDirectoryError> {
            Ok(Some(self.roles.clone()))
        }
    }

    fn request(provider_id: &str, operation_id: &str) -> AuthorizeConnectorOperationRequest {
        AuthorizeConnectorOperationRequest {
            workspace_id: "workspace-1".to_owned(),
            principal_issuer: ISSUER.to_owned(),
            principal_subject: SUBJECT.to_owned(),
            connection_ref: format!("{provider_id}-connection"),
            provider_id: provider_id.to_owned(),
            profile_ref: format!("{provider_id}-profile"),
            operation_id: operation_id.to_owned(),
            resource_ref: "source:target-1".to_owned(),
            arguments_digest_sha256: vec![0x27; 32],
            request_id: format!("request-{operation_id}"),
        }
    }

    async fn issue(
        catalog: &ConnectorOperationCatalog,
        role: &str,
        provider_id: &str,
        operation_id: &str,
    ) -> Result<(), Code> {
        let directory = FixedRolesDirectory {
            roles: BTreeSet::from([role.to_owned()]),
        };
        let signer = AuthorityCredentialSigner::new(
            "authority-test-key",
            SigningKey::from_bytes(&[5; 32]),
            std::time::Duration::from_secs(30),
        )
        .expect("Authority signing key is valid");
        let workload = AuthorityManagedWorkload {
            workload_id: "exchange-test".into(),
            audience: "exchange".into(),
            certificate_fingerprints_sha256: Vec::new(),
        };
        issue_connector_operation_grant_in_catalog(
            catalog,
            &directory,
            &signer,
            "org-1",
            "workspace-1",
            ISSUER,
            &workload,
            &request(provider_id, operation_id),
        )
        .await
        .map(|grant| {
            assert_eq!(grant.provider_id, provider_id);
            assert_eq!(grant.operation_id, operation_id);
        })
        .map_err(|status| status.code())
    }

    #[tokio::test]
    async fn issuing_a_grant_follows_the_approved_manifest() {
        let mut changed = manifest();
        operations(&mut changed).retain(|row| row["operationId"] != "github.repository.list");
        operations(&mut changed).push(demo_row());
        row(&mut changed, "microsoft_graph.planner.plan.get")["roles"] =
            json!([ROLE_OPERATOR, ROLE_OFFICE_TASK_WRITER]);
        row(&mut changed, "msteams.message.reply")["roles"] =
            json!([ROLE_OPERATOR, ROLE_AUTOMATION_VIEWER]);
        let catalog = load(&changed).expect("manifest is accepted");
        let operator = NAVIGATOR_CONNECTOR_OPERATOR_ROLE;
        let viewer = NAVIGATOR_AUTOMATION_VIEWER_ROLE;
        let writer = NAVIGATOR_OFFICE_TASK_WRITER_ROLE;

        // An operation the manifest doesn't declare is refused, even for the operator.
        assert_eq!(
            issue(&catalog, operator, "github", "github.repository.list").await,
            Err(Code::InvalidArgument)
        );
        // An operation it adds works for the operator, on that provider only.
        assert_eq!(
            issue(&catalog, operator, "demo", "demo.items.list").await,
            Ok(())
        );
        assert_eq!(
            issue(&catalog, operator, "github", "demo.items.list").await,
            Err(Code::InvalidArgument)
        );
        // Ordinary members don't get it.
        assert_eq!(
            issue(&catalog, viewer, "demo", "demo.items.list").await,
            Err(Code::PermissionDenied)
        );
        // A role the manifest removes from a row is refused, for that role only.
        for (role, provider_id, operation_id, expected) in [
            (
                viewer,
                "microsoft_graph",
                "microsoft_graph.planner.plan.get",
                Err(Code::PermissionDenied),
            ),
            (
                writer,
                "microsoft_graph",
                "microsoft_graph.planner.plan.get",
                Ok(()),
            ),
            (
                operator,
                "microsoft_graph",
                "microsoft_graph.planner.plan.get",
                Ok(()),
            ),
            (
                writer,
                "msteams",
                "msteams.message.reply",
                Err(Code::PermissionDenied),
            ),
            (viewer, "msteams", "msteams.message.reply", Ok(())),
            // A row the manifest leaves alone keeps its roles.
            (
                viewer,
                "microsoft_graph",
                "microsoft_graph.planner.task.get",
                Ok(()),
            ),
        ] {
            assert_eq!(
                issue(&catalog, role, provider_id, operation_id).await,
                expected,
                "{role} {operation_id}"
            );
        }
    }

    #[tokio::test]
    async fn the_builtin_catalog_refuses_operations_only_a_manifest_could_add() {
        let builtin = ConnectorOperationCatalog::builtin();
        assert_eq!(
            issue(
                &builtin,
                NAVIGATOR_CONNECTOR_OPERATOR_ROLE,
                "demo",
                "demo.items.list"
            )
            .await,
            Err(Code::InvalidArgument)
        );
        assert_eq!(
            issue(
                &builtin,
                NAVIGATOR_CONNECTOR_OPERATOR_ROLE,
                "github",
                "github.repository.list"
            )
            .await,
            Ok(())
        );
    }

    #[tokio::test]
    async fn the_entry_point_without_a_catalog_uses_the_builtin_lists() {
        let directory = FixedRolesDirectory {
            roles: BTreeSet::from([NAVIGATOR_CONNECTOR_OPERATOR_ROLE.to_owned()]),
        };
        let signer = AuthorityCredentialSigner::new(
            "authority-test-key",
            SigningKey::from_bytes(&[5; 32]),
            std::time::Duration::from_secs(30),
        )
        .expect("Authority signing key is valid");
        let workload = AuthorityManagedWorkload {
            workload_id: "exchange-test".into(),
            audience: "exchange".into(),
            certificate_fingerprints_sha256: Vec::new(),
        };
        let result = issue_connector_operation_grant(
            &directory,
            &signer,
            "org-1",
            "workspace-1",
            ISSUER,
            &workload,
            &request("demo", "demo.items.list"),
        )
        .await;
        assert_eq!(result.unwrap_err().code(), Code::InvalidArgument);
    }
}
