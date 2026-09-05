//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 reconcile.rs                                                    │
//! │  Module: cy_execution_fabric::reconcile                             │
//! │  Role: Combine desired, Agent, Provider, and Lease observations.    │
//! │                                                                     │
//! │  模块职责：组合 desired/Agent/Provider/Lease 证据判定终止语义。          │
//! └─────────────────────────────────────────────────────────────────────┘

use cy_proto::core_v1::{RuntimeObservedState, TerminationClassification};

/// Desired state persisted by the Fabric control plane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesiredRuntime {
    Running,
    Stopped,
}

/// Independent Provider observation when a Provider Adapter exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderObservation {
    Running,
    Preempted,
    Terminated,
    Unavailable,
    Unknown,
}

/// Current canonical Lease observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseObservation {
    Active,
    Expired,
    Missing,
}

/// Complete evidence used by one deterministic reconciliation decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconcileEvidence {
    pub desired: DesiredRuntime,
    pub agent_state: Option<RuntimeObservedState>,
    pub agent_termination: Option<TerminationClassification>,
    pub provider: ProviderObservation,
    pub lease: LeaseObservation,
    pub stop_acknowledged: bool,
}

/// Reconciled Runtime disposition. Connection loss alone is never terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeDisposition {
    Active,
    NetworkPartitionCandidate,
    ExpectedTermination,
    GracefulTermination,
    UnexpectedLoss,
    ExternalTermination,
    UnknownLoss,
}

/// Reconcile authority and observation evidence into one Runtime disposition.
pub fn reconcile_runtime(evidence: ReconcileEvidence) -> RuntimeDisposition {
    if evidence.agent_state == Some(RuntimeObservedState::Stopped)
        && evidence.agent_termination == Some(TerminationClassification::Graceful)
    {
        return RuntimeDisposition::GracefulTermination;
    }
    if matches!(
        evidence.provider,
        ProviderObservation::Preempted | ProviderObservation::Terminated
    ) {
        return if evidence.desired == DesiredRuntime::Stopped || evidence.stop_acknowledged {
            RuntimeDisposition::ExpectedTermination
        } else {
            RuntimeDisposition::ExternalTermination
        };
    }
    if matches!(
        evidence.lease,
        LeaseObservation::Expired | LeaseObservation::Missing
    ) {
        if evidence.desired == DesiredRuntime::Stopped && evidence.stop_acknowledged {
            return RuntimeDisposition::ExpectedTermination;
        }
        if evidence.desired == DesiredRuntime::Running
            && evidence.provider == ProviderObservation::Running
            && evidence.agent_state.is_none()
        {
            return RuntimeDisposition::NetworkPartitionCandidate;
        }
        if evidence.desired == DesiredRuntime::Running {
            return RuntimeDisposition::UnexpectedLoss;
        }
        return RuntimeDisposition::UnknownLoss;
    }
    RuntimeDisposition::Active
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disconnect_with_active_lease_is_not_a_crash() {
        let disposition = reconcile_runtime(ReconcileEvidence {
            desired: DesiredRuntime::Running,
            agent_state: None,
            agent_termination: None,
            provider: ProviderObservation::Unknown,
            lease: LeaseObservation::Active,
            stop_acknowledged: false,
        });
        assert_eq!(disposition, RuntimeDisposition::Active);
    }

    #[test]
    fn desired_stop_ack_plus_lease_loss_is_expected() {
        let disposition = reconcile_runtime(ReconcileEvidence {
            desired: DesiredRuntime::Stopped,
            agent_state: None,
            agent_termination: None,
            provider: ProviderObservation::Unknown,
            lease: LeaseObservation::Expired,
            stop_acknowledged: true,
        });
        assert_eq!(disposition, RuntimeDisposition::ExpectedTermination);
    }

    #[test]
    fn active_runtime_without_lease_is_unexpected_loss() {
        let disposition = reconcile_runtime(ReconcileEvidence {
            desired: DesiredRuntime::Running,
            agent_state: None,
            agent_termination: None,
            provider: ProviderObservation::Unavailable,
            lease: LeaseObservation::Expired,
            stop_acknowledged: false,
        });
        assert_eq!(disposition, RuntimeDisposition::UnexpectedLoss);
    }

    #[test]
    fn provider_running_without_agent_is_network_partition_candidate() {
        let disposition = reconcile_runtime(ReconcileEvidence {
            desired: DesiredRuntime::Running,
            agent_state: None,
            agent_termination: None,
            provider: ProviderObservation::Running,
            lease: LeaseObservation::Expired,
            stop_acknowledged: false,
        });
        assert_eq!(disposition, RuntimeDisposition::NetworkPartitionCandidate);
    }
}
