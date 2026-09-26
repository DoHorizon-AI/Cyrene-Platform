//! Navigator Product HTTP route mappings.
//!
//! Navigator owns the observed aggregate and Harness session view. This
//! module maps only approved reads and fails closed on event append until a
//! trusted Harness writer handoff exists.

use cy_proto::workspace_v1::{
    WorkspaceProductApiOperation as Operation, WorkspaceProductApiOwner as Owner,
    WorkspaceProductApiRequestKind as Kind,
};
use serde_json::Value;

use crate::{
    ProductInvocationError, ProductInvocationRequest, WorkspaceCallerContext,
    PRODUCT_JSON_BODY_MAX_BYTES,
};

use super::http::{ProductHttpMethod, ProductHttpPathSegment, ProductHttpTarget};

const MAX_SNAPSHOT_WORKSPACE_ID_BYTES: usize = 200;
const MAX_SNAPSHOT_READS: usize = 50;

/// Maps Navigator's read operations to fixed Product OpenAPI v1 routes.
///
/// `observeWorkspaceSnapshot` accepts only the nested READ routes represented
/// in the closed projection manifest. `get_session` derives its Workspace path
/// from the authenticated caller context and treats `resource_id` as one safe
/// session segment.
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
                    ProductHttpPathSegment::Static("api"),
                    ProductHttpPathSegment::Static("v1"),
                    ProductHttpPathSegment::Static("harness"),
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
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return false;
    };
    let Some(object) = value.as_object() else {
        return false;
    };
    if object.len() != 2
        || object
            .get("workspaceId")
            .and_then(Value::as_str)
            .is_none_or(|requested_workspace| {
                requested_workspace != workspace_id
                    || requested_workspace.is_empty()
                    || requested_workspace.len() > MAX_SNAPSHOT_WORKSPACE_ID_BYTES
            })
    {
        return false;
    }
    let Some(reads) = object.get("reads").and_then(Value::as_array) else {
        return false;
    };
    if reads.is_empty() || reads.len() > MAX_SNAPSHOT_READS {
        return false;
    }

    reads.iter().all(|read| {
        let Some(read) = read.as_object() else {
            return false;
        };
        if read.len() != 2 {
            return false;
        }
        let Some(product) = read.get("product").and_then(Value::as_str) else {
            return false;
        };
        let Some(path) = read.get("path").and_then(Value::as_str) else {
            return false;
        };
        approved_nested_read(product, path)
    })
}

fn approved_nested_read(product: &str, path: &str) -> bool {
    match product {
        "CATALYST" => path == "/api/v1/datasets",
        "ECHO" => uuid_path(path, "/api/v1/evaluation-suites/"),
        "EXCHANGE" => path == "/api/v1/gateway-routes",
        "REACTOR" => path == "/api/v1/model-imports",
        "YIELD" => uuid_path(path, "/api/v1/training-drafts/"),
        _ => false,
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
        ProductHttpTransport,
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
            "private-test-credential",
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
        let body = br#"{"workspaceId":"workspace-1","reads":[{"product":"CATALYST","path":"/api/v1/datasets"},{"product":"ECHO","path":"/api/v1/evaluation-suites/123e4567-e89b-12d3-a456-426614174000"},{"product":"EXCHANGE","path":"/api/v1/gateway-routes"},{"product":"REACTOR","path":"/api/v1/model-imports"},{"product":"YIELD","path":"/api/v1/training-drafts/123e4567-e89b-12d3-a456-426614174000"}]}"#;
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
            br#"{"workspaceId":"workspace-1","reads":[{"product":"EXCHANGE","path":"/api/v1/gateway-routes?limit=1"}]}"#.as_slice(),
            br#"{"workspaceId":"workspace-1","reads":[{"product":"YIELD","path":"/api/v1/training-drafts/../actions/start"}]}"#.as_slice(),
            br#"{"workspaceId":"another-workspace","reads":[{"product":"EXCHANGE","path":"/api/v1/gateway-routes"}]}"#.as_slice(),
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
            "https://navigator.test/api/v1/harness/workspaces/workspace-1/sessions/session-1"
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
