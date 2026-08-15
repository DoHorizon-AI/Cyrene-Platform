//! 插件实例 Actor 与沙箱运行时健康前置检查辅助函数

use std::sync::Arc;
use std::time::Duration;

use cy_kernel_daemon::watchdog::{
    InstanceActor, InstanceActorState, WorkerTransportCommand, WorkerTransportResponse,
};
use cy_platform_api::PluginError;
use cy_plugin_protocol::{
    envelope::Payload,
    pb::{Cancel, Envelope, Invoke, InvokeResult},
    CURRENT_PROTOCOL_VERSION,
};
use prost::Message;
use tokio::sync::{mpsc, Mutex as AsyncMutex};

use crate::transport::{
    connect, protocol_version_is_current, read_frame, write_frame, WorkerTransportError,
};

/// 在发起 RPC 调用前确保实例 Actor 处于就绪状态
pub async fn prepare_instance_actor<'a>(
    plugin_id: &str,
    actor_arc: &'a Arc<AsyncMutex<InstanceActor>>,
) -> Result<tokio::sync::MutexGuard<'a, InstanceActor>, PluginError> {
    let mut actor = actor_arc.lock().await;
    if actor.state() == InstanceActorState::Quarantined {
        return Err(PluginError::Unavailable(format!(
            "Instance {} is quarantined; execution blocked",
            plugin_id
        )));
    }
    if actor.state() == InstanceActorState::Stopped || actor.state() == InstanceActorState::Starting
    {
        if let Err(e) = actor.start() {
            return Err(PluginError::Unavailable(format!(
                "Failed to start instance actor for {}: {}",
                plugin_id, e
            )));
        }
    }
    if actor.state() != InstanceActorState::Healthy && actor.state() != InstanceActorState::Degraded
    {
        return Err(PluginError::Unavailable(format!(
            "Instance {} is not healthy (state={:?})",
            plugin_id,
            actor.state()
        )));
    }
    let needs_transport = !actor.transport_attached();
    drop(actor);
    if needs_transport {
        attach_transport(actor_arc.clone()).await?;
    }
    let actor = actor_arc.lock().await;
    if actor.state() != InstanceActorState::Healthy && actor.state() != InstanceActorState::Degraded
    {
        return Err(PluginError::Unavailable(format!(
            "Instance {} is not healthy after transport attach (state={:?})",
            plugin_id,
            actor.state()
        )));
    }
    Ok(actor)
}

pub async fn invoke_actor(
    actor: &mut InstanceActor,
    invoke: Invoke,
    timeout: Duration,
) -> Result<InvokeResult, PluginError> {
    let response = actor
        .invoke_raw(invoke.encode_to_vec(), timeout)
        .await
        .map_err(|error| PluginError::Execution(error.to_string()))?;
    let envelope = Envelope::decode(response.as_slice())
        .map_err(|error| PluginError::Execution(error.to_string()))?;
    match envelope.payload {
        Some(Payload::InvokeResult(result)) => Ok(result),
        Some(Payload::Error(error)) => Err(PluginError::Execution(error.message)),
        _ => Err(PluginError::Execution(
            "Invalid worker response payload".to_string(),
        )),
    }
}

async fn attach_transport(actor_arc: Arc<AsyncMutex<InstanceActor>>) -> Result<(), PluginError> {
    let (socket, expected_plugin_id, expected_generation, expected_fence_token) = {
        let actor = actor_arc.lock().await;
        if actor.transport_attached() {
            return Ok(());
        }
        let socket = actor.transport_socket().ok_or_else(|| {
            PluginError::Unavailable(
                "sandboxd started the worker without a byte-stream transport endpoint".to_string(),
            )
        })?;
        (
            socket,
            actor.instance_id().to_string(),
            actor.generation(),
            actor.fence_token(),
        )
    };
    let dispatcher = {
        let actor = actor_arc.lock().await;
        actor.transport_dispatcher()
    };
    let (mut reader, mut writer) = connect(&socket, Duration::from_secs(2))
        .await
        .map_err(|error| PluginError::Unavailable(error.to_string()))?;
    let (tx, mut outbound) = mpsc::channel::<WorkerTransportCommand>(32);
    {
        let mut actor = actor_arc.lock().await;
        if actor.transport_attached() {
            return Ok(());
        }
        actor.attach_transport_channel(tx);
    }

    let writer_plugin_id = expected_plugin_id.clone();
    let writer_actor = actor_arc.clone();
    tokio::spawn(async move {
        let mut sequence_number = 0_u64;
        while let Some(command) = outbound.recv().await {
            sequence_number = sequence_number.wrapping_add(1);
            let envelope = match command {
                WorkerTransportCommand::Request(request) => {
                    let Ok(invoke) = Invoke::decode(request.payload.as_slice()) else {
                        let mut actor = writer_actor.lock().await;
                        actor
                            .mark_transport_lost(expected_generation, expected_fence_token)
                            .await;
                        break;
                    };
                    Envelope {
                        request_id: request.request_id,
                        trace_id: String::new(),
                        plugin_id: writer_plugin_id.clone(),
                        protocol_version: CURRENT_PROTOCOL_VERSION,
                        deadline_ms: 0,
                        sequence_number,
                        generation: request.generation,
                        fence_token: request.fence_token,
                        payload: Some(Payload::Invoke(invoke)),
                    }
                }
                WorkerTransportCommand::Cancel {
                    request_id,
                    generation,
                    fence_token,
                } => Envelope {
                    request_id: format!("cancel-{request_id}"),
                    trace_id: String::new(),
                    plugin_id: writer_plugin_id.clone(),
                    protocol_version: CURRENT_PROTOCOL_VERSION,
                    deadline_ms: 0,
                    sequence_number,
                    generation,
                    fence_token,
                    payload: Some(Payload::Cancel(Cancel {
                        target_request_id: request_id,
                        reason: "invoke deadline exceeded".to_string(),
                    })),
                },
            };
            if write_frame(&mut writer, &envelope).await.is_err() {
                let mut actor = writer_actor.lock().await;
                actor
                    .mark_transport_lost(expected_generation, expected_fence_token)
                    .await;
                break;
            }
        }
    });

    let reader_plugin_id = expected_plugin_id;
    let reader_actor = actor_arc;
    tokio::spawn(async move {
        let mut buffer = bytes::BytesMut::with_capacity(8192);
        let mut last_sequence = 0_u64;
        let _result: Result<(), WorkerTransportError> = async {
            while let Some(envelope) = read_frame(&mut reader, &mut buffer).await? {
                if envelope.request_id.is_empty()
                    || envelope.plugin_id != reader_plugin_id
                    || !protocol_version_is_current(&envelope)
                    || envelope.generation != expected_generation
                    || envelope.fence_token != expected_fence_token
                    || envelope.sequence_number <= last_sequence
                {
                    return Err(WorkerTransportError::Protocol(
                        cy_plugin_protocol::ProtocolError::FramingError(
                            "worker response identity or sequence is invalid".to_string(),
                        ),
                    ));
                }
                last_sequence = envelope.sequence_number;
                let _ = dispatcher
                    .dispatch(WorkerTransportResponse {
                        request_id: envelope.request_id.clone(),
                        generation: envelope.generation,
                        fence_token: envelope.fence_token,
                        payload: envelope.encode_to_vec(),
                    })
                    .await;
            }
            Ok(())
        }
        .await;

        let mut actor = reader_actor.lock().await;
        actor
            .mark_transport_lost(expected_generation, expected_fence_token)
            .await;
    });
    Ok(())
}
