//! Real mTLS control-plane fixture for container-only Runtime Agent acceptance.
//!
//! 真实 mTLS 控制面 fixture：驱动容器 Agent 的 reconnect、Lease 与 fencing 验收。

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cy_artifact_transfer::{
    ArtifactKind, ArtifactRef, DevelopmentTransferTicketAuthority, TransferTicketIssuer,
    TransferTicketRequest,
};
use cy_execution_fabric::{
    validate_hello, DevelopmentEnrollmentProvider, EnrollmentProvider, NodeLifecycleProjection,
    RuntimeScope,
};
use cy_kernel_contract::Identity as ContractIdentity;
use cy_proto::core_v1::{
    self, control_plane_to_node,
    node_control_service_server::{NodeControlService, NodeControlServiceServer},
    node_to_control_plane, ControlPlaneToNode, ExecutionAgentWelcome, LeaseRenewalResult,
    NodeToControlPlane, RuntimeAssignment, StopCommand,
};
use cy_proto::semantic_v1;
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use tokio_stream::{wrappers::ReceiverStream, Stream};
use tonic::transport::{Certificate, Identity, Server, ServerTlsConfig};
use tonic::{Request, Response, Status, Streaming};

type ResponseStream = Pin<Box<dyn Stream<Item = Result<ControlPlaneToNode, Status>> + Send>>;

#[derive(Clone)]
struct Fixture {
    shared: Arc<Mutex<FixtureState>>,
}

struct FixtureState {
    trace_path: PathBuf,
    command_dir: PathBuf,
    organization_id: String,
    workspace_id: String,
    enrollment: DevelopmentEnrollmentProvider,
    authority_generation: u64,
    next_session: u64,
    disconnect_generation: u64,
    wait_for_assignment_trigger: bool,
    disconnected_generations: BTreeSet<u64>,
    stop_sent_generations: BTreeSet<u64>,
    leases: BTreeMap<u64, LeaseRecord>,
    nodes: BTreeMap<String, NodeLifecycleProjection>,
    artifact: ArtifactFixture,
    artifact_ticket_authority: DevelopmentTransferTicketAuthority,
    ticket_signature_path: PathBuf,
}

struct LeaseRecord {
    proto: semantic_v1::Lease,
    last_seen_unix_ms: u64,
    connected: bool,
    desired_stopped: bool,
    stop_ack: bool,
    loss_reported: bool,
    node_id: String,
}

#[derive(Clone)]
struct ArtifactFixture {
    uri: String,
    digest: String,
    size_bytes: u64,
    part_size_bytes: u64,
    part_digests: Vec<String>,
    replica_uri: String,
}

#[tonic::async_trait]
impl NodeControlService for Fixture {
    type ConnectStream = ResponseStream;

    async fn connect(
        &self,
        request: Request<Streaming<NodeToControlPlane>>,
    ) -> Result<Response<Self::ConnectStream>, Status> {
        let mut inbound = request.into_inner();
        let hello_frame = inbound
            .message()
            .await?
            .ok_or_else(|| Status::invalid_argument("missing hello"))?;
        let Some(node_to_control_plane::Body::ExecutionAgentHello(hello)) = hello_frame.body else {
            return Err(Status::invalid_argument("expected ExecutionAgentHello"));
        };
        validate_hello(&hello).map_err(|error| Status::invalid_argument(error.to_string()))?;
        let runtime = hello
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.identity.as_ref())
            .ok_or_else(|| Status::invalid_argument("missing Runtime identity"))?
            .clone();
        let descriptor = hello
            .node
            .as_ref()
            .ok_or_else(|| Status::invalid_argument("missing Node identity"))?
            .clone();
        let node_id = descriptor
            .node
            .as_ref()
            .ok_or_else(|| Status::invalid_argument("missing NodeRef"))?
            .node_id
            .clone();
        let (
            session_id,
            resume_token,
            assignment,
            disconnect_this_session,
            assignment_trigger,
            trace_path,
        ) = {
            let mut state = self
                .shared
                .lock()
                .map_err(|_| Status::internal("fixture state poisoned"))?;
            if runtime.generation < state.authority_generation {
                trace(
                    &state.trace_path,
                    &format!(
                        "STALE_GENERATION_REJECTED runtime={} generation={} authority={}",
                        runtime.id, runtime.generation, state.authority_generation
                    ),
                );
                return Err(Status::failed_precondition("STALE_GENERATION"));
            }
            authenticate(&mut state, &hello, &runtime).map_err(Status::unauthenticated)?;
            state.authority_generation = state.authority_generation.max(runtime.generation);
            fs::write(
                state.command_dir.join("authority-generation"),
                state.authority_generation.to_string(),
            )
            .map_err(io_status)?;
            state.next_session += 1;
            let session_id = format!("session-{}-{}", runtime.generation, state.next_session);
            let resume_token = format!("resume-{}-{}", runtime.id, runtime.generation);
            let reconnected = if let Some(node) = state.nodes.get_mut(&node_id) {
                node.reconnect(&descriptor, session_id.clone())
                    .map_err(|error| Status::failed_precondition(error.to_string()))?;
                true
            } else {
                state.nodes.insert(
                    node_id.clone(),
                    NodeLifecycleProjection::enroll(&hello, session_id.clone())
                        .map_err(|error| Status::invalid_argument(error.to_string()))?,
                );
                false
            };
            let lease = new_lease(&runtime, now_unix_ms().saturating_add(10_000));
            state
                .leases
                .entry(runtime.generation)
                .and_modify(|record| {
                    record.connected = true;
                    record.last_seen_unix_ms = now_unix_ms();
                })
                .or_insert(LeaseRecord {
                    proto: lease.clone(),
                    last_seen_unix_ms: now_unix_ms(),
                    connected: true,
                    desired_stopped: false,
                    stop_ack: false,
                    loss_reported: false,
                    node_id: node_id.clone(),
                });
            let disconnect = runtime.generation == state.disconnect_generation
                && !state.disconnected_generations.contains(&runtime.generation);
            trace(&state.trace_path, &format!("ENROLLED runtime={} generation={} node={} attachment={} persistent={} restart={} capabilities={}", runtime.id, runtime.generation, node_id, hello.attachment_type, descriptor.persistent.unwrap_or(false), hello.restart_capability, hello.capabilities.len()));
            if reconnected {
                trace(
                    &state.trace_path,
                    &format!("PERSISTENT_NODE_RECONNECTED node={node_id} state=ONLINE"),
                );
            } else {
                trace(
                    &state.trace_path,
                    &format!(
                        "NODE_ENROLLED node={node_id} persistent={}",
                        descriptor.persistent.unwrap_or(false)
                    ),
                );
            }
            trace(
                &state.trace_path,
                &format!(
                    "ASSIGNMENT generation={} logical_run=logical-run-1 attempt=attempt-{}",
                    runtime.generation, runtime.generation
                ),
            );
            (
                session_id,
                resume_token,
                make_assignment(&state, &runtime, lease),
                disconnect,
                state.wait_for_assignment_trigger.then(|| {
                    state
                        .command_dir
                        .join(format!("start-{}", runtime.generation))
                }),
                state.trace_path.clone(),
            )
        };

        let (outbound, receiver) = mpsc::channel(64);
        let shared = Arc::clone(&self.shared);
        let connection_node_id = node_id.clone();
        tokio::spawn(async move {
            let mut sequence = 1_u64;
            if send_control(
                &outbound,
                ControlPlaneToNode {
                    frame_id: format!("{session_id}-welcome"),
                    sequence_number: sequence,
                    session_id: session_id.clone(),
                    ack_sequence_number: 1,
                    body: Some(control_plane_to_node::Body::ExecutionAgentWelcome(
                        ExecutionAgentWelcome {
                            session_id: session_id.clone(),
                            selected_protocol_version: 2,
                            heartbeat_interval: Some(prost_types::Duration {
                                seconds: 1,
                                nanos: 0,
                            }),
                            server_time: Some(now_timestamp()),
                            resume_token,
                            acknowledged_agent_sequence: 1,
                            selected_contract: Some(semantic_v1::ContractRevision {
                                contract_id: "cyrene.kernel.semantic".to_string(),
                                major: 1,
                                minor: 0,
                            }),
                        },
                    )),
                },
            )
            .await
            .is_err()
            {
                return;
            }
            sequence += 1;
            if let Some(trigger) = assignment_trigger {
                trace(
                    &trace_path,
                    &format!(
                        "ASSIGNMENT_WAITING generation={} trigger={}",
                        runtime.generation,
                        trigger.display()
                    ),
                );
                while !trigger.exists() {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                trace(
                    &trace_path,
                    &format!("ASSIGNMENT_RELEASED generation={}", runtime.generation),
                );
            }
            if send_control(
                &outbound,
                control_frame(
                    &session_id,
                    sequence,
                    1,
                    control_plane_to_node::Body::RuntimeAssignment(assignment),
                ),
            )
            .await
            .is_err()
            {
                return;
            }
            let mut last_agent_sequence = 1_u64;
            let mut command_poll = tokio::time::interval(Duration::from_millis(100));
            command_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    frame = inbound.message() => match frame {
                        Ok(Some(frame)) => {
                            last_agent_sequence = last_agent_sequence.max(frame.sequence_number);
                            let outcome = observe_agent_frame(&shared, &runtime, &frame);
                            if let Some(request) = outcome.renewal {
                                sequence += 1;
                                let renewed = renew_lease(&shared, &runtime, &request);
                                let result = match renewed {
                                    Ok(lease) => LeaseRenewalResult { request_id: request.request_id, runtime: Some(runtime_ref(&runtime)), outcome: Some(core_v1::lease_renewal_result::Outcome::Lease(lease)) },
                                    Err(reason) => LeaseRenewalResult { request_id: request.request_id, runtime: Some(runtime_ref(&runtime)), outcome: Some(core_v1::lease_renewal_result::Outcome::Rejection(semantic_v1::Rejection { reason_code: reason, message: "fixture rejected renewal".to_string() })) },
                                };
                                if send_control(&outbound, control_frame(&session_id, sequence, last_agent_sequence, control_plane_to_node::Body::LeaseRenewalResult(result))).await.is_err() { break; }
                            }
                            if disconnect_this_session && outcome.running_heartbeat {
                                if let Ok(mut state) = shared.lock() {
                                    state.disconnected_generations.insert(runtime.generation);
                                    trace(&state.trace_path, &format!("CONTROL_CHANNEL_FORCED_DISCONNECT generation={}", runtime.generation));
                                }
                                break;
                            }
                        }
                        _ => break,
                    },
                    _ = command_poll.tick() => {
                        let should_stop = shared.lock().ok().is_some_and(|state| state.command_dir.join(format!("stop-{}", runtime.generation)).exists() && !state.stop_sent_generations.contains(&runtime.generation));
                        if should_stop {
                            if let Ok(mut state) = shared.lock() {
                                state.stop_sent_generations.insert(runtime.generation);
                                if let Some(lease) = state.leases.get_mut(&runtime.generation) { lease.desired_stopped = true; }
                                trace(&state.trace_path, &format!("STOP_COMMAND generation={}", runtime.generation));
                            }
                            sequence += 1;
                            let command = StopCommand { command_id: format!("stop-{}", runtime.generation), runtime: Some(runtime_ref(&runtime)), grace_period: Some(prost_types::Duration { seconds: 5, nanos: 0 }), reason_code: "USER_REQUESTED".to_string() };
                            if send_control(&outbound, control_frame(&session_id, sequence, last_agent_sequence, control_plane_to_node::Body::StopCommand(command))).await.is_err() { break; }
                        }
                    }
                }
            }
            if let Ok(mut state) = shared.lock() {
                if let Some(lease) = state.leases.get_mut(&runtime.generation) {
                    lease.connected = false;
                }
                if let Some(node) = state.nodes.get_mut(&connection_node_id) {
                    node.disconnect(true);
                }
                trace(
                    &state.trace_path,
                    &format!(
                        "CONTROL_CHANNEL_CLOSED generation={} node={} node_state=OFFLINE",
                        runtime.generation, connection_node_id
                    ),
                );
            }
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(receiver))))
    }
}

struct ObservationOutcome {
    renewal: Option<core_v1::LeaseRenewalRequest>,
    running_heartbeat: bool,
}

fn observe_agent_frame(
    shared: &Arc<Mutex<FixtureState>>,
    runtime: &semantic_v1::Identity,
    frame: &NodeToControlPlane,
) -> ObservationOutcome {
    let mut outcome = ObservationOutcome {
        renewal: None,
        running_heartbeat: false,
    };
    let Ok(mut state) = shared.lock() else {
        return outcome;
    };
    if let Some(record) = state.leases.get_mut(&runtime.generation) {
        record.last_seen_unix_ms = now_unix_ms();
    }
    match frame.body.as_ref() {
        Some(node_to_control_plane::Body::Heartbeat(heartbeat)) => trace(
            &state.trace_path,
            &format!(
                "NODE_HEARTBEAT generation={} node={} epoch={}",
                runtime.generation,
                heartbeat
                    .node
                    .as_ref()
                    .map(|node| node.node_id.as_str())
                    .unwrap_or(""),
                heartbeat.observed_generation
            ),
        ),
        Some(node_to_control_plane::Body::AssignmentAck(ack)) => trace(
            &state.trace_path,
            &format!(
                "ASSIGNMENT_ACK generation={} disposition={} reason={}",
                runtime.generation,
                ack.disposition,
                ack.rejection
                    .as_ref()
                    .map(|value| value.reason_code.as_str())
                    .unwrap_or("")
            ),
        ),
        Some(node_to_control_plane::Body::RuntimeHeartbeat(heartbeat)) => {
            outcome.running_heartbeat =
                heartbeat.observed_state == core_v1::RuntimeObservedState::Running as i32;
            trace(
                &state.trace_path,
                &format!(
                    "HEARTBEAT generation={} fence={} state={}",
                    runtime.generation, heartbeat.fence_token, heartbeat.observed_state
                ),
            );
        }
        Some(node_to_control_plane::Body::RuntimeObservation(observation)) => trace(
            &state.trace_path,
            &format!(
                "OBSERVATION generation={} state={} termination={} reason={} summary={}",
                runtime.generation,
                observation.observed_state,
                observation.termination,
                observation.reason_code,
                observation.summary
            ),
        ),
        Some(node_to_control_plane::Body::RuntimeProgress(progress)) => trace(
            &state.trace_path,
            &format!(
                "PROGRESS generation={} completed={} total={} unit={}",
                runtime.generation, progress.completed_units, progress.total_units, progress.unit
            ),
        ),
        Some(node_to_control_plane::Body::StructuredEvent(event)) => trace(
            &state.trace_path,
            &format!(
                "STRUCTURED_EVENT generation={} kind={} schema={}",
                runtime.generation, event.kind, event.schema_id
            ),
        ),
        Some(node_to_control_plane::Body::LogReference(reference)) => trace(
            &state.trace_path,
            &format!(
                "LOG_REFERENCE generation={} artifact={} digest={} size={}",
                runtime.generation, reference.artifact_uri, reference.digest, reference.size_bytes
            ),
        ),
        Some(node_to_control_plane::Body::ExecutionInventory(_)) => trace(
            &state.trace_path,
            &format!("INVENTORY generation={}", runtime.generation),
        ),
        Some(node_to_control_plane::Body::LeaseRenewal(request)) => {
            trace(
                &state.trace_path,
                &format!(
                    "LEASE_RENEWAL_REQUEST generation={} fence={}",
                    runtime.generation, request.fence_token
                ),
            );
            outcome.renewal = Some(request.clone());
        }
        Some(node_to_control_plane::Body::StopAck(_)) => {
            if let Some(record) = state.leases.get_mut(&runtime.generation) {
                record.stop_ack = true;
            }
            trace(
                &state.trace_path,
                &format!("STOP_ACK generation={}", runtime.generation),
            );
        }
        _ => {}
    }
    outcome
}

fn authenticate(
    state: &mut FixtureState,
    hello: &core_v1::ExecutionAgentHello,
    runtime: &semantic_v1::Identity,
) -> Result<(), String> {
    let expected_resume = format!("resume-{}-{}", runtime.id, runtime.generation);
    if !hello.resume_token.is_empty() {
        return (hello.resume_token == expected_resume)
            .then_some(())
            .ok_or_else(|| "resume token mismatch".to_string());
    }
    let scope = RuntimeScope {
        organization_id: state.organization_id.clone(),
        workspace_id: state.workspace_id.clone(),
        runtime: ContractIdentity {
            id: runtime.id.clone(),
            generation: runtime.generation,
        },
    };
    state
        .enrollment
        .enroll(&hello.enrollment_proof, scope, now_unix_ms())
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn make_assignment(
    state: &FixtureState,
    runtime: &semantic_v1::Identity,
    lease: semantic_v1::Lease,
) -> RuntimeAssignment {
    RuntimeAssignment {
        assignment_id: format!("logical-run-1-attempt-{}", runtime.generation),
        runtime: Some(runtime_ref(runtime)),
        operation: Some(semantic_v1::Identity {
            id: "operation-1".to_string(),
            generation: 1,
        }),
        attempt_id: format!("attempt-{}", runtime.generation),
        lease: Some(lease),
        workload_identity: Some(core_v1::WorkloadIdentity {
            identity: Some(semantic_v1::Identity {
                id: format!("workload-{}", runtime.generation),
                generation: 1,
            }),
            scope: Some(core_v1::AccountScope {
                user_id: "fixture-user".to_string(),
                organization_id: state.organization_id.clone(),
                workspace_id: state.workspace_id.clone(),
            }),
            runtime: Some(runtime_ref(runtime)),
            allowed_actions: vec!["artifact.read".to_string(), "operation.report".to_string()],
            expires_at: Some(timestamp_from_ms(now_unix_ms() + 60_000)),
        }),
        profile: Some(core_v1::RuntimeProfile {
            image_digest: format!("sha256:{}", "1".repeat(64)),
            resolved_digest: format!("sha256:{}", "2".repeat(64)),
        }),
        desired_state: core_v1::DesiredRuntimeState::Running as i32,
        artifacts: vec![core_v1::ArtifactTransferSpec {
            artifact_uri: state.artifact.uri.clone(),
            digest: state.artifact.digest.clone(),
            size_bytes: state.artifact.size_bytes,
            manifest_digest: String::new(),
            part_size_bytes: state.artifact.part_size_bytes,
            part_digests: state.artifact.part_digests.clone(),
            sources: vec![core_v1::ArtifactTransferSource {
                peer_id: "seed-peer-1".to_string(),
                replica_id: "seed-replica-1".to_string(),
                locator: state.artifact.replica_uri.clone(),
                transfer_ticket: transfer_ticket_json(state, runtime),
            }],
            part_sources: state
                .artifact
                .part_digests
                .iter()
                .enumerate()
                .map(|(index, _)| core_v1::ArtifactPartSource {
                    part_index: u32::try_from(index).unwrap_or(u32::MAX),
                    peer_id: "seed-peer-1".to_string(),
                    replica_id: "seed-replica-1".to_string(),
                })
                .collect(),
            destination_peer_id: format!("node-cache-{}", runtime.generation),
            artifact_kind: "generic".to_string(),
            ..Default::default()
        }],
        local_artifacts: Vec::new(),
    }
}

fn transfer_ticket_json(state: &FixtureState, runtime: &semantic_v1::Identity) -> String {
    let ticket = state
        .artifact_ticket_authority
        .issue(TransferTicketRequest {
            ticket_id: format!("ticket-{}", runtime.generation),
            artifact: ArtifactRef {
                uri: state.artifact.uri.clone(),
                digest: state.artifact.digest.clone(),
                size_bytes: state.artifact.size_bytes,
                kind: ArtifactKind::generic(),
                manifest_digest: None,
            },
            source_peer_id: "seed-peer-1".to_string(),
            destination_peer_id: format!("node-cache-{}", runtime.generation),
            allowed_parts: (0..state.artifact.part_digests.len())
                .filter_map(|index| u32::try_from(index).ok())
                .collect(),
            expires_at_unix_ms: now_unix_ms().saturating_add(60_000),
            max_bytes: state.artifact.size_bytes,
        })
        .expect("fixture Artifact authority must issue a valid ticket");
    fs::write(&state.ticket_signature_path, &ticket.signature)
        .expect("fixture ticket signature must be published before assignment");
    serde_json::to_string(&ticket).expect("fixture TransferTicket serialization")
}

fn new_lease(runtime: &semantic_v1::Identity, expiry: u64) -> semantic_v1::Lease {
    semantic_v1::Lease {
        identity: Some(semantic_v1::Identity {
            id: format!("lease-{}", runtime.id),
            generation: runtime.generation,
        }),
        holder: Some(runtime.clone()),
        resources: vec![semantic_v1::Identity {
            id: "resource-fixture-gpu".to_string(),
            generation: 1,
        }],
        state: semantic_v1::LeaseState::Active as i32,
        fence_token: runtime.generation,
        expires_at: Some(timestamp_from_ms(expiry)),
    }
}

fn renew_lease(
    shared: &Arc<Mutex<FixtureState>>,
    runtime: &semantic_v1::Identity,
    request: &core_v1::LeaseRenewalRequest,
) -> Result<semantic_v1::Lease, String> {
    let mut state = shared.lock().map_err(|_| "FIXTURE_LOCK".to_string())?;
    let trace_path = state.trace_path.clone();
    let record = state
        .leases
        .get_mut(&runtime.generation)
        .ok_or_else(|| "LEASE_NOT_FOUND".to_string())?;
    if request.fence_token != record.proto.fence_token {
        return Err("FENCE_MISMATCH".to_string());
    }
    let requested = request
        .requested_expires_at
        .as_ref()
        .map(timestamp_ms)
        .ok_or_else(|| "LEASE_EXPIRY_REQUIRED".to_string())?;
    record.proto.expires_at = Some(timestamp_from_ms(requested));
    trace(
        &trace_path,
        &format!(
            "LEASE_RENEWED generation={} expires={}",
            runtime.generation, requested
        ),
    );
    Ok(record.proto.clone())
}

fn control_frame(
    session_id: &str,
    sequence: u64,
    ack: u64,
    body: control_plane_to_node::Body,
) -> ControlPlaneToNode {
    ControlPlaneToNode {
        frame_id: format!("{session_id}-control-{sequence}"),
        sequence_number: sequence,
        session_id: session_id.to_string(),
        ack_sequence_number: ack,
        body: Some(body),
    }
}

async fn send_control(
    sender: &mpsc::Sender<Result<ControlPlaneToNode, Status>>,
    frame: ControlPlaneToNode,
) -> Result<(), ()> {
    sender.send(Ok(frame)).await.map_err(|_| ())
}

fn runtime_ref(runtime: &semantic_v1::Identity) -> core_v1::RuntimeRef {
    core_v1::RuntimeRef {
        identity: Some(runtime.clone()),
    }
}

fn artifact_fixture(
    path: &Path,
    replica_uri: String,
    part_size_bytes: u64,
) -> Result<ArtifactFixture, Box<dyn std::error::Error>> {
    let mut input = File::open(path)?;
    let size_bytes = input.metadata()?.len();
    let mut whole = Sha256::new();
    let mut part_digests = Vec::new();
    loop {
        let mut bytes = vec![0_u8; usize::try_from(part_size_bytes)?];
        let read = input.read(&mut bytes)?;
        if read == 0 {
            break;
        }
        bytes.truncate(read);
        whole.update(&bytes);
        part_digests.push(format!("sha256:{:x}", Sha256::digest(&bytes)));
    }
    let digest = format!("sha256:{:x}", whole.finalize());
    Ok(ArtifactFixture {
        uri: format!("artifact://sha256/{}", &digest[7..]),
        digest,
        size_bytes,
        part_size_bytes,
        part_digests,
        replica_uri,
    })
}

async fn loss_monitor(shared: Arc<Mutex<FixtureState>>) {
    let mut interval = tokio::time::interval(Duration::from_millis(100));
    loop {
        interval.tick().await;
        let Ok(mut state) = shared.lock() else {
            continue;
        };
        let now = now_unix_ms();
        let trace_path = state.trace_path.clone();
        let mut closed_nodes = Vec::new();
        for (generation, record) in &mut state.leases {
            let expiry = record
                .proto
                .expires_at
                .as_ref()
                .map(timestamp_ms)
                .unwrap_or_default();
            if !record.connected && now >= expiry && !record.loss_reported {
                record.loss_reported = true;
                let classification = if record.desired_stopped && record.stop_ack {
                    "EXPECTED_TERMINATION"
                } else {
                    "UNEXPECTED_LOSS"
                };
                trace(
                    &trace_path,
                    &format!(
                        "LEASE_EXPIRED generation={} classification={classification}",
                        generation
                    ),
                );
                closed_nodes.push(record.node_id.clone());
            }
        }
        for node_id in closed_nodes {
            let mut terminal_state = None;
            if let Some(node) = state.nodes.get_mut(&node_id) {
                node.lease_expired();
                if !node.persistent() {
                    let _ = node.terminate();
                    terminal_state = Some("TERMINATED");
                }
            }
            if let Some(terminal_state) = terminal_state {
                trace(
                    &trace_path,
                    &format!("EPHEMERAL_NODE_TERMINATED node={node_id} state={terminal_state}"),
                );
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let bind: SocketAddr = required("CYRENE_FIXTURE_BIND")?.parse()?;
    let trace_path = PathBuf::from(required("CYRENE_FIXTURE_TRACE")?);
    let command_dir = PathBuf::from(required("CYRENE_FIXTURE_COMMAND_DIR")?);
    fs::create_dir_all(&command_dir)?;
    fs::write(&trace_path, "FIXTURE_STARTED\n")?;
    let artifact = artifact_fixture(
        Path::new(&required("CYRENE_FIXTURE_ARTIFACT_FILE")?),
        required("CYRENE_FIXTURE_ARTIFACT_URL")?,
        env::var("CYRENE_FIXTURE_PART_SIZE")
            .unwrap_or_else(|_| "1048576".to_string())
            .parse()?,
    )?;
    let artifact_ticket_authority = DevelopmentTransferTicketAuthority::new(fs::read(required(
        "CYRENE_FIXTURE_ARTIFACT_TICKET_KEY",
    )?)?)?;
    let state = FixtureState {
        trace_path,
        command_dir,
        organization_id: "organization-fixture".to_string(),
        workspace_id: "workspace-fixture".to_string(),
        enrollment: DevelopmentEnrollmentProvider::new(
            (1..=16).map(|generation| format!("development-token-{generation}")),
            60_000,
        ),
        authority_generation: 0,
        next_session: 0,
        disconnect_generation: env::var("CYRENE_FIXTURE_DISCONNECT_GENERATION")
            .unwrap_or_else(|_| "2".to_string())
            .parse()?,
        wait_for_assignment_trigger: env::var("CYRENE_FIXTURE_WAIT_FOR_START")
            .is_ok_and(|value| value == "1"),
        disconnected_generations: BTreeSet::new(),
        stop_sent_generations: BTreeSet::new(),
        leases: BTreeMap::new(),
        nodes: BTreeMap::new(),
        artifact,
        artifact_ticket_authority,
        ticket_signature_path: PathBuf::from(required("CYRENE_FIXTURE_TICKET_SIGNATURE")?),
    };
    let shared = Arc::new(Mutex::new(state));
    tokio::spawn(loss_monitor(Arc::clone(&shared)));
    let tls = ServerTlsConfig::new()
        .identity(Identity::from_pem(
            fs::read(required("CYRENE_FIXTURE_SERVER_CERT")?)?,
            fs::read(required("CYRENE_FIXTURE_SERVER_KEY")?)?,
        ))
        .client_ca_root(Certificate::from_pem(fs::read(required(
            "CYRENE_FIXTURE_CLIENT_CA",
        )?)?));
    Server::builder()
        .tls_config(tls)?
        .add_service(NodeControlServiceServer::new(Fixture { shared }))
        .serve(bind)
        .await?;
    Ok(())
}

fn trace(path: &Path, line: &str) {
    if let Ok(mut output) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(output, "{line}");
        let _ = output.flush();
    }
}

fn required(name: &str) -> Result<String, std::io::Error> {
    env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("{name} is required"),
            )
        })
}

fn now_unix_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}
fn now_timestamp() -> prost_types::Timestamp {
    timestamp_from_ms(now_unix_ms())
}
fn timestamp_from_ms(value: u64) -> prost_types::Timestamp {
    prost_types::Timestamp {
        seconds: i64::try_from(value / 1000).unwrap_or(i64::MAX),
        nanos: i32::try_from((value % 1000) * 1_000_000).unwrap_or_default(),
    }
}
fn timestamp_ms(value: &prost_types::Timestamp) -> u64 {
    u64::try_from(value.seconds)
        .unwrap_or_default()
        .saturating_mul(1000)
        .saturating_add(u64::try_from(value.nanos).unwrap_or_default() / 1_000_000)
}
fn io_status(error: std::io::Error) -> Status {
    Status::internal(error.to_string())
}
