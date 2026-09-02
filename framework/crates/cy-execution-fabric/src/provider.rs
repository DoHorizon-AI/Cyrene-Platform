//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 provider.rs                                                     │
//! │  Module: cy_execution_fabric::provider                              │
//! │  Role: Provider-managed Runtime observation seam and fake.          │
//! │                                                                     │
//! │  模块职责：定义 Provider 管理 Runtime 的观察接口及测试 Fake。            │
//! └─────────────────────────────────────────────────────────────────────┘

use std::collections::BTreeMap;
use std::sync::Mutex;

use cy_kernel_contract::Identity;

use crate::{FabricContractError, ProviderObservation};

/// Read-only Provider Adapter seam; it cannot mint Runtime or Lease authority.
pub trait ProviderObservationSource: Send + Sync {
    fn observe(&self, runtime: &Identity) -> Result<ProviderObservation, FabricContractError>;
}

/// Deterministic reference Provider used to prove reconciliation semantics.
#[derive(Debug, Default)]
pub struct FakeProvider {
    observations: Mutex<BTreeMap<Identity, ProviderObservation>>,
}

impl FakeProvider {
    pub fn report(
        &self,
        runtime: Identity,
        observation: ProviderObservation,
    ) -> Result<(), FabricContractError> {
        self.observations
            .lock()
            .map_err(|_| {
                FabricContractError::new("PROVIDER_STATE_POISONED", "Fake Provider state failed")
            })?
            .insert(runtime, observation);
        Ok(())
    }
}

impl ProviderObservationSource for FakeProvider {
    fn observe(&self, runtime: &Identity) -> Result<ProviderObservation, FabricContractError> {
        Ok(self
            .observations
            .lock()
            .map_err(|_| {
                FabricContractError::new("PROVIDER_STATE_POISONED", "Fake Provider state failed")
            })?
            .get(runtime)
            .copied()
            .unwrap_or(ProviderObservation::Unknown))
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        reconcile_runtime, DesiredRuntime, LeaseObservation, ReconcileEvidence, RuntimeDisposition,
    };

    use super::*;

    #[test]
    fn fake_provider_preemption_drives_external_termination() {
        let runtime = Identity {
            id: "runtime-1".to_string(),
            generation: 3,
        };
        let provider = FakeProvider::default();
        provider
            .report(runtime.clone(), ProviderObservation::Preempted)
            .unwrap();
        let disposition = reconcile_runtime(ReconcileEvidence {
            desired: DesiredRuntime::Running,
            agent_state: None,
            agent_termination: None,
            provider: provider.observe(&runtime).unwrap(),
            lease: LeaseObservation::Expired,
            stop_acknowledged: false,
        });
        assert_eq!(disposition, RuntimeDisposition::ExternalTermination);
    }
}
