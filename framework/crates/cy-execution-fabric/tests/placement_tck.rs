// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: placement_tck.rs                                           ║
// ║ Module: cy_execution_fabric integration tests                        ║
// ║ Role: Product-neutral placement hard-constraint conformance.         ║
// ║                                                                      ║
// ║ 模块：cy_execution_fabric 集成测试                                   ║
// ║ 职责：验证资源、策略、生命周期与 Artifact placement 的硬约束。          ║
// ╚══════════════════════════════════════════════════════════════════════╝

use std::collections::{BTreeMap, BTreeSet};

use cy_artifact_transfer::{
    ArtifactKind, ArtifactPeer, ArtifactPeerKind, ArtifactRef, ArtifactReplica, TransferManifest,
    TransferPart, TransferPartSource, TransferPlan, TransferProtocol, TransferSource,
    TransferTicket,
};
use cy_execution_fabric::{
    artifact_transfer_capability, execution_capability, place_execution_target,
    plan_execution_placement, ArtifactAvailability, ArtifactPlacementQuote, ArtifactTransferQuote,
    ExecutionPlacementRequest, ExecutionTargetCandidate, NetworkRequirements, PlacementPolicy,
};
use cy_kernel_contract::{
    Capability as KernelCapability, CapabilityRequirement as KernelCapabilityRequirement, Identity,
    Provider, ProviderSnapshot, ProviderState, Quantity, Resource, ResourceQuery, ResourceState,
};
use cy_proto::core_v1::{ExecutionAttachmentType, NodeLifecycleState, NodeRef, RestartCapability};

const NOW: u64 = 100;
const ARTIFACT_POLICY: &str = "workspace-1/artifacts";

#[test]
fn one_lease_query_matches_each_real_provider_resource_class() {
    let artifact = artifact('a', 4);
    let request = request(artifact.clone());
    let candidate = candidate("node-gpu", local_quote(artifact, "peer-node-gpu"));

    let selected = place_execution_target(&request, std::slice::from_ref(&candidate)).unwrap();
    assert_eq!(selected.node.node_id, "node-gpu");
    let decision = plan_execution_placement(&request, std::slice::from_ref(&candidate)).unwrap();
    let evidence = decision.evaluations[0].resource_match.as_ref().unwrap();
    assert_eq!(evidence.provider, candidate.provider.identity);
    assert_eq!(evidence.snapshot_generation, 1);
    assert_eq!(evidence.required_count, 1);
    assert_eq!(evidence.matched_resources.len(), 1);

    let mut cpu = request.clone();
    cpu.resource_query = ResourceQuery {
        resource_class: "compute.cpu".to_string(),
        count: 1,
        required_capabilities: Vec::new(),
        minimum_capacity: BTreeMap::from([(
            "cores".to_string(),
            Quantity {
                value: 4,
                unit: "cores".to_string(),
            },
        )]),
    };
    assert!(place_execution_target(&cpu, std::slice::from_ref(&candidate)).is_ok());

    let mut ram = request.clone();
    ram.resource_query = ResourceQuery {
        resource_class: "memory.ram".to_string(),
        count: 1,
        required_capabilities: Vec::new(),
        minimum_capacity: BTreeMap::from([(
            "available_bytes".to_string(),
            Quantity {
                value: 8 * 1024 * 1024 * 1024,
                unit: "bytes".to_string(),
            },
        )]),
    };
    assert!(place_execution_target(&ram, std::slice::from_ref(&candidate)).is_ok());

    let mut two_accelerators = request;
    two_accelerators.resource_query.count = 2;
    let rejected = plan_execution_placement(&two_accelerators, &[candidate]).unwrap();
    assert!(has_reason(
        &rejected,
        "node-gpu",
        "RESOURCE_REQUIREMENTS_UNSATISFIED"
    ));
}

#[test]
fn verified_local_quote_beats_an_authorized_remote_quote() {
    let artifact = artifact('b', 4);
    let request = request(artifact.clone());
    let local = candidate(
        "node-z-local",
        local_quote(artifact.clone(), "peer-node-z-local"),
    );
    let mut remote = candidate(
        "node-a-remote",
        transfer_quote(&artifact, "peer-node-a-remote", 40, 100, 1),
    );
    remote.execution_cost_microunits = 0;
    remote.reliability_score = u32::MAX;

    let forward = plan_execution_placement(&request, &[remote.clone(), local.clone()]).unwrap();
    let reverse = plan_execution_placement(&request, &[local, remote]).unwrap();
    assert_eq!(forward.selected_node, reverse.selected_node);
    assert_eq!(forward.selected_node.unwrap().node_id, "node-z-local");
    assert_eq!(
        forward.evaluations[0].score.unwrap().local_artifact_bytes,
        4
    );
    assert_eq!(forward.evaluations[1].score.unwrap().transfer_bytes, 4);
}

#[test]
fn actual_transfer_estimate_drives_cost_and_deadline() {
    let artifact = artifact('c', 4);
    let quote = transfer_quote(&artifact, "peer-node-costly", 10, 100, 10);
    let mut request = request(artifact.clone());
    request.policy.maximum_total_cost_microunits = Some(5);
    let costly = candidate("node-costly", quote.clone());
    let decision = plan_execution_placement(&request, &[costly]).unwrap();
    assert!(has_reason(
        &decision,
        "node-costly",
        "PLACEMENT_COST_EXCEEDED"
    ));

    request.policy.maximum_total_cost_microunits = None;
    request.latest_start_unix_ms = Some(NOW + 5);
    let delayed = candidate(
        "node-delayed",
        ArtifactPlacementQuote {
            destination_peer_id: "peer-node-delayed".to_string(),
            ..quote
        },
    );
    let decision = plan_execution_placement(&request, &[delayed]).unwrap();
    assert!(has_reason(
        &decision,
        "node-delayed",
        "START_DEADLINE_UNSATISFIED"
    ));
}

#[test]
fn quote_scope_and_authorization_window_fail_closed() {
    let artifact_ref = artifact('d', 4);
    let placement_request = request(artifact_ref.clone());
    let mut quote = transfer_quote(&artifact_ref, "peer-node-quote", 10, 100, 1);
    quote.policy_scope = "other-policy".to_string();
    let wrong_scope = candidate("node-quote", quote);
    let decision = plan_execution_placement(&placement_request, &[wrong_scope]).unwrap();
    assert!(has_reason(
        &decision,
        "node-quote",
        "ARTIFACT_QUOTE_SCOPE_MISMATCH"
    ));

    let mut expiring = transfer_quote(&artifact_ref, "peer-node-expiring", 10, 100, 1);
    expiring.valid_until_unix_ms = NOW + 5;
    if let ArtifactAvailability::AuthorizedTransfer(transfer) = &mut expiring.availability {
        transfer.authorization_valid_until_unix_ms = NOW + 5;
    }
    let decision =
        plan_execution_placement(&placement_request, &[candidate("node-expiring", expiring)])
            .unwrap();
    assert!(has_reason(
        &decision,
        "node-expiring",
        "ARTIFACT_TRANSFER_AUTHORITY_EXPIRES"
    ));

    let mut shorter_quote = transfer_quote(&artifact_ref, "peer-node-shorter", 1, 100, 1);
    shorter_quote.valid_until_unix_ms = NOW + 100;
    if let ArtifactAvailability::AuthorizedTransfer(transfer) = &mut shorter_quote.availability {
        transfer.authorization_valid_until_unix_ms = NOW + 200;
    }
    assert!(place_execution_target(
        &placement_request,
        &[candidate("node-shorter", shorter_quote)]
    )
    .is_ok());

    let second_artifact = artifact('4', 4);
    let mut cumulative_request = request(second_artifact.clone());
    cumulative_request.artifacts.insert(0, artifact_ref.clone());
    let mut first_quote = transfer_quote(&artifact_ref, "peer-node-cumulative", 10, 100, 1);
    let mut second_quote = transfer_quote(&second_artifact, "peer-node-cumulative", 10, 100, 1);
    for quote in [&mut first_quote, &mut second_quote] {
        quote.valid_until_unix_ms = NOW + 15;
        if let ArtifactAvailability::AuthorizedTransfer(transfer) = &mut quote.availability {
            transfer.authorization_valid_until_unix_ms = NOW + 15;
        }
    }
    let mut cumulative = candidate("node-cumulative", first_quote);
    cumulative.artifact_quotes.push(second_quote);
    let decision = plan_execution_placement(&cumulative_request, &[cumulative]).unwrap();
    assert!(has_reason(
        &decision,
        "node-cumulative",
        "ARTIFACT_TRANSFER_AUTHORITY_EXPIRES"
    ));
}

#[test]
fn malformed_capability_requirements_are_rejected_at_the_request_boundary() {
    let artifact = artifact('5', 4);
    let mut request = request(artifact.clone());
    request
        .capability_requirements
        .push(cy_proto::semantic_v1::CapabilityRequirement {
            id: "Not.Valid".to_string(),
            minimum_revision: 1,
            required_properties: Default::default(),
        });

    let error = plan_execution_placement(
        &request,
        &[candidate(
            "node-malformed-capability",
            local_quote(artifact, "peer-node-malformed-capability"),
        )],
    )
    .unwrap_err();
    assert_eq!(error.reason_code, "CAPABILITY_REQUIREMENT_INVALID");
}

#[test]
fn remote_transfer_requires_https_and_canonical_transfer_capability() {
    let artifact = artifact('e', 4);
    let mut request = request(artifact.clone());
    request.network.outbound_https = false;
    let mut candidate = candidate(
        "node-no-transfer",
        transfer_quote(&artifact, "peer-node-no-transfer", 1, 100, 1),
    );
    candidate
        .capabilities
        .retain(|capability| capability.id != cy_execution_fabric::ARTIFACT_TRANSFER_CAPABILITY_ID);
    candidate.capabilities[0]
        .properties
        .insert("outbound_https".to_string(), "false".to_string());

    let decision = plan_execution_placement(&request, &[candidate]).unwrap();
    assert!(has_reason(
        &decision,
        "node-no-transfer",
        "ARTIFACT_TRANSFER_CAPABILITY_UNSATISFIED"
    ));
    assert!(has_reason(
        &decision,
        "node-no-transfer",
        "EXECUTION_BEHAVIOR_UNSATISFIED"
    ));
}

#[test]
fn lifecycle_and_snapshot_fail_closed_with_reasons() {
    let artifact = artifact('f', 4);
    let request = request(artifact.clone());
    let mut candidate = candidate("node-stale", local_quote(artifact, "peer-node-stale"));
    candidate.lifecycle_state = NodeLifecycleState::Offline;
    candidate.provider_snapshot.expires_at_unix_ms = NOW;
    candidate.capabilities[0]
        .properties
        .insert("checkpoint_resume".to_string(), "false".to_string());

    let decision = plan_execution_placement(&request, &[candidate]).unwrap();
    assert_eq!(decision.selected_node, None);
    assert!(has_reason(&decision, "node-stale", "NODE_NOT_ONLINE"));
    assert!(has_reason(
        &decision,
        "node-stale",
        "PROVIDER_SNAPSHOT_STALE"
    ));
    assert!(has_reason(
        &decision,
        "node-stale",
        "EXECUTION_BEHAVIOR_UNSATISFIED"
    ));
}

#[test]
fn duplicate_nodes_and_node_peer_aliases_are_rejected() {
    let artifact = artifact('1', 4);
    let request = request(artifact.clone());
    let first = candidate(
        "node-duplicate",
        local_quote(artifact.clone(), "peer-first"),
    );
    let second = candidate(
        "node-duplicate",
        local_quote(artifact.clone(), "peer-second"),
    );
    let error = plan_execution_placement(&request, &[first, second]).unwrap_err();
    assert_eq!(error.reason_code, "EXECUTION_TARGET_DUPLICATE");

    let mut alias = candidate("node-alias", local_quote(artifact, "peer-node-alias"));
    alias.artifact_destination_peer_id = alias.node.node_id.clone();
    alias.artifact_quotes[0].destination_peer_id = alias.node.node_id.clone();
    let decision = plan_execution_placement(&request, &[alias]).unwrap();
    assert!(has_reason(
        &decision,
        "node-alias",
        "ARTIFACT_DESTINATION_INVALID"
    ));
}

#[test]
fn tie_breaking_is_stable_and_zero_byte_local_artifacts_are_valid() {
    let artifact = artifact('2', 0);
    let request = request(artifact.clone());
    let left = candidate("node-b", local_quote(artifact.clone(), "peer-node-b"));
    let right = candidate("node-a", local_quote(artifact, "peer-node-a"));
    let decision = plan_execution_placement(&request, &[left, right]).unwrap();
    assert_eq!(decision.selected_node.unwrap().node_id, "node-a");
}

#[test]
fn local_artifact_evidence_must_cover_the_candidate_ready_time() {
    let artifact = artifact('6', 4);
    let request = request(artifact.clone());
    let mut quote = local_quote(artifact, "peer-node-late-local");
    quote.valid_until_unix_ms = NOW + 50;
    let mut late = candidate("node-late-local", quote);
    late.available_at_unix_ms = NOW + 100;

    let decision = plan_execution_placement(&request, &[late]).unwrap();
    assert!(has_reason(
        &decision,
        "node-late-local",
        "ARTIFACT_LOCALITY_EVIDENCE_EXPIRES"
    ));
}

fn request(artifact: ArtifactRef) -> ExecutionPlacementRequest {
    ExecutionPlacementRequest {
        capability_requirements: Vec::new(),
        resource_query: ResourceQuery {
            resource_class: "accelerator".to_string(),
            count: 1,
            required_capabilities: vec![KernelCapabilityRequirement {
                id: "accelerator.kind.gpu".to_string(),
                minimum_revision: 1,
                required_properties: BTreeMap::new(),
            }],
            minimum_capacity: BTreeMap::from([(
                "memory.allocatable".to_string(),
                Quantity {
                    value: 24 * 1024 * 1024 * 1024,
                    unit: "byte".to_string(),
                },
            )]),
        },
        allowed_attachments: BTreeSet::from([ExecutionAttachmentType::ContainerAgent]),
        persistent: Some(false),
        restart_capability: Some(RestartCapability::None),
        checkpoint_resume: true,
        network: NetworkRequirements::default(),
        artifacts: vec![artifact],
        artifact_policy_scope: ARTIFACT_POLICY.to_string(),
        policy: PlacementPolicy {
            allowed_residencies: BTreeSet::from(["us-east".to_string()]),
            required_trust_domain: Some("workspace-1".to_string()),
            required_classifications: BTreeSet::from(["internal".to_string()]),
            required_policy_tags: BTreeSet::from(["training".to_string()]),
            maximum_total_cost_microunits: None,
        },
        latest_start_unix_ms: Some(500),
        now_unix_ms: NOW,
    }
}

fn candidate(node_id: &str, quote: ArtifactPlacementQuote) -> ExecutionTargetCandidate {
    let provider_id = Identity {
        id: format!("provider-{node_id}"),
        generation: 1,
    };
    let mut execution = execution_capability(
        ExecutionAttachmentType::ContainerAgent,
        false,
        RestartCapability::None,
    );
    execution
        .properties
        .insert("checkpoint_resume".to_string(), "true".to_string());
    let destination_peer_id = quote.destination_peer_id.clone();
    let resources = vec![
        resource(
            &provider_id,
            format!("gpu-{node_id}"),
            "accelerator",
            vec![KernelCapability {
                id: "accelerator.kind.gpu".to_string(),
                revision: 1,
                properties: BTreeMap::new(),
            }],
            BTreeMap::from([(
                "memory.allocatable".to_string(),
                Quantity {
                    value: 32 * 1024 * 1024 * 1024,
                    unit: "byte".to_string(),
                },
            )]),
        ),
        resource(
            &provider_id,
            format!("cpu-{node_id}"),
            "compute.cpu",
            Vec::new(),
            BTreeMap::from([(
                "cores".to_string(),
                Quantity {
                    value: 8,
                    unit: "cores".to_string(),
                },
            )]),
        ),
        resource(
            &provider_id,
            format!("ram-{node_id}"),
            "memory.ram",
            Vec::new(),
            BTreeMap::from([(
                "available_bytes".to_string(),
                Quantity {
                    value: 16 * 1024 * 1024 * 1024,
                    unit: "bytes".to_string(),
                },
            )]),
        ),
    ];
    ExecutionTargetCandidate {
        node: NodeRef {
            node_id: node_id.to_string(),
            node_epoch: 1,
        },
        lifecycle_state: NodeLifecycleState::Online,
        attachment: ExecutionAttachmentType::ContainerAgent,
        persistent: false,
        restart_capability: RestartCapability::None,
        capabilities: vec![execution, artifact_transfer_capability()],
        provider: Provider {
            identity: provider_id.clone(),
            state: ProviderState::Ready,
            capabilities: Vec::new(),
        },
        provider_snapshot: ProviderSnapshot {
            provider: provider_id,
            snapshot_generation: 1,
            resources,
            workers: Vec::new(),
            endpoints: Vec::new(),
            sampled_at_unix_ms: 50,
            expires_at_unix_ms: 1_000,
        },
        residency: "us-east".to_string(),
        trust_domain: "workspace-1".to_string(),
        classifications: BTreeSet::from(["internal".to_string()]),
        policy_tags: BTreeSet::from(["training".to_string()]),
        artifact_destination_peer_id: destination_peer_id,
        artifact_quotes: vec![quote],
        execution_cost_microunits: 1,
        available_at_unix_ms: NOW,
        reliability_score: 100,
    }
}

fn artifact(seed: char, size_bytes: u64) -> ArtifactRef {
    let hex = seed.to_string().repeat(64);
    ArtifactRef {
        uri: format!("artifact://sha256/{hex}"),
        digest: format!("sha256:{hex}"),
        size_bytes,
        kind: ArtifactKind::generic(),
        manifest_digest: None,
    }
}

fn resource(
    provider: &Identity,
    id: String,
    resource_class: &str,
    capabilities: Vec<KernelCapability>,
    capacity: BTreeMap<String, Quantity>,
) -> Resource {
    Resource {
        identity: Identity { id, generation: 1 },
        provider: provider.clone(),
        resource_class: resource_class.to_string(),
        capabilities,
        capacity,
        attributes: BTreeMap::new(),
        state: ResourceState::Ready,
        reason_code: "resource.ready".to_string(),
        summary: format!("ready {resource_class}"),
        links: Vec::new(),
    }
}

fn local_quote(artifact: ArtifactRef, destination_peer_id: &str) -> ArtifactPlacementQuote {
    ArtifactPlacementQuote {
        quote_id: format!("local-{}", artifact.digest),
        artifact,
        destination_peer_id: destination_peer_id.to_string(),
        policy_scope: ARTIFACT_POLICY.to_string(),
        observed_at_unix_ms: 50,
        valid_until_unix_ms: 1_000,
        availability: ArtifactAvailability::VerifiedLocal {
            inventory_generation: 1,
        },
    }
}

fn transfer_quote(
    artifact: &ArtifactRef,
    destination_peer_id: &str,
    latency_ms: u64,
    bandwidth_mbps: u64,
    cost_microunits: u64,
) -> ArtifactPlacementQuote {
    let manifest = TransferManifest {
        artifact: artifact.clone(),
        part_size_bytes: artifact.size_bytes,
        parts: vec![TransferPart {
            index: 0,
            start: 0,
            end_exclusive: artifact.size_bytes,
            digest: format!("sha256:{}", "9".repeat(64)),
        }],
    };
    let source = peer("source-peer", latency_ms, bandwidth_mbps, cost_microunits);
    let plan = TransferPlan {
        plan_id: "plan-1".to_string(),
        artifact: artifact.clone(),
        destination_peer_id: destination_peer_id.to_string(),
        sources: vec![TransferSource {
            peer: source.clone(),
            replica: ArtifactReplica {
                replica_id: "replica-1".to_string(),
                artifact: artifact.clone(),
                peer_id: source.peer_id.clone(),
                protocol: TransferProtocol::HttpsRangeV1,
                locator: "https://artifact.example.test/value".to_string(),
                region: Some("us-east".to_string()),
                priority: 0,
                expires_at_unix_ms: None,
            },
            ticket: TransferTicket {
                ticket_id: "ticket-1".to_string(),
                artifact: artifact.clone(),
                source_peer_id: source.peer_id.clone(),
                destination_peer_id: destination_peer_id.to_string(),
                allowed_parts: BTreeSet::from([0]),
                expires_at_unix_ms: 1_000,
                max_bytes: artifact.size_bytes,
                signature: "fixture-signature".to_string(),
            },
        }],
        part_sources: vec![TransferPartSource {
            part_index: 0,
            peer_id: source.peer_id,
            replica_id: "replica-1".to_string(),
        }],
    };
    let estimate = plan.estimate(&manifest, NOW).unwrap();
    ArtifactPlacementQuote {
        quote_id: format!("transfer-{}", artifact.digest),
        artifact: artifact.clone(),
        destination_peer_id: destination_peer_id.to_string(),
        policy_scope: ARTIFACT_POLICY.to_string(),
        observed_at_unix_ms: NOW,
        valid_until_unix_ms: estimate.valid_until_unix_ms,
        availability: ArtifactAvailability::AuthorizedTransfer(ArtifactTransferQuote {
            bytes_to_transfer: estimate.bytes_to_transfer,
            estimated_transfer_millis: estimate.estimated_transfer_millis,
            cost_microunits: estimate.cost_microunits,
            source_count: estimate.source_count,
            authorization_valid_until_unix_ms: estimate.valid_until_unix_ms,
        }),
    }
}

fn peer(peer_id: &str, latency_ms: u64, bandwidth_mbps: u64, cost_microunits: u64) -> ArtifactPeer {
    ArtifactPeer {
        peer_id: peer_id.to_string(),
        kind: ArtifactPeerKind::NodeCache,
        authorized: true,
        residency: "us-east".to_string(),
        trust_domain: "workspace-1".to_string(),
        classifications: BTreeSet::from(["internal".to_string()]),
        policy_tags: BTreeSet::from(["training".to_string()]),
        healthy: true,
        latency_ms,
        bandwidth_mbps,
        cost_microunits,
    }
}

fn has_reason(
    decision: &cy_execution_fabric::PlacementDecision,
    node_id: &str,
    reason_code: &str,
) -> bool {
    decision
        .evaluations
        .iter()
        .find(|evaluation| evaluation.node.node_id == node_id)
        .is_some_and(|evaluation| {
            evaluation
                .reasons
                .iter()
                .any(|reason| reason.reason_code == reason_code)
        })
}
