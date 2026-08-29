//! 插件实例 Actor 与沙箱运行时健康前置检查辅助函数

use std::sync::Arc;
use std::time::Duration;

use cy_kernel_daemon::watchdog::{InstanceActor, InstanceActorState};
#[cfg(unix)]
use cy_kernel_daemon::watchdog::{WorkerTransportCommand, WorkerTransportResponse};
use cy_platform_api::PluginError;
use cy_plugin_protocol::pb::{Invoke, InvokeResult};
#[cfg(unix)]
use cy_plugin_protocol::{
    envelope::Payload,
    pb::{Cancel, Envelope, Hello, HelloAck},
    CURRENT_PROTOCOL_VERSION,
};
#[cfg(unix)]
use prost::Message;
#[cfg(unix)]
use tokio::sync::mpsc;
use tokio::sync::Mutex as AsyncMutex;

#[cfg(unix)]
use crate::transport::{
    connect, protocol_version_is_current, read_frame, write_frame, WorkerTransportError,
};
use crate::worker_control::WorkerControlClient;

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
    let plugin_id = actor.instance_id().to_string();
    WorkerControlClient::new(actor, plugin_id)
        .invoke_message(invoke, timeout)
        .await
        .map_err(|error| PluginError::Execution(error.to_string()))
}

#[cfg(unix)]
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

    let hello_request_id = format!("hello-{}", uuid::Uuid::new_v4());
    let hello = Envelope {
        request_id: hello_request_id.clone(),
        trace_id: String::new(),
        plugin_id: expected_plugin_id.clone(),
        protocol_version: CURRENT_PROTOCOL_VERSION,
        deadline_ms: 0,
        sequence_number: 1,
        generation: expected_generation,
        fence_token: expected_fence_token,
        payload: Some(Payload::Hello(Hello {
            min_protocol_version: CURRENT_PROTOCOL_VERSION,
            max_protocol_version: CURRENT_PROTOCOL_VERSION,
            host_version: env!("CARGO_PKG_VERSION").to_string(),
        })),
    };
    write_frame(&mut writer, &hello)
        .await
        .map_err(|error| PluginError::Unavailable(error.to_string()))?;
    let mut buffer = bytes::BytesMut::with_capacity(8192);
    let hello_ack =
        tokio::time::timeout(Duration::from_secs(2), read_frame(&mut reader, &mut buffer))
            .await
            .map_err(|_| PluginError::Unavailable("worker HelloAck timed out".to_string()))?
            .map_err(|error| PluginError::Unavailable(error.to_string()))?
            .ok_or_else(|| PluginError::Unavailable("worker closed before HelloAck".to_string()))?;
    validate_hello_ack(
        &hello_ack,
        &hello_request_id,
        &expected_plugin_id,
        expected_generation,
        expected_fence_token,
    )?;
    let hello_sequence = hello_ack.sequence_number;

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
        let mut sequence_number = hello_sequence;
        while let Some(command) = outbound.recv().await {
            sequence_number = sequence_number.wrapping_add(1);
            let envelope = match command {
                WorkerTransportCommand::Request(request) => {
                    let Ok(mut envelope) = Envelope::decode(request.payload.as_slice()) else {
                        let mut actor = writer_actor.lock().await;
                        actor
                            .mark_transport_lost(expected_generation, expected_fence_token)
                            .await;
                        break;
                    };
                    if envelope.request_id != request.request_id
                        || envelope.plugin_id != writer_plugin_id
                        || envelope.protocol_version != CURRENT_PROTOCOL_VERSION
                        || envelope.generation != request.generation
                        || envelope.fence_token != request.fence_token
                        || envelope.payload.is_none()
                    {
                        let mut actor = writer_actor.lock().await;
                        actor
                            .mark_transport_lost(expected_generation, expected_fence_token)
                            .await;
                        break;
                    }
                    envelope.sequence_number = sequence_number;
                    envelope
                }
                WorkerTransportCommand::Cancel {
                    cancel_request_id,
                    target_request_id,
                    generation,
                    fence_token,
                } => Envelope {
                    request_id: cancel_request_id,
                    trace_id: String::new(),
                    plugin_id: writer_plugin_id.clone(),
                    protocol_version: CURRENT_PROTOCOL_VERSION,
                    deadline_ms: 0,
                    sequence_number,
                    generation,
                    fence_token,
                    payload: Some(Payload::Cancel(Cancel {
                        target_request_id,
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
        let mut last_sequence = hello_sequence;
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
                if let Some(Payload::CancelAck(ack)) = envelope.payload.as_ref() {
                    if dispatcher.dispatch_cancel_ack(&envelope.request_id, &ack.target_request_id)
                    {
                        continue;
                    }
                }
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

#[cfg(not(unix))]
async fn attach_transport(_actor_arc: Arc<AsyncMutex<InstanceActor>>) -> Result<(), PluginError> {
    Err(PluginError::Unavailable(
        "live worker UDS transport is not supported on this platform".to_string(),
    ))
}

#[cfg(unix)]
fn validate_hello_ack(
    envelope: &Envelope,
    request_id: &str,
    expected_plugin_id: &str,
    expected_generation: u64,
    expected_fence_token: u64,
) -> Result<(), PluginError> {
    if envelope.request_id != request_id
        || envelope.plugin_id != expected_plugin_id
        || !protocol_version_is_current(envelope)
        || envelope.generation != expected_generation
        || envelope.fence_token != expected_fence_token
        || envelope.sequence_number == 0
    {
        return Err(PluginError::Unavailable(
            "worker HelloAck identity, protocol, fence, or sequence is invalid".to_string(),
        ));
    }
    match envelope.payload.as_ref() {
        Some(Payload::HelloAck(HelloAck {
            selected_protocol_version,
            plugin_id,
            ..
        })) if *selected_protocol_version == CURRENT_PROTOCOL_VERSION
            && plugin_id == expected_plugin_id =>
        {
            Ok(())
        }
        _ => Err(PluginError::Unavailable(
            "worker did not return a compatible HelloAck".to_string(),
        )),
    }
}
