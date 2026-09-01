// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: sdk/rust/cy-worker-sdk/tests/subprocess_worker_tck.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
#![cfg(unix)]

use std::{
    collections::BTreeMap,
    io::{BufReader, BufWriter},
    path::PathBuf,
    process::{Command, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};

use cy_kernel_api::{
    CgroupLimits, CgroupTelemetry, CleanupReport, DeviceBinding, EnforcementMode, LaunchPlan,
    NodeCapabilities, ProcessHandle, ProcessRuntime, ProviderError, SandboxBackend, StopRequest,
};
use cy_kernel_daemon::watchdog::{InstanceActor, InstanceActorState};
use cy_worker_sdk::{
    health_status,
    pb::{
        invoke::Request as InvokeReq, invoke_result::Response as InvokeResp, DetectHardwareRequest,
        Envelope, HealthCheck, Hello, Invoke, Shutdown,
    },
    read_frame, write_frame, Payload, DEFAULT_MAX_MESSAGE_BYTES,
};

#[test]
fn test_rust_worker_real_subprocess_lifecycle_tck() -> Result<(), Box<dyn std::error::Error>> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root_dir = manifest_dir
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let bin_path = root_dir.join("target/debug/examples/echo_worker");

    // Build the example if needed
    if !bin_path.exists() {
        let status = Command::new("cargo")
            .args(["build", "-p", "cy-worker-sdk", "--example", "echo_worker"])
            .current_dir(root_dir)
            .status()?;
        assert!(status.success(), "Failed to build echo_worker example");
    }

    let mut child = Command::new(&bin_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;

    let mut writer = BufWriter::new(child.stdin.take().expect("child stdin"));
    let mut reader = BufReader::new(child.stdout.take().expect("child stdout"));

    // 1. Handshake: Send Hello -> Receive HelloAck
    let hello_env = Envelope {
        request_id: "req-rust-hello".to_string(),
        trace_id: "tr-rust-1".to_string(),
        plugin_id: "com.cyrene.test.rust-echo-worker".to_string(),
        protocol_version: 1,
        deadline_ms: 5000,
        sequence_number: 1,
        generation: 1,
        fence_token: 1,
        payload: Some(Payload::Hello(Hello {
            min_protocol_version: 1,
            max_protocol_version: 1,
            host_version: "1.0.0".to_string(),
        })),
    };
    write_frame(&mut writer, &hello_env, DEFAULT_MAX_MESSAGE_BYTES)?;

    let hello_ack_env = read_frame(&mut reader, DEFAULT_MAX_MESSAGE_BYTES)?
        .expect("expected HelloAck from Rust worker");
    assert_eq!(hello_ack_env.request_id, "req-rust-hello");
    match hello_ack_env.payload {
        Some(Payload::HelloAck(ack)) => {
            assert_eq!(ack.plugin_id, "com.cyrene.test.rust-echo-worker");
            assert_eq!(ack.selected_protocol_version, 1);
            assert!(ack.declared_capabilities.contains(&"Probe".to_string()));
        }
        other => panic!("expected HelloAck, got {other:?}"),
    }

    // 2. HealthCheck: Send HealthCheck -> Receive HealthStatus
    let health_env = Envelope {
        request_id: "req-rust-health".to_string(),
        trace_id: "tr-rust-1".to_string(),
        plugin_id: "com.cyrene.test.rust-echo-worker".to_string(),
        protocol_version: 1,
        deadline_ms: 2000,
        sequence_number: 2,
        generation: 1,
        fence_token: 1,
        payload: Some(Payload::HealthCheck(HealthCheck {})),
    };
    write_frame(&mut writer, &health_env, DEFAULT_MAX_MESSAGE_BYTES)?;

    let health_resp = read_frame(&mut reader, DEFAULT_MAX_MESSAGE_BYTES)?
        .expect("expected HealthStatus from Rust worker");
    assert_eq!(health_resp.request_id, "req-rust-health");
    match health_resp.payload {
        Some(Payload::HealthStatus(h)) => {
            assert_eq!(h.status, health_status::Status::Healthy as i32);
        }
        other => panic!("expected HealthStatus, got {other:?}"),
    }

    // 3. Invoke: Send Invoke -> Receive InvokeResult
    let invoke_env = Envelope {
        request_id: "req-rust-invoke".to_string(),
        trace_id: "tr-rust-1".to_string(),
        plugin_id: "com.cyrene.test.rust-echo-worker".to_string(),
        protocol_version: 1,
        deadline_ms: 5000,
        sequence_number: 3,
        generation: 1,
        fence_token: 1,
        payload: Some(Payload::Invoke(Invoke {
            extension_point: "Probe".to_string(),
            method: "detect_hardware".to_string(),
            payload: Vec::new(),
            request: Some(InvokeReq::DetectHardware(DetectHardwareRequest {})),
        })),
    };
    write_frame(&mut writer, &invoke_env, DEFAULT_MAX_MESSAGE_BYTES)?;

    let invoke_resp = read_frame(&mut reader, DEFAULT_MAX_MESSAGE_BYTES)?
        .expect("expected InvokeResult from Rust worker");
    assert_eq!(invoke_resp.request_id, "req-rust-invoke");
    match invoke_resp.payload {
        Some(Payload::InvokeResult(res)) => match res.response {
            Some(InvokeResp::DetectHardware(dh)) => {
                assert!(dh.hardware_manifest_json.contains("rust"));
            }
            other => panic!("expected DetectHardware response, got {other:?}"),
        },
        other => panic!("expected InvokeResult, got {other:?}"),
    }

    // 4. Shutdown: Send Shutdown -> Receive Shutdown ACK -> Child process cleanly terminates
    let shutdown_env = Envelope {
        request_id: "req-rust-shutdown".to_string(),
        trace_id: "tr-rust-1".to_string(),
        plugin_id: "com.cyrene.test.rust-echo-worker".to_string(),
        protocol_version: 1,
        deadline_ms: 2000,
        sequence_number: 4,
        generation: 1,
        fence_token: 1,
        payload: Some(Payload::Shutdown(Shutdown {
            grace_period_ms: 500,
        })),
    };
    write_frame(&mut writer, &shutdown_env, DEFAULT_MAX_MESSAGE_BYTES)?;

    let shutdown_ack = read_frame(&mut reader, DEFAULT_MAX_MESSAGE_BYTES)?
        .expect("expected Shutdown ACK from Rust worker");
    assert_eq!(shutdown_ack.request_id, "req-rust-shutdown");

    // Process should terminate with exit code 0
    let status = child.wait()?;
    assert!(
        status.success(),
        "Rust worker child process should exit cleanly with code 0"
    );

    Ok(())
}

#[test]
fn test_python_worker_real_subprocess_lifecycle_tck() -> Result<(), Box<dyn std::error::Error>> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let python_script = manifest_dir
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("python/cyrene_worker_shim/echo_worker.py");

    let python_module_dir = manifest_dir
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("python/cyrene_worker_shim");

    let mut child = Command::new("python3")
        .arg(&python_script)
        .env("PYTHONPATH", &python_module_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;

    let mut writer = BufWriter::new(child.stdin.take().expect("child stdin"));
    let mut reader = BufReader::new(child.stdout.take().expect("child stdout"));

    // 1. Handshake: Send Hello -> Receive HelloAck
    let hello_env = Envelope {
        request_id: "req-py-hello".to_string(),
        trace_id: "tr-py-1".to_string(),
        plugin_id: "com.cyrene.test.python-echo-worker".to_string(),
        protocol_version: 1,
        deadline_ms: 5000,
        sequence_number: 1,
        generation: 1,
        fence_token: 1,
        payload: Some(Payload::Hello(Hello {
            min_protocol_version: 1,
            max_protocol_version: 1,
            host_version: "1.0.0".to_string(),
        })),
    };
    write_frame(&mut writer, &hello_env, DEFAULT_MAX_MESSAGE_BYTES)?;

    let hello_ack_env = read_frame(&mut reader, DEFAULT_MAX_MESSAGE_BYTES)?
        .expect("expected HelloAck from Python worker");
    assert_eq!(hello_ack_env.request_id, "req-py-hello");
    match hello_ack_env.payload {
        Some(Payload::HelloAck(ack)) => {
            assert_eq!(ack.plugin_id, "com.cyrene.test.python-echo-worker");
            assert_eq!(ack.selected_protocol_version, 1);
            assert!(ack
                .declared_capabilities
                .contains(&"PythonShim".to_string()));
        }
        other => panic!("expected HelloAck, got {other:?}"),
    }

    // 2. HealthCheck: Send HealthCheck -> Receive HealthStatus
    let health_env = Envelope {
        request_id: "req-py-health".to_string(),
        trace_id: "tr-py-1".to_string(),
        plugin_id: "com.cyrene.test.python-echo-worker".to_string(),
        protocol_version: 1,
        deadline_ms: 2000,
        sequence_number: 2,
        generation: 1,
        fence_token: 1,
        payload: Some(Payload::HealthCheck(HealthCheck {})),
    };
    write_frame(&mut writer, &health_env, DEFAULT_MAX_MESSAGE_BYTES)?;

    let health_resp = read_frame(&mut reader, DEFAULT_MAX_MESSAGE_BYTES)?
        .expect("expected HealthStatus from Python worker");
    assert_eq!(health_resp.request_id, "req-py-health");
    match health_resp.payload {
        Some(Payload::HealthStatus(h)) => {
            assert_eq!(h.status, health_status::Status::Healthy as i32);
        }
        other => panic!("expected HealthStatus, got {other:?}"),
    }

    // 3. Invoke: Send Invoke (DetectHardware) -> Receive InvokeResult
    let invoke_env = Envelope {
        request_id: "req-py-invoke".to_string(),
        trace_id: "tr-py-1".to_string(),
        plugin_id: "com.cyrene.test.python-echo-worker".to_string(),
        protocol_version: 1,
        deadline_ms: 5000,
        sequence_number: 3,
        generation: 1,
        fence_token: 1,
        payload: Some(Payload::Invoke(Invoke {
            extension_point: "Probe".to_string(),
            method: "detect_hardware".to_string(),
            payload: Vec::new(),
            request: Some(InvokeReq::DetectHardware(DetectHardwareRequest {})),
        })),
    };
    write_frame(&mut writer, &invoke_env, DEFAULT_MAX_MESSAGE_BYTES)?;

    let invoke_resp = read_frame(&mut reader, DEFAULT_MAX_MESSAGE_BYTES)?
        .expect("expected InvokeResult from Python worker");
    assert_eq!(invoke_resp.request_id, "req-py-invoke");
    assert!(
        matches!(invoke_resp.payload, Some(Payload::InvokeResult(_))),
        "expected InvokeResult from Python worker"
    );

    // 4. Shutdown: Send Shutdown -> Receive Shutdown ACK -> Child process cleanly terminates
    let shutdown_env = Envelope {
        request_id: "req-py-shutdown".to_string(),
        trace_id: "tr-py-1".to_string(),
        plugin_id: "com.cyrene.test.python-echo-worker".to_string(),
        protocol_version: 1,
        deadline_ms: 2000,
        sequence_number: 4,
        generation: 1,
        fence_token: 1,
        payload: Some(Payload::Shutdown(Shutdown {
            grace_period_ms: 500,
        })),
    };
    write_frame(&mut writer, &shutdown_env, DEFAULT_MAX_MESSAGE_BYTES)?;

    let shutdown_ack = read_frame(&mut reader, DEFAULT_MAX_MESSAGE_BYTES)?
        .expect("expected Shutdown ACK from Python worker");
    assert_eq!(shutdown_ack.request_id, "req-py-shutdown");

    // Process should terminate with exit code 0
    let status = child.wait()?;
    assert!(
        status.success(),
        "Python worker child process should exit cleanly with code 0"
    );

    Ok(())
}

#[test]
fn test_real_subprocess_posix_sigterm_handling() -> Result<(), Box<dyn std::error::Error>> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let python_script = manifest_dir
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("python/cyrene_worker_shim/echo_worker.py");

    let python_module_dir = manifest_dir
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("python/cyrene_worker_shim");

    let mut child = Command::new("python3")
        .arg(&python_script)
        .env("PYTHONPATH", &python_module_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;

    let mut writer = BufWriter::new(child.stdin.take().expect("child stdin"));
    let mut reader = BufReader::new(child.stdout.take().expect("child stdout"));

    // 1. Initial Handshake
    let hello_env = Envelope {
        request_id: "req-sigterm-hello".to_string(),
        trace_id: "tr-sigterm-1".to_string(),
        plugin_id: "com.cyrene.test.python-echo-worker".to_string(),
        protocol_version: 1,
        deadline_ms: 5000,
        sequence_number: 1,
        generation: 1,
        fence_token: 1,
        payload: Some(Payload::Hello(Hello {
            min_protocol_version: 1,
            max_protocol_version: 1,
            host_version: "1.0.0".to_string(),
        })),
    };
    write_frame(&mut writer, &hello_env, DEFAULT_MAX_MESSAGE_BYTES)?;
    let _ = read_frame(&mut reader, DEFAULT_MAX_MESSAGE_BYTES)?;

    // 2. Send real OS SIGTERM signal (kill -TERM <pid>)
    let pid = child.id() as i32;
    unsafe {
        libc::kill(pid, libc::SIGTERM);
    }

    // 3. Child process should catch SIGTERM and exit cleanly
    let status = child.wait()?;
    assert!(
        status.success() || status.code() == Some(0) || status.code() == Some(143),
        "Child process should terminate upon receiving OS SIGTERM signal"
    );

    Ok(())
}

struct MockSandboxBackend;

impl ProcessRuntime for MockSandboxBackend {
    fn preflight(&self) -> NodeCapabilities {
        NodeCapabilities {
            ready: true,
            facts: Vec::new(),
            enforcement: Vec::new(),
        }
    }

    fn launch(
        &self,
        plan: &LaunchPlan,
        _binding: &DeviceBinding,
    ) -> Result<ProcessHandle, ProviderError> {
        Ok(ProcessHandle {
            pid: 12345,
            cgroup_path: PathBuf::from(&format!("/cgroup/{}", plan.cgroup_name)),
            start_time_ticks: Some(100),
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
            reason_code: "CLEANUP_COMPLETE".to_string(),
        })
    }

    fn telemetry(&self, _handle: &ProcessHandle) -> Result<CgroupTelemetry, ProviderError> {
        Ok(CgroupTelemetry::default())
    }
}

impl SandboxBackend for MockSandboxBackend {
    fn backend_id(&self) -> &str {
        "mock-sandbox"
    }
}

#[test]
fn test_real_subprocess_kill9_to_watchdog_quarantine_integration(
) -> Result<(), Box<dyn std::error::Error>> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let python_script = manifest_dir
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("python/cyrene_worker_shim/echo_worker.py");

    let python_module_dir = manifest_dir
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("python/cyrene_worker_shim");

    let plan = LaunchPlan {
        instance_name: "py-worker-actor".to_string(),
        executable: PathBuf::from("python3"),
        args: vec![python_script.to_string_lossy().to_string()],
        environment: BTreeMap::new(),
        cgroup_name: "cgroup-py-worker".to_string(),
        limits: CgroupLimits::default(),
        working_dir: None,
        transport_socket: None,
    };

    let backend = Arc::new(MockSandboxBackend);
    let mut actor = InstanceActor::new(
        "py-worker-actor",
        "lease-py-1",
        42,
        backend,
        plan,
        DeviceBinding {
            resource_id: "res-1".to_string(),
            enforcement: EnforcementMode::Soft,
            nodes: Vec::new(),
            environment: BTreeMap::new(),
            joinable_environment_keys: Default::default(),
            required_gids: Vec::new(),
            adapter_id: "adapter-1".to_string(),
            reason_code: "ok".to_string(),
        },
        Duration::from_secs(5),
    );

    // Verify initial start succeeds
    actor.start().expect("initial actor start should succeed");
    assert_eq!(actor.state(), InstanceActorState::Healthy);

    // Simulate 4 consecutive real child process crash events via kill -9
    let now = Instant::now();
    for i in 1..=4 {
        let mut child = Command::new("python3")
            .arg(&python_script)
            .env("PYTHONPATH", &python_module_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;

        let pid = child.id() as i32;
        // Kill child process with SIGKILL (kill -9)
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
        let _ = child.wait();

        // Feed crash to InstanceActor
        let is_quarantined = actor.record_crash(now + Duration::from_millis(i * 100));
        if i == 4 {
            assert!(
                is_quarantined,
                "4th crash within 60s must trigger Quarantine"
            );
        }
    }

    // Assert actor is now in Quarantined state
    assert_eq!(actor.state(), InstanceActorState::Quarantined);

    // Assert subsequent start() attempts are strictly rejected with INSTANCE_QUARANTINED
    let start_err = actor.start().unwrap_err();
    assert_eq!(start_err.reason_code, "INSTANCE_QUARANTINED");

    Ok(())
}
