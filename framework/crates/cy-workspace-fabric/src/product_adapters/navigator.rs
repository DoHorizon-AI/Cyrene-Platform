//! Navigator Product HTTP route mappings.
//!
//! Navigator owns the observed aggregate and Harness session view. This
//! module maps only closed, scoped reads and fails closed on event append until
//! a trusted Harness writer handoff exists.

use cy_proto::workspace_v1::{
    WorkspaceProductApiContentType as ContentType, WorkspaceProductApiOperation as Operation,
    WorkspaceProductApiOwner as Owner, WorkspaceProductApiRequestKind as Kind,
};
use serde::Deserialize;

use crate::{
    ProductInvocationError, ProductInvocationRequest, ProductInvocationResponse,
    WorkspaceCallerContext, PRODUCT_JSON_BODY_MAX_BYTES,
};

use super::http::{ProductHttpMethod, ProductHttpPathSegment, ProductHttpTarget};

const MAX_SNAPSHOT_WORKSPACE_ID_BYTES: usize = 200;
const MAX_SNAPSHOT_READS: usize = 50;

/// Maps Navigator read operations to the fixed Product OpenAPI v1 routes.
///
/// `observeWorkspaceSnapshot` accepts only the nested READ routes represented
/// in the closed projection manifest. `getWorkspaceSession` derives its
/// Workspace path from the authenticated caller context and treats
/// `resource_id` as one safe session segment.
pub(super) fn target(
    caller: &WorkspaceCallerContext,
    request: &ProductInvocationRequest,
) -> Result<ProductHttpTarget, ProductInvocationError> {
    if request.owner != Owner::Navigator {
        return Err(ProductInvocationError::InvalidRequest);
    }

    match (request.operation, request.kind) {
        (Operation::WorkspaceProductApiOperation11, Kind::Read) => {
            if request.resource_id.is_some()
                || request.idempotency_key.is_some()
                || !valid_snapshot_request(caller.workspace_id(), &request.json_body)
            {
                return Err(ProductInvocationError::InvalidRequest);
            }
            ProductHttpTarget::new(
                Owner::Navigator,
                ProductHttpMethod::Post,
                vec![
                    ProductHttpPathSegment::Static("api"),
                    ProductHttpPathSegment::Static("v1"),
                    ProductHttpPathSegment::Static("workspace-snapshots"),
                ],
            )
        }
        (Operation::WorkspaceProductApiOperation12, Kind::Read) => {
            if !request.json_body.is_empty() || request.idempotency_key.is_some() {
                return Err(ProductInvocationError::InvalidRequest);
            }
            let session_id = request
                .resource_id
                .as_deref()
                .ok_or(ProductInvocationError::InvalidRequest)?;
            let workspace_id = caller.workspace_id();
            if workspace_id.is_empty() || workspace_id.len() > 512 {
                return Err(ProductInvocationError::InvalidRequest);
            }
            ProductHttpTarget::new(
                Owner::Navigator,
                ProductHttpMethod::Get,
                vec![
                    ProductHttpPathSegment::Static("internal"),
                    ProductHttpPathSegment::Static("workspace"),
                    ProductHttpPathSegment::Static("v1"),
                    ProductHttpPathSegment::Static("workspaces"),
                    ProductHttpPathSegment::resource(workspace_id)?,
                    ProductHttpPathSegment::Static("sessions"),
                    ProductHttpPathSegment::resource(session_id)?,
                ],
            )
        }
        (Operation::WorkspaceProductApiOperation13, Kind::Command) => {
            // No trusted Navigator Harness writer handoff is available yet.
            // Never forward writerToken from a browser or ordinary workload.
            Err(ProductInvocationError::PermissionDenied)
        }
        _ => Err(ProductInvocationError::InvalidRequest),
    }
}

fn valid_snapshot_request(workspace_id: &str, body: &[u8]) -> bool {
    if body.is_empty() || body.len() > PRODUCT_JSON_BODY_MAX_BYTES {
        return false;
    }
    let Ok(snapshot) = serde_json::from_slice::<SnapshotRequest>(body) else {
        return false;
    };
    snapshot.workspace_id == workspace_id
        && !snapshot.workspace_id.is_empty()
        && snapshot.workspace_id.len() <= MAX_SNAPSHOT_WORKSPACE_ID_BYTES
        && !snapshot.reads.is_empty()
        && snapshot.reads.len() <= MAX_SNAPSHOT_READS
        && snapshot
            .reads
            .iter()
            .all(|read| approved_nested_read(read.product, &read.path))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SnapshotRequest {
    workspace_id: String,
    reads: Vec<SnapshotRead>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotRead {
    product: Product,
    path: String,
}

fn approved_nested_read(product: Product, path: &str) -> bool {
    match product {
        Product::Catalyst => path == "/internal/workspace/v1/datasets",
        Product::Echo => uuid_path(path, "/internal/workspace/v1/evaluation-suites/"),
        Product::Exchange => path == "/api/v1/workspace/gateway-routes",
        Product::Reactor => path == "/internal/workspace/v1/model-imports",
        Product::Yield => uuid_path(path, "/internal/workspace/v1/training-drafts/"),
    }
}

fn uuid_path(path: &str, fixed_prefix: &str) -> bool {
    path.strip_prefix(fixed_prefix)
        .is_some_and(is_hyphenated_uuid)
}

fn is_hyphenated_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => *byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

/// Validates Navigator's successful responses against the closed Product
/// schemas before their JSON is returned to a Workspace caller.
pub(super) fn validate_response(
    caller: &WorkspaceCallerContext,
    request: &ProductInvocationRequest,
    response: ProductInvocationResponse,
) -> Result<ProductInvocationResponse, ProductInvocationError> {
    if request.owner != Owner::Navigator {
        return Err(ProductInvocationError::InvalidRequest);
    }
    if request.operation == Operation::WorkspaceProductApiOperation13 {
        return Err(ProductInvocationError::PermissionDenied);
    }
    if request.kind != Kind::Read {
        return Err(ProductInvocationError::InvalidRequest);
    }

    if !(200..300).contains(&response.status_code) {
        return Ok(response);
    }
    if response.status_code != 200 || response.content_type != ContentType::ApplicationJson {
        return Err(ProductInvocationError::Internal);
    }

    match request.operation {
        Operation::WorkspaceProductApiOperation11 => {
            let snapshot = serde_json::from_slice::<WorkspaceSnapshot>(&response.json_body)
                .map_err(|_| ProductInvocationError::Internal)?;
            if snapshot.workspace_id != caller.workspace_id()
                || snapshot.workspace_id.is_empty()
                || snapshot.workspace_id.len() > MAX_SNAPSHOT_WORKSPACE_ID_BYTES
                || !is_rfc3339_date_time(&snapshot.observed_at)
                || !snapshot.views.iter().all(ProductView::is_valid)
            {
                return Err(ProductInvocationError::Internal);
            }
            let _snapshot_status = snapshot.status;
        }
        Operation::WorkspaceProductApiOperation12 => {
            let session_id = request
                .resource_id
                .as_deref()
                .ok_or(ProductInvocationError::InvalidRequest)?;
            let summary = serde_json::from_slice::<WorkspaceSessionSummary>(&response.json_body)
                .map_err(|_| ProductInvocationError::Internal)?;
            if summary.revision.is_empty()
                || summary.product_metadata.workspace_id != caller.workspace_id()
                || summary.product_metadata.workspace_id.is_empty()
                || summary.product_metadata.workspace_id.len() > MAX_SCOPE_ID_BYTES
                || summary.product_metadata.session_id != session_id
                || summary.product_metadata.session_id.is_empty()
                || summary.product_metadata.session_id.len() > MAX_SCOPE_ID_BYTES
                || summary.product_metadata.metadata_version == 0
                || !matches!(
                    summary.product_metadata.owner_state,
                    OwnerState::Known | OwnerState::Unknown
                )
                || !matches!(
                    summary.product_metadata.source,
                    MetadataSource::Cyrene | MetadataSource::Legacy
                )
            {
                return Err(ProductInvocationError::Internal);
            }
            let _closed_optional_fields = (
                &summary.product_metadata.creator_actor_id,
                &summary.product_metadata.owner_actor_id,
                summary.product_metadata.created_at,
                summary.event_count,
                summary.last_activity_at,
            );
        }
        _ => return Err(ProductInvocationError::InvalidRequest),
    }

    Ok(response)
}

const MAX_SCOPE_ID_BYTES: usize = 512;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WorkspaceSnapshot {
    workspace_id: String,
    status: SnapshotStatus,
    views: Vec<ProductView>,
    observed_at: String,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum SnapshotStatus {
    Complete,
    Partial,
    Failed,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum Product {
    Catalyst,
    Echo,
    Exchange,
    Reactor,
    Yield,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProductView {
    product: Product,
    source_operation: SnapshotSourceOperation,
    observed_at: String,
    status: ViewStatus,
    #[serde(default, deserialize_with = "deserialize_optional_non_null")]
    resource_summary: Option<ResourceSummary>,
    #[serde(default, deserialize_with = "deserialize_optional_non_null")]
    problem: Option<ObservationProblem>,
}

impl ProductView {
    fn is_valid(&self) -> bool {
        let source_matches_product = matches!(
            (self.product, self.source_operation),
            (
                Product::Catalyst,
                SnapshotSourceOperation::WorkspaceListDatasets
            ) | (
                Product::Echo,
                SnapshotSourceOperation::WorkspaceGetEvaluationSuite
            ) | (
                Product::Exchange,
                SnapshotSourceOperation::ListWorkspaceGatewayRoutes
            ) | (
                Product::Reactor,
                SnapshotSourceOperation::WorkspaceListModelImports
            ) | (Product::Yield, SnapshotSourceOperation::WorkspaceGetDraft)
        );
        let status_has_required_detail = match self.status {
            ViewStatus::Available => self.resource_summary.is_some(),
            ViewStatus::Unavailable => self.problem.is_some(),
        };

        source_matches_product
            && is_rfc3339_date_time(&self.observed_at)
            && status_has_required_detail
            && self
                .resource_summary
                .as_ref()
                .is_none_or(ResourceSummary::is_valid)
            && self
                .problem
                .as_ref()
                .is_none_or(ObservationProblem::is_valid)
    }
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
enum SnapshotSourceOperation {
    WorkspaceListDatasets,
    WorkspaceGetEvaluationSuite,
    ListWorkspaceGatewayRoutes,
    WorkspaceListModelImports,
    WorkspaceGetDraft,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum ViewStatus {
    Available,
    Unavailable,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ResourceSummary {
    json_sha256: String,
    canonical_json_bytes: u64,
}

impl ResourceSummary {
    fn is_valid(&self) -> bool {
        let Some(digest) = self.json_sha256.strip_prefix("sha256:") else {
            return false;
        };
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            && (2..=PRODUCT_JSON_BODY_MAX_BYTES as u64).contains(&self.canonical_json_bytes)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ObservationProblem {
    code: String,
    detail: String,
    retryable: bool,
    #[serde(default, deserialize_with = "deserialize_optional_non_null")]
    upstream_status: Option<u16>,
}

impl ObservationProblem {
    fn is_valid(&self) -> bool {
        let _problem_fields = (&self.code, &self.detail, self.retryable);
        self.upstream_status
            .is_none_or(|status| (400..=599).contains(&status))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WorkspaceSessionSummary {
    product_metadata: ProductMetadata,
    revision: String,
    event_count: u64,
    last_activity_at: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProductMetadata {
    workspace_id: String,
    session_id: String,
    creator_actor_id: Option<String>,
    owner_actor_id: Option<String>,
    owner_state: OwnerState,
    metadata_version: u64,
    source: MetadataSource,
    created_at: Option<u64>,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum OwnerState {
    Known,
    Unknown,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum MetadataSource {
    Cyrene,
    Legacy,
}

fn deserialize_optional_non_null<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

fn is_rfc3339_date_time(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() < 20
        || bytes.get(4) != Some(&b'-')
        || bytes.get(7) != Some(&b'-')
        || !matches!(bytes.get(10), Some(b'T' | b't'))
        || bytes.get(13) != Some(&b':')
        || bytes.get(16) != Some(&b':')
    {
        return false;
    }

    let Some(year) = decimal(&bytes[0..4]) else {
        return false;
    };
    let Some(month) = decimal(&bytes[5..7]) else {
        return false;
    };
    let Some(day) = decimal(&bytes[8..10]) else {
        return false;
    };
    let Some(hour) = decimal(&bytes[11..13]) else {
        return false;
    };
    let Some(minute) = decimal(&bytes[14..16]) else {
        return false;
    };
    let Some(second) = decimal(&bytes[17..19]) else {
        return false;
    };
    let month_days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => return false,
    };
    if year == 0 || day == 0 || day > month_days || hour > 23 || minute > 59 || second > 59 {
        return false;
    }

    let mut cursor = 19;
    if bytes.get(cursor) == Some(&b'.') {
        cursor += 1;
        let fraction_start = cursor;
        while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
            cursor += 1;
        }
        if cursor == fraction_start {
            return false;
        }
    }

    match bytes.get(cursor..) {
        Some([b'Z' | b'z']) => true,
        Some([b'+' | b'-', hour_tens, hour_ones, b':', minute_tens, minute_ones]) => {
            let offset_hour = decimal(&[*hour_tens, *hour_ones]);
            let offset_minute = decimal(&[*minute_tens, *minute_ones]);
            offset_hour.is_some_and(|value| value <= 23)
                && offset_minute.is_some_and(|value| value <= 59)
        }
        _ => false,
    }
}

fn decimal(bytes: &[u8]) -> Option<u32> {
    bytes.iter().try_fold(0, |value, byte| {
        byte.is_ascii_digit()
            .then(|| value * 10 + u32::from(*byte - b'0'))
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::{Arc, Mutex};

    use cy_proto::workspace_v1::{
        UserIdentityRef, WorkspaceProductApiContentType as ContentType,
        WorkspaceProductApiOperation as Operation, WorkspaceProductApiOwner as Owner,
        WorkspaceProductApiRequestKind as Kind,
    };

    use super::*;
    use crate::product_adapters::http::{
        ConfiguredProductEndpointResolver, ProductHttpRequest, ProductHttpResponse,
        ProductHttpTransport, TEST_SERVICE_CREDENTIAL,
    };
    use crate::product_adapters::{ProductEndpointConfig, ProductHttpClient};

    struct MockTransport {
        requests: Arc<Mutex<Vec<ProductHttpRequest>>>,
        response: Mutex<Option<ProductHttpResponse>>,
    }

    #[tonic::async_trait]
    impl ProductHttpTransport for MockTransport {
        async fn send(
            &self,
            request: ProductHttpRequest,
        ) -> Result<ProductHttpResponse, ProductInvocationError> {
            self.requests.lock().unwrap().push(request);
            self.response
                .lock()
                .unwrap()
                .take()
                .ok_or(ProductInvocationError::Unavailable)
        }
    }

    fn member_caller(workspace_id: &str) -> WorkspaceCallerContext {
        WorkspaceCallerContext::user_member(
            UserIdentityRef {
                issuer: "https://identity.test".to_string(),
                subject: "user-1".to_string(),
            },
            "organization-1",
            workspace_id,
            BTreeSet::new(),
        )
        .unwrap()
    }

    fn mock_client(
        response_body: &[u8],
    ) -> (ProductHttpClient, Arc<Mutex<Vec<ProductHttpRequest>>>) {
        let resolver = ConfiguredProductEndpointResolver::new(vec![ProductEndpointConfig::new(
            Owner::Navigator,
            "organization-1",
            "workspace-1",
            "https://navigator.test/",
            TEST_SERVICE_CREDENTIAL,
        )])
        .unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let client = ProductHttpClient::with_transport(
            Arc::new(resolver),
            Arc::new(MockTransport {
                requests: requests.clone(),
                response: Mutex::new(Some(ProductHttpResponse::new(
                    200,
                    ContentType::ApplicationJson,
                    response_body.to_vec(),
                ))),
            }),
        );
        (client, requests)
    }

    fn invocation(
        operation: Operation,
        kind: Kind,
        resource_id: Option<&str>,
        json_body: Vec<u8>,
    ) -> ProductInvocationRequest {
        ProductInvocationRequest {
            owner: Owner::Navigator,
            operation,
            kind,
            resource_id: resource_id.map(str::to_owned),
            json_body,
            idempotency_key: None,
        }
    }

    #[tokio::test]
    async fn snapshot_uses_fixed_post_route_and_preserves_approved_read_body() {
        let body = br#"{"workspaceId":"workspace-1","reads":[{"product":"CATALYST","path":"/internal/workspace/v1/datasets"},{"product":"ECHO","path":"/internal/workspace/v1/evaluation-suites/123e4567-e89b-12d3-a456-426614174000"},{"product":"EXCHANGE","path":"/api/v1/workspace/gateway-routes"},{"product":"REACTOR","path":"/internal/workspace/v1/model-imports"},{"product":"YIELD","path":"/internal/workspace/v1/training-drafts/123e4567-e89b-12d3-a456-426614174000"}]}"#;
        let response_body = br#"{"workspaceId":"workspace-1","status":"COMPLETE","views":[]}"#;
        let (client, requests) = mock_client(response_body);
        let caller = member_caller("workspace-1");
        let request = invocation(
            Operation::WorkspaceProductApiOperation11,
            Kind::Read,
            None,
            body.to_vec(),
        );

        let response = client
            .send(&caller, target(&caller, &request).unwrap(), &request)
            .await
            .unwrap();

        assert_eq!(response.status_code, 200);
        assert_eq!(response.json_body, response_body);
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].owner(), Owner::Navigator);
        assert_eq!(requests[0].method(), ProductHttpMethod::Post);
        assert_eq!(
            requests[0].url().as_str(),
            "https://navigator.test/api/v1/workspace-snapshots"
        );
        assert_eq!(requests[0].body(), body);
        assert_eq!(requests[0].idempotency_key(), None);
    }

    #[test]
    fn snapshot_rejects_unlisted_paths_and_workspace_override() {
        let caller = member_caller("workspace-1");
        for body in [
            br#"{"workspaceId":"workspace-1","reads":[{"product":"EXCHANGE","path":"/api/v1/api-keys"}]}"#.as_slice(),
            br#"{"workspaceId":"workspace-1","reads":[{"product":"EXCHANGE","path":"/api/v1/workspace/gateway-routes?limit=1"}]}"#.as_slice(),
            br#"{"workspaceId":"workspace-1","reads":[{"product":"YIELD","path":"/internal/workspace/v1/training-drafts/../actions/start"}]}"#.as_slice(),
            br#"{"workspaceId":"another-workspace","reads":[{"product":"EXCHANGE","path":"/api/v1/workspace/gateway-routes"}]}"#.as_slice(),
        ] {
            let request = invocation(
                Operation::WorkspaceProductApiOperation11,
                Kind::Read,
                None,
                body.to_vec(),
            );
            assert_eq!(
                target(&caller, &request),
                Err(ProductInvocationError::InvalidRequest)
            );
        }
    }

    #[tokio::test]
    async fn get_session_uses_trusted_workspace_and_one_encoded_session_segment() {
        let response_body = br#"{"meta":{},"session":{"header":{"id":"session-1"},"events":[]}}"#;
        let (client, requests) = mock_client(response_body);
        let caller = member_caller("workspace-1");
        let request = invocation(
            Operation::WorkspaceProductApiOperation12,
            Kind::Read,
            Some("session-1"),
            Vec::new(),
        );

        let response = client
            .send(&caller, target(&caller, &request).unwrap(), &request)
            .await
            .unwrap();

        assert_eq!(response.json_body, response_body);
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method(), ProductHttpMethod::Get);
        assert_eq!(
            requests[0].url().as_str(),
            "https://navigator.test/internal/workspace/v1/workspaces/workspace-1/sessions/session-1"
        );
        assert!(requests[0].body().is_empty());
    }

    #[test]
    fn append_events_fails_closed_before_transport_and_never_forwards_writer_token() {
        let caller = member_caller("workspace-1");
        let request = invocation(
            Operation::WorkspaceProductApiOperation13,
            Kind::Command,
            Some("session-1"),
            br#"{"writerToken":"browser-secret","epoch":1,"batchId":"batch-1","events":[{"type":"message"}]}"#.to_vec(),
        );

        assert_eq!(
            target(&caller, &request),
            Err(ProductInvocationError::PermissionDenied)
        );
    }
}
