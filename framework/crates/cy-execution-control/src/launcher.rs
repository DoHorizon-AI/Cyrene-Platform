//! Provider launch boundary after canonical Lease acquisition.
use crate::DispatchError;
use cy_kernel_contract::Lease;
use cy_proto::core_v1::{NodeRef, RuntimeAssignment};
use std::{future::Future, pin::Pin};

/// Launch the exact Runtime generation after the controller has durably stored
/// the Lease evidence. The provider must persist its intent before side effects
/// and correlate by assignment_id / Runtime generation. It MUST NOT acquire a
/// second Lease, dispatch the assignment, or inject a Docker socket into Runtime.
///
/// Success means launch was accepted, not that Runtime authenticated or executed.
/// An error with reconciliation_required=false promises no live instance was
/// created. Any timeout / lost response after a possible create must be unknown.
pub trait RuntimeLauncher: Send + Sync {
    fn launch<'a>(
        &'a self,
        node: &'a NodeRef,
        assignment: &'a RuntimeAssignment,
        lease: &'a Lease,
    ) -> Pin<Box<dyn Future<Output = Result<(), DispatchError>> + Send + 'a>>;

    /// Reconcile one previously persisted launch whose outcome was unknown.
    /// Implementations may continue the exact recorded create/start sequence,
    /// but must never create a replacement instance or change its identity.
    /// The default fails closed so existing providers cannot accidentally turn
    /// a retry into a second execution.
    fn reconcile<'a>(
        &'a self,
        _node: &'a NodeRef,
        _assignment: &'a RuntimeAssignment,
        _lease: &'a Lease,
    ) -> Pin<Box<dyn Future<Output = Result<(), DispatchError>> + Send + 'a>> {
        Box::pin(async {
            Err(DispatchError::unknown(
                "Runtime launcher does not implement explicit reconciliation",
            ))
        })
    }
}
