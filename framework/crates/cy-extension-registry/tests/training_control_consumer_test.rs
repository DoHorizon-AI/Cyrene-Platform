// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: framework/crates/cy-extension-registry/tests/training_control_consumer_test.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
#![cfg(unix)]

use std::{collections::HashMap, sync::Arc};

use cy_extension_registry::RemoteTrainingBackend;
use cy_kernel_daemon::watchdog::InstanceActor;
use cy_manifest::{RuntimeManifest, TrainingRevision};
use cy_platform_api::TrainingBackend;
use cy_plugin_protocol::{
    envelope::Payload,
    pb::{
        invoke::Request, Envelope, HealthStatus, HelloAck, Invoke, InvokeResult,
        RunTrainingStepResponse,
    },
    CURRENT_PROTOCOL_VERSION,
};
use tokio::net::UnixListener;

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

fn socket_path() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "cyrene-training-control-{}.sock",
        uuid::Uuid::new_v4()
    ))
}

#[tokio::test]
async fn training_backend_consumes_the_same_generic_configure_invoke_client() {
    let socket = socket_path();
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
                declared_capabilities: vec!["training-backend".to_string()],
                metrics: HashMap::new(),
                capabilities_json: "{}".to_string(),
            }),
        )
        .await;

        let configure = read_frame(&mut reader, &mut buffer).await;
        match configure.payload.as_ref() {
            Some(Payload::Configure(configure)) => {
                assert_eq!(
                    configure.settings.get("workload_config_ref"),
                    Some(&"ref-training".to_string())
                );
            }
            other => panic!("expected Configure, got {other:?}"),
        }
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
                request: Some(Request::RunTrainingStep(_)),
                ..
            })) => {
                assert_eq!(extension_point, "training-backend");
                assert_eq!(method, "run_training_step");
            }
            other => panic!("expected typed Training Invoke over generic channel, got {other:?}"),
        }
        write_response(
            &mut writer,
            &invoke,
            3,
            Payload::InvokeResult(InvokeResult {
                payload: Vec::new(),
                response: Some(cy_plugin_protocol::pb::invoke_result::Response::RunTrainingStep(
                    RunTrainingStepResponse {
                        checkpoint_metadata_json: r#"{"run_id":"run-1","step":1,"digest":"sha256:checkpoint","metrics":{}}"#.to_string(),
                    },
                )),
            }),
        )
        .await;
    });

    let actor = Arc::new(tokio::sync::Mutex::new(
        InstanceActor::new_for_test_with_transport(&socket),
    ));
    let backend = RemoteTrainingBackend::new("test-instance", actor).with_configure_settings(
        HashMap::from([(
            "workload_config_ref".to_string(),
            "ref-training".to_string(),
        )]),
    );
    let runtime: RuntimeManifest = serde_json::from_str(include_str!(
        "../../../../contracts/schemas/examples/runtime_manifest.example.json"
    ))
    .expect("runtime manifest sample should parse");
    let revision: TrainingRevision = serde_json::from_str(include_str!(
        "../../../../contracts/schemas/examples/training_revision.example.json"
    ))
    .expect("training revision sample should parse");
    let checkpoint = backend
        .run_training_step(&runtime, &revision)
        .await
        .expect("training backend should complete Configure then Invoke");
    assert_eq!(checkpoint.run_id, "run-1");
    assert_eq!(checkpoint.step, 1);
    server
        .await
        .expect("training worker server should complete");
    let _ = std::fs::remove_file(socket);
}
