// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: kernel/crates/cy-sandbox-client/src/lib.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! Pure-safe Kernel client for the external CYRENE Sandbox Adapter Host.
//!
//! The Kernel uses this bounded UDS exchange to request already-authorized
//! lifecycle actions. It never opens cgroup files, starts a process, invokes a
//! device BPF syscall, or loads a privileged library itself.

#![cfg_attr(not(test), forbid(unsafe_code))]

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

#[cfg(unix)]
use std::io::{Read, Write};

use cy_adapter_client::PeerCredentialExpectation;
use cy_kernel_api::{
    CapabilityFact, CgroupLimits, CgroupTelemetry, CleanupReport, DeviceBinding, EnforcementMode,
    EnforcementReport, LaunchPlan, NodeCapabilities, ProcessCondition, ProcessHandle,
    ProcessRuntime, ProviderError, RuntimeProcessEvidence, SandboxBackend, StopRequest,
};
use cy_proto::sandbox_v1;
use prost::Message;

const PROTOCOL_VERSION: u32 = 1;
#[cfg(unix)]
const MAX_FRAME_BYTES: usize = 1024 * 1024;
const MIN_FORCED_CLEANUP_WAIT: Duration = Duration::from_millis(500);
const RECOVERY_CLEANUP_WAIT: Duration = Duration::from_secs(10);

/// Static configuration for the one privileged Sandbox Adapter Host used by a
/// Kernel process. Its ID is a protocol identity, never a backend selector.
/// 单个 Kernel 进程使用的特权 Sandbox Adapter Host 静态配置。其 ID 是协议身份，绝不是 backend 选择器。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxAdapterEndpoint {
    pub adapter_id: String,
    pub socket_path: PathBuf,
    pub timeout: Duration,
    pub peer_credentials: PeerCredentialExpectation,
}

impl SandboxAdapterEndpoint {
    pub fn new(adapter_id: impl Into<String>, socket_path: impl Into<PathBuf>) -> Self {
        Self {
            adapter_id: adapter_id.into(),
            socket_path: socket_path.into(),
            timeout: Duration::from_secs(2),
            peer_credentials: PeerCredentialExpectation::default(),
        }
    }
}

/// Kernel-side implementation of the generic sandbox lifecycle port.
/// 通用 sandbox 生命周期接口的 Kernel 侧实现。
#[derive(Debug, Clone)]
pub struct UdsSandboxAdapterClient {
    adapter_id: String,
    socket_path: PathBuf,
    timeout: Duration,
    peer_credentials: PeerCredentialExpectation,
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
            peer_credentials: endpoint.peer_credentials,
        })
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    fn call(
        &self,
        body: sandbox_v1::sandbox_request::Body,
    ) -> Result<sandbox_v1::SandboxResponse, ProviderError> {
        self.call_with_timeout(body, self.timeout)
    }

    fn call_with_timeout(
        &self,
        body: sandbox_v1::sandbox_request::Body,
        timeout: Duration,
    ) -> Result<sandbox_v1::SandboxResponse, ProviderError> {
        let request = sandbox_v1::SandboxRequest {
            protocol_version: PROTOCOL_VERSION,
            body: Some(body),
        };
        let payload = request.encode_to_vec();
        let payload = exchange(
            &self.adapter_id,
            &self.socket_path,
            timeout,
            self.peer_credentials,
            &payload,
        )?;
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

    fn cleanup_rpc_timeout(&self, cleanup_budget: Duration) -> Duration {
        bounded_rpc_timeout(self.timeout, cleanup_budget)
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
                transport_socket_path: plan
                    .transport_socket
                    .as_ref()
                    .map(|path| path.to_string_lossy().into_owned())
                    .unwrap_or_default(),
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
        let response = self.call_with_timeout(
            sandbox_v1::sandbox_request::Body::Stop(sandbox_v1::SandboxStopRequest {
                handle: Some(handle_to_proto(handle)),
                grace_period_ms: request.grace_period.as_millis().min(u128::from(u64::MAX)) as u64,
                immediate: request.immediate,
            }),
            self.cleanup_rpc_timeout(stop_cleanup_budget(request)),
        )?;
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

    fn discover_recovery_processes(&self) -> Result<Vec<RuntimeProcessEvidence>, ProviderError> {
        let response = self.call(sandbox_v1::sandbox_request::Body::DiscoverRecovery(
            sandbox_v1::SandboxDiscoverRecoveryRequest {},
        ))?;
        match response.body {
            Some(sandbox_v1::sandbox_response::Body::RecoveryProcesses(processes)) => processes
                .processes
                .into_iter()
                .map(recovery_evidence_from_proto)
                .collect(),
            _ => Err(self.protocol_response("recovery process evidence")),
        }
    }

    fn recover_stale_process(
        &self,
        evidence: &RuntimeProcessEvidence,
    ) -> Result<CleanupReport, ProviderError> {
        let response = self.call_with_timeout(
            sandbox_v1::sandbox_request::Body::RecoverStale(
                sandbox_v1::SandboxRecoverStaleRequest {
                    evidence: Some(recovery_evidence_to_proto(evidence)),
                },
            ),
            self.cleanup_rpc_timeout(recovery_cleanup_budget()),
        )?;
        match response.body {
            Some(sandbox_v1::sandbox_response::Body::Cleanup(report)) => {
                Ok(cleanup_from_proto(report))
            }
            _ => Err(self.protocol_response("a recovery cleanup report")),
        }
    }
}

fn stop_cleanup_budget(request: &StopRequest) -> Duration {
    let forced_cleanup_wait = std::cmp::max(request.grace_period, MIN_FORCED_CLEANUP_WAIT);
    if request.immediate {
        request.grace_period.saturating_add(forced_cleanup_wait)
    } else {
        request
            .grace_period
            .saturating_add(request.grace_period)
            .saturating_add(forced_cleanup_wait)
    }
}

fn recovery_cleanup_budget() -> Duration {
    RECOVERY_CLEANUP_WAIT.saturating_add(RECOVERY_CLEANUP_WAIT)
}

fn bounded_rpc_timeout(base_timeout: Duration, cleanup_budget: Duration) -> Duration {
    base_timeout.saturating_add(cleanup_budget)
}

fn recovery_evidence_to_proto(
    value: &RuntimeProcessEvidence,
) -> sandbox_v1::SandboxRuntimeEvidence {
    sandbox_v1::SandboxRuntimeEvidence {
        cgroup_name: value.cgroup_name.clone(),
        pid: value.pid,
        start_time_ticks: value.start_time_ticks,
    }
}

fn recovery_evidence_from_proto(
    value: sandbox_v1::SandboxRuntimeEvidence,
) -> Result<RuntimeProcessEvidence, ProviderError> {
    if value.cgroup_name.is_empty() || value.pid == 0 || value.start_time_ticks == 0 {
        return Err(ProviderError::new(
            "sandbox-adapter-client",
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
        device_id: value.resource_id.clone(),
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
        transport_socket_path: value
            .transport_socket
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default(),
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
        transport_socket: (!value.transport_socket_path.is_empty())
            .then(|| PathBuf::from(value.transport_socket_path)),
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
    peer_credentials: PeerCredentialExpectation,
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
    verify_connected_peer(adapter_id, &stream, peer_credentials)?;
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
    _peer_credentials: PeerCredentialExpectation,
    _payload: &[u8],
) -> Result<Vec<u8>, ProviderError> {
    Err(ProviderError::new(
        adapter_id,
        "SANDBOX_UNSUPPORTED_PLATFORM",
        "Unix-domain-socket sandbox adapters require a Unix host",
    ))
}

/// Pure admission decision for the Kernel-side sandbox peer check, mirroring
/// the `cy_adapter_client::credential` style: the SO_PEERCRED lookup lives in
/// the thin `verify_connected_peer` shell below.
/// Kernel 侧 sandbox 对端检查的纯接入判定，采用与 cy_adapter_client::credential 相同的方式：SO_PEERCRED 查询位于下面精简的 verify_connected_peer 外壳中。
#[cfg(any(test, target_os = "linux"))]
fn verify_sandbox_peer_credentials(
    adapter_id: &str,
    expected: PeerCredentialExpectation,
    actual_uid: u32,
    actual_gid: u32,
) -> Result<(), ProviderError> {
    if expected.uid.is_some_and(|uid| uid != actual_uid)
        || expected.gid.is_some_and(|gid| gid != actual_gid)
    {
        return Err(ProviderError::new(
            adapter_id,
            "SANDBOX_PEER_CREDENTIAL_MISMATCH",
            "UDS peer credentials do not match the configured sandbox adapter identity",
        ));
    }
    Ok(())
}

/// Verifies the connected Sandbox Adapter Host against the configured UDS
/// peer identity. The Kernel checks it after connect, before sending any
/// sandbox request; a mismatch fails closed and drops the connection.
/// 根据配置的 UDS 对端身份验证已连接的 Sandbox Adapter Host。Kernel 会在 connect 后、发送任何 sandbox 请求前完成检查；身份不匹配时会失败关闭并丢弃连接。
#[cfg(unix)]
fn verify_connected_peer(
    adapter_id: &str,
    stream: &std::os::unix::net::UnixStream,
    expected: PeerCredentialExpectation,
) -> Result<(), ProviderError> {
    if !expected.is_configured() {
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    {
        let credentials =
            nix::sys::socket::getsockopt(stream, nix::sys::socket::sockopt::PeerCredentials)
                .map_err(|error| {
                    ProviderError::new(
                        adapter_id,
                        "SANDBOX_PEER_CREDENTIAL_UNAVAILABLE",
                        &error.to_string(),
                    )
                })?;
        verify_sandbox_peer_credentials(adapter_id, expected, credentials.uid(), credentials.gid())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = stream;
        Err(ProviderError::new(
            adapter_id,
            "SANDBOX_PEER_CREDENTIAL_UNSUPPORTED",
            "configured UDS peer credential checks require a Linux Kernel host",
        ))
    }
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
            resource_id: "gpu-0".to_string(),
            nodes: vec![DeviceNode {
                path: PathBuf::from("/dev/example"),
                major: Some(1),
                minor: Some(3),
                required: true,
            }],
            environment: BTreeMap::from([("VISIBLE".to_string(), "0".to_string())]),
            joinable_environment_keys: Default::default(),
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

    #[test]
    fn sandbox_peer_credential_policy_covers_all_expectation_shapes() {
        // (expected_uid, expected_gid, actual_uid, actual_gid, accepted)
        // 元组字段依次为：期望 UID、期望 GID、实际 UID、实际 GID、是否接纳。
        let cases = [
            (Some(1000), Some(2000), 1000, 2000, true),
            (Some(1000), Some(2000), 1001, 2000, false),
            (Some(1000), Some(2000), 1000, 2001, false),
            (Some(1000), Some(2000), 1001, 2001, false),
            (Some(1000), None, 1000, 9999, true),
            (Some(1000), None, 1001, 1000, false),
            (None, Some(2000), 9999, 2000, true),
            (None, Some(2000), 2000, 2001, false),
            (None, None, 0, 0, true),
        ];
        for (uid, gid, actual_uid, actual_gid, accepted) in cases {
            let expected = PeerCredentialExpectation { uid, gid };
            let verdict =
                verify_sandbox_peer_credentials("sandboxd-test", expected, actual_uid, actual_gid);
            assert_eq!(
                verdict.is_ok(),
                accepted,
                "unexpected verdict for expectation ({uid:?}, {gid:?}) against ({actual_uid}, {actual_gid})"
            );
            if !accepted {
                assert_eq!(
                    verdict.unwrap_err().reason_code,
                    "SANDBOX_PEER_CREDENTIAL_MISMATCH"
                );
            }
        }
    }

    #[test]
    fn cleanup_rpc_budget_matches_bounded_sandbox_runtime_phases() {
        assert_eq!(
            stop_cleanup_budget(&StopRequest {
                grace_period: Duration::from_secs(1),
                immediate: false,
            }),
            Duration::from_secs(3)
        );
        assert_eq!(
            stop_cleanup_budget(&StopRequest {
                grace_period: Duration::from_secs(1),
                immediate: true,
            }),
            Duration::from_secs(2)
        );
        assert_eq!(
            stop_cleanup_budget(&StopRequest {
                grace_period: Duration::ZERO,
                immediate: false,
            }),
            MIN_FORCED_CLEANUP_WAIT
        );
        assert_eq!(recovery_cleanup_budget(), Duration::from_secs(20));
    }

    #[test]
    fn cleanup_rpc_deadline_saturates_without_overflow() {
        let request = StopRequest {
            grace_period: Duration::MAX,
            immediate: false,
        };
        assert_eq!(stop_cleanup_budget(&request), Duration::MAX);
        assert_eq!(
            bounded_rpc_timeout(Duration::MAX, Duration::MAX),
            Duration::MAX
        );
        assert_eq!(
            bounded_rpc_timeout(Duration::from_secs(2), Duration::MAX),
            Duration::MAX
        );
    }
}

/// Real Unix-domain-socket coverage for the Kernel↔sandboxd peer-credential
/// admission check. Gated to Linux because SO_PEERCRED is the supported
/// credential source; these cases are compiled out on other hosts.
/// 对 Kernel 与 sandboxd 之间的真实 Unix domain socket 对端凭证接入检查进行覆盖。由于 SO_PEERCRED 是受支持的凭证来源，此项仅在 Linux 上启用；其他主机不会编译这些用例。
#[cfg(all(test, target_os = "linux"))]
mod linux_uds {
    use std::{
        fs,
        os::unix::net::{UnixListener, UnixStream},
        path::PathBuf,
        sync::mpsc,
        time::Duration,
    };

    use cy_adapter_client::PeerCredentialExpectation;
    use cy_kernel_api::{
        ProcessHandle, ProcessRuntime, RuntimeProcessEvidence, SandboxBackend, StopRequest,
    };
    use cy_proto::sandbox_v1;
    use prost::Message;

    use crate::{
        exchange, read_frame, write_frame, SandboxAdapterEndpoint, UdsSandboxAdapterClient,
    };

    const ADAPTER_ID: &str = "sandboxd-test";

    /// SO_PEERCRED on a loopback socket pair reports this process, which is
    /// the ground truth both sides compare against.
    /// loopback socket pair 上的 SO_PEERCRED 会返回当前进程身份，这是通信双方比对的基准事实。
    fn own_peer_credentials() -> PeerCredentialExpectation {
        let (stream, _peer) = UnixStream::pair().unwrap();
        let credentials =
            nix::sys::socket::getsockopt(&stream, nix::sys::socket::sockopt::PeerCredentials)
                .unwrap();
        PeerCredentialExpectation {
            uid: Some(credentials.uid()),
            gid: Some(credentials.gid()),
        }
    }

    fn endpoint(tag: &str, peer_credentials: PeerCredentialExpectation) -> SandboxAdapterEndpoint {
        let directory = std::env::temp_dir().join(format!(
            "cyrene-sandbox-client-{}-{tag}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).unwrap();
        let mut endpoint = SandboxAdapterEndpoint::new(ADAPTER_ID, directory.join("sandboxd.sock"));
        endpoint.peer_credentials = peer_credentials;
        endpoint
    }

    #[derive(Clone, Copy)]
    enum ExpectedRequest {
        Preflight,
        Stop {
            grace_period_ms: u64,
            immediate: bool,
        },
        RecoverStale,
    }

    fn delayed_response_server(
        socket: PathBuf,
        delay: Duration,
        expected: ExpectedRequest,
    ) -> std::thread::JoinHandle<()> {
        let listener = UnixListener::bind(&socket).unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let payload = read_frame(&mut stream).unwrap();
            let request = sandbox_v1::SandboxRequest::decode(payload.as_slice()).unwrap();
            match (expected, request.body) {
                (
                    ExpectedRequest::Preflight,
                    Some(sandbox_v1::sandbox_request::Body::Preflight(_)),
                ) => {}
                (
                    ExpectedRequest::Stop {
                        grace_period_ms,
                        immediate,
                    },
                    Some(sandbox_v1::sandbox_request::Body::Stop(request)),
                ) => {
                    assert_eq!(request.grace_period_ms, grace_period_ms);
                    assert_eq!(request.immediate, immediate);
                }
                (
                    ExpectedRequest::RecoverStale,
                    Some(sandbox_v1::sandbox_request::Body::RecoverStale(request)),
                ) => {
                    let evidence = request.evidence.expect("recovery evidence");
                    assert_eq!(evidence.cgroup_name, "instance-stale");
                    assert_eq!(evidence.pid, 1234);
                    assert_eq!(evidence.start_time_ticks, 5678);
                }
                _ => panic!("mock server received an unexpected sandbox request"),
            }
            std::thread::sleep(delay);
            let response = match expected {
                ExpectedRequest::Preflight => sandbox_v1::SandboxResponse {
                    protocol_version: crate::PROTOCOL_VERSION,
                    adapter_id: ADAPTER_ID.to_string(),
                    body: Some(sandbox_v1::sandbox_response::Body::Capabilities(
                        sandbox_v1::SandboxCapabilities {
                            ready: true,
                            facts: Vec::new(),
                            enforcement: Vec::new(),
                        },
                    )),
                },
                ExpectedRequest::Stop { .. } | ExpectedRequest::RecoverStale => {
                    sandbox_v1::SandboxResponse {
                        protocol_version: crate::PROTOCOL_VERSION,
                        adapter_id: ADAPTER_ID.to_string(),
                        body: Some(sandbox_v1::sandbox_response::Body::Cleanup(
                            sandbox_v1::SandboxCleanupReport {
                                complete: true,
                                exit_code: Some(0),
                                oom_killed: false,
                                conditions: Vec::new(),
                                reason_code: "CLEANUP_COMPLETE".to_string(),
                            },
                        )),
                    }
                }
            };
            let _ = write_frame(&mut stream, &response.encode_to_vec());
        })
    }

    #[test]
    fn preflight_completes_when_sandboxd_peer_matches_configured_identity() {
        let endpoint = endpoint("positive", own_peer_credentials());
        let socket = endpoint.socket_path.clone();
        let listener = UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let payload = read_frame(&mut stream).unwrap();
            let request = sandbox_v1::SandboxRequest::decode(payload.as_slice()).unwrap();
            assert_eq!(request.protocol_version, crate::PROTOCOL_VERSION);
            let response = sandbox_v1::SandboxResponse {
                protocol_version: crate::PROTOCOL_VERSION,
                adapter_id: ADAPTER_ID.to_string(),
                body: Some(sandbox_v1::sandbox_response::Body::Capabilities(
                    sandbox_v1::SandboxCapabilities {
                        ready: true,
                        facts: Vec::new(),
                        enforcement: Vec::new(),
                    },
                )),
            };
            write_frame(&mut stream, &response.encode_to_vec()).unwrap();
        });
        let client = UdsSandboxAdapterClient::from_endpoint(endpoint).unwrap();
        assert!(client.preflight().ready);
        server.join().unwrap();
        let _ = fs::remove_dir_all(socket.parent().unwrap());
    }

    #[test]
    fn ordinary_rpc_keeps_the_configured_timeout() {
        let mut endpoint = endpoint("ordinary-timeout", PeerCredentialExpectation::default());
        endpoint.timeout = Duration::from_millis(20);
        let socket = endpoint.socket_path.clone();
        let server = delayed_response_server(
            socket.clone(),
            Duration::from_millis(120),
            ExpectedRequest::Preflight,
        );
        let client = UdsSandboxAdapterClient::from_endpoint(endpoint).unwrap();

        let capabilities = client.preflight();
        assert!(!capabilities.ready);
        assert!(capabilities
            .facts
            .iter()
            .any(|fact| fact.detail.contains("SANDBOX_READ_FAILED")));
        server.join().unwrap();
        let _ = fs::remove_dir_all(socket.parent().unwrap());
    }

    #[test]
    fn stop_rpc_allows_a_reply_past_the_ordinary_timeout_with_cleanup_budget() {
        let mut endpoint = endpoint("stop-cleanup-budget", PeerCredentialExpectation::default());
        endpoint.timeout = Duration::from_millis(20);
        let socket = endpoint.socket_path.clone();
        let server = delayed_response_server(
            socket.clone(),
            Duration::from_millis(120),
            ExpectedRequest::Stop {
                grace_period_ms: 80,
                immediate: false,
            },
        );
        let client = UdsSandboxAdapterClient::from_endpoint(endpoint).unwrap();

        let report = client
            .stop(
                &ProcessHandle {
                    pid: 1234,
                    cgroup_path: PathBuf::from("/sys/fs/cgroup/instance-stale"),
                    start_time_ticks: Some(5678),
                    transport_socket: None,
                },
                &StopRequest {
                    grace_period: Duration::from_millis(80),
                    immediate: false,
                },
            )
            .unwrap();
        assert!(report.complete);
        server.join().unwrap();
        let _ = fs::remove_dir_all(socket.parent().unwrap());
    }

    #[test]
    fn stop_rpc_expires_at_the_bounded_cleanup_deadline() {
        let mut endpoint = endpoint(
            "stop-cleanup-deadline",
            PeerCredentialExpectation::default(),
        );
        endpoint.timeout = Duration::from_millis(20);
        let socket = endpoint.socket_path.clone();
        let server = delayed_response_server(
            socket.clone(),
            Duration::from_millis(700),
            ExpectedRequest::Stop {
                grace_period_ms: 0,
                immediate: false,
            },
        );
        let client = UdsSandboxAdapterClient::from_endpoint(endpoint).unwrap();

        let error = client
            .stop(
                &ProcessHandle {
                    pid: 1234,
                    cgroup_path: PathBuf::from("/sys/fs/cgroup/instance-stale"),
                    start_time_ticks: Some(5678),
                    transport_socket: None,
                },
                &StopRequest {
                    grace_period: Duration::ZERO,
                    immediate: false,
                },
            )
            .unwrap_err();
        assert_eq!(error.reason_code, "SANDBOX_READ_FAILED");
        server.join().unwrap();
        let _ = fs::remove_dir_all(socket.parent().unwrap());
    }

    #[test]
    fn recover_stale_rpc_allows_a_reply_past_the_ordinary_timeout() {
        let mut endpoint = endpoint(
            "recover-cleanup-budget",
            PeerCredentialExpectation::default(),
        );
        endpoint.timeout = Duration::from_millis(20);
        let socket = endpoint.socket_path.clone();
        let server = delayed_response_server(
            socket.clone(),
            Duration::from_millis(120),
            ExpectedRequest::RecoverStale,
        );
        let client = UdsSandboxAdapterClient::from_endpoint(endpoint).unwrap();

        let report = client
            .recover_stale_process(&RuntimeProcessEvidence {
                cgroup_name: "instance-stale".to_string(),
                pid: 1234,
                start_time_ticks: 5678,
            })
            .unwrap();
        assert!(report.complete);
        server.join().unwrap();
        let _ = fs::remove_dir_all(socket.parent().unwrap());
    }

    #[test]
    fn exchange_fails_closed_before_writing_when_sandboxd_peer_mismatches() {
        let endpoint = endpoint("negative", PeerCredentialExpectation::default());
        let socket = endpoint.socket_path.clone();
        let listener = UnixListener::bind(&socket).unwrap();
        let (observed_tx, observed_rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_millis(500)))
                .unwrap();
            let observed = read_frame(&mut stream).map(|_| ());
            let _ = observed_tx.send(observed);
        });
        let mut expected = own_peer_credentials();
        expected.uid = expected.uid.map(|uid| uid.wrapping_add(1));
        let error = exchange(
            ADAPTER_ID,
            &socket,
            Duration::from_secs(2),
            expected,
            b"preflight",
        )
        .unwrap_err();
        assert_eq!(error.reason_code, "SANDBOX_PEER_CREDENTIAL_MISMATCH");
        // The sandboxd side must never observe a protocol frame from the
        // rejected Kernel client: its bounded read has to time out.
        // sandboxd 端绝不能看到被拒绝 Kernel 客户端发来的协议帧：其有界读取必须超时。
        let observed = observed_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let error = observed.unwrap_err();
        assert!(
            error.kind() == std::io::ErrorKind::WouldBlock
                || error.kind() == std::io::ErrorKind::TimedOut
                || error.kind() == std::io::ErrorKind::UnexpectedEof
                || error.kind() == std::io::ErrorKind::ConnectionReset,
            "server must observe a closed connection or time out waiting for a frame that was never sent, got {error}"
        );
        server.join().unwrap();
        let _ = fs::remove_dir_all(socket.parent().unwrap());
    }
}
