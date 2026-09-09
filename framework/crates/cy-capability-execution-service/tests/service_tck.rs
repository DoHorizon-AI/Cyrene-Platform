// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: framework/crates/cy-capability-execution-service/tests/service_tck.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! Generic Capability Execution Service TCK.
//!
//! These tests use only the generated public gRPC client. They deliberately
//! do not instantiate `CapabilityWorkerClient` or inspect `cy.plugin.v1`.

use std::{collections::HashMap, path::PathBuf, time::Duration};

use cy_manifest::{
    CapabilityDescriptor, CapabilityId, CapabilityInterfaceVersion, Edition, ExecutionMode,
    PluginCapabilitiesManifest, PluginDependencies, PluginManifest, PluginMetadata, RestartPolicy,
    Runtime,
};
use cy_platform_api::{MAX_APPLICATION_EVENT_BUFFER_CAPACITY, WorkerActivationOptions};
use cy_proto::capability_v1::{
    CapabilityEventStreamEnd, CapabilityEventStreamEndReason, CapabilityInvocationStreamItem,
    InvokeCapabilityRequest, SubscribeCapabilityEventsRequest, capability_event_stream_item,
    capability_execution_error,
    capability_execution_service_client::CapabilityExecutionServiceClient,
    capability_invocation_stream_item, invoke_capability_response,
};
use prost_types::Any;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::{
    Request,
    transport::{Channel, Endpoint, Server},
};

use cy_capability_execution_service::{
    CapabilityExecutionService, ConfiguredBinding, build_service, server,
};

fn platform_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn fixture_manifest() -> PluginManifest {
    PluginManifest {
        plugin: PluginMetadata {
            id: "com.cyrene.tck.generic-worker".to_string(),
            name: "Generic TCK Worker".to_string(),
            version: "1.0.0".to_string(),
            api_version: "1.0".to_string(),
            kind: "capability-worker".to_string(),
            edition: Edition::Community,
            runtime: Some(Runtime::SubprocessPython),
            license_gate: false,
            entrypoint: Some("fixtures.test_worker:GenericTckWorker".to_string()),
            description: Some("Platform service TCK worker".to_string()),
            author: None,
            license: None,
            source_target: None,
            status: Some("active".to_string()),
            protocol_version: None,
            scope: None,
            restart_policy: RestartPolicy::Never,
        },
        capabilities: PluginCapabilitiesManifest::default(),
        dependencies: PluginDependencies::default(),
        components: Vec::new(),
        launch: None,
        permissions: None,
        resources: None,
        package: None,
        capability_descriptors: vec![
            CapabilityDescriptor::new(
                CapabilityId::new("test.capability.v1").unwrap(),
                CapabilityInterfaceVersion::new("1").unwrap(),
                vec![ExecutionMode::Worker],
            )
            .unwrap(),
            CapabilityDescriptor::new(
                CapabilityId::new("test.application-events.v1").unwrap(),
                CapabilityInterfaceVersion::new("1").unwrap(),
                vec![ExecutionMode::Worker],
            )
            .unwrap(),
        ],
        artifact: None,
    }
}

fn worker_options() -> WorkerActivationOptions {
    let root = platform_root();
    let python_bin = std::env::var("CYRENE_PYTHON").unwrap_or_else(|_| {
        if cfg!(windows) {
            "python".to_string()
        } else {
            "python3".to_string()
        }
    });
    WorkerActivationOptions {
        working_dir: Some(root.join("framework/crates/cy-platform-api/tests")),
        python_path: vec![
            root.join("sdk/python"),
            root.join("sdk/python/cyrene_worker_shim"),
        ],
        python_executable: Some(python_bin),
        environment: HashMap::new(),
        handshake_timeout: Duration::from_secs(5),
        default_invoke_timeout: Duration::from_secs(5),
        shutdown_grace_period: Duration::from_secs(2),
        max_message_bytes: 1024 * 1024,
    }
}

fn worker_options_for_instance(instance_id: &str) -> WorkerActivationOptions {
    let mut options = worker_options();
    options.environment.insert(
        "CYRENE_TEST_INSTANCE_ID".to_string(),
        instance_id.to_string(),
    );
    options
}

fn two_instance_bindings() -> Vec<(String, WorkerActivationOptions)> {
    vec![
        ("main".to_string(), worker_options_for_instance("main")),
        (
            "secondary".to_string(),
            worker_options_for_instance("secondary"),
        ),
    ]
}

struct TestServer {
    client: CapabilityExecutionServiceClient<Channel>,
    service: CapabilityExecutionService,
    stop: Option<oneshot::Sender<()>>,
    server_task: JoinHandle<Result<(), tonic::transport::Error>>,
}

impl TestServer {
    async fn start(buffer_capacity: usize) -> Self {
        Self::start_with_options(buffer_capacity, worker_options()).await
    }

    async fn start_with_options(
        buffer_capacity: usize,
        worker_activation_options: WorkerActivationOptions,
    ) -> Self {
        Self::start_with_bindings(buffer_capacity, worker_activation_options, Vec::new()).await
    }

    async fn start_with_bindings(
        buffer_capacity: usize,
        worker_activation_options: WorkerActivationOptions,
        bindings: Vec<(String, WorkerActivationOptions)>,
    ) -> Self {
        let manifest = fixture_manifest();
        let configured_bindings = if bindings.is_empty() {
            None
        } else {
            Some(
                bindings
                    .iter()
                    .map(|(id, options)| ConfiguredBinding {
                        id: id.clone(),
                        environment: options.environment.clone(),
                    })
                    .collect(),
            )
        };
        let service = build_service(
            manifest,
            worker_activation_options,
            buffer_capacity,
            configured_bindings,
        )
        .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let incoming = TcpListenerStream::new(listener);
        let (stop, stop_receiver) = oneshot::channel();
        let server_service = service.clone();
        let server_task = tokio::spawn(async move {
            Server::builder()
                .add_service(server(server_service))
                .serve_with_incoming_shutdown(incoming, async move {
                    let _ = stop_receiver.await;
                })
                .await
        });
        let channel = Endpoint::from_shared(format!("http://{address}"))
            .unwrap()
            .connect()
            .await
            .unwrap();
        Self {
            client: CapabilityExecutionServiceClient::new(channel),
            service,
            stop: Some(stop),
            server_task,
        }
    }

    async fn shutdown(mut self) {
        self.service.shutdown().await;
        let _ = self.stop.take().unwrap().send(());
        self.server_task.await.unwrap().unwrap();
        assert_eq!(self.service.running_task_count(), 0);
        assert_eq!(self.service.active_count(), 0);
    }
}

fn any_json(value: serde_json::Value) -> Any {
    Any {
        type_url: "type.cyrene.io/tck.JsonValue".to_string(),
        value: serde_json::to_vec(&value).unwrap(),
    }
}

fn invoke_request(method: &str, payload: serde_json::Value) -> Request<InvokeCapabilityRequest> {
    invoke_request_for_binding(None, method, payload)
}

fn request_type_url_invoke_request() -> Request<InvokeCapabilityRequest> {
    let mut request = Request::new(InvokeCapabilityRequest {
        capability: "test.capability.v1".to_string(),
        interface_version: "1".to_string(),
        method: "request_type_url".to_string(),
        request: Some(Any {
            type_url: "type.googleapis.com/example.Request".to_string(),
            value: b"opaque-request".to_vec(),
        }),
        binding_id: None,
    });
    request.set_timeout(Duration::from_secs(5));
    request
}

fn invoke_request_for_binding(
    binding_id: Option<&str>,
    method: &str,
    payload: serde_json::Value,
) -> Request<InvokeCapabilityRequest> {
    let mut request = invoke_request_without_deadline_for_binding(binding_id, method, payload);
    request.set_timeout(Duration::from_secs(5));
    request
}

fn invoke_request_without_deadline(
    method: &str,
    payload: serde_json::Value,
) -> Request<InvokeCapabilityRequest> {
    invoke_request_without_deadline_for_binding(None, method, payload)
}

fn invoke_request_without_deadline_for_binding(
    binding_id: Option<&str>,
    method: &str,
    payload: serde_json::Value,
) -> Request<InvokeCapabilityRequest> {
    Request::new(InvokeCapabilityRequest {
        capability: "test.capability.v1".to_string(),
        interface_version: "1".to_string(),
        method: method.to_string(),
        request: Some(any_json(payload)),
        binding_id: binding_id.map(str::to_string),
    })
}

fn subscribe_request(mode: &str) -> Request<SubscribeCapabilityEventsRequest> {
    subscribe_request_for_binding(mode, None)
}

fn subscribe_request_for_binding(
    mode: &str,
    binding_id: Option<&str>,
) -> Request<SubscribeCapabilityEventsRequest> {
    let request = SubscribeCapabilityEventsRequest {
        capability: "test.application-events.v1".to_string(),
        interface_version: "1".to_string(),
        filter: Some(any_json(serde_json::json!({ "mode": mode }))),
        binding_id: binding_id.map(str::to_string),
    };
    let mut request = Request::new(request);
    request.set_timeout(Duration::from_secs(5));
    request
}

async fn next_stream_item(
    stream: &mut tonic::Streaming<cy_proto::capability_v1::CapabilityEventStreamItem>,
) -> cy_proto::capability_v1::CapabilityEventStreamItem {
    stream
        .message()
        .await
        .expect("gRPC stream read should succeed")
        .expect("stream should produce an item")
}

async fn next_invocation_stream_item(
    stream: &mut tonic::Streaming<CapabilityInvocationStreamItem>,
) -> CapabilityInvocationStreamItem {
    stream
        .message()
        .await
        .expect("gRPC invocation stream read should succeed")
        .expect("invocation stream should produce an item")
}

fn streaming_invoke_request(timeout: Duration) -> Request<InvokeCapabilityRequest> {
    streaming_invoke_request_for_method("stream", timeout)
}

fn streaming_invoke_request_for_method(
    method: &str,
    timeout: Duration,
) -> Request<InvokeCapabilityRequest> {
    let mut request = Request::new(InvokeCapabilityRequest {
        capability: "test.capability.v1".to_string(),
        interface_version: "1".to_string(),
        method: method.to_string(),
        request: Some(any_json(serde_json::json!({}))),
        binding_id: None,
    });
    request.set_timeout(timeout);
    request
}

fn end_reason(end: &CapabilityEventStreamEnd) -> CapabilityEventStreamEndReason {
    CapabilityEventStreamEndReason::try_from(end.reason).unwrap()
}

async fn wait_until_idle(service: &CapabilityExecutionService) {
    for _ in 0..300 {
        if service.running_task_count() == 0 && service.active_count() == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!(
        "service did not become idle: running_tasks={}, active={}",
        service.running_task_count(),
        service.active_count()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn service_tck_unary_uses_any_and_maps_invalid_request() {
    let mut server = TestServer::start(4).await;
    let response = server
        .client
        .invoke_capability(invoke_request(
            "echo",
            serde_json::json!({ "message": "service" }),
        ))
        .await
        .unwrap()
        .into_inner();
    let Some(invoke_capability_response::Result::Response(result)) = response.result else {
        panic!("expected Any response: {response:?}");
    };
    assert_eq!(
        result.type_url,
        "type.cyrene.io/capability/test.capability.v1/echo/response"
    );
    let result: serde_json::Value = serde_json::from_slice(&result.value).unwrap();
    assert_eq!(result["echo"], "service");

    let invalid = server
        .client
        .invoke_capability(invoke_request(
            "compute",
            serde_json::json!({ "value": -1 }),
        ))
        .await
        .unwrap()
        .into_inner();
    let Some(invoke_capability_response::Result::Error(error)) = invalid.result else {
        panic!("expected generic invalid-request error: {invalid:?}");
    };
    assert_eq!(
        error.code,
        capability_execution_error::Code::InvalidRequest as i32
    );

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn service_tck_forwards_request_any_type_url_to_worker() {
    let mut server = TestServer::start(4).await;
    let response = server
        .client
        .invoke_capability(request_type_url_invoke_request())
        .await
        .unwrap()
        .into_inner();
    let Some(invoke_capability_response::Result::Response(result)) = response.result else {
        panic!("expected request type URL response: {response:?}");
    };
    assert_eq!(
        result.type_url,
        "type.cyrene.io/capability/test.capability.v1/request_type_url/response"
    );
    assert_eq!(result.value, b"type.googleapis.com/example.Request");
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn service_tck_streams_typed_worker_chunks_and_terminal_outcome() {
    let mut server = TestServer::start(2).await;
    let mut stream = server
        .client
        .invoke_capability_stream(streaming_invoke_request(Duration::from_secs(5)))
        .await
        .unwrap()
        .into_inner();

    let first = next_invocation_stream_item(&mut stream).await;
    let Some(capability_invocation_stream_item::Item::Response(payload)) = first.item else {
        panic!("expected first typed response: {first:?}");
    };
    assert_eq!(first.sequence, 1);
    assert_eq!(payload.type_url, "type.googleapis.com/test.Chunk");
    assert_eq!(payload.value, b"first");

    let second = next_invocation_stream_item(&mut stream).await;
    let Some(capability_invocation_stream_item::Item::Response(payload)) = second.item else {
        panic!("expected second typed response: {second:?}");
    };
    assert_eq!(second.sequence, 2);
    assert_eq!(payload.value, b"second");

    let end = next_invocation_stream_item(&mut stream).await;
    let Some(capability_invocation_stream_item::Item::StreamEnd(end)) = end.item else {
        panic!("expected typed stream terminal: {end:?}");
    };
    assert_eq!(end.reason, 1);
    assert_eq!(end.message, "capability invocation stream completed");

    drop(stream);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn service_tck_preserves_empty_typed_worker_chunk() {
    let mut server = TestServer::start(2).await;
    let mut stream = server
        .client
        .invoke_capability_stream(streaming_invoke_request_for_method(
            "stream_empty",
            Duration::from_secs(5),
        ))
        .await
        .unwrap()
        .into_inner();

    let first = next_invocation_stream_item(&mut stream).await;
    let Some(capability_invocation_stream_item::Item::Response(payload)) = first.item else {
        panic!("expected first empty typed response: {first:?}");
    };
    assert_eq!(first.sequence, 1);
    assert_eq!(payload.type_url, "type.googleapis.com/test.Chunk");
    assert!(payload.value.is_empty());

    let second = next_invocation_stream_item(&mut stream).await;
    let Some(capability_invocation_stream_item::Item::Response(payload)) = second.item else {
        panic!("expected second typed response after empty chunk: {second:?}");
    };
    assert_eq!(second.sequence, 2);
    assert_eq!(payload.type_url, "type.googleapis.com/test.Chunk");
    assert_eq!(payload.value, b"after-empty");

    let end = next_invocation_stream_item(&mut stream).await;
    let Some(capability_invocation_stream_item::Item::StreamEnd(end)) = end.item else {
        panic!("expected typed stream terminal after empty chunk: {end:?}");
    };
    assert_eq!(end.reason, 1);

    drop(stream);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn service_tck_slow_invocation_consumer_does_not_block_worker_cleanup() {
    let mut server = TestServer::start(1).await;
    let mut stream = server
        .client
        .invoke_capability_stream(streaming_invoke_request(Duration::from_millis(350)))
        .await
        .unwrap()
        .into_inner();

    let first = next_invocation_stream_item(&mut stream).await;
    assert!(matches!(
        first.item,
        Some(capability_invocation_stream_item::Item::Response(_))
    ));

    // Leave the bounded receiver unread while the worker has more output. The
    // CES send path must observe the request deadline and cancel/clean up.
    tokio::time::sleep(Duration::from_millis(700)).await;
    drop(stream);
    wait_until_idle(&server.service).await;
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn configured_binding_single_instance_keeps_implicit_invoke_compatibility() {
    let mut server = TestServer::start_with_bindings(
        4,
        worker_options(),
        vec![("main".to_string(), worker_options_for_instance("main"))],
    )
    .await;
    let response = server
        .client
        .invoke_capability(invoke_request(
            "echo",
            serde_json::json!({ "message": "main" }),
        ))
        .await
        .unwrap()
        .into_inner();
    let Some(invoke_capability_response::Result::Response(result)) = response.result else {
        panic!("single configured instance should be selected implicitly: {response:?}");
    };
    let result: serde_json::Value = serde_json::from_slice(&result.value).unwrap();
    assert_eq!(result["instance_id"], "main");
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn configured_binding_multiple_instances_reject_implicit_invoke_as_ambiguous() {
    let mut server =
        TestServer::start_with_bindings(4, worker_options(), two_instance_bindings()).await;
    let response = server
        .client
        .invoke_capability(invoke_request(
            "echo",
            serde_json::json!({ "message": "ambiguous" }),
        ))
        .await
        .unwrap()
        .into_inner();
    let Some(invoke_capability_response::Result::Error(error)) = response.result else {
        panic!("implicit invocation must not select an arbitrary configured instance");
    };
    assert_eq!(
        error.code,
        capability_execution_error::Code::InvalidRequest as i32
    );
    assert!(error.message.contains("ambiguous"));
    assert!(error.message.contains("main"));
    assert!(error.message.contains("secondary"));
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn configured_binding_explicit_invoke_targets_each_instance() {
    let mut server =
        TestServer::start_with_bindings(4, worker_options(), two_instance_bindings()).await;
    for instance_id in ["main", "secondary"] {
        let response = server
            .client
            .invoke_capability(invoke_request_for_binding(
                Some(instance_id),
                "echo",
                serde_json::json!({ "message": instance_id }),
            ))
            .await
            .unwrap()
            .into_inner();
        let Some(invoke_capability_response::Result::Response(result)) = response.result else {
            panic!("explicit invocation should target {instance_id}: {response:?}");
        };
        let result: serde_json::Value = serde_json::from_slice(&result.value).unwrap();
        assert_eq!(result["instance_id"], instance_id);
    }
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn configured_binding_subscriptions_are_isolated_and_report_target_identity() {
    let mut server =
        TestServer::start_with_bindings(4, worker_options(), two_instance_bindings()).await;
    for instance_id in ["main", "secondary"] {
        let response = server
            .client
            .subscribe_capability_events(subscribe_request_for_binding("single", Some(instance_id)))
            .await
            .unwrap();
        let mut stream = response.into_inner();
        let Some(capability_event_stream_item::Item::ApplicationEvent(event)) =
            next_stream_item(&mut stream).await.item
        else {
            panic!("expected one event from {instance_id}");
        };
        assert_eq!(event.binding_id, instance_id);
        assert_eq!(
            String::from_utf8(event.payload.unwrap().value).unwrap(),
            format!("{instance_id}:one")
        );
        let Some(capability_event_stream_item::Item::StreamEnd(end)) =
            next_stream_item(&mut stream).await.item
        else {
            panic!("expected terminal event for {instance_id}");
        };
        assert_eq!(end.binding_id, instance_id);
        assert_eq!(end.error, None);
    }
    wait_until_idle(&server.service).await;
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn configured_binding_invoke_and_subscribe_use_the_same_identity() {
    let mut server =
        TestServer::start_with_bindings(4, worker_options(), two_instance_bindings()).await;
    let invoke = server
        .client
        .invoke_capability(invoke_request_for_binding(
            Some("main"),
            "echo",
            serde_json::json!({ "message": "coherent" }),
        ))
        .await
        .unwrap()
        .into_inner();
    let Some(invoke_capability_response::Result::Response(result)) = invoke.result else {
        panic!("explicit invocation should target main: {invoke:?}");
    };
    let result: serde_json::Value = serde_json::from_slice(&result.value).unwrap();
    assert_eq!(result["instance_id"], "main");

    let response = server
        .client
        .subscribe_capability_events(subscribe_request_for_binding("single", Some("main")))
        .await
        .unwrap();
    let mut stream = response.into_inner();
    let Some(capability_event_stream_item::Item::ApplicationEvent(event)) =
        next_stream_item(&mut stream).await.item
    else {
        panic!("expected event from main");
    };
    assert_eq!(event.binding_id, "main");
    assert_eq!(
        String::from_utf8(event.payload.unwrap().value).unwrap(),
        "main:one"
    );
    let _ = next_stream_item(&mut stream).await;
    wait_until_idle(&server.service).await;
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn configured_binding_identity_survives_runtime_generation_change() {
    let mut server =
        TestServer::start_with_bindings(4, worker_options(), two_instance_bindings()).await;
    let response = server
        .client
        .subscribe_capability_events(subscribe_request_for_binding("generation", Some("main")))
        .await
        .unwrap();
    let mut stream = response.into_inner();
    let Some(capability_event_stream_item::Item::StreamEnd(generation_end)) =
        next_stream_item(&mut stream).await.item
    else {
        panic!("expected generation terminal item");
    };
    assert_eq!(generation_end.binding_id, "main");
    assert_eq!(generation_end.generation, 1);
    assert_eq!(
        end_reason(&generation_end),
        CapabilityEventStreamEndReason::GenerationTerminated
    );

    let response = server
        .client
        .subscribe_capability_events(subscribe_request_for_binding("single", Some("main")))
        .await
        .unwrap();
    let mut stream = response.into_inner();
    let Some(capability_event_stream_item::Item::ApplicationEvent(event)) =
        next_stream_item(&mut stream).await.item
    else {
        panic!("expected event after the generation changed");
    };
    assert_eq!(event.binding_id, "main");
    assert_eq!(event.generation, 1);
    let _ = next_stream_item(&mut stream).await;
    wait_until_idle(&server.service).await;
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn configured_binding_unknown_target_is_deterministic() {
    let mut server =
        TestServer::start_with_bindings(4, worker_options(), two_instance_bindings()).await;
    let response = server
        .client
        .invoke_capability(invoke_request_for_binding(
            Some("missing"),
            "echo",
            serde_json::json!({}),
        ))
        .await
        .unwrap()
        .into_inner();
    let Some(invoke_capability_response::Result::Error(error)) = response.result else {
        panic!("unknown configured target must fail");
    };
    assert_eq!(
        error.code,
        capability_execution_error::Code::CapabilityUnavailable as i32
    );
    assert!(
        error
            .message
            .contains("unknown configured capability binding: missing")
    );
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn configured_binding_capability_mismatch_is_deterministic() {
    let mut server =
        TestServer::start_with_bindings(4, worker_options(), two_instance_bindings()).await;
    let mut request = Request::new(InvokeCapabilityRequest {
        capability: "missing.capability.v1".to_string(),
        interface_version: "1".to_string(),
        method: "echo".to_string(),
        request: Some(any_json(serde_json::json!({}))),
        binding_id: Some("main".to_string()),
    });
    request.set_timeout(Duration::from_secs(5));
    let response = server
        .client
        .invoke_capability(request)
        .await
        .unwrap()
        .into_inner();
    let Some(invoke_capability_response::Result::Error(error)) = response.result else {
        panic!("binding capability mismatch must fail");
    };
    assert_eq!(
        error.code,
        capability_execution_error::Code::InvalidRequest as i32
    );
    assert!(
        error
            .message
            .contains("does not expose capability missing.capability.v1")
    );
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn service_tck_streams_single_event_and_normal_end() {
    let mut server = TestServer::start(4).await;
    let response = server
        .client
        .subscribe_capability_events(subscribe_request("single"))
        .await
        .unwrap();
    let mut stream = response.into_inner();
    let Some(capability_event_stream_item::Item::ApplicationEvent(event)) =
        next_stream_item(&mut stream).await.item
    else {
        panic!("expected one application event");
    };
    assert_eq!(event.event_sequence, 1);
    assert_eq!(event.event_type, "synthetic");
    assert_eq!(event.payload.unwrap().value, b"one");
    let Some(capability_event_stream_item::Item::StreamEnd(end)) =
        next_stream_item(&mut stream).await.item
    else {
        panic!("expected normal-completion terminal item");
    };
    assert_eq!(
        end_reason(&end),
        CapabilityEventStreamEndReason::NormalCompletion
    );
    assert_eq!(end.subscription_id, event.subscription_id);
    assert_eq!(end.error, None);
    wait_until_idle(&server.service).await;
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn service_tck_streams_ordered_events_and_normal_end() {
    let mut server = TestServer::start(4).await;
    let response = server
        .client
        .subscribe_capability_events(subscribe_request("ordered"))
        .await
        .unwrap();
    let mut stream = response.into_inner();
    let mut subscription_id = None;
    let mut values = Vec::new();
    loop {
        match next_stream_item(&mut stream).await.item.unwrap() {
            capability_event_stream_item::Item::ApplicationEvent(event) => {
                subscription_id.get_or_insert_with(|| event.subscription_id.clone());
                assert_eq!(
                    subscription_id.as_deref(),
                    Some(event.subscription_id.as_str())
                );
                assert_eq!(event.capability, "test.application-events.v1");
                assert_eq!(event.generation, 1);
                assert!(!event.source_id.is_empty());
                assert_eq!(
                    event.payload.as_ref().unwrap().type_url,
                    "type.cyrene.io/cyrene.capability.v1.OpaquePayload"
                );
                values.push(String::from_utf8(event.payload.unwrap().value).unwrap());
            }
            capability_event_stream_item::Item::StreamEnd(end) => {
                assert_eq!(
                    end_reason(&end),
                    CapabilityEventStreamEndReason::NormalCompletion
                );
                assert_eq!(end.error, None);
                assert_eq!(end.subscription_id, subscription_id.unwrap());
                break;
            }
        }
    }
    assert_eq!(values, ["1", "2", "3"]);
    wait_until_idle(&server.service).await;
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn service_tck_has_bounded_backpressure_and_generation_end() {
    let mut server = TestServer::start(2).await;
    let response = server
        .client
        .subscribe_capability_events(subscribe_request("burst"))
        .await
        .unwrap();
    let mut stream = response.into_inner();
    // Do not read while the synthetic producer fills the bounded stream.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(matches!(
        next_stream_item(&mut stream).await.item,
        Some(capability_event_stream_item::Item::ApplicationEvent(_))
    ));
    assert!(matches!(
        next_stream_item(&mut stream).await.item,
        Some(capability_event_stream_item::Item::ApplicationEvent(_))
    ));
    let backpressure = next_stream_item(&mut stream).await;
    let Some(capability_event_stream_item::Item::StreamEnd(end)) = backpressure.item else {
        panic!("expected bounded backpressure terminal item");
    };
    assert_eq!(
        end_reason(&end),
        CapabilityEventStreamEndReason::Backpressure
    );
    assert_eq!(
        end.error.unwrap().code,
        capability_execution_error::Code::Backpressure as i32
    );
    wait_until_idle(&server.service).await;

    let generation = server
        .client
        .subscribe_capability_events(subscribe_request("generation"))
        .await
        .unwrap();
    let mut generation_stream = generation.into_inner();
    let Some(capability_event_stream_item::Item::StreamEnd(end)) =
        next_stream_item(&mut generation_stream).await.item
    else {
        panic!("expected generation terminal item");
    };
    assert_eq!(
        end_reason(&end),
        CapabilityEventStreamEndReason::GenerationTerminated
    );
    assert_eq!(
        end.error.unwrap().code,
        capability_execution_error::Code::GenerationTerminated as i32
    );
    wait_until_idle(&server.service).await;
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn service_tck_pre_cancelled_call_does_not_require_worker_activation() {
    let mut server = TestServer::start(4).await;
    let mut request = invoke_request("echo", serde_json::json!({ "message": "cancelled" }));
    request.set_timeout(Duration::ZERO);
    let result = server.client.invoke_capability(request).await;
    match result {
        Err(status) => assert!(matches!(
            status.code(),
            tonic::Code::Cancelled | tonic::Code::DeadlineExceeded
        )),
        Ok(response) => {
            let Some(invoke_capability_response::Result::Error(error)) =
                response.into_inner().result
            else {
                panic!("pre-cancelled request unexpectedly invoked the worker");
            };
            assert_eq!(error.code, capability_execution_error::Code::Timeout as i32);
        }
    }
    wait_until_idle(&server.service).await;
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn service_tck_maps_worker_crash_and_native_deadline_cancellation() {
    let mut server = TestServer::start(4).await;
    let crashed = server
        .client
        .invoke_capability(invoke_request("crash", serde_json::json!({})))
        .await
        .unwrap()
        .into_inner();
    let Some(invoke_capability_response::Result::Error(error)) = crashed.result else {
        panic!("expected worker-crash error");
    };
    assert_eq!(
        error.code,
        capability_execution_error::Code::WorkerCrashed as i32
    );
    wait_until_idle(&server.service).await;

    let mut cancellation_client = server.client.clone();
    let mut cancellation_request =
        invoke_request("slow_operation", serde_json::json!({ "delay_ms": 1000 }));
    cancellation_request.set_timeout(Duration::from_secs(10));
    let call = tokio::spawn(async move {
        cancellation_client
            .invoke_capability(cancellation_request)
            .await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    call.abort();
    let _ = call.await;
    wait_until_idle(&server.service).await;

    let mut deadline_request =
        invoke_request("slow_operation", serde_json::json!({ "delay_ms": 1000 }));
    deadline_request.set_timeout(Duration::from_millis(80));
    let deadline_result = server.client.invoke_capability(deadline_request).await;
    assert!(
        deadline_result.is_err(),
        "native gRPC deadline should be transport-visible"
    );
    assert!(matches!(
        deadline_result.unwrap_err().code(),
        tonic::Code::Cancelled | tonic::Code::DeadlineExceeded
    ));
    wait_until_idle(&server.service).await;
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn service_tck_uses_platform_timeout_fallback_without_public_timeout_field() {
    let mut options = worker_options();
    options.default_invoke_timeout = Duration::from_millis(100);
    let mut server = TestServer::start_with_options(4, options).await;
    let response = server
        .client
        .invoke_capability(invoke_request_without_deadline(
            "slow_operation",
            serde_json::json!({ "delay_ms": 1000 }),
        ))
        .await
        .unwrap()
        .into_inner();
    let Some(invoke_capability_response::Result::Error(error)) = response.result else {
        panic!("expected generic timeout error: {response:?}");
    };
    assert_eq!(error.code, capability_execution_error::Code::Timeout as i32);
    wait_until_idle(&server.service).await;
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn service_tck_stream_fallback_timeout_delivers_error_after_worker_cleanup() {
    let mut options = worker_options();
    // The invocation lasts longer than the cleanup window. Starting the
    // terminal-delivery window before invocation would turn Timeout into EOF.
    // 调用时长超过清理窗口；终止事件的发送窗口必须从清理完成后开始。
    options.default_invoke_timeout = Duration::from_millis(800);
    options.shutdown_grace_period = Duration::from_millis(400);
    let mut server = TestServer::start_with_options(4, options).await;
    let mut stream = server
        .client
        .invoke_capability_stream(invoke_request_without_deadline(
            "slow_operation",
            serde_json::json!({ "delay_ms": 1100 }),
        ))
        .await
        .unwrap()
        .into_inner();
    let terminal = tokio::time::timeout(
        Duration::from_secs(5),
        next_invocation_stream_item(&mut stream),
    )
    .await
    .expect("fallback timeout must deliver a bounded terminal outcome");
    let Some(capability_invocation_stream_item::Item::Error(error)) = terminal.item else {
        panic!("expected typed timeout error, got {terminal:?}");
    };
    assert_eq!(terminal.sequence, 1);
    assert_eq!(error.code, capability_execution_error::Code::Timeout as i32);
    assert!(stream.message().await.unwrap().is_none());
    wait_until_idle(&server.service).await;
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn service_tck_stream_cancellation_and_normal_service_shutdown_cleanup() {
    let mut server = TestServer::start(MAX_APPLICATION_EVENT_BUFFER_CAPACITY).await;
    let response = server
        .client
        .subscribe_capability_events(subscribe_request("hold"))
        .await
        .unwrap();
    let stream = response.into_inner();
    drop(stream);
    wait_until_idle(&server.service).await;

    let response = server
        .client
        .subscribe_capability_events(subscribe_request("hold"))
        .await
        .unwrap();
    let mut stream = response.into_inner();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let service = server.service.clone();
    service.shutdown().await;
    let Some(capability_event_stream_item::Item::StreamEnd(end)) =
        next_stream_item(&mut stream).await.item
    else {
        panic!("expected normal service-shutdown terminal item");
    };
    assert_eq!(
        end_reason(&end),
        CapabilityEventStreamEndReason::NormalCompletion
    );
    assert_eq!(end.error, None);
    wait_until_idle(&server.service).await;
    server.shutdown().await;
}
