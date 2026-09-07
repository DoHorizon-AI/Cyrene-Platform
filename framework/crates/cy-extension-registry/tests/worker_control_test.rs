// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: framework/crates/cy-extension-registry/tests/worker_control_test.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
#![cfg(unix)]

use std::{collections::HashMap, sync::Arc, time::Duration};

use cy_extension_registry::{prepare_instance_actor, WorkerControlClient};
use cy_kernel_daemon::watchdog::InstanceActor;
use cy_plugin_protocol::{
    envelope::Payload,
    pb::{CancelAck, Configure, Envelope, HealthStatus, HelloAck, Invoke, InvokeResult},
    CURRENT_PROTOCOL_VERSION,
};
use tokio::net::UnixListener;
use tokio::time::timeout;

async fn read_frame(
    reader: &mut tokio::net::unix::OwnedReadHalf,
    buffer: &mut bytes::BytesMut,
) -> Envelope {
    cy_extension_registry::transport::read_frame(reader, buffer)
        .await
        .expect("worker frame should decode")
        .expect("worker should keep the control channel open")
}

async fn write_response(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    request: &Envelope,
    sequence_number: u64,
    payload: Payload,
) {
    let response = Envelope {
        request_id: request.request_id.clone(),
        trace_id: request.trace_id.clone(),
        plugin_id: request.plugin_id.clone(),
        protocol_version: CURRENT_PROTOCOL_VERSION,
        deadline_ms: 0,
        sequence_number,
        generation: request.generation,
        fence_token: request.fence_token,
        payload: Some(payload),
    };
    cy_extension_registry::transport::write_frame(writer, &response)
        .await
        .expect("worker response should be written");
}

fn socket_path(test_name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "cyrene-worker-control-{test_name}-{}.sock",
        uuid::Uuid::new_v4()
    ))
}

#[tokio::test]
async fn host_completes_generic_hello_configure_invoke_cancel_channel() {
    let socket = socket_path("lifecycle");
    let listener = UnixListener::bind(&socket).expect("bind worker control socket");
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept worker");
        let (mut reader, mut writer) = stream.into_split();
        let mut buffer = bytes::BytesMut::with_capacity(4096);

        let hello = read_frame(&mut reader, &mut buffer).await;
        assert!(matches!(hello.payload, Some(Payload::Hello(_))));
        assert_eq!(hello.sequence_number, 1);
        assert_eq!(hello.generation, 1);
        assert_eq!(hello.fence_token, 0);
        write_response(
            &mut writer,
            &hello,
            1,
            Payload::HelloAck(HelloAck {
                selected_protocol_version: CURRENT_PROTOCOL_VERSION,
                plugin_id: "test-instance".to_string(),
                plugin_version: "1.0.0".to_string(),
                api_version: "1".to_string(),
                declared_capabilities: vec!["custom".to_string()],
                metrics: HashMap::new(),
                capabilities_json: "{}".to_string(),
            }),
        )
        .await;

        let configure = read_frame(&mut reader, &mut buffer).await;
        match configure.payload.as_ref() {
            Some(Payload::Configure(Configure { settings })) => {
                assert_eq!(
                    settings.get("workload_config_ref"),
                    Some(&"ref-1".to_string())
                );
                assert_eq!(
                    settings.get("worker_visible_path"),
                    Some(&"/mnt/workload".to_string())
                );
            }
            other => panic!("expected Configure, got {other:?}"),
        }
        assert_eq!(configure.sequence_number, 2);
        write_response(
            &mut writer,
            &configure,
            2,
            Payload::HealthStatus(HealthStatus {
                status: 0,
                message: "Configured".to_string(),
            }),
        )
        .await;

        let invoke = read_frame(&mut reader, &mut buffer).await;
        match invoke.payload.as_ref() {
            Some(Payload::Invoke(Invoke {
                extension_point,
                method,
                payload,
                payload_type_url,
                request: None,
                ..
            })) => {
                assert_eq!(extension_point, "custom.capability");
                assert_eq!(method, "run");
                assert_eq!(payload.as_slice(), b"opaque-request");
                assert!(payload_type_url.is_empty());
            }
            other => panic!("expected opaque Invoke, got {other:?}"),
        }
        assert_eq!(invoke.sequence_number, 3);
        write_response(
            &mut writer,
            &invoke,
            3,
            Payload::InvokeResult(InvokeResult {
                payload: b"opaque-result".to_vec(),
                payload_type_url: String::new(),
                response: None,
            }),
        )
        .await;

        let cancel = read_frame(&mut reader, &mut buffer).await;
        match cancel.payload.as_ref() {
            Some(Payload::Cancel(cancel)) => {
                assert_eq!(cancel.target_request_id, "invoke-request");
                assert_eq!(cancel.reason, "cooperative stop");
            }
            other => panic!("expected Cancel, got {other:?}"),
        }
        assert_eq!(cancel.sequence_number, 4);
        write_response(
            &mut writer,
            &cancel,
            4,
            Payload::CancelAck(CancelAck {
                target_request_id: "invoke-request".to_string(),
            }),
        )
        .await;
    });

    let actor = Arc::new(tokio::sync::Mutex::new(
        InstanceActor::new_for_test_with_transport(&socket),
    ));
    let mut actor_guard = prepare_instance_actor("test-instance", &actor)
        .await
        .expect("Hello handshake should attach the worker transport");
    let mut client = WorkerControlClient::new(&mut actor_guard, "test-instance");

    let settings = HashMap::from([
        ("workload_config_ref".to_string(), "ref-1".to_string()),
        (
            "worker_visible_path".to_string(),
            "/mnt/workload".to_string(),
        ),
    ]);
    client
        .configure(settings, Duration::from_secs(1))
        .await
        .expect("Configure should be acknowledged");
    let result = client
        .invoke(
            "custom.capability",
            "run",
            b"opaque-request".to_vec(),
            Duration::from_secs(1),
        )
        .await
        .expect("Invoke should be acknowledged");
    assert_eq!(result.payload, b"opaque-result");
    client
        .cancel("invoke-request", "cooperative stop", Duration::from_secs(1))
        .await
        .expect("Cancel should return a receipt");

    server.await.expect("worker server should complete");
    let _ = std::fs::remove_file(socket);
}

#[tokio::test]
async fn hello_fence_mismatch_fails_closed_before_transport_attach() {
    let socket = socket_path("hello-fence");
    let listener = UnixListener::bind(&socket).expect("bind worker control socket");
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept worker");
        let (mut reader, mut writer) = stream.into_split();
        let mut buffer = bytes::BytesMut::with_capacity(4096);
        let hello = read_frame(&mut reader, &mut buffer).await;
        let response = Envelope {
            request_id: hello.request_id.clone(),
            trace_id: hello.trace_id.clone(),
            plugin_id: hello.plugin_id.clone(),
            protocol_version: CURRENT_PROTOCOL_VERSION,
            deadline_ms: 0,
            sequence_number: 1,
            generation: hello.generation,
            fence_token: hello.fence_token + 1,
            payload: Some(Payload::HelloAck(HelloAck {
                selected_protocol_version: CURRENT_PROTOCOL_VERSION,
                plugin_id: "test-instance".to_string(),
                plugin_version: "1.0.0".to_string(),
                api_version: "1".to_string(),
                declared_capabilities: Vec::new(),
                metrics: HashMap::new(),
                capabilities_json: "{}".to_string(),
            })),
        };
        cy_extension_registry::transport::write_frame(&mut writer, &response)
            .await
            .expect("mismatched HelloAck should be written");
    });

    let actor = Arc::new(tokio::sync::Mutex::new(
        InstanceActor::new_for_test_with_transport(&socket),
    ));
    let error = match prepare_instance_actor("test-instance", &actor).await {
        Ok(_) => panic!("mismatched HelloAck fence must be rejected"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("HelloAck"));
    server.await.expect("worker server should complete");
    let _ = std::fs::remove_file(socket);
}

#[tokio::test]
async fn generic_configure_timeout_is_not_reported_as_success() {
    let socket = socket_path("timeout");
    let listener = UnixListener::bind(&socket).expect("bind worker control socket");
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept worker");
        let (mut reader, mut writer) = stream.into_split();
        let mut buffer = bytes::BytesMut::with_capacity(4096);
        let hello = read_frame(&mut reader, &mut buffer).await;
        write_response(
            &mut writer,
            &hello,
            1,
            Payload::HelloAck(HelloAck {
                selected_protocol_version: CURRENT_PROTOCOL_VERSION,
                plugin_id: "test-instance".to_string(),
                plugin_version: "1.0.0".to_string(),
                api_version: "1".to_string(),
                declared_capabilities: Vec::new(),
                metrics: HashMap::new(),
                capabilities_json: "{}".to_string(),
            }),
        )
        .await;
        let _configure = read_frame(&mut reader, &mut buffer).await;
        let cancel = timeout(Duration::from_secs(1), read_frame(&mut reader, &mut buffer))
            .await
            .expect("actor should send cooperative cancel after timeout");
        assert!(matches!(cancel.payload, Some(Payload::Cancel(_))));
    });

    let actor = Arc::new(tokio::sync::Mutex::new(
        InstanceActor::new_for_test_with_transport(&socket),
    ));
    let mut actor_guard = prepare_instance_actor("test-instance", &actor)
        .await
        .expect("Hello handshake should attach the worker transport");
    let mut client = WorkerControlClient::new(&mut actor_guard, "test-instance");
    let error = client
        .configure(HashMap::new(), Duration::from_millis(30))
        .await
        .expect_err("missing Configure response must fail");
    assert!(matches!(
        error,
        cy_extension_registry::WorkerControlError::Timeout
    ));
    server.await.expect("worker server should complete");
    let _ = std::fs::remove_file(socket);
}

#[tokio::test]
async fn response_sequence_mismatch_fails_closed() {
    let socket = socket_path("sequence");
    let listener = UnixListener::bind(&socket).expect("bind worker control socket");
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept worker");
        let (mut reader, mut writer) = stream.into_split();
        let mut buffer = bytes::BytesMut::with_capacity(4096);
        let hello = read_frame(&mut reader, &mut buffer).await;
        write_response(
            &mut writer,
            &hello,
            1,
            Payload::HelloAck(HelloAck {
                selected_protocol_version: CURRENT_PROTOCOL_VERSION,
                plugin_id: "test-instance".to_string(),
                plugin_version: "1.0.0".to_string(),
                api_version: "1".to_string(),
                declared_capabilities: Vec::new(),
                metrics: HashMap::new(),
                capabilities_json: "{}".to_string(),
            }),
        )
        .await;
        let configure = read_frame(&mut reader, &mut buffer).await;
        write_response(
            &mut writer,
            &configure,
            1,
            Payload::HealthStatus(HealthStatus {
                status: 0,
                message: "invalid sequence".to_string(),
            }),
        )
        .await;
        let _ = timeout(Duration::from_secs(1), read_frame(&mut reader, &mut buffer)).await;
    });

    let actor = Arc::new(tokio::sync::Mutex::new(
        InstanceActor::new_for_test_with_transport(&socket),
    ));
    let mut actor_guard = prepare_instance_actor("test-instance", &actor)
        .await
        .expect("Hello handshake should attach the worker transport");
    let mut client = WorkerControlClient::new(&mut actor_guard, "test-instance");
    let error = client
        .configure(HashMap::new(), Duration::from_millis(60))
        .await
        .expect_err("non-increasing worker sequence must not succeed");
    assert!(matches!(
        error,
        cy_extension_registry::WorkerControlError::Timeout
    ));
    server.await.expect("worker server should complete");
    let _ = std::fs::remove_file(socket);
}
