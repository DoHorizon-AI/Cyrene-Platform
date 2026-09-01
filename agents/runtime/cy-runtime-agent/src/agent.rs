//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 agent.rs                                                        │
//! │  Module: cy_runtime_agent::agent                                    │
//! │  Role: Reconnecting execution-control loop and Lease enforcement.   │
//! │                                                                     │
//! │  模块职责：维护出站控制连接、执行 Lease 围栏并协调 Artifact/子进程。      │
//! └─────────────────────────────────────────────────────────────────────┘

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cy_artifact_transfer::{
    ArtifactKind, ArtifactRef, ArtifactReplica, HttpRangeTransfer, TransferManifest, TransferPart,
    TransferProtocol, TransferSession,
};
use cy_execution_fabric::{
    validate_assignment, validate_renewal, AdmissionDisposition, FabricContractError,
    ObservationCursor,
};
use cy_kernel_contract::Lease as SemanticLease;
use cy_proto::core_v1::{
    self, control_plane_to_node, node_control_service_client::NodeControlServiceClient,
    node_to_control_plane, AssignmentAck, AssignmentAckDisposition, ControlPlaneToNode,
    ExecutionAgentHello, ExecutionAgentWelcome, LeaseRenewalRequest, NodeToControlPlane,
    RuntimeHeartbeat, RuntimeObservation, RuntimeObservedState, StopAck, TerminationClassification,
};
use cy_proto::semantic_v1;
use thiserror::Error;
use tokio::sync::mpsc;
use tokio::time;
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint, Identity};
use tonic::Request;

use crate::outbox::AgentOutbox;
use crate::{ChildSupervisor, RuntimeAgentConfig};

const PROTOCOL_VERSION: u32 = 2;

/// Runtime Agent configuration, transport, integrity, or supervision failure.
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
    #[error("Artifact transfer failed: {0}")]
    Artifact(String),
    #[error("child supervision failed: {0}")]
    Child(String),
    #[error("Runtime Agent observation outbox is full")]
    OutboxFull,
}

struct ActiveAssignment {
    assignment_id: String,
    lease: SemanticLease,
    lease_proto: semantic_v1::Lease,
    pending_renewal: Option<String>,
}

struct AgentState {
    outbox: AgentOutbox,
    child: ChildSupervisor,
    assignment: Option<ActiveAssignment>,
    observed_state: RuntimeObservedState,
    accepted_assignments: BTreeSet<String>,
    accepted_stops: BTreeSet<String>,
    renewal_counter: u64,
}

impl AgentState {
    fn new(workload: Vec<String>) -> Self {
        Self {
            outbox: AgentOutbox::default(),
            child: ChildSupervisor::new(workload),
            assignment: None,
            observed_state: RuntimeObservedState::Enrolled,
            accepted_assignments: BTreeSet::new(),
            accepted_stops: BTreeSet::new(),
            renewal_counter: 0,
        }
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
pub async fn run_runtime_agent(config: RuntimeAgentConfig) -> Result<(), RuntimeAgentError> {
    config.validate()?;
    fs::create_dir_all(&config.state_dir)
        .map_err(|error| RuntimeAgentError::Configuration(error.to_string()))?;
    fs::create_dir_all(&config.artifact_destination_root)
        .map_err(|error| RuntimeAgentError::Configuration(error.to_string()))?;
    let mut state = AgentState::new(config.workload.clone());
    let mut resume_token = config.resume_token.clone();
    let mut delay = config.reconnect_min;
    let mut terminate = termination_signal()?;

    loop {
        match connect_once(&config, &mut state, &resume_token, &mut terminate).await {
            Ok(ConnectionOutcome::Reconnect(token)) => {
                resume_token = token;
                delay = config.reconnect_min;
            }
            Ok(ConnectionOutcome::Completed) => return Ok(()),
            Err(error) => {
                if state.assignment.is_none()
                    && matches!(
                        error,
                        RuntimeAgentError::Configuration(_) | RuntimeAgentError::EmptyCredential(_)
                    )
                {
                    return Err(error);
                }
                tokio::select! {
                    _ = time::sleep(delay) => delay = backoff(delay, config.reconnect_max),
                    _ = terminate.recv() => {
                        stop_for_local_signal(&config, &mut state).await?;
                        return Ok(());
                    }
                }
            }
        }
    }
}

async fn connect_once(
    config: &RuntimeAgentConfig,
    state: &mut AgentState,
    resume_token: &str,
    terminate: &mut tokio::signal::unix::Signal,
) -> Result<ConnectionOutcome, RuntimeAgentError> {
    let channel = control_plane_channel(config).await?;
    let mut client = NodeControlServiceClient::new(channel);
    let (outbound, inbound) = mpsc::channel::<NodeToControlPlane>(64);
    outbound
        .send(hello_frame(config, resume_token))
        .await
        .map_err(|_| {
            RuntimeAgentError::Transport("control-plane request stream closed".to_string())
        })?;
    let mut inbound = client
        .connect(Request::new(ReceiverStream::new(inbound)))
        .await
        .map_err(transport_status)?
        .into_inner();
    let welcome_frame = inbound
        .message()
        .await
        .map_err(transport_status)?
        .ok_or_else(|| {
            RuntimeAgentError::Transport("control plane closed before welcome".to_string())
        })?;
    let welcome = accept_welcome(&welcome_frame)?;
    let session_id = welcome.session_id.clone();
    let mut control_cursor = ObservationCursor::default();
    control_cursor.admit(&welcome_frame.frame_id, welcome_frame.sequence_number)?;
    state
        .outbox
        .acknowledge(welcome.acknowledged_agent_sequence);
    state.outbox.begin_session();
    enqueue_observation(
        config,
        state,
        "SESSION_ESTABLISHED",
        "execution session established",
    )?;
    flush_outbox(
        &outbound,
        state,
        &session_id,
        control_cursor.last_sequence(),
    )
    .await?;

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
                if check_process_and_lease(config, state).await? {
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
                persistence_class: core_v1::PersistenceClass::Ephemeral as i32,
                agent_version: config.agent_version.clone(),
                min_protocol_version: PROTOCOL_VERSION,
                max_protocol_version: PROTOCOL_VERSION,
                resume_token: resume_token.to_string(),
                capabilities: vec![
                    capability("cyrene.execution.child-process"),
                    capability("cyrene.artifact.https-range"),
                ],
                enrollment_proof,
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
    let lease = match validate_assignment(&config.runtime, &assignment, now) {
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

    enqueue_assignment_ack(
        config,
        state,
        &assignment,
        AssignmentAckDisposition::Accepted,
        None,
    )?;
    state
        .accepted_assignments
        .insert(assignment.assignment_id.clone());
    state.observed_state = RuntimeObservedState::Staging;
    enqueue_observation(
        config,
        state,
        "ARTIFACT_STAGING",
        "staging immutable Artifact inputs",
    )?;
    let (downloaded_parts, reused_parts) = stage_artifacts(config, &assignment).await?;
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
    state
        .child
        .start(None, &Default::default())
        .await
        .map_err(|error| RuntimeAgentError::Child(error.to_string()))?;
    state.assignment = Some(ActiveAssignment {
        assignment_id: assignment.assignment_id,
        lease,
        lease_proto: assignment.lease.expect("validated Assignment has Lease"),
        pending_renewal: None,
    });
    state.observed_state = RuntimeObservedState::Running;
    enqueue_observation(
        config,
        state,
        "WORKLOAD_RUNNING",
        "fixed workload command is running",
    )?;
    Ok(())
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
            kind: ArtifactKind::Generic,
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
        let session = TransferSession {
            session_id: format!("{}-{digest_hex}", assignment.assignment_id),
            manifest,
            replica: ArtifactReplica {
                replica_id: format!("{}-primary", spec.artifact_uri),
                artifact,
                protocol: TransferProtocol::HttpsRangeV1,
                locator: spec.replica_uri.clone(),
                region: None,
                priority: 0,
                expires_at_unix_ms: None,
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
    if state.accepted_stops.insert(command.command_id.clone()) {
        state.outbox.enqueue(
            &config.runtime.id,
            node_to_control_plane::Body::StopAck(StopAck {
                command_id: command.command_id,
                runtime: Some(runtime_ref(config)),
                accepted_at: Some(now_timestamp()),
            }),
        )?;
    }
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
    state.outbox.enqueue(
        &config.runtime.id,
        node_to_control_plane::Body::RuntimeObservation(RuntimeObservation {
            runtime: Some(runtime_ref(config)),
            observed_state: state.observed_state as i32,
            termination: termination as i32,
            reason_code: reason.to_string(),
            summary: summary.to_string(),
            observed_at: Some(now_timestamp()),
            worker: None,
            operation: None,
        }),
    )
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
            "client key {} must not be readable by group or other users",
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

fn capability(id: &str) -> semantic_v1::Capability {
    semantic_v1::Capability {
        id: id.to_string(),
        revision: 1,
        properties: Default::default(),
    }
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
