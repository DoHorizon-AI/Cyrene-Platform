// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: kernel/crates/cy-kernel-daemon/src/convert/worker.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! Worker and launch/process-control projections between Kernel and Core Proto.
//!
//! Kernel 与 Core Proto 的 Worker、launch/process-control projection。
use std::collections::BTreeMap;

use cy_kernel_api::{semantic, CgroupLimits, ProviderError};
use cy_proto::{core_v1, semantic_v1};
use tonic::Status;

use super::common::{
    now_timestamp, semantic_identity_from_proto, semantic_status, to_semantic_proto_identity,
};
use crate::{
    daemon::KernelDaemon,
    session::{ManagedProcess, WorkerHeartbeatConfig},
    watchdog::InstanceActorState,
};

/// Map supported generic Worker ceilings into enforced cgroup limits.
/// Existing Lease ceilings can only be tightened, never discarded or relaxed.
/// 将 Worker 执行上限映射到 cgroup; 既有 Lease 上限只能收紧。
pub(crate) fn enforced_worker_limits(
    lease: &CgroupLimits,
    requested: &BTreeMap<String, semantic::Quantity>,
) -> Result<CgroupLimits, semantic::Rejection> {
    let mut limits = lease.clone();
    for (name, quantity) in requested {
        let reject = || semantic::Rejection {
            reason_code: "WORKER_LIMIT_UNSUPPORTED".to_string(),
            message: format!("unsupported Worker limit or unit: {name}"),
        };
        if quantity.value == 0 {
            return Err(reject());
        }
        match (name.as_str(), quantity.unit.as_str()) {
            ("memory.bytes", "byte") => {
                limits.memory_max_bytes = Some(
                    limits
                        .memory_max_bytes
                        .map_or(quantity.value, |value| value.min(quantity.value)),
                );
            }
            ("cpu.time", "millicore") => {
                let value = u32::try_from(quantity.value).map_err(|_| reject())?;
                limits.cpu_max_millicores = Some(
                    limits
                        .cpu_max_millicores
                        .map_or(value, |current| current.min(value)),
                );
            }
            _ => return Err(reject()),
        }
    }
    Ok(limits)
}

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
        last_heartbeat_at: process.last_heartbeat_at,
    }
}

pub(crate) fn managed_runtime_state(process: &ManagedProcess) -> i32 {
    // `InstanceActor` is the single source of truth for kernel-side lifecycle.
    // `Healthy` surfaces the worker-reported `runtime_state` (preserving prior
    // behavior); `Degraded` reuses the last worker-reported value because
    // `PluginRuntimeState` has no degraded variant.
    // 中文：`InstanceActor` 是 Kernel 侧生命周期状态的唯一真实来源。`Healthy` 会暴露 Worker 报告的 `runtime_state`（保持原有行为）；`Degraded` 会沿用 Worker 最近一次报告的值，因为 `PluginRuntimeState` 没有 degraded 变体。
    match process.actor.state() {
        InstanceActorState::Starting => core_v1::PluginRuntimeState::Starting as i32,
        InstanceActorState::Healthy => process.runtime_state,
        InstanceActorState::Degraded => process.runtime_state,
        InstanceActorState::Draining => core_v1::PluginRuntimeState::Stopping as i32,
        InstanceActorState::Stopping => core_v1::PluginRuntimeState::Stopping as i32,
        InstanceActorState::Stopped => core_v1::PluginRuntimeState::Stopped as i32,
        InstanceActorState::Quarantined => core_v1::PluginRuntimeState::Quarantined as i32,
    }
}

pub(crate) fn semantic_worker_from_proto(
    worker: semantic_v1::Worker,
    // The authority `Principal` is supplied by the trusted transport
    // (SO_PEERCRED), never taken from the caller body. The request may still
    // carry a `principal` field, but it is ignored: a client must not be able
    // to assert which authority Principal it acts as.
    // 中文：权限 `Principal` 由可信传输层（SO_PEERCRED）提供，绝不能从调用方请求正文中读取。请求仍可能带有 `principal` 字段，但会被忽略：客户端不得自行声明它要以哪个权限主体身份操作。
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
        // 中文：连接的 Principal 会覆盖调用方声明的值；不会读取请求正文中的 `principal` 字段。
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

#[cfg(test)]
mod limit_tests {
    use super::*;

    #[test]
    fn worker_ceilings_never_relax_existing_lease_or_cpuset() {
        let lease = CgroupLimits {
            memory_max_bytes: Some(1024),
            cpu_max_millicores: Some(2000),
            cpuset_cpus: Some("0-1".to_string()),
        };
        let requested = BTreeMap::from([
            (
                "memory.bytes".to_string(),
                semantic::Quantity {
                    value: 4096,
                    unit: "byte".to_string(),
                },
            ),
            (
                "cpu.time".to_string(),
                semantic::Quantity {
                    value: 500,
                    unit: "millicore".to_string(),
                },
            ),
        ]);
        let actual = enforced_worker_limits(&lease, &requested).unwrap();
        assert_eq!(actual.memory_max_bytes, Some(1024));
        assert_eq!(actual.cpu_max_millicores, Some(500));
        assert_eq!(actual.cpuset_cpus.as_deref(), Some("0-1"));
        let no_lease_ceiling =
            enforced_worker_limits(&CgroupLimits::default(), &requested).unwrap();
        assert_eq!(no_lease_ceiling.memory_max_bytes, Some(4096));
        assert_eq!(no_lease_ceiling.cpu_max_millicores, Some(500));
    }

    #[test]
    fn unknown_units_limits_zero_and_overflow_are_rejected_before_launch() {
        for (key, value, unit) in [
            ("memory.bytes", 10, "gibibyte"),
            ("memory.bytes", 0, "byte"),
            ("cpu.time", u64::from(u32::MAX) + 1, "millicore"),
            ("unknown.ceiling", 10, "byte"),
        ] {
            let requested = BTreeMap::from([(
                key.to_string(),
                semantic::Quantity {
                    value,
                    unit: unit.to_string(),
                },
            )]);
            assert_eq!(
                enforced_worker_limits(&CgroupLimits::default(), &requested)
                    .unwrap_err()
                    .reason_code,
                "WORKER_LIMIT_UNSUPPORTED"
            );
        }
    }
}
