// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: contracts/rust/cy-kernel-contract/src/tests.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! cy-kernel-contract 契约单元测试与 TCK TSV 验证套件。

use std::collections::{BTreeMap, BTreeSet};

use super::*;
use crate::validation::{
    validate_namespaced_id, validate_timestamp, MAX_CAPABILITIES, MAX_ENDPOINTS_PER_SNAPSHOT,
    MAX_ERROR_MESSAGE_BYTES, MAX_EVENTS_PER_PAGE, MAX_EVENT_BODY_BYTES, MAX_EXECUTION_REF_BYTES,
    MAX_ID_BYTES, MAX_NAMESPACED_ID_BYTES, MAX_PROPERTIES, MAX_RESOURCES_PER_LEASE,
    MAX_RESOURCES_PER_SNAPSHOT, MAX_TIMESTAMP_UNIX_MS, MAX_WORKERS_PER_SNAPSHOT,
};

fn identity(id: &str, generation: u64) -> Identity {
    Identity {
        id: id.to_string(),
        generation,
    }
}

fn resource() -> Resource {
    Resource {
        identity: identity("resource-1", 1),
        provider: identity("provider-1", 3),
        resource_class: "accelerator".to_string(),
        capabilities: vec![Capability {
            id: "accelerator.compute".to_string(),
            revision: 2,
            properties: BTreeMap::from([("numeric".to_string(), "bf16".to_string())]),
        }],
        capacity: BTreeMap::from([(
            "memory".to_string(),
            Quantity {
                value: 80,
                unit: "gib".to_string(),
            },
        )]),
        attributes: BTreeMap::new(),
        state: ResourceState::Ready,
        reason_code: "ready".to_string(),
        summary: "resource is ready".to_string(),
        links: Vec::new(),
    }
}

fn worker(provider: &Identity) -> Worker {
    Worker {
        identity: identity("worker-1", 1),
        principal: identity("principal-1", 1),
        provider: provider.clone(),
        lease: identity("lease-1", 1),
        state: WorkerState::Running,
        execution_ref: "artifact.sha256.0123456789abcdef".to_string(),
        limits: BTreeMap::new(),
    }
}

fn endpoint(provider: &Identity) -> Endpoint {
    Endpoint {
        identity: identity("endpoint-1", 1),
        provider: provider.clone(),
        owner: identity("worker-1", 1),
        transport: "local.uds".to_string(),
        schema_id: "cyrene.endpoint.echo.v1".to_string(),
        capabilities: Vec::new(),
        public_attributes: BTreeMap::new(),
    }
}

#[test]
fn generic_query_matches_without_vendor_knowledge() {
    let query = ResourceQuery {
        resource_class: "accelerator".to_string(),
        count: 1,
        required_capabilities: vec![CapabilityRequirement {
            id: "accelerator.compute".to_string(),
            minimum_revision: 1,
            required_properties: BTreeMap::from([("numeric".to_string(), "bf16".to_string())]),
        }],
        minimum_capacity: BTreeMap::from([(
            "memory".to_string(),
            Quantity {
                value: 40,
                unit: "gib".to_string(),
            },
        )]),
    };
    query.validate().unwrap();
    assert!(query.matches(&resource()));
}

#[test]
fn quantity_units_and_capability_properties_are_not_coerced() {
    let mut wrong_unit = ResourceQuery {
        resource_class: "accelerator".to_string(),
        count: 1,
        required_capabilities: Vec::new(),
        minimum_capacity: BTreeMap::from([(
            "memory".to_string(),
            Quantity {
                value: 40,
                unit: "gb".to_string(),
            },
        )]),
    };
    assert!(!wrong_unit.matches(&resource()));
    wrong_unit.minimum_capacity.clear();
    wrong_unit
        .required_capabilities
        .push(CapabilityRequirement {
            id: "accelerator.compute".to_string(),
            minimum_revision: 1,
            required_properties: BTreeMap::from([("numeric".to_string(), "fp8".to_string())]),
        });
    assert!(!wrong_unit.matches(&resource()));
}

#[test]
fn query_rejects_duplicate_requirements_and_unbounded_counts() {
    let requirement = CapabilityRequirement {
        id: "accelerator.compute".to_string(),
        minimum_revision: 1,
        required_properties: BTreeMap::new(),
    };
    let mut query = ResourceQuery {
        resource_class: "accelerator".to_string(),
        count: 1,
        required_capabilities: vec![requirement.clone(), requirement],
        minimum_capacity: BTreeMap::new(),
    };
    assert_eq!(
        query.validate().unwrap_err().reason_code,
        "CAPABILITY_REQUIREMENT_DUPLICATE"
    );
    query.required_capabilities.clear();
    query.count = MAX_RESOURCES_PER_LEASE as u32 + 1;
    assert_eq!(
        query.validate().unwrap_err().reason_code,
        "RESOURCE_COUNT_LIMIT_EXCEEDED"
    );
}

#[test]
fn lease_authority_requires_identity_generation_fence_and_expiry() {
    let holder = identity("worker-1", 2);
    let resource = identity("resource-1", 5);
    let lease = Lease {
        identity: identity("lease-1", 1),
        holder: holder.clone(),
        resources: vec![resource.clone()],
        state: LeaseState::Active,
        fence_token: 42,
        expires_at_unix_ms: Some(1_000),
    };
    assert!(lease.authorizes(&holder, &resource, 42, 999));
    assert!(!lease.authorizes(&identity("worker-1", 1), &resource, 42, 999));
    assert!(!lease.authorizes(&holder, &resource, 41, 999));
    assert!(!lease.authorizes(&holder, &resource, 42, 1_000));

    let mut unbounded = lease;
    unbounded.expires_at_unix_ms = None;
    assert_eq!(
        unbounded.validate().unwrap_err().reason_code,
        "LEASE_EXPIRY_REQUIRED"
    );
    assert!(!unbounded.authorizes(&holder, &resource, 42, 999));
}

#[test]
fn endpoint_grant_requires_both_grant_and_lease_authority() {
    let lease = Lease {
        identity: identity("lease-1", 2),
        holder: identity("worker-client", 4),
        resources: vec![identity("resource-1", 3)],
        state: LeaseState::Active,
        fence_token: 9,
        expires_at_unix_ms: Some(2_000),
    };
    let grant = EndpointGrant {
        identity: identity("grant-1", 1),
        endpoint: identity("endpoint-1", 1),
        grantee: identity("worker-client", 4),
        lease: lease.identity.clone(),
        fence_token: lease.fence_token,
        expires_at_unix_ms: 1_500,
    };
    assert!(grant.authorizes(&grant.endpoint, &grant.grantee, &lease, 1_000));
    assert!(!grant.authorizes(
        &grant.endpoint,
        &identity("worker-client", 3),
        &lease,
        1_000
    ));
    assert!(!grant.authorizes(&grant.endpoint, &grant.grantee, &lease, 1_500));
}

#[test]
fn lifecycle_transitions_are_forward_only_and_terminal() {
    assert!(LeaseState::Active.can_transition_to(LeaseState::Releasing));
    assert!(LeaseState::Releasing.can_transition_to(LeaseState::Released));
    assert!(!LeaseState::Released.can_transition_to(LeaseState::Active));

    assert!(WorkerState::Registered.can_transition_to(WorkerState::Starting));
    assert!(WorkerState::Running.can_transition_to(WorkerState::Draining));
    assert!(!WorkerState::Stopped.can_transition_to(WorkerState::Running));

    assert!(OperationState::Created.can_transition_to(OperationState::Pending));
    assert!(OperationState::Running.can_transition_to(OperationState::Succeeded));
    assert!(!OperationState::Succeeded.can_transition_to(OperationState::Running));
}

#[test]
fn namespaced_identifiers_and_timestamps_have_one_frozen_grammar() {
    for value in ["Vendor.cuda", "vendor..cuda", ".vendor", "vendor-"] {
        assert_eq!(
            validate_namespaced_id("test", value)
                .unwrap_err()
                .reason_code,
            "NAMESPACED_ID_INVALID"
        );
    }
    for value in ["vendor.nvidia.cuda", "local.uds", "byte", "schema-v1"] {
        validate_namespaced_id("test", value).unwrap();
    }
    assert_eq!(
        validate_timestamp("test", MAX_TIMESTAMP_UNIX_MS + 1)
            .unwrap_err()
            .reason_code,
        "TIMESTAMP_INVALID"
    );
}

#[test]
fn snapshots_reject_unowned_or_duplicate_resources() {
    let provider = identity("provider-1", 3);
    let mut snapshot = ProviderSnapshot {
        provider: provider.clone(),
        snapshot_generation: 1,
        resources: vec![resource()],
        workers: Vec::new(),
        endpoints: Vec::new(),
        sampled_at_unix_ms: 10,
        expires_at_unix_ms: 20,
    };
    snapshot.validate().unwrap();
    let mut duplicate_incarnation = resource();
    duplicate_incarnation.identity.generation += 1;
    snapshot.resources.push(duplicate_incarnation);
    assert_eq!(
        snapshot.validate().unwrap_err().reason_code,
        "RESOURCE_IDENTITY_DUPLICATE"
    );

    snapshot.resources.pop();
    snapshot
        .workers
        .push(worker(&identity("provider-other", 1)));
    assert_eq!(
        snapshot.validate().unwrap_err().reason_code,
        "WORKER_PROVIDER_MISMATCH"
    );
    snapshot.workers.clear();
    snapshot
        .endpoints
        .push(endpoint(&identity("provider-other", 1)));
    assert_eq!(
        snapshot.validate().unwrap_err().reason_code,
        "ENDPOINT_PROVIDER_MISMATCH"
    );
}

#[test]
fn event_body_is_bounded() {
    let event = Event {
        sequence: 1,
        source: identity("node-1", 7),
        subject: identity("worker-1", 1),
        kind: "worker.started".to_string(),
        observed_at_unix_ms: 1,
        schema_id: "cyrene.event.worker-started.v1".to_string(),
        body: vec![0; MAX_EVENT_BODY_BYTES + 1],
    };
    assert_eq!(
        event.validate().unwrap_err().reason_code,
        "EVENT_BODY_LIMIT_EXCEEDED"
    );
}

#[test]
fn event_pages_are_source_scoped_ordered_and_gap_explicit() {
    let source = identity("node-1", 7);
    let event = Event {
        sequence: 5,
        source: source.clone(),
        subject: identity("worker-1", 1),
        kind: "worker.running".to_string(),
        observed_at_unix_ms: 10,
        schema_id: "cyrene.event.worker-running.v1".to_string(),
        body: Vec::new(),
    };
    let page = EventPage {
        source,
        status: ReplayStatus::Current,
        events: vec![event],
        oldest_available_sequence: 3,
        latest_available_sequence: 5,
        next_sequence: 5,
    };
    page.validate().unwrap();

    let mut gap_with_data = page;
    gap_with_data.status = ReplayStatus::Gap;
    assert_eq!(
        gap_with_data.validate().unwrap_err().reason_code,
        "EVENT_RECONCILE_REQUIRED"
    );
}

fn fixture_rows(input: &str) -> impl Iterator<Item = Vec<&str>> {
    input
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| line.split('|').collect())
}

fn lease_state(value: &str) -> LeaseState {
    match value {
        "ACTIVE" => LeaseState::Active,
        "RELEASING" => LeaseState::Releasing,
        "RELEASED" => LeaseState::Released,
        "EXPIRED" => LeaseState::Expired,
        "REVOKED" => LeaseState::Revoked,
        "FAILED" => LeaseState::Failed,
        _ => panic!("unknown Lease state: {value}"),
    }
}

fn worker_state(value: &str) -> WorkerState {
    match value {
        "REGISTERED" => WorkerState::Registered,
        "STARTING" => WorkerState::Starting,
        "RUNNING" => WorkerState::Running,
        "DRAINING" => WorkerState::Draining,
        "STOPPED" => WorkerState::Stopped,
        "FAILED" => WorkerState::Failed,
        "LOST" => WorkerState::Lost,
        _ => panic!("unknown Worker state: {value}"),
    }
}

fn operation_state(value: &str) -> OperationState {
    match value {
        "CREATED" => OperationState::Created,
        "PENDING" => OperationState::Pending,
        "RUNNING" => OperationState::Running,
        "SUCCEEDED" => OperationState::Succeeded,
        "FAILED" => OperationState::Failed,
        "CANCELLING" => OperationState::Cancelling,
        "CANCELLED" => OperationState::Cancelled,
        "LOST" => OperationState::Lost,
        _ => panic!("unknown Operation state: {value}"),
    }
}

fn fixture_properties(value: &str) -> BTreeMap<String, String> {
    if value == "-" {
        return BTreeMap::new();
    }
    value
        .split(',')
        .map(|item| {
            let (key, value) = item.split_once('=').unwrap();
            (key.to_string(), value.to_string())
        })
        .collect()
}

fn fixture_capacity(value: &str) -> BTreeMap<String, Quantity> {
    if value == "-" {
        return BTreeMap::new();
    }
    value
        .split(',')
        .map(|item| {
            let (key, quantity) = item.split_once('=').unwrap();
            let (value, unit) = quantity.split_once('@').unwrap();
            (
                key.to_string(),
                Quantity {
                    value: value.parse().unwrap(),
                    unit: unit.to_string(),
                },
            )
        })
        .collect()
}

#[test]
fn frozen_tck_limits_identifiers_and_negotiation_match_rust() {
    let expected_limits = BTreeMap::from([
        ("max_id_bytes", MAX_ID_BYTES as u64),
        ("max_namespaced_id_bytes", MAX_NAMESPACED_ID_BYTES as u64),
        ("max_capabilities", MAX_CAPABILITIES as u64),
        ("max_properties", MAX_PROPERTIES as u64),
        (
            "max_resources_per_snapshot",
            MAX_RESOURCES_PER_SNAPSHOT as u64,
        ),
        ("max_resources_per_lease", MAX_RESOURCES_PER_LEASE as u64),
        ("max_workers_per_snapshot", MAX_WORKERS_PER_SNAPSHOT as u64),
        (
            "max_endpoints_per_snapshot",
            MAX_ENDPOINTS_PER_SNAPSHOT as u64,
        ),
        ("max_execution_ref_bytes", MAX_EXECUTION_REF_BYTES as u64),
        ("max_error_message_bytes", MAX_ERROR_MESSAGE_BYTES as u64),
        ("max_event_body_bytes", MAX_EVENT_BODY_BYTES as u64),
        ("max_events_per_page", MAX_EVENTS_PER_PAGE as u64),
        ("max_timestamp_unix_ms", MAX_TIMESTAMP_UNIX_MS),
    ]);
    let actual_limits = fixture_rows(include_str!("../../../tck/kernel-semantic/v1/limits.tsv"))
        .map(|row| (row[0], row[1].parse::<u64>().unwrap()))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(actual_limits, expected_limits);

    for row in fixture_rows(include_str!(
        "../../../tck/kernel-semantic/v1/identifiers.tsv"
    )) {
        let actual = match row[1] {
            "namespaced" => validate_namespaced_id("fixture", row[2]),
            "identity" => Identity {
                id: match row[2] {
                    "<empty>" => "",
                    "<c0>" => "\u{0001}",
                    "<c1>" => "\u{0085}",
                    value => value,
                }
                .to_string(),
                generation: 1,
            }
            .validate(),
            "timestamp" => validate_timestamp("fixture", row[2].parse().unwrap()),
            kind => panic!("unknown identifier fixture kind: {kind}"),
        };
        let actual = actual
            .map(|()| "ACCEPT")
            .unwrap_or_else(|error| error.reason_code);
        assert_eq!(actual, row[3], "{}", row[0]);
    }

    for row in fixture_rows(include_str!(
        "../../../tck/kernel-semantic/v1/negotiation.tsv"
    )) {
        let local = ContractRevision {
            contract_id: row[1].to_string(),
            major: row[2].parse().unwrap(),
            minor: row[3].parse().unwrap(),
        };
        let offered = ContractRevision {
            contract_id: row[4].to_string(),
            major: row[5].parse().unwrap(),
            minor: row[6].parse().unwrap(),
        };
        let actual = local.negotiate(&offered).map_or_else(
            || "INCOMPATIBLE".to_string(),
            |value| format!("{}.{}", value.major, value.minor),
        );
        assert_eq!(actual, row[7], "{}", row[0]);
    }
}

#[test]
fn frozen_tck_state_matrices_are_complete() {
    for row in fixture_rows(include_str!(
        "../../../tck/kernel-semantic/v1/transitions.tsv"
    )) {
        let allowed = row[2].split(',').collect::<BTreeSet<_>>();
        let all_states: &[&str] = match row[0] {
            "lease" => &[
                "ACTIVE",
                "RELEASING",
                "RELEASED",
                "EXPIRED",
                "REVOKED",
                "FAILED",
            ],
            "worker" => &[
                "REGISTERED",
                "STARTING",
                "RUNNING",
                "DRAINING",
                "STOPPED",
                "FAILED",
                "LOST",
            ],
            "operation" => &[
                "CREATED",
                "PENDING",
                "RUNNING",
                "SUCCEEDED",
                "FAILED",
                "CANCELLING",
                "CANCELLED",
                "LOST",
            ],
            noun => panic!("unknown lifecycle noun: {noun}"),
        };
        for target in all_states {
            let actual = match row[0] {
                "lease" => lease_state(row[1]).can_transition_to(lease_state(target)),
                "worker" => worker_state(row[1]).can_transition_to(worker_state(target)),
                "operation" => operation_state(row[1]).can_transition_to(operation_state(target)),
                _ => unreachable!(),
            };
            assert_eq!(
                actual,
                allowed.contains(target),
                "{}.{} -> {target}",
                row[0],
                row[1]
            );
        }
    }
}

#[test]
fn frozen_tck_matching_authority_and_replay_match_rust() {
    for row in fixture_rows(include_str!("../../../tck/kernel-semantic/v1/matching.tsv")) {
        let capability = Capability {
            id: row[1].to_string(),
            revision: row[2].parse().unwrap(),
            properties: fixture_properties(row[3]),
        };
        let requirement = CapabilityRequirement {
            id: row[4].to_string(),
            minimum_revision: row[5].parse().unwrap(),
            required_properties: fixture_properties(row[6]),
        };
        let capacity = fixture_capacity(row[7]);
        let minimum = fixture_capacity(row[8]);
        let actual = requirement.matches(&capability)
            && minimum.iter().all(|(key, minimum)| {
                capacity
                    .get(key)
                    .is_some_and(|actual| actual.satisfies(minimum))
            });
        assert_eq!(actual, row[9] == "true", "{}", row[0]);
    }

    for row in fixture_rows(include_str!(
        "../../../tck/kernel-semantic/v1/authority.tsv"
    )) {
        let state = lease_state(row[2]);
        let holder = identity("worker-holder", 2);
        let presented_holder = if row[3] == "true" {
            holder.clone()
        } else {
            identity("worker-holder", 1)
        };
        let resource = identity("resource-1", 3);
        let presented_resource = if row[4] == "true" {
            resource.clone()
        } else {
            identity("resource-1", 2)
        };
        let lease_identity = identity("lease-1", 4);
        let referenced_lease = if row[5] == "true" {
            lease_identity.clone()
        } else {
            identity("lease-1", 3)
        };
        let lease = Lease {
            identity: lease_identity,
            holder: holder.clone(),
            resources: vec![resource],
            state,
            fence_token: 9,
            expires_at_unix_ms: Some(row[7].parse().unwrap()),
        };
        let now = row[9].parse().unwrap();
        let actual = match row[1] {
            "lease" => lease.authorizes(
                &presented_holder,
                &presented_resource,
                if row[6] == "true" { 9 } else { 8 },
                now,
            ),
            "grant" => EndpointGrant {
                identity: identity("grant-1", 1),
                endpoint: identity("endpoint-1", 1),
                grantee: holder,
                lease: referenced_lease,
                fence_token: if row[6] == "true" { 9 } else { 8 },
                expires_at_unix_ms: row[8].parse().unwrap(),
            }
            .authorizes(
                &if row[4] == "true" {
                    identity("endpoint-1", 1)
                } else {
                    identity("endpoint-1", 2)
                },
                &presented_holder,
                &lease,
                now,
            ),
            kind => panic!("unknown authority kind: {kind}"),
        };
        assert_eq!(actual, row[10] == "true", "{}", row[0]);
    }

    for row in fixture_rows(include_str!("../../../tck/kernel-semantic/v1/renewal.tsv")) {
        let lease = Lease {
            identity: identity("lease-1", 4),
            holder: identity("worker-holder", 2),
            resources: vec![identity("resource-1", 3)],
            state: lease_state(row[1]),
            fence_token: 9,
            expires_at_unix_ms: Some(row[3].parse().unwrap()),
        };
        let actual = lease
            .renew(
                if row[2] == "true" { 9 } else { 8 },
                row[4].parse().unwrap(),
                row[5].parse().unwrap(),
            )
            .map(|_| "ACCEPT")
            .unwrap_or_else(|error| error.reason_code);
        assert_eq!(actual, row[6], "{}", row[0]);
    }

    for row in fixture_rows(include_str!("../../../tck/kernel-semantic/v1/replay.tsv")) {
        let source = identity("node-1", 7);
        let cursor = EventCursor {
            source: if row[1] == "true" {
                source.clone()
            } else {
                identity("node-1", 6)
            },
            sequence: row[2].parse().unwrap(),
        };
        let status = cursor.status_against(&source, row[3].parse().unwrap());
        let actual_status = match status {
            ReplayStatus::Current => "CURRENT",
            ReplayStatus::Gap => "GAP",
            ReplayStatus::SourceChanged => "SOURCE_CHANGED",
        };
        let sequences = if status == ReplayStatus::Current {
            let start = cursor
                .sequence
                .saturating_add(1)
                .max(row[3].parse().unwrap());
            (start..=row[4].parse().unwrap())
                .take(row[5].parse().unwrap())
                .collect::<Vec<u64>>()
        } else {
            Vec::new()
        };
        let actual_sequences = if sequences.is_empty() {
            "-".to_string()
        } else {
            sequences
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(",")
        };
        assert_eq!(
            (actual_status, actual_sequences.as_str()),
            (row[6], row[7]),
            "{}",
            row[0]
        );
    }
}
