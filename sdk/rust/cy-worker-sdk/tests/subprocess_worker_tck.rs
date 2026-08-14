use std::{
    io::{BufReader, BufWriter},
    path::PathBuf,
    process::{Command, Stdio},
};

use cy_worker_sdk::{
    health_status,
    pb::{
        invoke::Request as InvokeReq, invoke_result::Response as InvokeResp,
        DetectHardwareRequest, Envelope, HealthCheck, Hello, Invoke, Shutdown,
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
        payload: Some(Payload::Invoke(Invoke {
            extension_point: "Probe".to_string(),
            method: "detect_hardware".to_string(),
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
            assert!(ack.declared_capabilities.contains(&"PythonShim".to_string()));
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
        payload: Some(Payload::Invoke(Invoke {
            extension_point: "Probe".to_string(),
            method: "detect_hardware".to_string(),
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
fn test_python_worker_abrupt_crash_detection() -> Result<(), Box<dyn std::error::Error>> {
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

    // Handshake
    let hello_env = Envelope {
        request_id: "req-crash-hello".to_string(),
        trace_id: "tr-crash-1".to_string(),
        plugin_id: "com.cyrene.test.python-echo-worker".to_string(),
        protocol_version: 1,
        deadline_ms: 5000,
        sequence_number: 1,
        payload: Some(Payload::Hello(Hello {
            min_protocol_version: 1,
            max_protocol_version: 1,
            host_version: "1.0.0".to_string(),
        })),
    };
    write_frame(&mut writer, &hello_env, DEFAULT_MAX_MESSAGE_BYTES)?;
    let _ = read_frame(&mut reader, DEFAULT_MAX_MESSAGE_BYTES)?;

    // Abruptly kill the worker child process (simulating SIGKILL / segfault)
    child.kill()?;
    let _ = child.wait();

    // Reading from child stdout must immediately return EOF (None) without blocking or deadlock
    let eof_resp = read_frame(&mut reader, DEFAULT_MAX_MESSAGE_BYTES)?;
    assert!(
        eof_resp.is_none(),
        "Reading from killed child process must immediately return EOF"
    );

    Ok(())
}
