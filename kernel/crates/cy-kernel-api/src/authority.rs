//! 内核稳定权威端口 (Kernel Authority Port).

use cy_kernel_contract as semantic;

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

    /// Register or refresh one authenticated Provider incarnation.
    fn register_provider(
        &self,
        principal: &semantic::Principal,
        provider: semantic::Provider,
    ) -> Result<semantic::Provider, semantic::Rejection>;

    /// Return the semantic contract revision implemented by this authority.
    fn contract_revision(&self) -> semantic::ContractRevision {
        semantic::ContractRevision::current()
    }

    /// Publish or reconcile one complete, expiring Provider observation.
    fn reconcile_provider(
        &self,
        principal: &semantic::Principal,
        snapshot: semantic::ProviderSnapshot,
    ) -> Result<(), semantic::Rejection>;

    /// Atomically acquire resources for a holder under the authenticated
    /// principal. Policy must already be compiled into the bounded query.
    fn acquire_lease(
        &self,
        principal: &semantic::Principal,
        holder: semantic::Identity,
        query: semantic::ResourceQuery,
        expires_at_unix_ms: u64,
    ) -> Result<semantic::Lease, semantic::Rejection>;

    /// Renew an active lease without changing its resource authority.
    fn renew_lease(
        &self,
        principal: &semantic::Principal,
        lease: &semantic::Identity,
        fence_token: u64,
        expires_at_unix_ms: u64,
    ) -> Result<semantic::Lease, semantic::Rejection>;

    /// Revoke or release a lease using its current fencing authority.
    fn release_lease(
        &self,
        principal: &semantic::Principal,
        lease: &semantic::Identity,
        fence_token: u64,
    ) -> Result<semantic::Lease, semantic::Rejection>;

    /// Start a Worker from an immutable externally verified execution
    /// reference. The implementation delegates process work to a Provider.
    fn start_worker(
        &self,
        principal: &semantic::Principal,
        worker: semantic::Worker,
    ) -> Result<semantic::Operation, semantic::Rejection>;

    /// Drain or stop one Worker using its generation and lease fence.
    fn stop_worker(
        &self,
        principal: &semantic::Principal,
        worker: &semantic::Identity,
        lease: &semantic::Identity,
        fence_token: u64,
    ) -> Result<semantic::Operation, semantic::Rejection>;

    /// Record liveness for the exact Worker/Lease generation and fence.
    fn heartbeat_worker(
        &self,
        principal: &semantic::Principal,
        worker: &semantic::Identity,
        lease: &semantic::Identity,
        fence_token: u64,
    ) -> Result<semantic::Worker, semantic::Rejection>;

    /// Create one bounded operation owned by the authenticated Principal.
    fn create_operation(
        &self,
        principal: &semantic::Principal,
        operation: semantic::Operation,
    ) -> Result<semantic::Operation, semantic::Rejection>;

    /// Apply an idempotent or legal forward-only Operation report.
    fn report_operation(
        &self,
        principal: &semantic::Principal,
        operation: semantic::Operation,
    ) -> Result<semantic::Operation, semantic::Rejection>;

    /// Cancel a managed Operation using its current identity generation.
    fn cancel_operation(
        &self,
        principal: &semantic::Principal,
        operation: &semantic::Identity,
    ) -> Result<semantic::Operation, semantic::Rejection>;

    /// Publish Endpoint authorization metadata. Data remains outside Kernel.
    fn publish_endpoint(
        &self,
        principal: &semantic::Principal,
        endpoint: semantic::Endpoint,
    ) -> Result<semantic::Endpoint, semantic::Rejection>;

    /// Authorize one grantee while its lease/fence authority remains valid.
    fn authorize_endpoint(
        &self,
        principal: &semantic::Principal,
        grant: semantic::EndpointGrant,
    ) -> Result<semantic::EndpointGrant, semantic::Rejection>;

    /// Revoke a previously issued Endpoint grant.
    fn revoke_endpoint(
        &self,
        principal: &semantic::Principal,
        grant: &semantic::Identity,
    ) -> Result<(), semantic::Rejection>;

    /// Replay immutable facts after a cursor scoped to one authority
    /// incarnation. A source mismatch requires a fresh snapshot/reconcile.
    fn events_after(
        &self,
        principal: &semantic::Principal,
        cursor: &semantic::EventCursor,
        limit: usize,
    ) -> Result<semantic::EventPage, semantic::Rejection>;
}
