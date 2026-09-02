//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 placement.rs                                                    │
//! │  Module: cy_execution_fabric::placement                             │
//! │  Role: Provider-neutral execution target placement.                 │
//! │                                                                     │
//! │  模块职责：按 capability、数据局部性、策略、成本和可靠性选择执行目标。    │
//! └─────────────────────────────────────────────────────────────────────┘

use cy_proto::core_v1::NodeRef;
use cy_proto::semantic_v1::{Capability, CapabilityRequirement};

use crate::FabricContractError;

/// Scheduler input for one Node without cloud-vendor-specific behavior.
#[derive(Debug, Clone, PartialEq)]
pub struct ExecutionTargetCandidate {
    pub node: NodeRef,
    pub capabilities: Vec<Capability>,
    pub policy_allowed: bool,
    pub data_locality_score: u32,
    pub estimated_cost_microunits: u64,
    pub reliability_score: u32,
}

/// Select an eligible target after policy and canonical Capability filtering.
pub fn place_execution_target<'a>(
    candidates: &'a [ExecutionTargetCandidate],
    requirements: &[CapabilityRequirement],
) -> Result<&'a ExecutionTargetCandidate, FabricContractError> {
    let mut eligible = candidates
        .iter()
        .filter(|candidate| candidate.policy_allowed)
        .filter(|candidate| {
            requirements.iter().all(|requirement| {
                candidate.capabilities.iter().any(|capability| {
                    capability.id == requirement.id
                        && capability.revision >= requirement.minimum_revision
                        && requirement
                            .required_properties
                            .iter()
                            .all(|(key, value)| capability.properties.get(key) == Some(value))
                })
            })
        })
        .collect::<Vec<_>>();
    eligible.sort_by(|left, right| {
        right
            .data_locality_score
            .cmp(&left.data_locality_score)
            .then_with(|| {
                left.estimated_cost_microunits
                    .cmp(&right.estimated_cost_microunits)
            })
            .then_with(|| right.reliability_score.cmp(&left.reliability_score))
            .then_with(|| left.node.node_id.cmp(&right.node.node_id))
    });
    eligible.into_iter().next().ok_or_else(|| {
        FabricContractError::new(
            "WAITING_FOR_EXECUTION_TARGET",
            "no policy-allowed target satisfies the canonical Capability requirements",
        )
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn candidate(id: &str, policy_allowed: bool, locality: u32) -> ExecutionTargetCandidate {
        ExecutionTargetCandidate {
            node: NodeRef {
                node_id: id.to_string(),
                node_epoch: 1,
            },
            capabilities: vec![Capability {
                id: "accelerator.nvidia".to_string(),
                revision: 1,
                properties: HashMap::from([("vram_gib".to_string(), "24".to_string())]),
            }],
            policy_allowed,
            data_locality_score: locality,
            estimated_cost_microunits: 10,
            reliability_score: 900,
        }
    }

    #[test]
    fn policy_filter_precedes_locality_and_cost_scoring() {
        let candidates = [
            candidate("fast-but-denied", false, 100),
            candidate("allowed", true, 1),
        ];
        let selected = place_execution_target(
            &candidates,
            &[CapabilityRequirement {
                id: "accelerator.nvidia".to_string(),
                minimum_revision: 1,
                required_properties: HashMap::new(),
            }],
        )
        .unwrap();
        assert_eq!(selected.node.node_id, "allowed");
    }
}
