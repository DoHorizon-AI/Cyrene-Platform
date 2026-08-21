//! Versioned request mapping for the privileged Sandbox Adapter Host.
//!
//! This module is intentionally outside `kernel/`: its backend may use Linux
//! cgroup, pidfd, BPF, process, or container-runtime mechanisms. The mapping
//! itself remains deterministic and rejects incomplete requests before a
//! backend receives them.

use std::{collections::BTreeMap, path::PathBuf, time::Duration};

use cy_kernel_api::{
    CgroupLimits, CgroupTelemetry, CleanupReport, DeviceBinding, DeviceNode, EnforcementMode,
    LaunchPlan, NodeCapabilities, ProcessHandle, ProviderError, RuntimeProcessEvidence,
    SandboxBackend, StopRequest,
};
use cy_proto::sandbox_v1;

const PROTOCOL_VERSION: u32 = 1;

pub fn handle_request(
    runtime: &dyn SandboxBackend,
    adapter_id: &str,
    request: sandbox_v1::SandboxRequest,
) -> sandbox_v1::SandboxResponse {
    if request.protocol_version != PROTOCOL_VERSION {
        return error_response(
            adapter_id,
            "SANDBOX_PROTOCOL_VERSION",
            "sandbox request uses an incompatible protocol version",
        );
    }
    let result = match request.body {
        Some(sandbox_v1::sandbox_request::Body::Preflight(_)) => {
            Ok(sandbox_v1::sandbox_response::Body::Capabilities(
                capabilities_to_proto(runtime.preflight()),
            ))
        }
        Some(sandbox_v1::sandbox_request::Body::Launch(request)) => {
            launch_request(runtime, request)
                .map(|handle| sandbox_v1::sandbox_response::Body::Process(handle_to_proto(handle)))
        }
        Some(sandbox_v1::sandbox_request::Body::Stop(request)) => stop_request(runtime, request)
            .map(|report| sandbox_v1::sandbox_response::Body::Cleanup(cleanup_to_proto(report))),
        Some(sandbox_v1::sandbox_request::Body::Telemetry(request)) => {
            telemetry_request(runtime, request).map(|telemetry| {
                sandbox_v1::sandbox_response::Body::Telemetry(telemetry_to_proto(telemetry))
            })
        }
        Some(sandbox_v1::sandbox_request::Body::DiscoverRecovery(_)) => {
            runtime.discover_recovery_processes().map(|processes| {
                sandbox_v1::sandbox_response::Body::RecoveryProcesses(
                    sandbox_v1::SandboxRecoveryProcesses {
                        processes: processes
                            .into_iter()
                            .map(recovery_evidence_to_proto)
                            .collect(),
                    },
                )
            })
        }
        Some(sandbox_v1::sandbox_request::Body::RecoverStale(request)) => {
            recover_stale_request(runtime, request)
                .map(|report| sandbox_v1::sandbox_response::Body::Cleanup(cleanup_to_proto(report)))
        }
        None => Err(ProviderError::new(
            adapter_id,
            "SANDBOX_PROTOCOL_REQUEST",
            "sandbox request body is required",
        )),
    };
    match result {
        Ok(body) => sandbox_v1::SandboxResponse {
            protocol_version: PROTOCOL_VERSION,
            adapter_id: adapter_id.to_string(),
            body: Some(body),
        },
        Err(error) => error_response(adapter_id, &error.reason_code, &error.message),
    }
}

fn launch_request(
    runtime: &dyn SandboxBackend,
    request: sandbox_v1::SandboxLaunchRequest,
) -> Result<ProcessHandle, ProviderError> {
    let limits = request.limits.ok_or_else(|| {
        ProviderError::new(
            "sandboxd",
            "SANDBOX_LIMITS_REQUIRED",
            "launch limits are required",
        )
    })?;
    let binding = request.binding.ok_or_else(|| {
        ProviderError::new(
            "sandboxd",
            "SANDBOX_BINDING_REQUIRED",
            "device binding is required",
        )
    })?;
    if request.instance_name.is_empty()
        || request.cgroup_name.is_empty()
        || request.executable.is_empty()
    {
        return Err(ProviderError::new(
            "sandboxd",
            "SANDBOX_LAUNCH_INVALID",
            "instance name, cgroup name, and executable are required",
        ));
    }
    let plan = LaunchPlan {
        instance_name: request.instance_name,
        executable: PathBuf::from(request.executable),
        args: request.args,
        environment: request.environment.into_iter().collect(),
        cgroup_name: request.cgroup_name,
        limits: limits_from_proto(limits),
        transport_socket: (!request.transport_socket_path.is_empty())
            .then(|| PathBuf::from(request.transport_socket_path)),
    };
    runtime.launch(&plan, &binding_from_proto(binding))
}

fn stop_request(
    runtime: &dyn SandboxBackend,
    request: sandbox_v1::SandboxStopRequest,
) -> Result<CleanupReport, ProviderError> {
    let handle = request.handle.ok_or_else(|| {
        ProviderError::new(
            "sandboxd",
            "SANDBOX_PROCESS_HANDLE_REQUIRED",
            "stop requires a process handle",
        )
    })?;
    runtime.stop(
        &handle_from_proto(handle)?,
        &StopRequest {
            grace_period: Duration::from_millis(request.grace_period_ms),
            immediate: request.immediate,
        },
    )
}

fn telemetry_request(
    runtime: &dyn SandboxBackend,
    request: sandbox_v1::SandboxTelemetryRequest,
) -> Result<CgroupTelemetry, ProviderError> {
    let handle = request.handle.ok_or_else(|| {
        ProviderError::new(
            "sandboxd",
            "SANDBOX_PROCESS_HANDLE_REQUIRED",
            "telemetry requires a process handle",
        )
    })?;
    runtime.telemetry(&handle_from_proto(handle)?)
}

fn recover_stale_request(
    runtime: &dyn SandboxBackend,
    request: sandbox_v1::SandboxRecoverStaleRequest,
) -> Result<CleanupReport, ProviderError> {
    let evidence = request.evidence.ok_or_else(|| {
        ProviderError::new(
            "sandboxd",
            "SANDBOX_RECOVERY_EVIDENCE_REQUIRED",
            "stale recovery requires persisted process evidence",
        )
    })?;
    runtime.recover_stale_process(&recovery_evidence_from_proto(evidence)?)
}

fn error_response(
    adapter_id: &str,
    reason_code: &str,
    message: &str,
) -> sandbox_v1::SandboxResponse {
    sandbox_v1::SandboxResponse {
        protocol_version: PROTOCOL_VERSION,
        adapter_id: adapter_id.to_string(),
        body: Some(sandbox_v1::sandbox_response::Body::Error(
            sandbox_v1::SandboxError {
                reason_code: reason_code.to_string(),
                message: message.to_string(),
                retryable: false,
            },
        )),
    }
}

fn capabilities_to_proto(value: NodeCapabilities) -> sandbox_v1::SandboxCapabilities {
    sandbox_v1::SandboxCapabilities {
        ready: value.ready,
        facts: value
            .facts
            .into_iter()
            .map(|fact| sandbox_v1::SandboxFact {
                name: fact.name,
                available: fact.available,
                required: fact.required,
                detail: fact.detail,
            })
            .collect(),
        enforcement: value
            .enforcement
            .into_iter()
            .map(|report| sandbox_v1::SandboxEnforcementReport {
                resource_kind: report.resource_kind,
                mode: enforcement_mode_to_proto(report.mode),
                adapter_id: report.adapter_id,
                reason_code: report.reason_code,
            })
            .collect(),
    }
}

fn limits_from_proto(value: sandbox_v1::SandboxLimits) -> CgroupLimits {
    CgroupLimits {
        cpu_max_millicores: value.cpu_max_millicores,
        memory_max_bytes: value.memory_max_bytes,
        cpuset_cpus: value.cpuset_cpus,
    }
}

fn binding_from_proto(value: sandbox_v1::SandboxDeviceBinding) -> DeviceBinding {
    DeviceBinding {
        resource_id: value.device_id,
        nodes: value
            .nodes
            .into_iter()
            .map(|node| DeviceNode {
                path: PathBuf::from(node.path),
                major: node.major,
                minor: node.minor,
                required: node.required,
            })
            .collect(),
        environment: value.environment.into_iter().collect::<BTreeMap<_, _>>(),
        required_gids: value.required_gids,
        enforcement: enforcement_mode_from_proto(value.enforcement),
        adapter_id: value.adapter_id,
        reason_code: value.reason_code,
    }
}

fn handle_to_proto(value: ProcessHandle) -> sandbox_v1::SandboxProcessHandle {
    sandbox_v1::SandboxProcessHandle {
        pid: value.pid,
        cgroup_path: value.cgroup_path.to_string_lossy().into_owned(),
        start_time_ticks: value.start_time_ticks,
        transport_socket_path: value
            .transport_socket
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default(),
    }
}

fn handle_from_proto(
    value: sandbox_v1::SandboxProcessHandle,
) -> Result<ProcessHandle, ProviderError> {
    if value.pid == 0 || value.cgroup_path.is_empty() {
        return Err(ProviderError::new(
            "sandboxd",
            "SANDBOX_PROCESS_HANDLE_INVALID",
            "process handle must contain a PID and cgroup path",
        ));
    }
    Ok(ProcessHandle {
        pid: value.pid,
        cgroup_path: PathBuf::from(value.cgroup_path),
        start_time_ticks: value.start_time_ticks,
        transport_socket: (!value.transport_socket_path.is_empty())
            .then(|| PathBuf::from(value.transport_socket_path)),
    })
}

fn recovery_evidence_to_proto(value: RuntimeProcessEvidence) -> sandbox_v1::SandboxRuntimeEvidence {
    sandbox_v1::SandboxRuntimeEvidence {
        cgroup_name: value.cgroup_name,
        pid: value.pid,
        start_time_ticks: value.start_time_ticks,
    }
}

fn recovery_evidence_from_proto(
    value: sandbox_v1::SandboxRuntimeEvidence,
) -> Result<RuntimeProcessEvidence, ProviderError> {
    if value.cgroup_name.is_empty() || value.pid == 0 || value.start_time_ticks == 0 {
        return Err(ProviderError::new(
            "sandboxd",
            "SANDBOX_RECOVERY_EVIDENCE_INVALID",
            "recovery evidence requires cgroup name, non-zero PID, and start time",
        ));
    }
    Ok(RuntimeProcessEvidence {
        cgroup_name: value.cgroup_name,
        pid: value.pid,
        start_time_ticks: value.start_time_ticks,
    })
}

fn cleanup_to_proto(value: CleanupReport) -> sandbox_v1::SandboxCleanupReport {
    sandbox_v1::SandboxCleanupReport {
        complete: value.complete,
        exit_code: value.exit_code,
        oom_killed: value.oom_killed,
        conditions: value
            .conditions
            .into_iter()
            .map(|condition| sandbox_v1::SandboxProcessCondition {
                reason_code: condition.reason_code,
                summary: condition.summary,
            })
            .collect(),
        reason_code: value.reason_code,
    }
}

fn telemetry_to_proto(value: CgroupTelemetry) -> sandbox_v1::SandboxTelemetry {
    sandbox_v1::SandboxTelemetry {
        memory_current_bytes: value.memory_current_bytes,
        memory_peak_bytes: value.memory_peak_bytes,
        cpu_usage_usec: value.cpu_usage_usec,
        cpu_user_usec: value.cpu_user_usec,
        cpu_system_usec: value.cpu_system_usec,
        oom_kill_count: value.oom_kill_count,
    }
}

fn enforcement_mode_to_proto(value: EnforcementMode) -> i32 {
    match value {
        EnforcementMode::Hard => sandbox_v1::SandboxEnforcementMode::Hard as i32,
        EnforcementMode::Soft => sandbox_v1::SandboxEnforcementMode::Soft as i32,
        EnforcementMode::VisibilityOnly => {
            sandbox_v1::SandboxEnforcementMode::VisibilityOnly as i32
        }
        EnforcementMode::ObserveOnly => sandbox_v1::SandboxEnforcementMode::ObserveOnly as i32,
        EnforcementMode::Unenforced => sandbox_v1::SandboxEnforcementMode::Unenforced as i32,
    }
}

fn enforcement_mode_from_proto(value: i32) -> EnforcementMode {
    match sandbox_v1::SandboxEnforcementMode::try_from(value).ok() {
        Some(sandbox_v1::SandboxEnforcementMode::Hard) => EnforcementMode::Hard,
        Some(sandbox_v1::SandboxEnforcementMode::Soft) => EnforcementMode::Soft,
        Some(sandbox_v1::SandboxEnforcementMode::VisibilityOnly) => EnforcementMode::VisibilityOnly,
        Some(sandbox_v1::SandboxEnforcementMode::ObserveOnly) => EnforcementMode::ObserveOnly,
        _ => EnforcementMode::Unenforced,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cy_kernel_api::{CapabilityFact, EnforcementReport, ProcessRuntime};
    use std::sync::Mutex;

    struct RecordingBackend {
        launched: Mutex<bool>,
        recovered: Mutex<Vec<RuntimeProcessEvidence>>,
    }

    impl ProcessRuntime for RecordingBackend {
        fn preflight(&self) -> NodeCapabilities {
            NodeCapabilities {
                ready: true,
                facts: vec![CapabilityFact {
                    name: "cgroup_v2".to_string(),
                    available: true,
                    required: true,
                    detail: "test".to_string(),
                }],
                enforcement: vec![EnforcementReport {
                    resource_kind: "gpu".to_string(),
                    mode: EnforcementMode::Hard,
                    adapter_id: "sandboxd".to_string(),
                    reason_code: "TEST".to_string(),
                }],
            }
        }

        fn launch(
            &self,
            _plan: &LaunchPlan,
            _binding: &DeviceBinding,
        ) -> Result<ProcessHandle, ProviderError> {
            *self.launched.lock().unwrap() = true;
            Ok(ProcessHandle {
                pid: 42,
                cgroup_path: PathBuf::from("/test/instance-worker"),
                start_time_ticks: Some(1),
                transport_socket: None,
            })
        }

        fn stop(
            &self,
            _handle: &ProcessHandle,
            _request: &StopRequest,
        ) -> Result<CleanupReport, ProviderError> {
            Ok(CleanupReport {
                complete: true,
                exit_code: Some(0),
                oom_killed: false,
                conditions: Vec::new(),
                reason_code: "STOPPED".to_string(),
            })
        }
    }

    impl SandboxBackend for RecordingBackend {
        fn backend_id(&self) -> &str {
            "test"
        }

        fn discover_recovery_processes(
            &self,
        ) -> Result<Vec<RuntimeProcessEvidence>, ProviderError> {
            Ok(vec![RuntimeProcessEvidence {
                cgroup_name: "instance-worker".to_string(),
                pid: 42,
                start_time_ticks: 7,
            }])
        }

        fn recover_stale_process(
            &self,
            evidence: &RuntimeProcessEvidence,
        ) -> Result<CleanupReport, ProviderError> {
            self.recovered.lock().unwrap().push(evidence.clone());
            Ok(CleanupReport {
                complete: true,
                exit_code: None,
                oom_killed: false,
                conditions: Vec::new(),
                reason_code: "RECOVERY_CLEANUP_COMPLETE".to_string(),
            })
        }
    }

    #[test]
    fn protocol_launch_forwards_only_complete_request() {
        let backend = RecordingBackend {
            launched: Mutex::new(false),
            recovered: Mutex::new(Vec::new()),
        };
        let response = handle_request(
            &backend,
            "sandboxd",
            sandbox_v1::SandboxRequest {
                protocol_version: PROTOCOL_VERSION,
                body: Some(sandbox_v1::sandbox_request::Body::Launch(
                    sandbox_v1::SandboxLaunchRequest {
                        instance_name: "worker".to_string(),
                        executable: "/bin/worker".to_string(),
                        args: Vec::new(),
                        environment: Default::default(),
                        cgroup_name: "instance-worker".to_string(),
                        limits: Some(sandbox_v1::SandboxLimits {
                            cpu_max_millicores: Some(1000),
                            memory_max_bytes: None,
                            cpuset_cpus: None,
                        }),
                        binding: Some(sandbox_v1::SandboxDeviceBinding {
                            device_id: "gpu-0".to_string(),
                            nodes: Vec::new(),
                            environment: Default::default(),
                            required_gids: Vec::new(),
                            enforcement: sandbox_v1::SandboxEnforcementMode::Hard as i32,
                            adapter_id: "nvidia".to_string(),
                            reason_code: "TEST".to_string(),
                        }),
                        transport_socket_path: String::new(),
                    },
                )),
            },
        );
        assert!(*backend.launched.lock().unwrap());
        assert!(matches!(
            response.body,
            Some(sandbox_v1::sandbox_response::Body::Process(_))
        ));
    }

    #[test]
    fn protocol_recovery_is_local_and_forwards_exact_evidence() {
        let backend = RecordingBackend {
            launched: Mutex::new(false),
            recovered: Mutex::new(Vec::new()),
        };
        let discovery = handle_request(
            &backend,
            "sandboxd",
            sandbox_v1::SandboxRequest {
                protocol_version: PROTOCOL_VERSION,
                body: Some(sandbox_v1::sandbox_request::Body::DiscoverRecovery(
                    sandbox_v1::SandboxDiscoverRecoveryRequest {},
                )),
            },
        );
        assert!(matches!(
            discovery.body,
            Some(sandbox_v1::sandbox_response::Body::RecoveryProcesses(_))
        ));
        let recovery = handle_request(
            &backend,
            "sandboxd",
            sandbox_v1::SandboxRequest {
                protocol_version: PROTOCOL_VERSION,
                body: Some(sandbox_v1::sandbox_request::Body::RecoverStale(
                    sandbox_v1::SandboxRecoverStaleRequest {
                        evidence: Some(sandbox_v1::SandboxRuntimeEvidence {
                            cgroup_name: "instance-worker".to_string(),
                            pid: 42,
                            start_time_ticks: 7,
                        }),
                    },
                )),
            },
        );
        assert!(matches!(
            recovery.body,
            Some(sandbox_v1::sandbox_response::Body::Cleanup(_))
        ));
        assert_eq!(backend.recovered.lock().unwrap().len(), 1);
    }
}
