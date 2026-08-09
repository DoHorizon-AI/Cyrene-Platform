use cy_local_transport::{LocalTransport, StdioTransport};
use cy_plugin_protocol::pb::{envelope, DetectHardwareRequest, Envelope, Hello, Invoke, Shutdown};
use std::path::PathBuf;

#[tokio::test]
async fn test_stdio_transport_zero_port_echo_plugin_integration() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let repo_root = manifest_dir.join("../../..");
    let echo_script = repo_root.join("tests/fixtures/echo_plugin.py");
    let windows_venv = repo_root.join(".venv/Scripts/python.exe");
    let unix_venv = repo_root.join(".venv/bin/python");

    let python_bin = std::env::var("CYRENE_TEST_PYTHON").unwrap_or_else(|_| {
        if windows_venv.exists() {
            windows_venv.to_string_lossy().to_string()
        } else if unix_venv.exists() {
            unix_venv.to_string_lossy().to_string()
        } else {
            "python".to_string()
        }
    });

    let script_path = echo_script.to_string_lossy().to_string();
    println!("Python bin: {}, Script path: {}", python_bin, script_path);
    assert!(
        echo_script.exists(),
        "Echo plugin script does not exist at {}",
        script_path
    );

    // 1. Spawn plugin process over stdio (zero TCP ports)
    let mut transport =
        StdioTransport::spawn(&python_bin, &[&script_path]).expect("Failed to spawn echo plugin");

    // 2. Handshake: Hello -> HelloAck
    let hello_env = Envelope {
        request_id: "req-hello-1".to_string(),
        trace_id: "trace-1".to_string(),
        plugin_id: "".to_string(),
        protocol_version: 1,
        deadline_ms: 5000,
        sequence_number: 0,
        payload: Some(envelope::Payload::Hello(Hello {
            min_protocol_version: 1,
            max_protocol_version: 1,
            host_version: "1.0.0".to_string(),
        })),
    };

    transport
        .send(hello_env)
        .await
        .expect("Failed to send Hello");

    let ack_env = transport
        .receive()
        .await
        .expect("Receive failed")
        .expect("Expected HelloAck");
    println!("Received ack_env: {:?}", ack_env);
    assert_eq!(ack_env.request_id, "req-hello-1");
    assert_eq!(ack_env.plugin_id, "com.cy.probe.nvidia");
    match &ack_env.payload {
        Some(envelope::Payload::HelloAck(ack)) => {
            assert_eq!(ack.api_version, "1.0");
            assert_eq!(ack.declared_capabilities, vec!["nvidia", "cuda"]);
        }
        other => panic!("Expected HelloAck payload, got: {:?}", other),
    }

    // 3. Invoke RPC: DetectHardware
    let invoke_env = Envelope {
        request_id: "req-invoke-2".to_string(),
        trace_id: "trace-1".to_string(),
        plugin_id: "com.cy.probe.nvidia".to_string(),
        protocol_version: 1,
        deadline_ms: 5000,
        sequence_number: 0,
        payload: Some(envelope::Payload::Invoke(Invoke {
            extension_point: "probe".to_string(),
            method: "detect_hardware".to_string(),
            request: Some(cy_plugin_protocol::pb::invoke::Request::DetectHardware(
                DetectHardwareRequest {},
            )),
        })),
    };

    transport
        .send(invoke_env)
        .await
        .expect("Failed to send Invoke");

    let result_env = transport
        .receive()
        .await
        .expect("Receive failed")
        .expect("Expected InvokeResult");
    assert_eq!(result_env.request_id, "req-invoke-2");

    if let Some(envelope::Payload::InvokeResult(res)) = result_env.payload {
        assert!(res.response.is_some());
    } else {
        panic!("Payload was not InvokeResult!");
    }

    // 4. Shutdown & Close
    let shutdown_env = Envelope {
        request_id: "req-shutdown-3".to_string(),
        trace_id: "trace-1".to_string(),
        plugin_id: "com.cy.probe.nvidia".to_string(),
        protocol_version: 1,
        deadline_ms: 1000,
        sequence_number: 0,
        payload: Some(envelope::Payload::Shutdown(Shutdown {
            grace_period_ms: 1000,
        })),
    };

    transport
        .send(shutdown_env)
        .await
        .expect("Failed to send Shutdown");
    transport.close().await.expect("Failed to close transport");
}
