//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 product_projection.rs                                           │
//! │  Module: cy_workspace_fabric::product_projection                   │
//! │  Role: Closed, bounded Product API projection and invocation port. │
//! │                                                                     │
//! │  模块职责：封闭且有界的 Product API 投影及调用 port。                   │
//! └─────────────────────────────────────────────────────────────────────┘
//!
//! This module maps only the operations listed in the Workspace Product API
//! TCK. Product services retain their domain data, command effects, and replay
//! authority. No caller-supplied URL, path, or HTTP method crosses this port.

use cy_proto::workspace_v1::{
    WorkspaceProductApiContentType, WorkspaceProductApiOperation, WorkspaceProductApiOwner,
    WorkspaceProductApiRequest, WorkspaceProductApiRequestKind, WorkspaceProductApiResponse,
};
use serde_json::Value;
use thiserror::Error;

use crate::product_authorization::{
    authorize_product_operation, ProductAuthorizationOperation, ProductAuthorizationPrincipal,
    ProductCommand, ProductReadOwner,
};
use crate::WorkspaceCallerContext;

/// Maximum Product request or response JSON body size.
pub const PRODUCT_JSON_BODY_MAX_BYTES: usize = 4 * 1024 * 1024;

/// Maximum encoded Workspace API gRPC message, including protobuf wrappers.
///
/// The extra 1 MiB is reserved for the enclosing Workspace/Relay messages and
/// authenticated connection metadata around a maximum-sized JSON body.
pub const WORKSPACE_API_GRPC_MESSAGE_MAX_BYTES: usize = 5 * 1024 * 1024;

const MAX_RESOURCE_ID_BYTES: usize = 512;
const MAX_IDEMPOTENCY_KEY_BYTES: usize = 256;
pub(crate) const MAX_WORKSPACE_REQUEST_ID_BYTES: usize = 128;
pub(crate) const MAX_WORKSPACE_ID_BYTES: usize = 512;

/// A validated invocation selected from the closed TCK mapping.
///
/// This type intentionally contains no URL, path, or HTTP method. An adapter
/// must map the typed owner and operation to its fixed Product contract route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductInvocationRequest {
    /// The Product service that owns the selected operation.
    pub owner: WorkspaceProductApiOwner,
    /// The stable Workspace wire operation key.
    pub operation: WorkspaceProductApiOperation,
    /// The operation's fixed READ or COMMAND classification.
    pub kind: WorkspaceProductApiRequestKind,
    /// Opaque Product resource identity, if the mapped operation requires it.
    pub resource_id: Option<String>,
    /// Original JSON body bytes, preserved without normalization.
    pub json_body: Vec<u8>,
    /// Optional owner-validated replay key.
    pub idempotency_key: Option<String>,
}

/// A bounded JSON response returned by the owning Product adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductInvocationResponse {
    /// Product-owned HTTP status preserved by the Workspace projection.
    pub status_code: u16,
    /// Original JSON response bytes, preserved without normalization.
    pub json_body: Vec<u8>,
    /// One of the two JSON media types admitted by the wire contract.
    pub content_type: WorkspaceProductApiContentType,
}

/// Fixed failures safe to expose from a Product invocation adapter.
///
/// Adapter diagnostics and credentials are never included in the Workspace
/// response; these variants map to fixed public status codes and messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ProductInvocationError {
    /// The request is invalid for the selected Product operation.
    #[error("WORKSPACE_PRODUCT_API_REQUEST_INVALID")]
    InvalidRequest,
    /// The authenticated caller is not authorized for the selected operation.
    #[error("WORKSPACE_PRODUCT_API_DENIED")]
    PermissionDenied,
    /// The Product-owned resource does not exist.
    #[error("WORKSPACE_PRODUCT_API_NOT_FOUND")]
    NotFound,
    /// The operation cannot be applied in the Product's current state.
    #[error("WORKSPACE_PRODUCT_API_FAILED_PRECONDITION")]
    FailedPrecondition,
    /// The Product adapter is not configured or is temporarily unavailable.
    #[error("WORKSPACE_PRODUCT_API_UNAVAILABLE")]
    Unavailable,
    /// The Product adapter returned an unsafe or malformed response.
    #[error("WORKSPACE_PRODUCT_API_FAILURE")]
    Internal,
}

impl ProductInvocationError {
    /// Returns the fixed Google RPC status projection for this failure.
    pub(crate) const fn rpc_status(self) -> (i32, &'static str) {
        match self {
            Self::InvalidRequest => (3, "WORKSPACE_PRODUCT_API_REQUEST_INVALID"),
            Self::PermissionDenied => (7, "WORKSPACE_PRODUCT_API_DENIED"),
            Self::NotFound => (5, "WORKSPACE_PRODUCT_API_NOT_FOUND"),
            Self::FailedPrecondition => (9, "WORKSPACE_PRODUCT_API_FAILED_PRECONDITION"),
            Self::Unavailable => (14, "WORKSPACE_PRODUCT_API_UNAVAILABLE"),
            Self::Internal => (13, "WORKSPACE_PRODUCT_API_FAILURE"),
        }
    }
}

/// Server-side Product invocation port.
///
/// Implementations receive authenticated caller context and one allowlisted
/// operation. They must use fixed owner routes and keep Product-owned state and
/// replay rules in the owner service. Responses must be public Product JSON;
/// they must not include Docker addresses, local filesystem paths, container
/// identifiers, internal Product URLs, or Navigator writer credentials.
#[tonic::async_trait]
pub trait ProductInvocationPort: Send + Sync + 'static {
    /// Invokes one validated owner operation without exposing transport routing
    /// or translating Product state into a Platform-owned store. For Navigator's
    /// append operation, the adapter must source writer credentials from trusted
    /// server configuration and must never accept or return them in JSON.
    async fn invoke(
        &self,
        caller: &WorkspaceCallerContext,
        request: ProductInvocationRequest,
    ) -> Result<ProductInvocationResponse, ProductInvocationError>;
}

/// Fail-closed port used until a Product adapter is explicitly injected.
pub(crate) struct UnconfiguredProductInvocationPort;

#[tonic::async_trait]
impl ProductInvocationPort for UnconfiguredProductInvocationPort {
    async fn invoke(
        &self,
        _caller: &WorkspaceCallerContext,
        _request: ProductInvocationRequest,
    ) -> Result<ProductInvocationResponse, ProductInvocationError> {
        Err(ProductInvocationError::Unavailable)
    }
}

/// Validates the owner/operation/kind triple against the frozen 13-row TCK.
///
/// # Errors
/// Returns [`ProductInvocationError::InvalidRequest`] for unknown enum values,
/// owner/kind mismatches, or oversized request fields.
pub(crate) fn validate_product_invocation(
    request: WorkspaceProductApiRequest,
) -> Result<ProductInvocationRequest, ProductInvocationError> {
    let owner = WorkspaceProductApiOwner::try_from(request.owner)
        .map_err(|_| ProductInvocationError::InvalidRequest)?;
    let operation = WorkspaceProductApiOperation::try_from(request.operation)
        .map_err(|_| ProductInvocationError::InvalidRequest)?;
    let kind = WorkspaceProductApiRequestKind::try_from(request.kind)
        .map_err(|_| ProductInvocationError::InvalidRequest)?;

    let (expected_owner, expected_kind, needs_resource_id) =
        operation_mapping(operation).ok_or(ProductInvocationError::InvalidRequest)?;
    if owner != expected_owner || kind != expected_kind {
        return Err(ProductInvocationError::InvalidRequest);
    }

    if request.resource_id.len() > MAX_RESOURCE_ID_BYTES
        || request.idempotency_key.len() > MAX_IDEMPOTENCY_KEY_BYTES
    {
        return Err(ProductInvocationError::InvalidRequest);
    }
    if !valid_resource_id(&request.resource_id)
        || request.idempotency_key.chars().any(char::is_control)
        || (!request.idempotency_key.is_empty() && request.idempotency_key.trim().is_empty())
    {
        return Err(ProductInvocationError::InvalidRequest);
    }
    if needs_resource_id && request.resource_id.is_empty() {
        return Err(ProductInvocationError::InvalidRequest);
    }
    if operation == WorkspaceProductApiOperation::WorkspaceProductApiOperation08
        && request.idempotency_key.is_empty()
    {
        return Err(ProductInvocationError::InvalidRequest);
    }
    if request.json_body.len() > PRODUCT_JSON_BODY_MAX_BYTES {
        return Err(ProductInvocationError::InvalidRequest);
    }

    Ok(ProductInvocationRequest {
        owner,
        operation,
        kind,
        resource_id: (!request.resource_id.is_empty()).then_some(request.resource_id),
        json_body: request.json_body,
        idempotency_key: (!request.idempotency_key.is_empty()).then_some(request.idempotency_key),
    })
}

/// Parses a bounded request body only after the caller passes authorization.
pub(crate) fn validate_product_request_body(
    request: &ProductInvocationRequest,
) -> Result<(), ProductInvocationError> {
    if !request.json_body.is_empty() {
        serde_json::from_slice::<Value>(&request.json_body)
            .map_err(|_| ProductInvocationError::InvalidRequest)?;
    }
    Ok(())
}

/// Authorizes a Product operation from the authenticated Workspace caller.
///
/// Directory membership allows reads. Each user command requires its own exact
/// versioned Directory role. Current Workload callers remain untrusted for
/// Navigator append until a separate typed server-side writer handoff exists.
pub(crate) fn authorize_product_invocation(
    caller: &WorkspaceCallerContext,
    workspace_id: &str,
    request: &ProductInvocationRequest,
) -> Result<(), ProductInvocationError> {
    if workspace_id.is_empty()
        || workspace_id.len() > MAX_WORKSPACE_ID_BYTES
        || caller.workspace_id() != workspace_id
    {
        return Err(ProductInvocationError::PermissionDenied);
    }

    let principal = match caller.principal() {
        crate::WorkspaceCallerPrincipal::User(_) => ProductAuthorizationPrincipal::DirectoryUser,
        crate::WorkspaceCallerPrincipal::Device(_) => {
            ProductAuthorizationPrincipal::WorkspaceDevice
        }
        crate::WorkspaceCallerPrincipal::Workload(_) => {
            ProductAuthorizationPrincipal::UntrustedWorkload
        }
    };
    authorize_product_operation(
        principal,
        caller.roles(),
        authorization_operation(request.operation),
    )
    .map_err(|_| ProductInvocationError::PermissionDenied)
}

fn authorization_operation(
    operation: WorkspaceProductApiOperation,
) -> ProductAuthorizationOperation {
    use WorkspaceProductApiOperation as Operation;

    match operation {
        Operation::WorkspaceProductApiOperation01 => {
            ProductAuthorizationOperation::Read(ProductReadOwner::Catalyst)
        }
        Operation::WorkspaceProductApiOperation02 => {
            ProductAuthorizationOperation::Command(ProductCommand::CatalystCreateDataset)
        }
        Operation::WorkspaceProductApiOperation03 => {
            ProductAuthorizationOperation::Read(ProductReadOwner::Yield)
        }
        Operation::WorkspaceProductApiOperation04 => {
            ProductAuthorizationOperation::Command(ProductCommand::YieldStartRun)
        }
        Operation::WorkspaceProductApiOperation05 => {
            ProductAuthorizationOperation::Read(ProductReadOwner::Reactor)
        }
        Operation::WorkspaceProductApiOperation06 => {
            ProductAuthorizationOperation::Command(ProductCommand::ReactorCreateModelImport)
        }
        Operation::WorkspaceProductApiOperation07 => {
            ProductAuthorizationOperation::Read(ProductReadOwner::Exchange)
        }
        Operation::WorkspaceProductApiOperation08 => {
            ProductAuthorizationOperation::Command(ProductCommand::ExchangeCreateRouteDraft)
        }
        Operation::WorkspaceProductApiOperation09 => {
            ProductAuthorizationOperation::Read(ProductReadOwner::Echo)
        }
        Operation::WorkspaceProductApiOperation10 => {
            ProductAuthorizationOperation::Command(ProductCommand::EchoCreateEvaluationSuite)
        }
        Operation::WorkspaceProductApiOperation11 | Operation::WorkspaceProductApiOperation12 => {
            ProductAuthorizationOperation::Read(ProductReadOwner::Navigator)
        }
        Operation::WorkspaceProductApiOperation13 => {
            ProductAuthorizationOperation::NavigatorHarnessAppendEvents
        }
        Operation::Unspecified => ProductAuthorizationOperation::Unmapped,
    }
}

/// Validates a Product response before returning it to the frontend.
///
/// # Errors
/// Returns [`ProductInvocationError::Internal`] for unsupported statuses,
/// media types, oversized bodies, invalid JSON, or inconsistent problem status.
pub(crate) fn validate_product_response(
    response: ProductInvocationResponse,
) -> Result<WorkspaceProductApiResponse, ProductInvocationError> {
    if !(200..=299).contains(&response.status_code) && !(400..=599).contains(&response.status_code)
    {
        return Err(ProductInvocationError::Internal);
    }
    if matches!(response.status_code, 204 | 205) {
        return Err(ProductInvocationError::Internal);
    }
    if response.json_body.is_empty() || response.json_body.len() > PRODUCT_JSON_BODY_MAX_BYTES {
        return Err(ProductInvocationError::Internal);
    }

    let content_type = match response.content_type {
        WorkspaceProductApiContentType::ApplicationJson => {
            WorkspaceProductApiContentType::ApplicationJson
        }
        WorkspaceProductApiContentType::ApplicationProblemJson => {
            let problem = serde_json::from_slice::<Value>(&response.json_body)
                .map_err(|_| ProductInvocationError::Internal)?;
            validate_problem_status(&problem, response.status_code)?;
            WorkspaceProductApiContentType::ApplicationProblemJson
        }
        WorkspaceProductApiContentType::Unspecified => {
            return Err(ProductInvocationError::Internal)
        }
    };
    serde_json::from_slice::<Value>(&response.json_body)
        .map_err(|_| ProductInvocationError::Internal)?;

    Ok(WorkspaceProductApiResponse {
        status_code: u32::from(response.status_code),
        json_body: response.json_body,
        content_type: content_type as i32,
    })
}

fn validate_problem_status(
    problem: &Value,
    response_status: u16,
) -> Result<(), ProductInvocationError> {
    let Some(problem) = problem.as_object() else {
        return Err(ProductInvocationError::Internal);
    };
    for field in ["type", "title", "detail", "instance"] {
        if problem.get(field).is_some_and(|value| !value.is_string()) {
            return Err(ProductInvocationError::Internal);
        }
    }
    if problem
        .get("status")
        .is_some_and(|status| status.as_u64() != Some(u64::from(response_status)))
    {
        return Err(ProductInvocationError::Internal);
    }
    Ok(())
}

fn operation_mapping(
    operation: WorkspaceProductApiOperation,
) -> Option<(
    WorkspaceProductApiOwner,
    WorkspaceProductApiRequestKind,
    bool,
)> {
    use WorkspaceProductApiOperation as Operation;
    use WorkspaceProductApiOwner as Owner;
    use WorkspaceProductApiRequestKind as Kind;

    let mapping = match operation {
        Operation::WorkspaceProductApiOperation01 => (Owner::Catalyst, Kind::Read, false),
        Operation::WorkspaceProductApiOperation02 => (Owner::Catalyst, Kind::Command, false),
        Operation::WorkspaceProductApiOperation03 => (Owner::Yield, Kind::Read, true),
        Operation::WorkspaceProductApiOperation04 => (Owner::Yield, Kind::Command, true),
        Operation::WorkspaceProductApiOperation05 => (Owner::Reactor, Kind::Read, false),
        Operation::WorkspaceProductApiOperation06 => (Owner::Reactor, Kind::Command, false),
        Operation::WorkspaceProductApiOperation07 => (Owner::Exchange, Kind::Read, false),
        Operation::WorkspaceProductApiOperation08 => (Owner::Exchange, Kind::Command, false),
        Operation::WorkspaceProductApiOperation09 => (Owner::Echo, Kind::Read, false),
        Operation::WorkspaceProductApiOperation10 => (Owner::Echo, Kind::Command, false),
        Operation::WorkspaceProductApiOperation11 => (Owner::Navigator, Kind::Read, false),
        Operation::WorkspaceProductApiOperation12 => (Owner::Navigator, Kind::Read, true),
        Operation::WorkspaceProductApiOperation13 => (Owner::Navigator, Kind::Command, true),
        Operation::Unspecified => return None,
    };
    Some(mapping)
}

fn valid_resource_id(resource_id: &str) -> bool {
    !resource_id.chars().any(|character| {
        character.is_control()
            || character.is_whitespace()
            || matches!(character, '/' | '\\' | '?' | '#' | '%' | ':')
    }) && !matches!(resource_id, "." | "..")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::product_authorization::{
        CATALYST_CREATE_DATASET_ROLE, ECHO_CREATE_EVALUATION_SUITE_ROLE,
        EXCHANGE_CREATE_ROUTE_DRAFT_ROLE, REACTOR_CREATE_MODEL_IMPORT_ROLE, WORKSPACE_MEMBER_ROLE,
        YIELD_START_RUN_ROLE,
    };
    use crate::{WorkspaceCallerContext, WorkspaceCallerPrincipal};
    use cy_proto::core_v1::{AccountScope, WorkloadIdentity};
    use cy_proto::semantic_v1::Identity;
    use cy_proto::workspace_v1::{
        relay_frame, RelayForwardedRequest, RelayFrame, RelayHello, RelayParticipantRole,
        UserIdentityRef, WorkspaceApiRequest, WorkspaceDirectRequest,
    };
    use std::collections::BTreeSet;

    fn request(
        owner: WorkspaceProductApiOwner,
        operation: WorkspaceProductApiOperation,
        kind: WorkspaceProductApiRequestKind,
    ) -> WorkspaceProductApiRequest {
        WorkspaceProductApiRequest {
            owner: owner as i32,
            operation: operation as i32,
            kind: kind as i32,
            resource_id: String::new(),
            json_body: Vec::new(),
            idempotency_key: String::new(),
        }
    }

    fn user_caller(roles: &[&str]) -> WorkspaceCallerContext {
        WorkspaceCallerContext::from_verified(
            WorkspaceCallerPrincipal::User(UserIdentityRef {
                issuer: "https://identity.test".to_string(),
                subject: "user-1".to_string(),
            }),
            "organization-1",
            "workspace-1",
            roles.iter().map(|role| (*role).to_string()).collect(),
        )
        .expect("valid caller context")
    }

    fn invocation(operation: WorkspaceProductApiOperation) -> ProductInvocationRequest {
        let (owner, kind, needs_resource_id) =
            operation_mapping(operation).expect("mapped Product operation");
        let mut input = request(owner, operation, kind);
        if needs_resource_id {
            input.resource_id = "owner-issued-id".to_string();
        }
        if operation == WorkspaceProductApiOperation::WorkspaceProductApiOperation08 {
            input.idempotency_key = "exchange-command-key".to_string();
        }
        validate_product_invocation(input).expect("valid mapped operation")
    }

    fn navigator_workload_caller() -> WorkspaceCallerContext {
        WorkspaceCallerContext::from_verified(
            WorkspaceCallerPrincipal::Workload(WorkloadIdentity {
                identity: Some(Identity {
                    id: "navigator-writer-1".to_string(),
                    generation: 1,
                }),
                scope: Some(AccountScope {
                    user_id: String::new(),
                    organization_id: "organization-1".to_string(),
                    workspace_id: "workspace-1".to_string(),
                }),
                runtime: None,
                allowed_actions: vec!["navigator.append_events".to_string()],
                expires_at: None,
            }),
            "organization-1",
            "workspace-1",
            BTreeSet::from([crate::NAVIGATOR_SERVICE_WRITER_ROLE.to_string()]),
        )
        .expect("valid workload caller context")
    }

    #[test]
    fn directory_membership_allows_reads_but_membership_alone_denies_commands() {
        let member = user_caller(&[WORKSPACE_MEMBER_ROLE]);
        for operation in [
            WorkspaceProductApiOperation::WorkspaceProductApiOperation01,
            WorkspaceProductApiOperation::WorkspaceProductApiOperation03,
            WorkspaceProductApiOperation::WorkspaceProductApiOperation05,
            WorkspaceProductApiOperation::WorkspaceProductApiOperation07,
            WorkspaceProductApiOperation::WorkspaceProductApiOperation09,
            WorkspaceProductApiOperation::WorkspaceProductApiOperation11,
            WorkspaceProductApiOperation::WorkspaceProductApiOperation12,
        ] {
            assert_eq!(
                authorize_product_invocation(&member, "workspace-1", &invocation(operation)),
                Ok(())
            );
        }

        for operation in [
            WorkspaceProductApiOperation::WorkspaceProductApiOperation02,
            WorkspaceProductApiOperation::WorkspaceProductApiOperation04,
            WorkspaceProductApiOperation::WorkspaceProductApiOperation06,
            WorkspaceProductApiOperation::WorkspaceProductApiOperation08,
            WorkspaceProductApiOperation::WorkspaceProductApiOperation10,
            WorkspaceProductApiOperation::WorkspaceProductApiOperation13,
        ] {
            assert_eq!(
                authorize_product_invocation(&member, "workspace-1", &invocation(operation)),
                Err(ProductInvocationError::PermissionDenied)
            );
        }

        let nonmember = user_caller(&[]);
        assert_eq!(
            authorize_product_invocation(
                &nonmember,
                "workspace-1",
                &invocation(WorkspaceProductApiOperation::WorkspaceProductApiOperation01),
            ),
            Err(ProductInvocationError::PermissionDenied)
        );
    }

    #[test]
    fn each_versioned_command_role_grants_only_its_exact_operation() {
        let commands = [
            (
                WorkspaceProductApiOperation::WorkspaceProductApiOperation02,
                CATALYST_CREATE_DATASET_ROLE,
            ),
            (
                WorkspaceProductApiOperation::WorkspaceProductApiOperation04,
                YIELD_START_RUN_ROLE,
            ),
            (
                WorkspaceProductApiOperation::WorkspaceProductApiOperation06,
                REACTOR_CREATE_MODEL_IMPORT_ROLE,
            ),
            (
                WorkspaceProductApiOperation::WorkspaceProductApiOperation08,
                EXCHANGE_CREATE_ROUTE_DRAFT_ROLE,
            ),
            (
                WorkspaceProductApiOperation::WorkspaceProductApiOperation10,
                ECHO_CREATE_EVALUATION_SUITE_ROLE,
            ),
        ];

        for (authorized_operation, authorized_role) in commands {
            let caller = user_caller(&[WORKSPACE_MEMBER_ROLE, authorized_role]);
            for (operation, _) in commands {
                let expected = if operation == authorized_operation {
                    Ok(())
                } else {
                    Err(ProductInvocationError::PermissionDenied)
                };
                assert_eq!(
                    authorize_product_invocation(&caller, "workspace-1", &invocation(operation)),
                    expected,
                    "role {authorized_role} must be scoped to {authorized_operation:?}"
                );
            }
        }

        let no_roles = user_caller(&[]);
        for (operation, _) in commands {
            assert_eq!(
                authorize_product_invocation(&no_roles, "workspace-1", &invocation(operation)),
                Err(ProductInvocationError::PermissionDenied)
            );
        }
    }

    #[test]
    fn current_workload_identity_cannot_append_with_a_writer_role_string() {
        let caller = navigator_workload_caller();
        assert_eq!(
            authorize_product_invocation(
                &caller,
                "workspace-1",
                &invocation(WorkspaceProductApiOperation::WorkspaceProductApiOperation13),
            ),
            Err(ProductInvocationError::PermissionDenied)
        );
    }

    #[test]
    fn validates_the_closed_owner_operation_kind_table() {
        for (owner, operation, kind, resource_id) in [
            (
                WorkspaceProductApiOwner::Catalyst,
                WorkspaceProductApiOperation::WorkspaceProductApiOperation01,
                WorkspaceProductApiRequestKind::Read,
                false,
            ),
            (
                WorkspaceProductApiOwner::Catalyst,
                WorkspaceProductApiOperation::WorkspaceProductApiOperation02,
                WorkspaceProductApiRequestKind::Command,
                false,
            ),
            (
                WorkspaceProductApiOwner::Yield,
                WorkspaceProductApiOperation::WorkspaceProductApiOperation03,
                WorkspaceProductApiRequestKind::Read,
                true,
            ),
            (
                WorkspaceProductApiOwner::Yield,
                WorkspaceProductApiOperation::WorkspaceProductApiOperation04,
                WorkspaceProductApiRequestKind::Command,
                true,
            ),
            (
                WorkspaceProductApiOwner::Reactor,
                WorkspaceProductApiOperation::WorkspaceProductApiOperation05,
                WorkspaceProductApiRequestKind::Read,
                false,
            ),
            (
                WorkspaceProductApiOwner::Reactor,
                WorkspaceProductApiOperation::WorkspaceProductApiOperation06,
                WorkspaceProductApiRequestKind::Command,
                false,
            ),
            (
                WorkspaceProductApiOwner::Exchange,
                WorkspaceProductApiOperation::WorkspaceProductApiOperation07,
                WorkspaceProductApiRequestKind::Read,
                false,
            ),
            (
                WorkspaceProductApiOwner::Exchange,
                WorkspaceProductApiOperation::WorkspaceProductApiOperation08,
                WorkspaceProductApiRequestKind::Command,
                false,
            ),
            (
                WorkspaceProductApiOwner::Echo,
                WorkspaceProductApiOperation::WorkspaceProductApiOperation09,
                WorkspaceProductApiRequestKind::Read,
                false,
            ),
            (
                WorkspaceProductApiOwner::Echo,
                WorkspaceProductApiOperation::WorkspaceProductApiOperation10,
                WorkspaceProductApiRequestKind::Command,
                false,
            ),
            (
                WorkspaceProductApiOwner::Navigator,
                WorkspaceProductApiOperation::WorkspaceProductApiOperation11,
                WorkspaceProductApiRequestKind::Read,
                false,
            ),
            (
                WorkspaceProductApiOwner::Navigator,
                WorkspaceProductApiOperation::WorkspaceProductApiOperation12,
                WorkspaceProductApiRequestKind::Read,
                true,
            ),
            (
                WorkspaceProductApiOwner::Navigator,
                WorkspaceProductApiOperation::WorkspaceProductApiOperation13,
                WorkspaceProductApiRequestKind::Command,
                true,
            ),
        ] {
            let mut input = request(owner, operation, kind);
            if resource_id {
                input.resource_id = "owner-issued-id".to_string();
            }
            if operation == WorkspaceProductApiOperation::WorkspaceProductApiOperation08 {
                input.idempotency_key = "exchange-command-key".to_string();
            }
            let validated = validate_product_invocation(input).expect("TCK mapping is valid");
            assert_eq!(validated.owner, owner);
            assert_eq!(validated.operation, operation);
            assert_eq!(validated.kind, kind);
        }

        let wrong_owner = request(
            WorkspaceProductApiOwner::Echo,
            WorkspaceProductApiOperation::WorkspaceProductApiOperation01,
            WorkspaceProductApiRequestKind::Read,
        );
        assert_eq!(
            validate_product_invocation(wrong_owner),
            Err(ProductInvocationError::InvalidRequest)
        );

        let wrong_kind = request(
            WorkspaceProductApiOwner::Catalyst,
            WorkspaceProductApiOperation::WorkspaceProductApiOperation01,
            WorkspaceProductApiRequestKind::Command,
        );
        assert_eq!(
            validate_product_invocation(wrong_kind),
            Err(ProductInvocationError::InvalidRequest)
        );
    }

    #[test]
    fn bounds_and_parses_request_json_without_rewriting_it() {
        let mut input = request(
            WorkspaceProductApiOwner::Catalyst,
            WorkspaceProductApiOperation::WorkspaceProductApiOperation02,
            WorkspaceProductApiRequestKind::Command,
        );
        input.json_body = br#"{"name":"dataset"}"#.to_vec();
        input.idempotency_key = "idempotency-1".to_string();
        let validated = validate_product_invocation(input).expect("bounded request body");
        validate_product_request_body(&validated).expect("valid JSON body");
        assert_eq!(validated.json_body, br#"{"name":"dataset"}"#);
        assert_eq!(validated.idempotency_key.as_deref(), Some("idempotency-1"));

        let mut invalid_json = request(
            WorkspaceProductApiOwner::Catalyst,
            WorkspaceProductApiOperation::WorkspaceProductApiOperation02,
            WorkspaceProductApiRequestKind::Command,
        );
        invalid_json.json_body = b"not-json".to_vec();
        let invalid_json = validate_product_invocation(invalid_json).expect("bounded JSON bytes");
        assert_eq!(
            validate_product_request_body(&invalid_json),
            Err(ProductInvocationError::InvalidRequest)
        );

        let empty_body = validate_product_invocation(request(
            WorkspaceProductApiOwner::Catalyst,
            WorkspaceProductApiOperation::WorkspaceProductApiOperation02,
            WorkspaceProductApiRequestKind::Command,
        ))
        .expect("empty optional body");
        assert!(validate_product_request_body(&empty_body).is_ok());

        let mut oversized = request(
            WorkspaceProductApiOwner::Catalyst,
            WorkspaceProductApiOperation::WorkspaceProductApiOperation02,
            WorkspaceProductApiRequestKind::Command,
        );
        oversized.json_body = vec![b' '; PRODUCT_JSON_BODY_MAX_BYTES + 1];
        assert_eq!(
            validate_product_invocation(oversized),
            Err(ProductInvocationError::InvalidRequest)
        );
    }

    #[test]
    fn validates_response_status_media_type_and_problem_status() {
        let response = ProductInvocationResponse {
            status_code: 201,
            json_body: br#"{"id":"owner-issued-id"}"#.to_vec(),
            content_type: WorkspaceProductApiContentType::ApplicationJson,
        };
        let projected = validate_product_response(response).expect("safe JSON response");
        assert_eq!(projected.status_code, 201);
        assert_eq!(projected.json_body, br#"{"id":"owner-issued-id"}"#);

        let mismatched_problem = ProductInvocationResponse {
            status_code: 422,
            json_body: br#"{"title":"invalid","status":500}"#.to_vec(),
            content_type: WorkspaceProductApiContentType::ApplicationProblemJson,
        };
        assert_eq!(
            validate_product_response(mismatched_problem),
            Err(ProductInvocationError::Internal)
        );

        let redirect = ProductInvocationResponse {
            status_code: 302,
            json_body: br#"{"title":"redirect"}"#.to_vec(),
            content_type: WorkspaceProductApiContentType::ApplicationJson,
        };
        assert_eq!(
            validate_product_response(redirect),
            Err(ProductInvocationError::Internal)
        );
    }

    #[test]
    fn message_limit_leaves_room_for_a_maximum_body_and_wire_wrappers() {
        let mut input = request(
            WorkspaceProductApiOwner::Catalyst,
            WorkspaceProductApiOperation::WorkspaceProductApiOperation02,
            WorkspaceProductApiRequestKind::Command,
        );
        input.json_body = Vec::with_capacity(PRODUCT_JSON_BODY_MAX_BYTES);
        input.json_body.push(b'"');
        input
            .json_body
            .extend(std::iter::repeat_n(b'x', PRODUCT_JSON_BODY_MAX_BYTES - 2));
        input.json_body.push(b'"');
        input.resource_id = "i".repeat(MAX_RESOURCE_ID_BYTES);
        input.idempotency_key = "k".repeat(MAX_IDEMPOTENCY_KEY_BYTES);
        let api_request = WorkspaceApiRequest {
            request_id: "r".repeat(MAX_WORKSPACE_REQUEST_ID_BYTES),
            workspace_id: "w".repeat(MAX_WORKSPACE_ID_BYTES),
            traceparent: "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01".to_string(),
            request: Some(
                cy_proto::workspace_v1::workspace_api_request::Request::ProductApi(input),
            ),
        };
        let api_len = prost::Message::encoded_len(&api_request);
        assert!(api_len > PRODUCT_JSON_BODY_MAX_BYTES);
        assert!(api_len < WORKSPACE_API_GRPC_MESSAGE_MAX_BYTES);

        let direct_request = WorkspaceDirectRequest {
            frontend: Some(RelayHello {
                role: RelayParticipantRole::Frontend as i32,
                session_credential: "s".repeat(8 * 1024),
                user: Some(UserIdentityRef {
                    issuer: "i".repeat(256),
                    subject: "u".repeat(256),
                }),
                organization_id: "o".repeat(256),
                workspace_id: "w".repeat(MAX_WORKSPACE_ID_BYTES),
                device: None,
            }),
            request: Some(api_request.clone()),
        };
        assert!(
            prost::Message::encoded_len(&direct_request) < WORKSPACE_API_GRPC_MESSAGE_MAX_BYTES
        );

        let relay_frame = RelayFrame {
            frame_id: "f".repeat(MAX_WORKSPACE_REQUEST_ID_BYTES),
            body: Some(relay_frame::Body::ForwardedRequest(RelayForwardedRequest {
                frontend_session_id: "s".repeat(MAX_WORKSPACE_REQUEST_ID_BYTES),
                request: Some(api_request),
                caller_principal: None,
                caller_organization_id: "o".repeat(256),
                caller_workspace_id: "w".repeat(MAX_WORKSPACE_ID_BYTES),
                caller_roles: vec!["r".repeat(256)],
            })),
        };
        assert!(prost::Message::encoded_len(&relay_frame) < WORKSPACE_API_GRPC_MESSAGE_MAX_BYTES);
    }
}
