use std::collections::BTreeMap;

use cy_kernel_api::{semantic, ProviderError};
use cy_proto::{core_v1, semantic_v1};
use tonic::Status;

use super::common::{
    now_timestamp, semantic_identity_from_proto, semantic_status, to_semantic_proto_identity,
};
use crate::{
    daemon::KernelDaemon,
    sandboxed_process::SandboxedProcessState,
    session::{ManagedProcess, WorkerHeartbeatConfig},
};

pub(crate) fn to_plugin_instance(
    daemon: &KernelDaemon,
    name: &str,
    process: &ManagedProcess,
    adapter_available: bool,
) -> core_v1::PluginInstance {
    core_v1::PluginInstance {
        name: name.to_string(),
        plugin: Some(process.plugin.clone()),
        node: Some(core_v1::NodeRef {
            node_id: daemon.node_id.clone(),
            node_epoch: daemon.node_epoch,
        }),
        generation: process.generation,
        observed_generation: process.generation,
        desired_state: if process.watchdog_triggered {
            core_v1::DesiredPluginState::Stopped as i32
        } else {
            core_v1::DesiredPluginState::Running as i32
        },
        runtime_state: managed_runtime_state(process),
        health: if adapter_available {
            process.health.clone()
        } else {
            Some(core_v1::HealthReport {
                status: core_v1::HealthStatus::Degraded as i32,
                reason_code: "ADAPTER_DEGRADED".to_string(),
                summary: "external hardware facts are unavailable or expired".to_string(),
            })
        },
        lease: process.lease.clone(),
        restart_count: process.restart_count,
        created_at: None,
        updated_at: Some(now_timestamp()),
        last_heartbeat_at: process.last_heartbeat_at.clone(),
    }
}

pub(crate) fn managed_runtime_state(process: &ManagedProcess) -> i32 {
    match process.instance.state() {
        SandboxedProcessState::Discovered => core_v1::PluginRuntimeState::Discovered as i32,
        SandboxedProcessState::Starting => core_v1::PluginRuntimeState::Starting as i32,
        SandboxedProcessState::Healthy => process.runtime_state,
        SandboxedProcessState::Stopping => core_v1::PluginRuntimeState::Stopping as i32,
        SandboxedProcessState::Stopped => core_v1::PluginRuntimeState::Stopped as i32,
        SandboxedProcessState::Quarantined => core_v1::PluginRuntimeState::Quarantined as i32,
    }
}

pub(crate) fn semantic_worker_from_proto(
    worker: semantic_v1::Worker,
    // The authority `Principal` is supplied by the trusted transport
    // (SO_PEERCRED), never taken from the caller body. The request may still
    // carry a `principal` field, but it is ignored: a client must not be able
    // to assert which authority Principal it acts as.
    principal: semantic::Identity,
) -> Result<semantic::Worker, Status> {
    let state = semantic_v1::WorkerState::try_from(worker.state).map_err(|_| {
        semantic_status(
            tonic::Code::InvalidArgument,
            "UNKNOWN_ENUM_VALUE",
            "worker state is unknown",
        )
    })?;
    let state = match state {
        semantic_v1::WorkerState::Registered => semantic::WorkerState::Registered,
        semantic_v1::WorkerState::Starting => semantic::WorkerState::Starting,
        semantic_v1::WorkerState::Running => semantic::WorkerState::Running,
        semantic_v1::WorkerState::Draining => semantic::WorkerState::Draining,
        semantic_v1::WorkerState::Stopped => semantic::WorkerState::Stopped,
        semantic_v1::WorkerState::Failed => semantic::WorkerState::Failed,
        semantic_v1::WorkerState::Lost => semantic::WorkerState::Lost,
        semantic_v1::WorkerState::Unspecified => {
            return Err(semantic_status(
                tonic::Code::InvalidArgument,
                "UNKNOWN_ENUM_VALUE",
                "worker state cannot be UNSPECIFIED",
            ));
        }
    };
    let worker = semantic::Worker {
        identity: semantic_identity_from_proto(worker.identity, "worker identity")?,
        // The connection Principal overrides any caller-asserted value. The
        // request body's `principal` field is deliberately not consulted.
        principal,
        provider: semantic_identity_from_proto(worker.provider, "worker provider")?,
        lease: semantic_identity_from_proto(worker.lease, "worker lease")?,
        state,
        execution_ref: worker.execution_ref,
        limits: worker
            .limits
            .into_iter()
            .map(|(key, quantity)| {
                (
                    key,
                    semantic::Quantity {
                        value: quantity.value,
                        unit: quantity.unit,
                    },
                )
            })
            .collect(),
    };
    worker.validate().map_err(|error| {
        semantic_status(
            tonic::Code::InvalidArgument,
            error.reason_code,
            &error.message,
        )
    })?;
    Ok(worker)
}

pub(crate) fn to_semantic_proto_worker(worker: &semantic::Worker) -> semantic_v1::Worker {
    semantic_v1::Worker {
        identity: Some(to_semantic_proto_identity(&worker.identity)),
        principal: Some(to_semantic_proto_identity(&worker.principal)),
        provider: Some(to_semantic_proto_identity(&worker.provider)),
        lease: Some(to_semantic_proto_identity(&worker.lease)),
        state: match worker.state {
            semantic::WorkerState::Registered => semantic_v1::WorkerState::Registered,
            semantic::WorkerState::Starting => semantic_v1::WorkerState::Starting,
            semantic::WorkerState::Running => semantic_v1::WorkerState::Running,
            semantic::WorkerState::Draining => semantic_v1::WorkerState::Draining,
            semantic::WorkerState::Stopped => semantic_v1::WorkerState::Stopped,
            semantic::WorkerState::Failed => semantic_v1::WorkerState::Failed,
            semantic::WorkerState::Lost => semantic_v1::WorkerState::Lost,
        } as i32,
        execution_ref: worker.execution_ref.clone(),
        limits: worker
            .limits
            .iter()
            .map(|(key, quantity)| {
                (
                    key.clone(),
                    semantic_v1::Quantity {
                        value: quantity.value,
                        unit: quantity.unit.clone(),
                    },
                )
            })
            .collect(),
    }
}

pub(crate) fn inject_heartbeat_environment(
    mut environment: BTreeMap<String, String>,
    heartbeat: &WorkerHeartbeatConfig,
    instance_name: &str,
    generation: u64,
) -> Result<BTreeMap<String, String>, ProviderError> {
    let injected = [
        (
            "CYRENE_HEARTBEAT_SOCKET",
            heartbeat.socket_path.to_string_lossy().into_owned(),
        ),
        (
            "CYRENE_WORKER_CONTROL_SOCKET",
            heartbeat.socket_path.to_string_lossy().into_owned(),
        ),
        ("CYRENE_PLUGIN_INSTANCE_NAME", instance_name.to_string()),
        ("CYRENE_PLUGIN_INSTANCE_GENERATION", generation.to_string()),
        (
            "CYRENE_HEARTBEAT_INTERVAL_MS",
            heartbeat.interval.as_millis().to_string(),
        ),
    ];
    if injected
        .iter()
        .any(|(key, _)| environment.contains_key(*key))
    {
        return Err(ProviderError::new(
            "kernel-daemon",
            "RESERVED_HEARTBEAT_ENVIRONMENT",
            "installation record attempted to override Kernel heartbeat configuration",
        ));
    }
    environment.extend(injected.map(|(key, value)| (key.to_string(), value)));
    Ok(environment)
}
