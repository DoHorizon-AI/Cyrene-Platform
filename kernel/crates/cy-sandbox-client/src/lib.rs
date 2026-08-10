//! Pure-safe Kernel client for the external CYRENE Sandbox Adapter Host.
//!
//! The Kernel uses this bounded UDS exchange to request already-authorized
//! lifecycle actions. It never opens cgroup files, starts a process, invokes a
//! device BPF syscall, or loads a privileged library itself.

#![forbid(unsafe_code)]

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

#[cfg(unix)]
use std::io::{Read, Write};

use cy_kernel_api::{
    CapabilityFact, CgroupLimits, CgroupTelemetry, CleanupReport, DeviceBinding, EnforcementMode,
    EnforcementReport, LaunchPlan, NodeCapabilities, ProcessCondition, ProcessHandle,
    ProcessRuntime, ProviderError, SandboxBackend, StopRequest,
};
use cy_proto::sandbox_v1;
use prost::Message;

const PROTOCOL_VERSION: u32 = 1;
#[cfg(unix)]
const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// Static configuration for the one privileged Sandbox Adapter Host used by a
/// Kernel process. Its ID is a protocol identity, never a backend selector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxAdapterEndpoint {
    pub adapter_id: String,
    pub socket_path: PathBuf,
    pub timeout: Duration,
}

impl SandboxAdapterEndpoint {
    pub fn new(adapter_id: impl Into<String>, socket_path: impl Into<PathBuf>) -> Self {
        Self {
            adapter_id: adapter_id.into(),
            socket_path: socket_path.into(),
            timeout: Duration::from_secs(2),
        }
    }
}

/// Kernel-side implementation of the generic sandbox lifecycle port.
#[derive(Debug, Clone)]
pub struct UdsSandboxAdapterClient {
    adapter_id: String,
    socket_path: PathBuf,
    timeout: Duration,
}

impl UdsSandboxAdapterClient {
    pub fn from_endpoint(endpoint: SandboxAdapterEndpoint) -> Result<Self, ProviderError> {
        if !safe_adapter_id(&endpoint.adapter_id) || !endpoint.socket_path.is_absolute() {
            return Err(ProviderError::new(
                "sandbox-adapter-client",
                "ADAPTER_ENDPOINT_INVALID",
                "sandbox adapter ID must be safe and its UDS path must be absolute",
            ));
        }
        Ok(Self {
            adapter_id: endpoint.adapter_id,
            socket_path: endpoint.socket_path,
            timeout: endpoint.timeout,
        })
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    fn call(
        &self,
        body: sandbox_v1::sandbox_request::Body,
    ) -> Result<sandbox_v1::SandboxResponse, ProviderError> {
        let request = sandbox_v1::SandboxRequest {
            protocol_version: PROTOCOL_VERSION,
            body: Some(body),
        };
        let payload = request.encode_to_vec();
        let payload = exchange(&self.adapter_id, &self.socket_path, self.timeout, &payload)?;
        let response =
            sandbox_v1::SandboxResponse::decode(payload.as_slice()).map_err(|error| {
                ProviderError::new(
                    &self.adapter_id,
                    "SANDBOX_PROTOCOL_DECODE",
                    &error.to_string(),
                )
            })?;
        if response.protocol_version != PROTOCOL_VERSION {
            return Err(ProviderError::new(
                &self.adapter_id,
                "SANDBOX_PROTOCOL_VERSION",
                "sandbox adapter returned an incompatible protocol version",
            ));
        }
        if response.adapter_id != self.adapter_id {
            return Err(ProviderError::new(
                &self.adapter_id,
                "SANDBOX_IDENTITY_MISMATCH",
                "UDS endpoint returned a different sandbox adapter ID",
            ));
        }
        if let Some(sandbox_v1::sandbox_response::Body::Error(error)) = &response.body {
            return Err(ProviderError::new(
                &self.adapter_id,
                &error.reason_code,
                &error.message,
            ));
        }
        Ok(response)
    }

    fn protocol_response(&self, expected: &str) -> ProviderError {
        ProviderError::new(
            &self.adapter_id,
            "SANDBOX_PROTOCOL_RESPONSE",
            &format!("sandbox adapter did not return {expected}"),
        )
    }
}

impl ProcessRuntime for UdsSandboxAdapterClient {
    fn preflight(&self) -> NodeCapabilities {
        let response = self.call(sandbox_v1::sandbox_request::Body::Preflight(
            sandbox_v1::SandboxPreflightRequest {},
        ));
        match response {
            Ok(response) => match response.body {
                Some(sandbox_v1::sandbox_response::Body::Capabilities(capabilities)) => {
                    capabilities_from_proto(capabilities)
                }
                _ => unavailable_capabilities(self.protocol_response("capabilities")),
            },
            Err(error) => unavailable_capabilities(error),
        }
    }

    fn launch(
        &self,
        plan: &LaunchPlan,
        binding: &DeviceBinding,
    ) -> Result<ProcessHandle, ProviderError> {
        let response = self.call(sandbox_v1::sandbox_request::Body::Launch(
            sandbox_v1::SandboxLaunchRequest {
                instance_name: plan.instance_name.clone(),
                executable: plan.executable.to_string_lossy().into_owned(),
                args: plan.args.clone(),
                environment: plan.environment.clone().into_iter().collect(),
                cgroup_name: plan.cgroup_name.clone(),
                limits: Some(limits_to_proto(&plan.limits)),
                binding: Some(binding_to_proto(binding)),
            },
        ))?;
        match response.body {
            Some(sandbox_v1::sandbox_response::Body::Process(handle)) => handle_from_proto(handle),
            _ => Err(self.protocol_response("a process handle")),
        }
    }

    fn stop(
        &self,
        handle: &ProcessHandle,
        request: &StopRequest,
    ) -> Result<CleanupReport, ProviderError> {
        let response = self.call(sandbox_v1::sandbox_request::Body::Stop(
            sandbox_v1::SandboxStopRequest {
                handle: Some(handle_to_proto(handle)),
                grace_period_ms: request.grace_period.as_millis().min(u128::from(u64::MAX)) as u64,
                immediate: request.immediate,
            },
        ))?;
        match response.body {
            Some(sandbox_v1::sandbox_response::Body::Cleanup(report)) => {
                Ok(cleanup_from_proto(report))
            }
            _ => Err(self.protocol_response("a cleanup report")),
        }
    }

    fn telemetry(&self, handle: &ProcessHandle) -> Result<CgroupTelemetry, ProviderError> {
        let response = self.call(sandbox_v1::sandbox_request::Body::Telemetry(
            sandbox_v1::SandboxTelemetryRequest {
                handle: Some(handle_to_proto(handle)),
            },
        ))?;
        match response.body {
            Some(sandbox_v1::sandbox_response::Body::Telemetry(telemetry)) => {
                Ok(telemetry_from_proto(telemetry))
            }
            _ => Err(self.protocol_response("telemetry")),
        }
    }
}

impl SandboxBackend for UdsSandboxAdapterClient {
    fn backend_id(&self) -> &str {
        "uds-sandbox-adapter"
    }
}

fn unavailable_capabilities(error: ProviderError) -> NodeCapabilities {
    NodeCapabilities {
        ready: false,
        facts: vec![CapabilityFact {
            name: "sandbox-adapter".to_string(),
            available: false,
            required: true,
            detail: format!("{}: {}", error.reason_code, error.message),
        }],
        enforcement: Vec::new(),
    }
}

fn capabilities_from_proto(value: sandbox_v1::SandboxCapabilities) -> NodeCapabilities {
    NodeCapabilities {
        ready: value.ready,
        facts: value
            .facts
            .into_iter()
            .map(|fact| CapabilityFact {
                name: fact.name,
                available: fact.available,
                required: fact.required,
                detail: fact.detail,
            })
            .collect(),
        enforcement: value
            .enforcement
            .into_iter()
            .map(|report| EnforcementReport {
                resource_kind: report.resource_kind,
                mode: enforcement_mode_from_proto(report.mode),
                adapter_id: report.adapter_id,
                reason_code: report.reason_code,
            })
            .collect(),
    }
}

fn limits_to_proto(value: &CgroupLimits) -> sandbox_v1::SandboxLimits {
    sandbox_v1::SandboxLimits {
        cpu_max_millicores: value.cpu_max_millicores,
        memory_max_bytes: value.memory_max_bytes,
        cpuset_cpus: value.cpuset_cpus.clone(),
    }
}

fn binding_to_proto(value: &DeviceBinding) -> sandbox_v1::SandboxDeviceBinding {
    sandbox_v1::SandboxDeviceBinding {
        device_id: value.device_id.clone(),
        nodes: value
            .nodes
            .iter()
            .map(|node| sandbox_v1::SandboxDeviceNode {
                path: node.path.to_string_lossy().into_owned(),
                major: node.major,
                minor: node.minor,
                required: node.required,
            })
            .collect(),
        environment: value.environment.clone().into_iter().collect(),
        required_gids: value.required_gids.clone(),
        enforcement: enforcement_mode_to_proto(value.enforcement),
        adapter_id: value.adapter_id.clone(),
        reason_code: value.reason_code.clone(),
    }
}

fn handle_to_proto(value: &ProcessHandle) -> sandbox_v1::SandboxProcessHandle {
    sandbox_v1::SandboxProcessHandle {
        pid: value.pid,
        cgroup_path: value.cgroup_path.to_string_lossy().into_owned(),
        start_time_ticks: value.start_time_ticks,
    }
}

fn handle_from_proto(
    value: sandbox_v1::SandboxProcessHandle,
) -> Result<ProcessHandle, ProviderError> {
    if value.pid == 0 || value.cgroup_path.is_empty() {
        return Err(ProviderError::new(
            "sandbox-adapter-client",
            "SANDBOX_PROCESS_HANDLE_INVALID",
            "sandbox adapter returned an incomplete process handle",
        ));
    }
    Ok(ProcessHandle {
        pid: value.pid,
        cgroup_path: PathBuf::from(value.cgroup_path),
        start_time_ticks: value.start_time_ticks,
    })
}

fn cleanup_from_proto(value: sandbox_v1::SandboxCleanupReport) -> CleanupReport {
    CleanupReport {
        complete: value.complete,
        exit_code: value.exit_code,
        oom_killed: value.oom_killed,
        conditions: value
            .conditions
            .into_iter()
            .map(|condition| ProcessCondition {
                reason_code: condition.reason_code,
                summary: condition.summary,
            })
            .collect(),
        reason_code: value.reason_code,
    }
}

fn telemetry_from_proto(value: sandbox_v1::SandboxTelemetry) -> CgroupTelemetry {
    CgroupTelemetry {
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

fn safe_adapter_id(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

#[cfg(unix)]
fn exchange(
    adapter_id: &str,
    socket_path: &Path,
    timeout: Duration,
    payload: &[u8],
) -> Result<Vec<u8>, ProviderError> {
    use std::os::unix::net::UnixStream;

    if payload.len() > MAX_FRAME_BYTES {
        return Err(ProviderError::new(
            adapter_id,
            "SANDBOX_FRAME_TOO_LARGE",
            "sandbox request exceeds the bounded UDS frame limit",
        ));
    }
    let mut stream = UnixStream::connect(socket_path).map_err(|error| {
        ProviderError::new(adapter_id, "SANDBOX_UNAVAILABLE", &error.to_string())
    })?;
    stream
        .set_read_timeout(Some(timeout))
        .and_then(|_| stream.set_write_timeout(Some(timeout)))
        .map_err(|error| {
            ProviderError::new(adapter_id, "SANDBOX_TIMEOUT_CONFIG", &error.to_string())
        })?;
    write_frame(&mut stream, payload).map_err(|error| {
        ProviderError::new(adapter_id, "SANDBOX_WRITE_FAILED", &error.to_string())
    })?;
    read_frame(&mut stream)
        .map_err(|error| ProviderError::new(adapter_id, "SANDBOX_READ_FAILED", &error.to_string()))
}

#[cfg(not(unix))]
fn exchange(
    adapter_id: &str,
    _socket_path: &Path,
    _timeout: Duration,
    _payload: &[u8],
) -> Result<Vec<u8>, ProviderError> {
    Err(ProviderError::new(
        adapter_id,
        "SANDBOX_UNSUPPORTED_PLATFORM",
        "Unix-domain-socket sandbox adapters require a Unix host",
    ))
}

#[cfg(unix)]
fn read_frame(mut reader: impl Read) -> std::io::Result<Vec<u8>> {
    let mut length = [0_u8; 4];
    reader.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    if length > MAX_FRAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "sandbox adapter frame exceeds limit",
        ));
    }
    let mut payload = vec![0; length];
    reader.read_exact(&mut payload)?;
    Ok(payload)
}

#[cfg(unix)]
fn write_frame(mut writer: impl Write, payload: &[u8]) -> std::io::Result<()> {
    if payload.len() > MAX_FRAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "sandbox adapter frame exceeds limit",
        ));
    }
    writer.write_all(&(payload.len() as u32).to_be_bytes())?;
    writer.write_all(payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cy_kernel_api::DeviceNode;
    use std::collections::BTreeMap;

    #[test]
    fn endpoint_rejects_relative_socket_and_unsafe_identity() {
        assert!(
            UdsSandboxAdapterClient::from_endpoint(SandboxAdapterEndpoint::new(
                "sandboxd!",
                "/run/cyrene/sandboxd.sock",
            ))
            .is_err()
        );
        assert!(
            UdsSandboxAdapterClient::from_endpoint(SandboxAdapterEndpoint::new(
                "sandboxd",
                "relative.sock",
            ))
            .is_err()
        );
    }

    #[test]
    fn binding_round_trip_preserves_hard_enforcement() {
        let binding = DeviceBinding {
            device_id: "gpu-0".to_string(),
            nodes: vec![DeviceNode {
                path: PathBuf::from("/dev/example"),
                major: Some(1),
                minor: Some(3),
                required: true,
            }],
            environment: BTreeMap::from([("VISIBLE".to_string(), "0".to_string())]),
            required_gids: vec![44],
            enforcement: EnforcementMode::Hard,
            adapter_id: "nvidia".to_string(),
            reason_code: "DEVICE_BPF".to_string(),
        };
        let encoded = binding_to_proto(&binding);
        assert_eq!(
            encoded.enforcement,
            sandbox_v1::SandboxEnforcementMode::Hard as i32
        );
        assert_eq!(encoded.nodes[0].major, Some(1));
    }
}
