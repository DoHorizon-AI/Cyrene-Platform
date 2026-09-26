//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 agent.rs                                                        │
//! │  Module: cy_runtime_agent::agent                                    │
//! │  Role: Reconnecting execution-control loop and Lease enforcement.   │
//! │                                                                     │
//! │  模块职责：维护出站控制连接、执行 Lease 围栏并协调 Artifact/子进程。      │
//! └─────────────────────────────────────────────────────────────────────┘

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cy_artifact_transfer::{
    ArtifactKind, ArtifactPeer, ArtifactPeerKind, ArtifactRef, ArtifactReplica,
    DevelopmentTransferTicketAuthority, HttpRangeTransfer, TransferManifest, TransferPart,
    TransferPartSource, TransferPlan, TransferProtocol, TransferSession, TransferSource,
    TransferTicket, TransferTicketVerifier,
};
use cy_execution_fabric::{
    artifact_transfer_capability, execution_capability, validate_assignment, validate_renewal,
    AdmissionDisposition, FabricContractError, ObservationCursor,
};
use cy_kernel_contract::Lease as SemanticLease;
use cy_proto::core_v1::{
    self, control_plane_to_node, node_control_service_client::NodeControlServiceClient,
    node_to_control_plane, AssignmentAck, AssignmentAckDisposition, ControlPlaneToNode,
    ExecutionAgentHello, ExecutionAgentWelcome, LeaseRenewalRequest, NodeToControlPlane,
    RuntimeHeartbeat, RuntimeObservation, RuntimeObservedState, StopAck, TerminationClassification,
};
use cy_proto::semantic_v1;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::sync::mpsc;
use tokio::time;
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint, Identity};
use tonic::Request;

use crate::journal::Journal;
use crate::outbox::AgentOutbox;
use crate::{ChildSupervisor, RuntimeAgentConfig, WorkloadOutputStream};

const PROTOCOL_VERSION: u32 = 2;

/// Runtime Agent configuration, transport, integrity, or supervision failure.
/// Runtime Agent 的 configuration、transport、integrity 或 supervision 失败。
#[derive(Debug, Error)]
pub enum RuntimeAgentError {
    #[error("Runtime Agent configuration is invalid: {0}")]
    Configuration(String),
    #[error("failed to read credential from {path}: {source}")]
    CredentialRead {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("credential at {0} is empty")]
    EmptyCredential(PathBuf),
    #[error("control-plane transport failed: {0}")]
    Transport(String),
    #[error("control-plane protocol failed: {0}")]
    Protocol(#[from] FabricContractError),
    #[error("Runtime Agent state failed: {0}")]
    State(String),
    #[error("Artifact transfer failed: {0}")]
    Artifact(String),
    #[error("child supervision failed: {0}")]
    Child(String),
    #[error("Runtime Agent observation outbox is full")]
    OutboxFull,
}

struct ActiveAssignment {
    assignment_id: String,
    attempt_id: String,
    lease: SemanticLease,
    lease_proto: semantic_v1::Lease,
    pending_renewal: Option<String>,
}

struct AgentState {
    journal: Journal,
    recovery_uncertain: bool,
    outbox: AgentOutbox,
    child: ChildSupervisor,
    assignment: Option<ActiveAssignment>,
    observed_state: RuntimeObservedState,
    accepted_assignments: BTreeSet<String>,
    accepted_stops: BTreeSet<String>,
    renewal_counter: u64,
    event_counter: u64,
    log_reference_published: bool,
}

impl AgentState {
    fn open(config: &RuntimeAgentConfig) -> Result<Self, RuntimeAgentError> {
        let journal = Journal::open(config)?;
        let terminal = journal.terminal()?;
        let recovery_uncertain =
            journal.admission().is_some_and(|a| !a.rejected) && terminal.is_none();
        let observed_state = terminal
            .as_ref()
            .and_then(|t| RuntimeObservedState::try_from(t.observed_state).ok())
            .unwrap_or(if recovery_uncertain {
                RuntimeObservedState::Lost
            } else {
                RuntimeObservedState::Enrolled
            });
        let mut state = Self {
            journal,
            recovery_uncertain,
            outbox: AgentOutbox::default(),
            child: ChildSupervisor::new(config.workload.clone()),
            assignment: None,
            observed_state,
            accepted_assignments: BTreeSet::new(),
            accepted_stops: BTreeSet::new(),
            renewal_counter: 0,
            event_counter: 0,
            log_reference_published: false,
        };
        if let Some(terminal) = terminal {
            state.outbox.enqueue(
                &config.runtime.id,
                node_to_control_plane::Body::RuntimeObservation(terminal),
            )?;
        }
        Ok(state)
    }
}

enum ConnectionOutcome {
    Reconnect(String),
    Completed,
}

enum ControlAction {
    Continue,
    Stop(Duration),
}

/// Run one unprivileged Runtime Agent until its workload reaches a terminal state.
/// 运行一个无特权 Runtime Agent，直到其 workload 进入终态。
pub async fn run_runtime_agent(config: RuntimeAgentConfig) -> Result<(), RuntimeAgentError> {
    config.validate()?;
    fs::create_dir_all(&config.artifact_destination_root)
        .map_err(|error| RuntimeAgentError::Configuration(error.to_string()))?;
    let mut state = AgentState::open(&config)?;
    let mut resume_token = config.resolve_resume_token(&state.journal.directory)?;
    let mut delay = config.reconnect_min;
    let mut terminate = termination_signal()?;

    loop {
        match connect_once(&config, &mut state, &mut resume_token, &mut terminate).await {
            Ok(ConnectionOutcome::Reconnect(token)) => {
                resume_token = token;
                delay = config.reconnect_min;
            }
            Ok(ConnectionOutcome::Completed) => return Ok(()),
            Err(error) => {
                eprintln!("runtime-agent connection failed: {error}");
                if matches!(error, RuntimeAgentError::State(_))
                    || (state.assignment.is_none()
                        && matches!(
                            error,
                            RuntimeAgentError::Configuration(_)
                                | RuntimeAgentError::EmptyCredential(_)
                        ))
                {
                    return Err(error);
                }
                if await_with_supervision(&config, &mut state, &mut terminate, async {
                    time::sleep(delay).await;
                    Ok(())
                })
                .await?
                .is_none()
                {
                    return Ok(());
                }
                delay = backoff(delay, config.reconnect_max);
            }
        }
    }
}

async fn connect_once(
    config: &RuntimeAgentConfig,
    state: &mut AgentState,
    resume_token: &mut String,
    terminate: &mut tokio::signal::unix::Signal,
) -> Result<ConnectionOutcome, RuntimeAgentError> {
    let Some((outbound, mut inbound, welcome_frame)) =
        await_with_supervision(config, state, terminate, async {
            time::timeout(
                config.reconnect_max,
                open_control_stream(config, resume_token),
            )
            .await
            .map_err(|_| RuntimeAgentError::Transport("control handshake timed out".into()))?
        })
        .await?
    else {
        return Ok(ConnectionOutcome::Completed);
    };
    let welcome = accept_welcome(&welcome_frame)?;
    // A Welcome consumes a single-use enrollment proof. Preserve its resume
    // token before any local staging work can fail and force a reconnect.
    // Welcome 会消耗一次性 enrollment proof；本地 staging 前先保存 resume token。
    config.persist_resume_token(&state.journal.directory, &welcome.resume_token)?;
    *resume_token = welcome.resume_token.clone();
    let session_id = welcome.session_id.clone();
    let mut control_cursor = ObservationCursor::default();
    control_cursor.admit(&welcome_frame.frame_id, welcome_frame.sequence_number)?;
    state
        .outbox
        .acknowledge(welcome.acknowledged_agent_sequence);
    state.outbox.begin_session();
    if state.journal.terminal()?.is_none() {
        enqueue_observation(
            config,
            state,
            if state.recovery_uncertain {
                "RECOVERY_REQUIRES_RECONCILIATION"
            } else {
                "SESSION_ESTABLISHED"
            },
            if state.recovery_uncertain {
                "previous workload outcome is unknown; no automatic restart"
            } else {
                "execution session established"
            },
        )?;
    }
    state.outbox.enqueue(
        &config.runtime.id,
        node_to_control_plane::Body::ExecutionInventory(core_v1::ExecutionInventory {
            runtime: Some(runtime_ref(config)),
            // Container attachments advertise semantic Capabilities in Hello;
            // Provider-managed inventory arrives through the existing snapshot authority.
            // 容器 Capability 位于 Hello；Provider inventory 复用现有 snapshot 权威。
            provider_snapshot: None,
        }),
    )?;
    flush_outbox(
        &outbound,
        state,
        &session_id,
        control_cursor.last_sequence(),
    )
    .await?;

    if state.journal.terminal()?.is_some() {
        // Replay durable evidence only; never run a workload after terminal recovery.
        time::sleep(Duration::from_millis(100)).await;
        return Ok(ConnectionOutcome::Completed);
    }

    let heartbeat_every = proto_duration(welcome.heartbeat_interval.as_ref())
        .unwrap_or_else(|| Duration::from_secs(2));
    let mut heartbeat = time::interval(heartbeat_every);
    heartbeat.set_missed_tick_behavior(time::MissedTickBehavior::Delay);
    heartbeat.tick().await;
    let mut process_poll = time::interval(Duration::from_millis(100));
    process_poll.set_missed_tick_behavior(time::MissedTickBehavior::Delay);
    process_poll.tick().await;

    loop {
        tokio::select! {
            frame = inbound.message() => {
                let Some(frame) = frame.map_err(transport_status)? else {
                    return Ok(ConnectionOutcome::Reconnect(welcome.resume_token.clone()));
                };
                if frame.session_id != session_id {
                    return Err(RuntimeAgentError::Transport("control frame belongs to another session".to_string()));
                }
                state.outbox.acknowledge(frame.ack_sequence_number);
                match control_cursor.admit(&frame.frame_id, frame.sequence_number)? {
                    AdmissionDisposition::Duplicate => {}
                    AdmissionDisposition::Accepted => {
                        let action = handle_control_frame(config, state, frame).await?;
                        flush_outbox(&outbound, state, &session_id, control_cursor.last_sequence()).await?;
                        if let ControlAction::Stop(grace) = action {
                            // Give tonic's request stream a scheduling turn so StopAck is
                            // observable before local child termination begins.
                            // 让 tonic request stream 先获得一个调度机会，使 StopAck 能在本地 child termination 开始前被观测到。
                            time::sleep(Duration::from_millis(50)).await;
                            finish_control_stop(config, state, grace).await?;
                            flush_outbox(&outbound, state, &session_id, control_cursor.last_sequence()).await?;
                            time::sleep(Duration::from_millis(100)).await;
                            return Ok(ConnectionOutcome::Completed);
                        }
                    }
                }
                flush_outbox(&outbound, state, &session_id, control_cursor.last_sequence()).await?;
            }
            _ = heartbeat.tick() => {
                heartbeat_and_renew(config, state, heartbeat_every)?;
                flush_outbox(&outbound, state, &session_id, control_cursor.last_sequence()).await?;
            }
            _ = process_poll.tick() => {
                forward_workload_output(config, state)?;
                if check_process_and_lease(config, state).await? {
                    time::sleep(Duration::from_millis(20)).await;
                    forward_workload_output(config, state)?;
                    publish_log_reference(config, state)?;
                    flush_outbox(&outbound, state, &session_id, control_cursor.last_sequence()).await?;
                    time::sleep(Duration::from_millis(100)).await;
                    return Ok(ConnectionOutcome::Completed);
                }
            }
            _ = terminate.recv() => {
                stop_for_local_signal(config, state).await?;
                flush_outbox(&outbound, state, &session_id, control_cursor.last_sequence()).await?;
                time::sleep(Duration::from_millis(100)).await;
                return Ok(ConnectionOutcome::Completed);
            }
        }
    }
}

/// Connection establishment/backoff must not suspend local Lease enforcement.
async fn await_with_supervision<T>(
    config: &RuntimeAgentConfig,
    state: &mut AgentState,
    terminate: &mut tokio::signal::unix::Signal,
    future: impl std::future::Future<Output = Result<T, RuntimeAgentError>>,
) -> Result<Option<T>, RuntimeAgentError> {
    tokio::pin!(future);
    let mut poll = time::interval(Duration::from_millis(100));
    poll.set_missed_tick_behavior(time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            result = &mut future => return result.map(Some),
            _ = terminate.recv() => {
                stop_for_local_signal(config, state).await?;
                return Ok(None);
            }
            _ = poll.tick() => {
                if !state.recovery_uncertain && state.journal.terminal()?.is_none() {
                    // Enforce expiry even if logs/observations cannot currently be sent.
                    let terminal = check_process_and_lease(config, state).await?;
                    forward_workload_output(config, state)?;
                    if terminal { publish_log_reference(config, state)?; }
                }
            }
        }
    }
}

async fn open_control_stream(
    config: &RuntimeAgentConfig,
    resume_token: &str,
) -> Result<
    (
        mpsc::Sender<NodeToControlPlane>,
        tonic::Streaming<ControlPlaneToNode>,
        ControlPlaneToNode,
    ),
    RuntimeAgentError,
> {
    let channel = control_plane_channel(config).await?;
    let mut client = NodeControlServiceClient::new(channel);
    let (outbound, inbound) = mpsc::channel::<NodeToControlPlane>(64);
    outbound
        .send(hello_frame(config, resume_token))
        .await
        .map_err(|_| RuntimeAgentError::Transport("control-plane request stream closed".into()))?;
    let mut inbound = client
        .connect(Request::new(ReceiverStream::new(inbound)))
        .await
        .map_err(transport_status)?
        .into_inner();
    let welcome = inbound
        .message()
        .await
        .map_err(transport_status)?
        .ok_or_else(|| {
            RuntimeAgentError::Transport("control plane closed before welcome".into())
        })?;
    Ok((outbound, inbound, welcome))
}

fn hello_frame(config: &RuntimeAgentConfig, resume_token: &str) -> NodeToControlPlane {
    let enrollment_proof = if resume_token.is_empty() {
        config.enrollment_proof.clone()
    } else {
        String::new()
    };
    NodeToControlPlane {
        frame_id: format!("{}-hello", config.runtime.id),
        sequence_number: 1,
        session_id: String::new(),
        ack_sequence_number: 0,
        body: Some(node_to_control_plane::Body::ExecutionAgentHello(
            ExecutionAgentHello {
                runtime: Some(runtime_ref(config)),
                scope: Some(core_v1::AccountScope {
                    user_id: String::new(),
                    organization_id: config.organization_id.clone(),
                    workspace_id: config.workspace_id.clone(),
                }),
                attachment_type: core_v1::ExecutionAttachmentType::ContainerAgent as i32,
                persistence_class: if config.persistent {
                    core_v1::PersistenceClass::Persistent as i32
                } else {
                    core_v1::PersistenceClass::Ephemeral as i32
                },
                agent_version: config.agent_version.clone(),
                min_protocol_version: PROTOCOL_VERSION,
                max_protocol_version: PROTOCOL_VERSION,
                resume_token: resume_token.to_string(),
                capabilities: vec![
                    execution_capability(
                        core_v1::ExecutionAttachmentType::ContainerAgent,
                        config.persistent,
                        core_v1::RestartCapability::None,
                    ),
                    artifact_transfer_capability(),
                ],
                enrollment_proof,
                node: Some(core_v1::ExecutionNodeDescriptor {
                    node: Some(config.node.clone()),
                    node_type: config.node_type.clone(),
                    persistent: Some(config.persistent),
                }),
                restart_capability: core_v1::RestartCapability::None as i32,
            },
        )),
    }
}

fn accept_welcome(frame: &ControlPlaneToNode) -> Result<ExecutionAgentWelcome, RuntimeAgentError> {
    if frame.sequence_number != 1 || frame.session_id.is_empty() {
        return Err(RuntimeAgentError::Transport(
            "welcome sequence or session is invalid".to_string(),
        ));
    }
    let Some(control_plane_to_node::Body::ExecutionAgentWelcome(welcome)) = frame.body.clone()
    else {
        return Err(RuntimeAgentError::Transport(
            "first frame is not ExecutionAgentWelcome".to_string(),
        ));
    };
    if welcome.session_id != frame.session_id
        || welcome.selected_protocol_version != PROTOCOL_VERSION
        || welcome.resume_token.is_empty()
    {
        return Err(RuntimeAgentError::Transport(
            "welcome negotiation is invalid".to_string(),
        ));
    }
    Ok(welcome)
}

async fn handle_control_frame(
    config: &RuntimeAgentConfig,
    state: &mut AgentState,
    frame: ControlPlaneToNode,
) -> Result<ControlAction, RuntimeAgentError> {
    match frame.body {
        Some(control_plane_to_node::Body::RuntimeAssignment(assignment)) => {
            handle_assignment(config, state, assignment).await?;
            Ok(ControlAction::Continue)
        }
        Some(control_plane_to_node::Body::LeaseRenewalResult(result)) => {
            handle_renewal(state, result)?;
            Ok(ControlAction::Continue)
        }
        Some(control_plane_to_node::Body::StopCommand(command)) => {
            prepare_control_stop(config, state, command)
        }
        _ => Err(RuntimeAgentError::Transport(
            "unsupported protocol-v2 control frame".to_string(),
        )),
    }
}

async fn handle_assignment(
    config: &RuntimeAgentConfig,
    state: &mut AgentState,
    assignment: core_v1::RuntimeAssignment,
) -> Result<(), RuntimeAgentError> {
    if let Some(admission) = state.journal.admission() {
        if admission.id == assignment.assignment_id {
            if admission.fingerprint != Journal::fingerprint(&assignment) {
                return Err(RuntimeAgentError::Transport(
                    "assignment id reused with a different payload".into(),
                ));
            }
            if state.recovery_uncertain {
                // A rejection could authorize rollback of the original Lease.
                return Err(RuntimeAgentError::Transport(
                    "original workload requires reconciliation".into(),
                ));
            }
            if admission.rejected {
                return enqueue_assignment_ack(
                    config,
                    state,
                    &assignment,
                    AssignmentAckDisposition::Rejected,
                    Some(&FabricContractError {
                        reason_code: "WORKLOAD_START_FAILED",
                        message: "persisted spawn rejection".into(),
                    }),
                );
            }
            return enqueue_assignment_ack(
                config,
                state,
                &assignment,
                AssignmentAckDisposition::Duplicate,
                None,
            );
        }
    }
    if state.journal.admission().is_some() || state.journal.terminal()?.is_some() {
        return enqueue_assignment_ack(
            config,
            state,
            &assignment,
            AssignmentAckDisposition::Rejected,
            Some(&FabricContractError {
                reason_code: "RUNTIME_BUSY",
                message: "Runtime generation already has execution evidence".into(),
            }),
        );
    }
    if state
        .accepted_assignments
        .contains(&assignment.assignment_id)
    {
        enqueue_assignment_ack(
            config,
            state,
            &assignment,
            AssignmentAckDisposition::Duplicate,
            None,
        )?;
        return Ok(());
    }
    let now = now_unix_ms();
    let _lease = match validate_assignment(&config.runtime, &assignment, now) {
        Ok(lease) => lease,
        Err(error) => {
            enqueue_assignment_ack(
                config,
                state,
                &assignment,
                AssignmentAckDisposition::Rejected,
                Some(&error),
            )?;
            return Ok(());
        }
    };
    if state.assignment.is_some() || state.child.is_running() {
        let error = FabricContractError {
            reason_code: "RUNTIME_BUSY",
            message: "Runtime generation already owns an active Assignment".to_string(),
        };
        enqueue_assignment_ack(
            config,
            state,
            &assignment,
            AssignmentAckDisposition::Rejected,
            Some(&error),
        )?;
        return Ok(());
    }

    state.observed_state = RuntimeObservedState::Staging;
    enqueue_observation(
        config,
        state,
        "ARTIFACT_STAGING",
        "staging immutable Artifact inputs",
    )?;
    let (downloaded_parts, reused_parts) = match stage_artifacts(config, &assignment).await {
        Ok(result) => result,
        Err(error) => {
            reject_assignment_preparation(
                config,
                state,
                &assignment,
                "ARTIFACT_STAGING_FAILED",
                &error.to_string(),
            )?;
            return Ok(());
        }
    };
    if reused_parts > 0 {
        enqueue_observation(
            config,
            state,
            "ARTIFACT_RESUMED",
            &format!("reused {reused_parts} verified parts and downloaded {downloaded_parts}"),
        )?;
    }
    state.observed_state = RuntimeObservedState::Starting;
    enqueue_observation(
        config,
        state,
        "WORKLOAD_STARTING",
        "starting fixed workload command",
    )?;
    let log_path = workload_log_path(config);
    if log_path.exists() {
        fs::remove_file(&log_path).map_err(|error| RuntimeAgentError::Child(error.to_string()))?;
    }
    state.log_reference_published = false;
    // Staging can outlive the Lease. Revalidate before persisting spawn intent.
    let lease = validate_assignment(&config.runtime, &assignment, now_unix_ms())?;
    state.journal.begin(&assignment)?;
    if let Err(error) = state.child.start(None, &Default::default()).await {
        state.journal.mark_started(false)?;
        reject_assignment_preparation(
            config,
            state,
            &assignment,
            "WORKLOAD_START_FAILED",
            &error.to_string(),
        )?;
        return Ok(());
    }
    state.journal.mark_started(true)?;
    state
        .accepted_assignments
        .insert(assignment.assignment_id.clone());
    state.assignment = Some(ActiveAssignment {
        assignment_id: assignment.assignment_id.clone(),
        attempt_id: assignment.attempt_id.clone(),
        lease,
        lease_proto: assignment
            .lease
            .clone()
            .expect("validated Assignment has Lease"),
        pending_renewal: None,
    });
    enqueue_assignment_ack(
        config,
        state,
        &assignment,
        AssignmentAckDisposition::Accepted,
        None,
    )?;
    state.observed_state = RuntimeObservedState::Running;
    enqueue_observation(
        config,
        state,
        "WORKLOAD_RUNNING",
        "fixed workload command is running",
    )?;
    Ok(())
}

fn reject_assignment_preparation(
    config: &RuntimeAgentConfig,
    state: &mut AgentState,
    assignment: &core_v1::RuntimeAssignment,
    reason_code: &'static str,
    message: &str,
) -> Result<(), RuntimeAgentError> {
    state.observed_state = RuntimeObservedState::Failed;
    enqueue_observation(config, state, reason_code, message)?;
    enqueue_assignment_ack(
        config,
        state,
        assignment,
        AssignmentAckDisposition::Rejected,
        Some(&FabricContractError {
            reason_code,
            message: message.to_string(),
        }),
    )
}

fn enqueue_assignment_ack(
    config: &RuntimeAgentConfig,
    state: &mut AgentState,
    assignment: &core_v1::RuntimeAssignment,
    disposition: AssignmentAckDisposition,
    error: Option<&FabricContractError>,
) -> Result<(), RuntimeAgentError> {
    state.outbox.enqueue(
        &config.runtime.id,
        node_to_control_plane::Body::AssignmentAck(AssignmentAck {
            assignment_id: assignment.assignment_id.clone(),
            runtime: Some(runtime_ref(config)),
            disposition: disposition as i32,
            rejection: error.map(|error| semantic_v1::Rejection {
                reason_code: error.reason_code.to_string(),
                message: error.message.clone(),
            }),
        }),
    )
}

async fn stage_artifacts(
    config: &RuntimeAgentConfig,
    assignment: &core_v1::RuntimeAssignment,
) -> Result<(usize, usize), RuntimeAgentError> {
    // Verify every local CAS input before reading transfer credentials or
    // starting any remote transfer. Local Artifact identity never comes from a
    // path on the wire; the digest is the only path component.
    // 在读取 transfer credential 或开始任何远端传输前，先校验每个本地 CAS input。
    // Local Artifact identity 不来自 wire path；digest 是唯一用于组成路径的部分。
    for input in &assignment.local_artifacts {
        verify_local_artifact(config, input)?;
    }

    if assignment.artifacts.is_empty() {
        return Ok((0, 0));
    }

    let ticket_key_path = config.artifact_ticket_key.as_ref().ok_or_else(|| {
        RuntimeAgentError::Artifact(
            "Artifact transfer requires a configured ticket verification key".to_string(),
        )
    })?;
    let ticket_key = read_credential(ticket_key_path, true)?;
    let ticket_verifier = DevelopmentTransferTicketAuthority::new(ticket_key)
        .map_err(|error| RuntimeAgentError::Artifact(error.to_string()))?;
    let ca = read_credential(&config.artifact_ca, false)?;
    let transfer = HttpRangeTransfer::with_pem_ca(&ca)
        .map_err(|error| RuntimeAgentError::Artifact(error.to_string()))?;
    let mut downloaded_parts = 0;
    let mut reused_parts = 0;
    for spec in &assignment.artifacts {
        let digest_hex = spec
            .digest
            .strip_prefix("sha256:")
            .ok_or_else(|| RuntimeAgentError::Artifact("invalid Artifact digest".to_string()))?;
        let artifact = ArtifactRef {
            uri: spec.artifact_uri.clone(),
            digest: spec.digest.clone(),
            size_bytes: spec.size_bytes,
            kind: ArtifactKind::generic(),
            manifest_digest: (!spec.manifest_digest.is_empty())
                .then(|| spec.manifest_digest.clone()),
        };
        let parts = build_parts(spec)?;
        let manifest = TransferManifest {
            artifact: artifact.clone(),
            part_size_bytes: spec.part_size_bytes,
            parts,
        };
        let destination = config.artifact_destination_root.join(digest_hex);
        let checkpoint_path = config
            .state_dir
            .join(format!("artifact-{digest_hex}.checkpoint.json"));
        let destination_peer_id = if spec.destination_peer_id.is_empty() {
            format!("node-cache-{}", config.node.node_id)
        } else {
            spec.destination_peer_id.clone()
        };
        let sources = build_transfer_sources(spec, &artifact, &ticket_verifier)?;
        let part_sources = spec
            .part_sources
            .iter()
            .map(|route| TransferPartSource {
                part_index: route.part_index,
                peer_id: route.peer_id.clone(),
                replica_id: route.replica_id.clone(),
            })
            .collect();
        let session = TransferSession {
            // Artifact resume identity is content-scoped, not Attempt- or
            // Runtime-generation-scoped, so replacement Attempts reuse parts.
            // Artifact 恢复身份按内容定域，不随 Attempt/Runtime generation 改变。
            session_id: format!("artifact-{digest_hex}"),
            manifest,
            plan: TransferPlan {
                plan_id: format!("plan-{digest_hex}"),
                artifact,
                destination_peer_id,
                sources,
                part_sources,
            },
            destination,
            checkpoint_path,
            concurrency: 4,
        };
        let result = tokio::task::spawn_blocking({
            let transfer = transfer.clone();
            move || transfer.transfer(&session)
        })
        .await
        .map_err(|error| RuntimeAgentError::Artifact(error.to_string()))?
        .map_err(|error| RuntimeAgentError::Artifact(error.to_string()))?;
        downloaded_parts += result.downloaded_parts;
        reused_parts += result.reused_parts;
    }
    Ok((downloaded_parts, reused_parts))
}

/// Re-read and verify one local CAS Artifact before workload startup.
///
/// The CAS layout is deliberately derived from the validated digest rather
/// than accepting a path from the control wire. A missing file, changed
/// content, or declared-size mismatch is an Artifact error and is handled by
/// `handle_assignment` as a rejected assignment with no running child.
/// 在 workload 启动前重新读取并校验一个本地 CAS Artifact。
/// CAS 布局有意由已验证的 digest 推导，不接受 control wire 提供的 path。文件缺失、内容变化或声明大小不匹配都属于 Artifact error；
/// handle_assignment 会将其作为拒绝的 assignment 处理，并且不会留下运行中的 child。
fn verify_local_artifact(
    config: &RuntimeAgentConfig,
    input: &core_v1::ArtifactLocalInput,
) -> Result<(), RuntimeAgentError> {
    let artifact = ArtifactRef {
        uri: input.artifact_uri.clone(),
        digest: input.digest.clone(),
        size_bytes: input.size_bytes,
        kind: ArtifactKind::generic(),
        manifest_digest: (!input.manifest_digest.is_empty()).then(|| input.manifest_digest.clone()),
    };
    artifact.validate().map_err(RuntimeAgentError::Artifact)?;
    let digest_hex = artifact
        .digest
        .strip_prefix("sha256:")
        .ok_or_else(|| RuntimeAgentError::Artifact("invalid Artifact digest".to_string()))?;
    let destination = config.artifact_destination_root.join(digest_hex);
    let metadata = fs::symlink_metadata(&destination).map_err(|error| {
        RuntimeAgentError::Artifact(format!("local Artifact is unavailable: {error}"))
    })?;
    if !metadata.file_type().is_file() {
        return Err(RuntimeAgentError::Artifact(
            "local Artifact CAS entry is not a regular file".to_string(),
        ));
    }
    let (actual_digest, actual_size) = digest_file(&destination)?;
    if actual_digest != artifact.digest {
        return Err(RuntimeAgentError::Artifact(format!(
            "local Artifact digest mismatch: expected {}, got {actual_digest}",
            artifact.digest
        )));
    }
    if actual_size != artifact.size_bytes {
        return Err(RuntimeAgentError::Artifact(format!(
            "local Artifact size mismatch: expected {}, got {actual_size}",
            artifact.size_bytes
        )));
    }
    Ok(())
}

fn build_transfer_sources(
    spec: &core_v1::ArtifactTransferSpec,
    artifact: &ArtifactRef,
    ticket_verifier: &dyn TransferTicketVerifier,
) -> Result<Vec<TransferSource>, RuntimeAgentError> {
    if spec.sources.is_empty() {
        return Err(RuntimeAgentError::Artifact(
            "Artifact transfer requires an Artifact Plane-authorized source Peer and ticket"
                .to_string(),
        ));
    }
    spec.sources
        .iter()
        .map(|source| {
            let ticket: TransferTicket = serde_json::from_str(&source.transfer_ticket)
                .map_err(|error| RuntimeAgentError::Artifact(error.to_string()))?;
            ticket_verifier
                .verify(&ticket)
                .map_err(|error| RuntimeAgentError::Artifact(error.to_string()))?;
            Ok(TransferSource {
                peer: ArtifactPeer {
                    peer_id: source.peer_id.clone(),
                    kind: ArtifactPeerKind::Generic,
                    authorized: true,
                    residency: "control-plane-authorized".to_string(),
                    trust_domain: "workspace".to_string(),
                    classifications: BTreeSet::from(["assigned".to_string()]),
                    policy_tags: BTreeSet::new(),
                    healthy: true,
                    latency_ms: 0,
                    bandwidth_mbps: 0,
                    cost_microunits: 0,
                },
                replica: ArtifactReplica {
                    replica_id: source.replica_id.clone(),
                    artifact: artifact.clone(),
                    peer_id: source.peer_id.clone(),
                    protocol: TransferProtocol::HttpsRangeV1,
                    locator: source.locator.clone(),
                    region: None,
                    priority: 0,
                    expires_at_unix_ms: Some(ticket.expires_at_unix_ms),
                },
                ticket,
            })
        })
        .collect()
}

fn build_parts(
    spec: &core_v1::ArtifactTransferSpec,
) -> Result<Vec<TransferPart>, RuntimeAgentError> {
    let expected = spec.size_bytes.div_ceil(spec.part_size_bytes);
    if spec.part_digests.len() as u64 != expected {
        return Err(RuntimeAgentError::Artifact(
            "part digest count does not cover Artifact size".to_string(),
        ));
    }
    Ok(spec
        .part_digests
        .iter()
        .enumerate()
        .map(|(index, digest)| {
            let start = index as u64 * spec.part_size_bytes;
            TransferPart {
                index: index as u32,
                start,
                end_exclusive: (start + spec.part_size_bytes).min(spec.size_bytes),
                digest: digest.clone(),
            }
        })
        .collect())
}

fn handle_renewal(
    state: &mut AgentState,
    result: core_v1::LeaseRenewalResult,
) -> Result<(), RuntimeAgentError> {
    let Some(active) = state.assignment.as_mut() else {
        return Ok(());
    };
    if active.pending_renewal.as_deref() != Some(&result.request_id) {
        return Ok(());
    }
    active.pending_renewal = None;
    match result.outcome {
        Some(core_v1::lease_renewal_result::Outcome::Lease(lease)) => {
            let renewed = validate_renewal(&active.lease, &lease, now_unix_ms())?;
            active.lease = renewed;
            active.lease_proto = lease;
            Ok(())
        }
        Some(core_v1::lease_renewal_result::Outcome::Rejection(rejection)) => {
            Err(RuntimeAgentError::Transport(format!(
                "Lease renewal rejected: {}",
                rejection.reason_code
            )))
        }
        None => Err(RuntimeAgentError::Transport(
            "Lease renewal result has no outcome".to_string(),
        )),
    }
}

fn prepare_control_stop(
    config: &RuntimeAgentConfig,
    state: &mut AgentState,
    command: core_v1::StopCommand,
) -> Result<ControlAction, RuntimeAgentError> {
    if !runtime_matches(config, command.runtime.as_ref()) {
        return Ok(ControlAction::Continue);
    }
    if state.recovery_uncertain {
        return Err(RuntimeAgentError::Transport(
            "cannot confirm termination of a recovered unknown workload".into(),
        ));
    }
    if state.assignment.is_none() {
        return Ok(ControlAction::Continue);
    }
    state.accepted_stops.insert(command.command_id.clone());
    state.outbox.enqueue(
        &config.runtime.id,
        node_to_control_plane::Body::StopAck(StopAck {
            command_id: command.command_id,
            runtime: Some(runtime_ref(config)),
            accepted_at: Some(now_timestamp()),
        }),
    )?;
    state.observed_state = RuntimeObservedState::Stopping;
    enqueue_observation(config, state, "STOP_ACCEPTED", "graceful stop accepted")?;
    let grace =
        proto_duration(command.grace_period.as_ref()).unwrap_or_else(|| Duration::from_secs(10));
    Ok(ControlAction::Stop(grace))
}

async fn finish_control_stop(
    config: &RuntimeAgentConfig,
    state: &mut AgentState,
    grace: Duration,
) -> Result<(), RuntimeAgentError> {
    if state.child.is_running() {
        state
            .child
            .stop(grace)
            .await
            .map_err(|error| RuntimeAgentError::Child(error.to_string()))?;
    }
    time::sleep(Duration::from_millis(20)).await;
    forward_workload_output(config, state)?;
    publish_log_reference(config, state)?;
    state.observed_state = RuntimeObservedState::Stopped;
    enqueue_terminal(
        config,
        state,
        TerminationClassification::Graceful,
        "GRACEFUL_TERMINATION",
        "workload exited after StopCommand",
    )?;
    Ok(())
}

fn heartbeat_and_renew(
    config: &RuntimeAgentConfig,
    state: &mut AgentState,
    heartbeat_every: Duration,
) -> Result<(), RuntimeAgentError> {
    state.outbox.enqueue(
        &config.runtime.id,
        node_to_control_plane::Body::Heartbeat(core_v1::NodeHeartbeat {
            node: Some(config.node.clone()),
            observed_generation: config.node.node_epoch,
            observed_at: Some(now_timestamp()),
        }),
    )?;
    let Some(active) = state.assignment.as_mut() else {
        return Ok(());
    };
    state.outbox.enqueue(
        &config.runtime.id,
        node_to_control_plane::Body::RuntimeHeartbeat(RuntimeHeartbeat {
            runtime: Some(runtime_ref(config)),
            lease: active.lease_proto.identity.clone(),
            fence_token: active.lease.fence_token,
            observed_state: state.observed_state as i32,
            observed_at: Some(now_timestamp()),
        }),
    )?;
    let now = now_unix_ms();
    let renewal_window_ms = u64::try_from(heartbeat_every.as_millis())
        .unwrap_or(u64::MAX)
        .saturating_mul(3);
    if active.pending_renewal.is_none()
        && active
            .lease
            .expires_at_unix_ms
            .is_some_and(|expiry| expiry.saturating_sub(now) <= renewal_window_ms)
    {
        state.renewal_counter += 1;
        let request_id = format!("{}-renew-{}", active.assignment_id, state.renewal_counter);
        let requested_expiry = now.saturating_add(renewal_window_ms.saturating_mul(4));
        active.pending_renewal = Some(request_id.clone());
        state.outbox.enqueue(
            &config.runtime.id,
            node_to_control_plane::Body::LeaseRenewal(LeaseRenewalRequest {
                request_id,
                runtime: Some(runtime_ref(config)),
                lease: active.lease_proto.identity.clone(),
                fence_token: active.lease.fence_token,
                requested_expires_at: Some(timestamp_from_ms(requested_expiry)),
            }),
        )?;
    }
    Ok(())
}

async fn check_process_and_lease(
    config: &RuntimeAgentConfig,
    state: &mut AgentState,
) -> Result<bool, RuntimeAgentError> {
    if let Some(exit) = state
        .child
        .try_wait()
        .map_err(|error| RuntimeAgentError::Child(error.to_string()))?
    {
        state.observed_state = if exit.success {
            RuntimeObservedState::Stopped
        } else {
            RuntimeObservedState::Failed
        };
        enqueue_terminal(
            config,
            state,
            TerminationClassification::External,
            "WORKLOAD_EXITED",
            &format!("workload exited with code {:?}", exit.code),
        )?;
        return Ok(true);
    }
    let expired = state
        .assignment
        .as_ref()
        .and_then(|active| active.lease.expires_at_unix_ms)
        .is_some_and(|expiry| now_unix_ms() >= expiry);
    if expired {
        if state.child.is_running() {
            state
                .child
                .stop(Duration::from_secs(2))
                .await
                .map_err(|error| RuntimeAgentError::Child(error.to_string()))?;
        }
        state.observed_state = RuntimeObservedState::Lost;
        enqueue_terminal(
            config,
            state,
            TerminationClassification::UnexpectedLoss,
            "LEASE_EXPIRED",
            "workload stopped after canonical Lease expiry",
        )?;
        return Ok(true);
    }
    Ok(false)
}

async fn stop_for_local_signal(
    config: &RuntimeAgentConfig,
    state: &mut AgentState,
) -> Result<(), RuntimeAgentError> {
    if state.recovery_uncertain || state.journal.terminal()?.is_some() || state.assignment.is_none()
    {
        // No child handle after restart is not evidence that the old task stopped.
        return Ok(());
    }
    state.observed_state = RuntimeObservedState::Stopping;
    enqueue_observation(
        config,
        state,
        "LOCAL_TERMINATION_SIGNAL",
        "container or process manager requested graceful termination",
    )?;
    if state.child.is_running() {
        state
            .child
            .stop(Duration::from_secs(10))
            .await
            .map_err(|error| RuntimeAgentError::Child(error.to_string()))?;
    }
    time::sleep(Duration::from_millis(20)).await;
    forward_workload_output(config, state)?;
    publish_log_reference(config, state)?;
    state.observed_state = RuntimeObservedState::Stopped;
    enqueue_terminal(
        config,
        state,
        TerminationClassification::Graceful,
        "GRACEFUL_TERMINATION",
        "workload exited after local termination signal",
    )?;
    persist_local_terminal(&config.state_dir, "GRACEFUL_TERMINATION")
}

fn enqueue_observation(
    config: &RuntimeAgentConfig,
    state: &mut AgentState,
    reason: &str,
    summary: &str,
) -> Result<(), RuntimeAgentError> {
    let (assignment_id, attempt_id) = state
        .assignment
        .as_ref()
        .map(|active| (active.assignment_id.clone(), active.attempt_id.clone()))
        .unwrap_or_default();
    state.outbox.enqueue(
        &config.runtime.id,
        node_to_control_plane::Body::RuntimeObservation(RuntimeObservation {
            runtime: Some(runtime_ref(config)),
            observed_state: state.observed_state as i32,
            termination: TerminationClassification::Unspecified as i32,
            reason_code: reason.to_string(),
            summary: summary.to_string(),
            observed_at: Some(now_timestamp()),
            worker: None,
            operation: None,
            assignment_id,
            attempt_id,
        }),
    )
}

fn enqueue_terminal(
    config: &RuntimeAgentConfig,
    state: &mut AgentState,
    termination: TerminationClassification,
    reason: &str,
    summary: &str,
) -> Result<(), RuntimeAgentError> {
    let active = state.assignment.as_ref().ok_or_else(|| {
        RuntimeAgentError::State("terminal evidence requires an active assignment".to_string())
    })?;
    let observation = RuntimeObservation {
        runtime: Some(runtime_ref(config)),
        observed_state: state.observed_state as i32,
        termination: termination as i32,
        reason_code: reason.to_string(),
        summary: summary.to_string(),
        observed_at: Some(now_timestamp()),
        worker: None,
        operation: None,
        assignment_id: active.assignment_id.clone(),
        attempt_id: active.attempt_id.clone(),
    };
    state.journal.finish(&observation)?;
    state.outbox.enqueue(
        &config.runtime.id,
        node_to_control_plane::Body::RuntimeObservation(observation),
    )
}

fn forward_workload_output(
    config: &RuntimeAgentConfig,
    state: &mut AgentState,
) -> Result<(), RuntimeAgentError> {
    let output = state.child.drain_output(256);
    if output.is_empty() {
        return Ok(());
    }
    let log_path = workload_log_path(config);
    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|error| RuntimeAgentError::Child(error.to_string()))?;
    for line in output {
        let stream = match line.stream {
            WorkloadOutputStream::Stdout => "stdout",
            WorkloadOutputStream::Stderr => "stderr",
        };
        writeln!(log, "{stream}: {}", line.line)
            .map_err(|error| RuntimeAgentError::Child(error.to_string()))?;
        state.event_counter = state.event_counter.saturating_add(1);
        let body = serde_json::to_vec(&serde_json::json!({
            "stream": stream,
            "line": line.line,
        }))
        .map_err(|error| RuntimeAgentError::Child(error.to_string()))?;
        state.outbox.enqueue(
            &config.runtime.id,
            node_to_control_plane::Body::StructuredEvent(core_v1::StructuredAgentEvent {
                runtime: Some(runtime_ref(config)),
                event_id: format!(
                    "{}-{}-{}",
                    config.runtime.id, config.runtime.generation, state.event_counter
                ),
                kind: "workload.log".to_string(),
                schema_id: "cyrene.workload.log.v1".to_string(),
                body,
                observed_at: Some(now_timestamp()),
            }),
        )?;
        if let Some((completed, total, unit)) = parse_progress_line(&line.line) {
            state.outbox.enqueue(
                &config.runtime.id,
                node_to_control_plane::Body::RuntimeProgress(core_v1::RuntimeProgress {
                    runtime: Some(runtime_ref(config)),
                    completed_units: completed,
                    total_units: total,
                    unit,
                    observed_at: Some(now_timestamp()),
                }),
            )?;
        }
    }
    log.flush()
        .map_err(|error| RuntimeAgentError::Child(error.to_string()))
}

fn parse_progress_line(line: &str) -> Option<(u64, u64, String)> {
    let remainder = line.strip_prefix("CYRENE_PROGRESS ")?;
    let mut fields = remainder.splitn(2, ' ');
    let mut counts = fields.next()?.splitn(2, '/');
    let completed = counts.next()?.parse().ok()?;
    let total = counts.next()?.parse().ok()?;
    let unit = fields.next().unwrap_or("units").trim().to_string();
    (completed <= total && total > 0 && !unit.is_empty()).then_some((completed, total, unit))
}

fn publish_log_reference(
    config: &RuntimeAgentConfig,
    state: &mut AgentState,
) -> Result<(), RuntimeAgentError> {
    if state.log_reference_published {
        return Ok(());
    }
    let log_path = workload_log_path(config);
    if !log_path.exists() || fs::metadata(&log_path).is_ok_and(|metadata| metadata.len() == 0) {
        return Ok(());
    }
    let (digest, size_bytes) = digest_file(&log_path)?;
    let digest_hex = digest
        .strip_prefix("sha256:")
        .expect("generated SHA-256 digest");
    let destination = config.artifact_destination_root.join(digest_hex);
    if destination.exists() {
        let (existing_digest, existing_size) = digest_file(&destination)?;
        if existing_digest != digest || existing_size != size_bytes {
            return Err(RuntimeAgentError::Artifact(
                "existing log Artifact does not match its digest path".to_string(),
            ));
        }
    } else {
        let temporary = config.artifact_destination_root.join(format!(
            ".log-publish-{}-{}",
            config.runtime.id, config.runtime.generation
        ));
        fs::copy(&log_path, &temporary)
            .map_err(|error| RuntimeAgentError::Artifact(error.to_string()))?;
        File::open(&temporary)
            .and_then(|handle| handle.sync_all())
            .map_err(|error| RuntimeAgentError::Artifact(error.to_string()))?;
        fs::rename(&temporary, &destination)
            .map_err(|error| RuntimeAgentError::Artifact(error.to_string()))?;
    }
    state.outbox.enqueue(
        &config.runtime.id,
        node_to_control_plane::Body::LogReference(core_v1::LogReference {
            runtime: Some(runtime_ref(config)),
            artifact_uri: format!("artifact://sha256/{digest_hex}"),
            digest,
            size_bytes,
        }),
    )?;
    state.log_reference_published = true;
    Ok(())
}

fn digest_file(path: &Path) -> Result<(String, u64), RuntimeAgentError> {
    let mut input =
        File::open(path).map_err(|error| RuntimeAgentError::Artifact(error.to_string()))?;
    let mut hasher = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = input
            .read(&mut buffer)
            .map_err(|error| RuntimeAgentError::Artifact(error.to_string()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        size = size
            .checked_add(u64::try_from(read).map_err(|_| {
                RuntimeAgentError::Artifact("Artifact size exceeds u64".to_string())
            })?)
            .ok_or_else(|| RuntimeAgentError::Artifact("Artifact size exceeds u64".to_string()))?;
    }
    Ok((format!("sha256:{:x}", hasher.finalize()), size))
}

fn workload_log_path(config: &RuntimeAgentConfig) -> PathBuf {
    config.state_dir.join(format!(
        "workload-{}-{}.log",
        config.runtime.id, config.runtime.generation
    ))
}

async fn flush_outbox(
    outbound: &mpsc::Sender<NodeToControlPlane>,
    state: &mut AgentState,
    session_id: &str,
    control_ack: u64,
) -> Result<(), RuntimeAgentError> {
    for frame in state.outbox.unsent_frames(session_id, control_ack) {
        outbound.send(frame).await.map_err(|_| {
            RuntimeAgentError::Transport("control-plane request stream closed".to_string())
        })?;
    }
    Ok(())
}

async fn control_plane_channel(config: &RuntimeAgentConfig) -> Result<Channel, RuntimeAgentError> {
    let ca = read_credential(&config.control_plane_ca, false)?;
    let certificate = read_credential(&config.client_certificate, false)?;
    let key = read_credential(&config.client_key, true)?;
    let tls = ClientTlsConfig::new()
        .domain_name(config.control_plane_server_name.clone())
        .ca_certificate(Certificate::from_pem(ca))
        .identity(Identity::from_pem(certificate, key));
    Endpoint::from_shared(config.control_plane_endpoint.clone())
        .map_err(|error| RuntimeAgentError::Configuration(error.to_string()))?
        .connect_timeout(config.reconnect_max)
        .tls_config(tls)
        .map_err(|error| RuntimeAgentError::Configuration(error.to_string()))?
        .connect()
        .await
        .map_err(|error| RuntimeAgentError::Transport(error.to_string()))
}

fn read_credential(path: &Path, private_key: bool) -> Result<Vec<u8>, RuntimeAgentError> {
    if private_key {
        validate_private_key_permissions(path)?;
    }
    let bytes = fs::read(path).map_err(|source| RuntimeAgentError::CredentialRead {
        path: path.to_path_buf(),
        source,
    })?;
    if bytes.is_empty() {
        return Err(RuntimeAgentError::EmptyCredential(path.to_path_buf()));
    }
    Ok(bytes)
}

#[cfg(unix)]
fn validate_private_key_permissions(path: &Path) -> Result<(), RuntimeAgentError> {
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::metadata(path)
        .map_err(|source| RuntimeAgentError::CredentialRead {
            path: path.to_path_buf(),
            source,
        })?
        .permissions()
        .mode();
    if mode & 0o077 != 0 {
        return Err(RuntimeAgentError::Configuration(format!(
            "private credential {} must not be readable by group or other users",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_private_key_permissions(_path: &Path) -> Result<(), RuntimeAgentError> {
    Ok(())
}

fn runtime_ref(config: &RuntimeAgentConfig) -> core_v1::RuntimeRef {
    core_v1::RuntimeRef {
        identity: Some(semantic_v1::Identity {
            id: config.runtime.id.clone(),
            generation: config.runtime.generation,
        }),
    }
}

fn runtime_matches(config: &RuntimeAgentConfig, runtime: Option<&core_v1::RuntimeRef>) -> bool {
    runtime
        .and_then(|runtime| runtime.identity.as_ref())
        .is_some_and(|identity| {
            identity.id == config.runtime.id && identity.generation == config.runtime.generation
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

fn proto_duration(value: Option<&prost_types::Duration>) -> Option<Duration> {
    let value = value?;
    if value.seconds < 0 || value.nanos < 0 {
        return None;
    }
    let seconds = u64::try_from(value.seconds).ok()?;
    let nanos = u32::try_from(value.nanos).ok()?;
    (nanos < 1_000_000_000)
        .then(|| Duration::new(seconds, nanos))
        .filter(|duration| !duration.is_zero())
}

fn transport_status(status: tonic::Status) -> RuntimeAgentError {
    RuntimeAgentError::Transport(format!("{}: {}", status.code(), status.message()))
}

fn backoff(current: Duration, maximum: Duration) -> Duration {
    current.checked_mul(2).unwrap_or(maximum).min(maximum)
}

fn persist_local_terminal(state_dir: &Path, reason: &str) -> Result<(), RuntimeAgentError> {
    fs::write(state_dir.join("last-termination"), reason)
        .map_err(|error| RuntimeAgentError::Child(error.to_string()))
}

#[cfg(unix)]
fn termination_signal() -> Result<tokio::signal::unix::Signal, RuntimeAgentError> {
    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|error| RuntimeAgentError::Configuration(error.to_string()))
}

#[cfg(not(unix))]
compile_error!("cy-runtime-agent v1 requires a Unix container runtime");

#[cfg(test)]
mod tests {
    use cy_artifact_transfer::TransferTicketSigner;
    use tempfile::tempdir;

    use super::*;

    #[tokio::test]
    async fn terminal_survives_restart_without_executing_assignment_twice() {
        let directory = tempdir().unwrap();
        let marker = directory.path().join("executions");
        let config = test_config(
            directory.path(),
            vec![
                "/bin/sh".into(),
                "-c".into(),
                format!("echo executed >> '{}'", marker.display()),
            ],
        );
        let assignment = test_assignment(Vec::new());
        let mut state = AgentState::open(&config).unwrap();
        handle_assignment(&config, &mut state, assignment.clone())
            .await
            .unwrap();
        for _ in 0..100 {
            if check_process_and_lease(&config, &mut state).await.unwrap() {
                break;
            }
            time::sleep(Duration::from_millis(10)).await;
        }
        let terminal = state.journal.terminal().unwrap().unwrap();
        assert_eq!(terminal.reason_code, "WORKLOAD_EXITED");
        assert_eq!(terminal.assignment_id, assignment.assignment_id);
        assert_eq!(terminal.attempt_id, assignment.attempt_id);
        drop(state);
        let mut recovered = AgentState::open(&config).unwrap();
        assert_eq!(recovered.journal.terminal().unwrap().unwrap(), terminal);
        handle_assignment(&config, &mut recovered, assignment.clone())
            .await
            .unwrap();
        assert!(!recovered.child.is_running());
        assert_eq!(fs::read_to_string(marker).unwrap(), "executed\n");
        recovered.outbox.begin_session();
        assert!(recovered.outbox.unsent_frames("restarted", 1).iter().any(|f|
            matches!(&f.body, Some(node_to_control_plane::Body::RuntimeObservation(o)) if o == &terminal)));
        let mut changed = assignment;
        changed.attempt_id = "different-attempt".into();
        assert!(handle_assignment(&config, &mut recovered, changed)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn interrupted_spawn_intent_requires_reconciliation_and_cannot_claim_stopped() {
        let directory = tempdir().unwrap();
        let config = test_config(directory.path(), vec!["/bin/true".into()]);
        let assignment = test_assignment(Vec::new());
        let mut state = AgentState::open(&config).unwrap();
        state.journal.begin(&assignment).unwrap();
        drop(state);
        let mut recovered = AgentState::open(&config).unwrap();
        assert!(recovered.recovery_uncertain);
        assert_eq!(recovered.observed_state, RuntimeObservedState::Lost);
        assert!(handle_assignment(&config, &mut recovered, assignment)
            .await
            .is_err());
        assert!(!recovered.child.is_running());
        assert!(prepare_control_stop(
            &config,
            &mut recovered,
            core_v1::StopCommand {
                command_id: "stop-recovered".into(),
                runtime: Some(runtime_ref(&config)),
                ..Default::default()
            }
        )
        .is_err());
        stop_for_local_signal(&config, &mut recovered)
            .await
            .unwrap();
        assert!(recovered.journal.terminal().unwrap().is_none());
    }

    #[tokio::test]
    async fn journal_write_failure_prevents_workload_start() {
        let directory = tempdir().unwrap();
        let marker = directory.path().join("should-not-exist");
        let config = test_config(
            directory.path(),
            vec!["/usr/bin/touch".into(), marker.display().to_string()],
        );
        let mut state = AgentState::open(&config).unwrap();
        let path = config.state_dir.join(
            config
                .resume_token_state_name()
                .replace("runtime-resume-token-", "runtime-execution-"),
        );
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(matches!(
            handle_assignment(&config, &mut state, test_assignment(Vec::new())).await,
            Err(RuntimeAgentError::State(_))
        ));
        assert!(!state.child.is_running());
        assert!(!marker.exists());
    }

    #[test]
    fn journal_rejects_changed_launch_config_corruption_and_legacy_resume_only_state() {
        let directory = tempdir().unwrap();
        let config = test_config(directory.path(), vec!["/bin/true".into()]);
        drop(AgentState::open(&config).unwrap());
        let mut changed = config.clone();
        changed.workload = vec!["/bin/false".into()];
        assert!(AgentState::open(&changed).is_err());
        let path = config.state_dir.join(
            config
                .resume_token_state_name()
                .replace("runtime-resume-token-", "runtime-execution-"),
        );
        fs::write(&path, b"{broken").unwrap();
        assert!(AgentState::open(&config).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"{broken");
        fs::remove_file(&path).unwrap();
        let state_directory = config.prepare_state_directory().unwrap();
        config
            .persist_resume_token(&state_directory, "legacy-token")
            .unwrap();
        drop(state_directory);
        assert!(AgentState::open(&config).is_err());
        changed = config;
        changed.runtime.generation += 1;
        assert!(AgentState::open(&changed).is_ok());
    }

    #[test]
    fn journal_rejects_symlinks_and_shared_permissions_without_overwriting_evidence() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let directory = tempdir().unwrap();
        let config = test_config(directory.path(), vec!["/bin/true".into()]);
        drop(AgentState::open(&config).unwrap());
        let path = config.state_dir.join(
            config
                .resume_token_state_name()
                .replace("runtime-resume-token-", "runtime-execution-"),
        );
        let evidence = fs::read(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(AgentState::open(&config).is_err());
        assert_eq!(fs::read(&path).unwrap(), evidence);
        let target = directory.path().join("preserved-journal");
        fs::rename(&path, &target).unwrap();
        symlink(&target, &path).unwrap();
        assert!(AgentState::open(&config).is_err());
        assert_eq!(fs::read(&target).unwrap(), evidence);
    }

    #[tokio::test]
    async fn disconnected_handshake_wait_still_enforces_original_lease_expiry() {
        let directory = tempdir().unwrap();
        let config = test_config(directory.path(), vec!["/bin/sleep".into(), "30".into()]);
        let mut state = AgentState::open(&config).unwrap();
        let mut assignment = test_assignment(Vec::new());
        assignment.lease.as_mut().unwrap().expires_at =
            Some(timestamp_from_ms(now_unix_ms() + 250));
        handle_assignment(&config, &mut state, assignment)
            .await
            .unwrap();
        let mut terminate = termination_signal().unwrap();
        await_with_supervision(&config, &mut state, &mut terminate, async {
            time::sleep(Duration::from_millis(650)).await;
            Ok(())
        })
        .await
        .unwrap();
        assert!(!state.child.is_running());
        assert_eq!(
            state.journal.terminal().unwrap().unwrap().reason_code,
            "LEASE_EXPIRED"
        );
    }

    #[tokio::test]
    async fn child_start_failure_rejects_without_accepting_assignment() {
        let directory = tempdir().unwrap();
        let config = test_config(
            directory.path(),
            vec!["/definitely/not/a/cyrene-workload".to_string()],
        );
        let mut state = AgentState::open(&config).unwrap();
        let assignment = test_assignment(Vec::new());

        handle_assignment(&config, &mut state, assignment)
            .await
            .unwrap();

        assert!(state.assignment.is_none());
        assert!(!state.child.is_running());
        assert!(state.accepted_assignments.is_empty());
        assert_eq!(
            rejection_reason(&mut state),
            Some("WORKLOAD_START_FAILED".to_string())
        );
    }

    #[tokio::test]
    async fn artifact_staging_failure_rejects_without_starting_workload() {
        let directory = tempdir().unwrap();
        let config = test_config(directory.path(), vec!["/bin/true".to_string()]);
        let mut state = AgentState::open(&config).unwrap();
        let assignment = test_assignment(vec![authorized_transfer_spec()]);

        handle_assignment(&config, &mut state, assignment)
            .await
            .unwrap();

        assert!(state.assignment.is_none());
        assert!(!state.child.is_running());
        assert!(state.accepted_assignments.is_empty());
        assert_eq!(
            rejection_reason(&mut state),
            Some("ARTIFACT_STAGING_FAILED".to_string())
        );
    }

    #[tokio::test]
    async fn verified_local_artifact_is_accepted_without_transfer_credentials() {
        let directory = tempdir().unwrap();
        let config = test_config(directory.path(), vec!["/bin/true".to_string()]);
        fs::create_dir_all(&config.artifact_destination_root).unwrap();
        let content = b"local-cas";
        let (spec, digest_hex) = local_artifact_spec(content);
        fs::write(config.artifact_destination_root.join(digest_hex), content).unwrap();

        assert_eq!(
            stage_artifacts(&config, &test_assignment_with_local(vec![spec], Vec::new()))
                .await
                .unwrap(),
            (0, 0)
        );
    }

    #[tokio::test]
    async fn missing_local_artifact_rejects_without_starting_workload() {
        let directory = tempdir().unwrap();
        let config = test_config(directory.path(), vec!["/bin/true".to_string()]);
        let (spec, _) = local_artifact_spec(b"local-cas");
        let mut state = AgentState::open(&config).unwrap();

        handle_assignment(
            &config,
            &mut state,
            test_assignment_with_local(vec![spec], Vec::new()),
        )
        .await
        .unwrap();

        assert!(state.assignment.is_none());
        assert!(!state.child.is_running());
        assert!(state.accepted_assignments.is_empty());
        assert_eq!(
            rejection_reason(&mut state),
            Some("ARTIFACT_STAGING_FAILED".to_string())
        );
    }

    #[tokio::test]
    async fn local_artifact_digest_mismatch_rejects_without_starting_workload() {
        let directory = tempdir().unwrap();
        let config = test_config(directory.path(), vec!["/bin/true".to_string()]);
        fs::create_dir_all(&config.artifact_destination_root).unwrap();
        let (spec, digest_hex) = local_artifact_spec(b"local-cas");
        fs::write(
            config.artifact_destination_root.join(digest_hex),
            b"tampered",
        )
        .unwrap();
        let mut state = AgentState::open(&config).unwrap();

        handle_assignment(
            &config,
            &mut state,
            test_assignment_with_local(vec![spec], Vec::new()),
        )
        .await
        .unwrap();

        assert!(state.assignment.is_none());
        assert!(!state.child.is_running());
        assert!(state.accepted_assignments.is_empty());
        assert_eq!(
            rejection_reason(&mut state),
            Some("ARTIFACT_STAGING_FAILED".to_string())
        );
    }

    #[tokio::test]
    async fn local_artifact_size_mismatch_rejects_without_starting_workload() {
        let directory = tempdir().unwrap();
        let config = test_config(directory.path(), vec!["/bin/true".to_string()]);
        fs::create_dir_all(&config.artifact_destination_root).unwrap();
        let content = b"local-cas";
        let (mut spec, digest_hex) = local_artifact_spec(content);
        spec.size_bytes += 1;
        fs::write(config.artifact_destination_root.join(digest_hex), content).unwrap();
        let mut state = AgentState::open(&config).unwrap();

        handle_assignment(
            &config,
            &mut state,
            test_assignment_with_local(vec![spec], Vec::new()),
        )
        .await
        .unwrap();

        assert!(state.assignment.is_none());
        assert!(!state.child.is_running());
        assert!(state.accepted_assignments.is_empty());
        assert_eq!(
            rejection_reason(&mut state),
            Some("ARTIFACT_STAGING_FAILED".to_string())
        );
    }

    #[tokio::test]
    async fn mixed_artifacts_verify_local_inputs_before_transfer_credentials() {
        let directory = tempdir().unwrap();
        let config = test_config(directory.path(), vec!["/bin/true".to_string()]);
        let (local, _) = local_artifact_spec(b"local-cas");
        let assignment = test_assignment_with_local(vec![local], vec![authorized_transfer_spec()]);

        assert!(matches!(
            stage_artifacts(&config, &assignment).await,
            Err(RuntimeAgentError::Artifact(message))
                if message.contains("local Artifact is unavailable")
        ));
    }

    #[test]
    fn tampered_artifact_ticket_is_rejected_before_transfer() {
        let authority = DevelopmentTransferTicketAuthority::new([7_u8; 32]).unwrap();
        let artifact = ArtifactRef {
            uri: format!("artifact://sha256/{}", "a".repeat(64)),
            digest: format!("sha256:{}", "a".repeat(64)),
            size_bytes: 1,
            kind: ArtifactKind::generic(),
            manifest_digest: None,
        };
        let mut ticket = TransferTicket {
            ticket_id: "ticket-1".to_string(),
            artifact: artifact.clone(),
            source_peer_id: "seed-peer-1".to_string(),
            destination_peer_id: "runtime-cache-1".to_string(),
            allowed_parts: BTreeSet::from([0]),
            expires_at_unix_ms: u64::MAX,
            max_bytes: 1,
            signature: String::new(),
        };
        authority.sign(&mut ticket).unwrap();
        ticket.destination_peer_id = "attacker-cache".to_string();
        let mut spec = authorized_transfer_spec();
        spec.sources[0].transfer_ticket = serde_json::to_string(&ticket).unwrap();

        assert!(matches!(
            build_transfer_sources(&spec, &artifact, &authority),
            Err(RuntimeAgentError::Artifact(message)) if message.contains("signature is invalid")
        ));
    }

    fn test_config(root: &Path, workload: Vec<String>) -> RuntimeAgentConfig {
        RuntimeAgentConfig {
            control_plane_endpoint: "https://control.example".to_string(),
            control_plane_server_name: "control.example".to_string(),
            control_plane_ca: root.join("control-ca.pem"),
            client_certificate: root.join("client.pem"),
            client_key: root.join("client.key"),
            artifact_ca: root.join("missing-artifact-ca.pem"),
            artifact_ticket_key: None,
            organization_id: "organization-1".to_string(),
            workspace_id: "workspace-1".to_string(),
            node: core_v1::NodeRef {
                node_id: "node-1".to_string(),
                node_epoch: 1,
            },
            node_type: "container".to_string(),
            persistent: false,
            runtime: cy_kernel_contract::Identity {
                id: "runtime-1".to_string(),
                generation: 1,
            },
            agent_version: "test".to_string(),
            enrollment_proof: "one-shot-proof".to_string(),
            resume_token: String::new(),
            state_dir: root.join("state"),
            artifact_destination_root: root.join("artifacts"),
            reconnect_min: Duration::from_millis(10),
            reconnect_max: Duration::from_millis(100),
            workload,
        }
    }

    fn test_assignment(
        artifacts: Vec<core_v1::ArtifactTransferSpec>,
    ) -> core_v1::RuntimeAssignment {
        let now = now_unix_ms();
        let runtime = semantic_v1::Identity {
            id: "runtime-1".to_string(),
            generation: 1,
        };
        core_v1::RuntimeAssignment {
            assignment_id: "assignment-1".to_string(),
            runtime: Some(core_v1::RuntimeRef {
                identity: Some(runtime.clone()),
            }),
            operation: Some(semantic_v1::Identity {
                id: "operation-1".to_string(),
                generation: 1,
            }),
            attempt_id: "attempt-1".to_string(),
            lease: Some(semantic_v1::Lease {
                identity: Some(semantic_v1::Identity {
                    id: "lease-1".to_string(),
                    generation: 1,
                }),
                holder: Some(runtime.clone()),
                resources: vec![semantic_v1::Identity {
                    id: "resource-1".to_string(),
                    generation: 1,
                }],
                state: semantic_v1::LeaseState::Active as i32,
                fence_token: 1,
                expires_at: Some(timestamp_from_ms(now + 30_000)),
            }),
            workload_identity: Some(core_v1::WorkloadIdentity {
                identity: Some(semantic_v1::Identity {
                    id: "workload-1".to_string(),
                    generation: 1,
                }),
                scope: Some(core_v1::AccountScope {
                    user_id: "user-1".to_string(),
                    organization_id: "organization-1".to_string(),
                    workspace_id: "workspace-1".to_string(),
                }),
                runtime: Some(core_v1::RuntimeRef {
                    identity: Some(runtime),
                }),
                allowed_actions: vec!["operation.report".to_string()],
                expires_at: Some(timestamp_from_ms(now + 60_000)),
            }),
            profile: Some(core_v1::RuntimeProfile {
                image_digest: format!("sha256:{}", "1".repeat(64)),
                resolved_digest: format!("sha256:{}", "2".repeat(64)),
            }),
            desired_state: core_v1::DesiredRuntimeState::Running as i32,
            artifacts,
            local_artifacts: Vec::new(),
        }
    }

    fn test_assignment_with_local(
        local_artifacts: Vec<core_v1::ArtifactLocalInput>,
        artifacts: Vec<core_v1::ArtifactTransferSpec>,
    ) -> core_v1::RuntimeAssignment {
        core_v1::RuntimeAssignment {
            local_artifacts,
            ..test_assignment(artifacts)
        }
    }

    fn authorized_transfer_spec() -> core_v1::ArtifactTransferSpec {
        core_v1::ArtifactTransferSpec {
            artifact_uri: format!("artifact://sha256/{}", "a".repeat(64)),
            digest: format!("sha256:{}", "a".repeat(64)),
            size_bytes: 1,
            manifest_digest: String::new(),
            part_size_bytes: 1,
            part_digests: vec![format!("sha256:{}", "b".repeat(64))],
            sources: vec![core_v1::ArtifactTransferSource {
                peer_id: "seed-peer-1".to_string(),
                replica_id: "replica-1".to_string(),
                locator: "https://artifact.example/blob".to_string(),
                transfer_ticket: "not-read-before-ca".to_string(),
            }],
            part_sources: vec![core_v1::ArtifactPartSource {
                part_index: 0,
                peer_id: "seed-peer-1".to_string(),
                replica_id: "replica-1".to_string(),
            }],
            destination_peer_id: "runtime-cache-1".to_string(),
            artifact_kind: "generic".to_string(),
            ..Default::default()
        }
    }

    fn local_artifact_spec(content: &[u8]) -> (core_v1::ArtifactLocalInput, String) {
        let digest_hex = format!("{:x}", Sha256::digest(content));
        (
            core_v1::ArtifactLocalInput {
                artifact_uri: format!("artifact://sha256/{digest_hex}"),
                digest: format!("sha256:{digest_hex}"),
                size_bytes: content.len() as u64,
                artifact_kind: "generic".to_string(),
                manifest_digest: String::new(),
            },
            digest_hex,
        )
    }

    fn rejection_reason(state: &mut AgentState) -> Option<String> {
        state.outbox.begin_session();
        state
            .outbox
            .unsent_frames("session-1", 1)
            .into_iter()
            .find_map(|frame| match frame.body {
                Some(node_to_control_plane::Body::AssignmentAck(ack))
                    if AssignmentAckDisposition::try_from(ack.disposition).ok()
                        == Some(AssignmentAckDisposition::Rejected) =>
                {
                    ack.rejection.map(|rejection| rejection.reason_code)
                }
                _ => None,
            })
    }
}
