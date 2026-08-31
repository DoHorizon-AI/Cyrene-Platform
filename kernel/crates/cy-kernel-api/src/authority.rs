// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: kernel/crates/cy-kernel-api/src/authority.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! 内核稳定权威端口 (Kernel Authority Port).

use cy_kernel_contract as semantic;

pub const DEFAULT_NAMESPACE: &str = "default";

/// A validated scope for authority-owned objects. It is intentionally distinct
/// from the semantic v1 `Identity`, which remains version-frozen.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct NamespaceId(String);

impl NamespaceId {
    pub fn new(value: impl Into<String>) -> Result<Self, semantic::Rejection> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 128
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return Err(semantic::Rejection::new(
                "NAMESPACE_INVALID",
                "namespace must contain 1..=128 ASCII alphanumeric, '.', '_' or '-' bytes",
            ));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for NamespaceId {
    fn default() -> Self {
        Self(DEFAULT_NAMESPACE.to_string())
    }
}

/// A typed semantic identity scoped to a namespace. Transport projections use
/// this rather than encoding scope into caller-provided identity strings.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ObjectRef {
    pub namespace: NamespaceId,
    pub identity: semantic::Identity,
}

/// Transport-neutral call metadata required for every state-changing authority
/// action after contract negotiation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorityCallContext {
    pub contract: semantic::ContractRevision,
    pub namespace: NamespaceId,
    pub request_id: String,
    pub idempotency_key: String,
}

impl AuthorityCallContext {
    pub fn validate_for(
        &self,
        local: &semantic::ContractRevision,
    ) -> Result<(), semantic::Rejection> {
        if local.negotiate(&self.contract).as_ref() != Some(&self.contract) {
            return Err(semantic::Rejection::new(
                "CONTRACT_INCOMPATIBLE",
                "authority call did not use a revision selected by this Kernel",
            ));
        }
        if self.request_id.is_empty() && self.idempotency_key.is_empty() {
            return Err(semantic::Rejection::new(
                "REQUEST_ID_REQUIRED",
                "authority call requires request_id or idempotency_key",
            ));
        }
        NamespaceId::new(self.namespace.as_str())?;
        Ok(())
    }

    pub fn object_ref(&self, identity: semantic::Identity) -> ObjectRef {
        ObjectRef {
            namespace: self.namespace.clone(),
            identity,
        }
    }

    pub fn effective_idempotency_key(&self) -> &str {
        if self.idempotency_key.is_empty() {
            &self.request_id
        } else {
            &self.idempotency_key
        }
    }
}

/// Stable authority port implemented by a Kernel generation and projected by
/// UDS/gRPC, C ABI and JVM clients. Principal identity is supplied by an
/// authenticated transport adapter, never trusted from caller-controlled data.
pub trait KernelAuthority: Send + Sync {
    /// Negotiate semantic compatibility independently from transport/schema
    /// versions. Failure is fail-closed before any state-changing action.
    fn negotiate(
        &self,
        principal: &semantic::Principal,
        offered: &[semantic::ContractRevision],
    ) -> Result<semantic::ContractRevision, semantic::Rejection>;

    /// Return the semantic contract revision implemented by this authority.
    fn contract_revision(&self) -> semantic::ContractRevision {
        semantic::ContractRevision::current()
    }

    /// Atomically acquire resources for a holder under the authenticated
    /// principal. Policy must already be compiled into the bounded query.
    fn acquire_lease(
        &self,
        context: &AuthorityCallContext,
        principal: &semantic::Principal,
        holder: semantic::Identity,
        query: semantic::ResourceQuery,
        expires_at_unix_ms: u64,
    ) -> Result<semantic::Lease, semantic::Rejection>;

    /// Renew an active lease without changing its resource authority.
    fn renew_lease(
        &self,
        context: &AuthorityCallContext,
        principal: &semantic::Principal,
        lease: &semantic::Identity,
        fence_token: u64,
        expires_at_unix_ms: u64,
    ) -> Result<semantic::Lease, semantic::Rejection>;

    /// Revoke or release a lease using its current fencing authority.
    fn release_lease(
        &self,
        context: &AuthorityCallContext,
        principal: &semantic::Principal,
        lease: &semantic::Identity,
        fence_token: u64,
    ) -> Result<semantic::Lease, semantic::Rejection>;

    /// Start a Worker from an immutable externally verified execution
    /// reference. The implementation delegates process work to a Provider.
    fn start_worker(
        &self,
        context: &AuthorityCallContext,
        principal: &semantic::Principal,
        worker: semantic::Worker,
    ) -> Result<semantic::Operation, semantic::Rejection>;

    /// Drain or stop one Worker using its generation and lease fence.
    fn stop_worker(
        &self,
        context: &AuthorityCallContext,
        principal: &semantic::Principal,
        worker: &semantic::Identity,
        lease: &semantic::Identity,
        fence_token: u64,
        grace_period: std::time::Duration,
    ) -> Result<semantic::Operation, semantic::Rejection>;

    /// Record liveness for the exact Worker/Lease generation and fence.
    fn heartbeat_worker(
        &self,
        context: &AuthorityCallContext,
        principal: &semantic::Principal,
        worker: &semantic::Identity,
        lease: &semantic::Identity,
        fence_token: u64,
    ) -> Result<semantic::Worker, semantic::Rejection>;

    /// Create one bounded operation owned by the authenticated Principal.
    fn create_operation(
        &self,
        context: &AuthorityCallContext,
        principal: &semantic::Principal,
        operation: semantic::Operation,
    ) -> Result<semantic::Operation, semantic::Rejection>;

    /// Apply an idempotent or legal forward-only Operation report.
    fn report_operation(
        &self,
        context: &AuthorityCallContext,
        principal: &semantic::Principal,
        operation: semantic::Operation,
    ) -> Result<semantic::Operation, semantic::Rejection>;

    /// Cancel a managed Operation using its current identity generation.
    fn cancel_operation(
        &self,
        context: &AuthorityCallContext,
        principal: &semantic::Principal,
        operation: &semantic::Identity,
    ) -> Result<semantic::Operation, semantic::Rejection>;

    /// Publish Endpoint authorization metadata. Data remains outside Kernel.
    fn publish_endpoint(
        &self,
        context: &AuthorityCallContext,
        principal: &semantic::Principal,
        endpoint: semantic::Endpoint,
    ) -> Result<semantic::Endpoint, semantic::Rejection>;

    /// Authorize one grantee while its lease/fence authority remains valid.
    fn authorize_endpoint(
        &self,
        context: &AuthorityCallContext,
        principal: &semantic::Principal,
        grant: semantic::EndpointGrant,
    ) -> Result<semantic::EndpointGrant, semantic::Rejection>;

    /// Revoke a previously issued Endpoint grant.
    fn revoke_endpoint(
        &self,
        context: &AuthorityCallContext,
        principal: &semantic::Principal,
        grant: &semantic::Identity,
    ) -> Result<(), semantic::Rejection>;

    /// Replay immutable facts after a cursor scoped to one authority
    /// incarnation. A source mismatch requires a fresh snapshot/reconcile.
    fn events_after(
        &self,
        context: &AuthorityCallContext,
        principal: &semantic::Principal,
        cursor: &semantic::EventCursor,
        limit: usize,
    ) -> Result<semantic::EventPage, semantic::Rejection>;
}

/// Internal reconciliation decisions. These describe authority work without
/// introducing another semantic domain noun or exposing provider transport
/// details through the public contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderReconcileAction {
    Noop,
    RefreshResource(semantic::Identity),
    MarkWorkerLost(semantic::Identity),
    RevokeLease(semantic::Identity),
    TerminateStaleWorker(semantic::Identity),
    RestartWorker(semantic::Identity),
    MarkOperationLost(semantic::Identity),
    RevokeEndpoint(semantic::Identity),
}

/// Result of reconciling recorded authority with one Provider observation.
/// Repeating a reconcile after all actions have converged returns `Noop`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderReconcileResult {
    pub provider: semantic::Identity,
    pub snapshot_generation: u64,
    pub actions: Vec<ProviderReconcileAction>,
}

/// A point-in-time authority view used after cursor/source changes. It only
/// carries existing semantic entities; provider transport and runtime details
/// remain outside the Kernel contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthoritySnapshot {
    pub source: semantic::Identity,
    pub cursor: semantic::EventCursor,
    pub providers: Vec<semantic::Provider>,
    pub workers: Vec<semantic::Worker>,
    pub leases: Vec<semantic::Lease>,
    pub operations: Vec<semantic::Operation>,
    pub endpoints: Vec<semantic::Endpoint>,
    pub endpoint_grants: Vec<semantic::EndpointGrant>,
}

/// Provider lifecycle remains a distinct authority surface. Provider identity
/// is logical identity plus session generation; an inventory snapshot and each
/// Resource retain their own independent generations.
pub trait KernelProviderAuthority: Send + Sync {
    fn register_provider(
        &self,
        context: &AuthorityCallContext,
        principal: &semantic::Principal,
        provider: semantic::Provider,
    ) -> Result<semantic::Provider, semantic::Rejection>;

    /// Records a Provider observation only. It does not alter desired state or
    /// carry out lifecycle repair; that belongs exclusively to reconciliation.
    fn publish_inventory(
        &self,
        context: &AuthorityCallContext,
        principal: &semantic::Principal,
        snapshot: semantic::ProviderSnapshot,
    ) -> Result<semantic::ProviderSnapshot, semantic::Rejection>;

    fn reconcile_provider(
        &self,
        context: &AuthorityCallContext,
        principal: &semantic::Principal,
        provider: &semantic::Identity,
    ) -> Result<ProviderReconcileResult, semantic::Rejection>;
}
