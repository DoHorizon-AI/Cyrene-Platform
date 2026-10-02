//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  Module: cy_workspace_control_plane::authority::outbox             │
//! │  Role: Typed persistence boundary for approved invocation lifecycle.│
//! │                                                                     │
//! │  模块职责：批准调用状态机的强类型持久化边界。                          │
//! └─────────────────────────────────────────────────────────────────────┘

use async_trait::async_trait;
use cy_proto::cyrene::workspace::authority::v2::{
    ApprovedInvocation, ExecutionCredential, ExecutionOutcomeStatus, InvocationState,
};
use cy_proto::cyrene::workspace::product::v2::ProductApiResponseV2;
use std::time::Duration;
use thiserror::Error;
use uuid::Uuid;

/// Current registry identity of the Connector allowed to claim one execution target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionDevicePeer {
    /// Directory-owned Organization of the connected Connector.
    pub organization_id: String,
    /// Directory-owned Workspace of the connected Connector.
    pub workspace_id: String,
    /// Device identifier resolved from the authenticated peer certificate.
    pub device_id: String,
    /// Stable Directory authorization record UUID owning the peer certificate.
    pub authorization_id: Uuid,
    /// Current registry authorization generation for the peer certificate.
    pub authorization_generation: u64,
    /// SHA-256 fingerprint of the authenticated TLS leaf certificate.
    pub certificate_fingerprint_sha256: [u8; 32],
}

/// Approved invocation plus the exact canonical envelope bytes used to sign it.
#[derive(Clone, Debug)]
pub struct NewAuthorityInvocation {
    /// Full RPC projection, including its canonical envelope and signed credential.
    pub approved_invocation: ApprovedInvocation,
    /// Deterministic protobuf bytes of CanonicalInvocationEnvelope.
    pub canonical_envelope_proto: Vec<u8>,
    /// SHA-256 of the Authority-signed target-binding import used for this target.
    pub target_binding_manifest_sha256: [u8; 32],
    /// SHA-256 of the immutable Product contract-bundle manifest.
    pub bundle_manifest_sha256: [u8; 32],
    /// Source commit pinned for the operation owner in the active snapshot.
    pub owner_source_commit: String,
}

/// Durable record returned only after the outbox has committed its state transition.
#[derive(Clone, Debug)]
pub struct AuthorityInvocationRecord {
    /// Canonical persisted invocation and current opaque credential.
    pub approved_invocation: ApprovedInvocation,
    /// State read from durable outbox storage.
    pub state: InvocationState,
    /// Persisted Product result, present only after accepted result submission.
    pub result: Option<PersistedInvocationResult>,
}

/// Atomic enqueue disposition for an idempotency-keyed request.
#[derive(Clone, Debug)]
pub enum EnqueueOutcome {
    /// The transaction inserted this invocation and its immutable approval envelope.
    Inserted(AuthorityInvocationRecord),
    /// The idempotency key resolved to this complete, previously persisted invocation.
    Existing(AuthorityInvocationRecord),
}

/// Result payload persisted after Authority response-contract and scope validation.
#[derive(Clone, Debug)]
pub struct PersistedInvocationResult {
    /// Stable outbox invocation id.
    pub invocation_id: String,
    /// Credential that binds this result to its complete approved invocation.
    pub credential: ExecutionCredential,
    /// Execution disposition reported by the authenticated Connector.
    pub outcome_status: ExecutionOutcomeStatus,
    /// Product response bytes and status, if the Connector reported a response.
    pub product_response: Option<ProductApiResponseV2>,
    /// SHA-256 of the exact result payload envelope.
    pub result_digest_sha256: Vec<u8>,
    /// Stable failure code, empty for a successful result.
    pub error_code: String,
    /// Bounded, non-secret error description.
    pub error_message: String,
    /// Receipt proving the result came from the active delivery attempt.
    pub delivery_receipt: Vec<u8>,
    /// Stable Authority receipt returned only after the result transaction commits.
    pub result_receipt: Vec<u8>,
}

/// Result of attempting to durably accept a Connector submission.
#[derive(Clone, Debug)]
pub struct SubmitResultOutcome {
    /// Whether the exact submission was accepted or an identical replay was already stored.
    pub accepted: bool,
    /// Current durable invocation state.
    pub state: InvocationState,
    /// Stable receipt returned only after durable persistence.
    pub result_receipt: Vec<u8>,
}

/// Identity and session scope required to read a result from the user-facing wait RPC.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvocationWaitScope {
    /// Directory-derived organization id.
    pub organization_id: String,
    /// Directory-derived workspace id.
    pub workspace_id: String,
    /// OIDC issuer from the independently verified access token.
    pub principal_issuer: String,
    /// OIDC subject from the independently verified access token.
    pub principal_subject: String,
    /// Persistent authority session id for the exact bearer.
    pub session_id: String,
    /// Persistent authority session generation.
    pub session_generation: u64,
}

/// Fail-closed failures from the Authority invocation store.
#[derive(Debug, Error)]
pub enum AuthorityOutboxError {
    /// The idempotency key is already bound to a different immutable envelope.
    #[error("idempotency key conflicts with a different invocation")]
    IdempotencyConflict,
    /// The web session or execution device was revoked or advanced during the transaction.
    #[error("invocation authorization fence is no longer current")]
    AuthorizationRevoked,
    /// A durable outbox record is invalid or its state transition is not allowed.
    #[error("invocation state transition is invalid")]
    InvalidState,
    /// The requested invocation does not exist in the caller's scope.
    #[error("invocation was not found in the requested scope")]
    NotFound,
    /// Persistent outbox data or its canonical envelope is invalid.
    #[error("persisted invocation data is invalid")]
    InvalidRecord,
    /// The database or durable store is unavailable.
    #[error("invocation store is unavailable")]
    Unavailable,
}

/// Durable transaction boundary for authorization, dispatch, and result state.
#[async_trait]
pub trait AuthorityOutboxStore: Send + Sync {
    /// Atomically checks live session/device generations and inserts the exact signed envelope.
    async fn enqueue_authorized(
        &self,
        invocation: NewAuthorityInvocation,
    ) -> Result<EnqueueOutcome, AuthorityOutboxError>;

    /// Claims pending work only for the TLS-authenticated target Connector identity.
    async fn claim_for_execution_device(
        &self,
        peer: &ExecutionDevicePeer,
        max_batch_size: usize,
    ) -> Result<Vec<AuthorityInvocationRecord>, AuthorityOutboxError>;

    /// Re-reads and verifies a persisted approval against the current Connector identity.
    async fn validate_execution_credential(
        &self,
        peer: &ExecutionDevicePeer,
        credential: &ExecutionCredential,
    ) -> Result<AuthorityInvocationRecord, AuthorityOutboxError>;

    /// Acknowledges one persisted delivery attempt for its exact authenticated Connector.
    async fn acknowledge_delivery(
        &self,
        peer: &ExecutionDevicePeer,
        invocation_id: &str,
        credential: &ExecutionCredential,
        delivery_receipt: &[u8],
    ) -> Result<bool, AuthorityOutboxError>;

    /// Atomically rechecks session/device revocation and persists a validated result.
    async fn submit_result(
        &self,
        peer: &ExecutionDevicePeer,
        result: PersistedInvocationResult,
    ) -> Result<SubmitResultOutcome, AuthorityOutboxError>;

    /// Waits for a scoped result without holding database locks between reads.
    async fn wait_for_result(
        &self,
        invocation_id: &str,
        scope: &InvocationWaitScope,
        timeout: Duration,
    ) -> Result<Option<AuthorityInvocationRecord>, AuthorityOutboxError>;
}
