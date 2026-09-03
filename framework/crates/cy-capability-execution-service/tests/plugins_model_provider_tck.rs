//! Cross-repository acceptance TCK for the official model provider worker.
//!
//! This test deliberately runs the real Platform Capability Execution Service,
//! the Python worker shim, and the Plugins model-api-connector. The fake HTTP
//! upstream holds selected requests open and records the peer's EOF/reset, so
//! a cancelled CES call cannot pass merely by returning a late CANCELLED code.

use std::{
    collections::HashMap,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use cy_capability_execution_service::{
    CapabilityExecutionConfig, CapabilityExecutionService, server,
};
use cy_manifest::PluginManifest;
use cy_platform_api::{CapabilityRegistry, WorkerActivationOptions, normalize_official_manifest};
use cy_proto::{
    capability_v1::{
        InvokeCapabilityRequest,
        capability_execution_service_client::CapabilityExecutionServiceClient,
        invoke_capability_response,
    },
    model_provider::{
        CAPABILITY_ID as MODEL_PROVIDER_CAPABILITY_ID, EMBEDDINGS_METHOD,
        EMBEDDINGS_REQUEST_TYPE_URL, EMBEDDINGS_RESPONSE_TYPE_URL,
    },
    model_provider_v1::{EmbeddingsRequest, EmbeddingsResponse, embeddings_response},
};
use prost::Message;
use prost_types::Any;
use tokio::sync::oneshot;
use tokio::task::JoinHandle as TokioJoinHandle;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::{
    Request,
    transport::{Channel, Endpoint, Server},
};

#[derive(Clone)]
struct UpstreamSignals {
    block_next_embedding: Arc<AtomicBool>,
    block_next_chat: Arc<AtomicBool>,
    embedding_started: Arc<AtomicBool>,
    chat_started: Arc<AtomicBool>,
    embedding_disconnected: Arc<AtomicBool>,
    chat_disconnected: Arc<AtomicBool>,
    responses_sent: Arc<AtomicUsize>,
}

struct FakeUpstream {
    address: String,
    signals: UpstreamSignals,
    stop: Arc<AtomicBool>,
    listener_thread: Option<JoinHandle<()>>,
    handlers: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl FakeUpstream {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let signals = UpstreamSignals {
            block_next_embedding: Arc::new(AtomicBool::new(false)),
            block_next_chat: Arc::new(AtomicBool::new(false)),
            embedding_started: Arc::new(AtomicBool::new(false)),
            chat_started: Arc::new(AtomicBool::new(false)),
            embedding_disconnected: Arc::new(AtomicBool::new(false)),
            chat_disconnected: Arc::new(AtomicBool::new(false)),
            responses_sent: Arc::new(AtomicUsize::new(0)),
        };
        let stop = Arc::new(AtomicBool::new(false));
        let handlers = Arc::new(Mutex::new(Vec::new()));
        let accept_signals = signals.clone();
        let accept_stop = Arc::clone(&stop);
        let accept_handlers = Arc::clone(&handlers);
        let listener_thread = thread::spawn(move || {
            while !accept_stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let signals = accept_signals.clone();
                        let handler =
                            thread::spawn(move || handle_upstream_connection(stream, signals));
                        accept_handlers.lock().unwrap().push(handler);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            address,
            signals,
            stop,
            listener_thread: Some(listener_thread),
            handlers,
        }
    }

    fn url(&self) -> String {
        format!("http://{}", self.address)
    }
}

impl Drop for FakeUpstream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(listener_thread) = self.listener_thread.take() {
            let _ = listener_thread.join();
        }
        if let Ok(mut handlers) = self.handlers.lock() {
            for handler in handlers.drain(..) {
                let _ = handler.join();
            }
        }
    }
}

fn handle_upstream_connection(mut stream: TcpStream, signals: UpstreamSignals) {
    let Some((path, body)) = read_http_request(&mut stream) else {
        return;
    };
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let is_chat = path.ends_with("/chat/completions");
    let should_block = if is_chat {
        signals.chat_started.store(true, Ordering::SeqCst);
        signals.block_next_chat.swap(false, Ordering::SeqCst)
    } else {
        signals.embedding_started.store(true, Ordering::SeqCst);
        signals.block_next_embedding.swap(false, Ordering::SeqCst)
    };
    if should_block {
        let disconnected = if is_chat {
            Arc::clone(&signals.chat_disconnected)
        } else {
            Arc::clone(&signals.embedding_disconnected)
        };
        wait_for_peer_disconnect(&stream, disconnected);
        return;
    }

    let response = if is_chat {
        serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "real CES chat"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 2, "completion_tokens": 2}
        })
    } else {
        let model = value
            .get("model")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("embedding-model");
        serde_json::json!({
            "model": model,
            "data": [{"index": 0, "embedding": [1.0, 2.0, 3.0]}]
        })
    };
    let encoded = serde_json::to_vec(&response).unwrap();
    let headers = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        encoded.len()
    );
    if stream.write_all(headers.as_bytes()).is_ok() && stream.write_all(&encoded).is_ok() {
        signals.responses_sent.fetch_add(1, Ordering::SeqCst);
    }
}

fn read_http_request(stream: &mut TcpStream) -> Option<(String, Vec<u8>)> {
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    let mut bytes = Vec::new();
    let header_end = loop {
        let mut chunk = [0u8; 512];
        let count = stream.read(&mut chunk).ok()?;
        if count == 0 {
            return None;
        }
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
        if bytes.len() > 64 * 1024 {
            return None;
        }
    };
    let header_text = String::from_utf8_lossy(&bytes[..header_end]);
    let path = header_text
        .lines()
        .next()?
        .split_whitespace()
        .nth(1)?
        .to_string();
    let content_length = header_text.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        (name.eq_ignore_ascii_case("content-length"))
            .then(|| value.trim().parse::<usize>().ok())
            .flatten()
    })?;
    while bytes.len() < header_end + content_length {
        let mut chunk = [0u8; 512];
        let count = stream.read(&mut chunk).ok()?;
        if count == 0 {
            return None;
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
    Some((
        path,
        bytes[header_end..header_end + content_length].to_vec(),
    ))
}

fn wait_for_peer_disconnect(stream: &TcpStream, disconnected: Arc<AtomicBool>) {
    stream
        .set_read_timeout(Some(Duration::from_millis(50)))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        let mut probe = [0u8; 1];
        match stream.peek(&mut probe) {
            Ok(0) => {
                disconnected.store(true, Ordering::SeqCst);
                return;
            }
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => {
                disconnected.store(true, Ordering::SeqCst);
                return;
            }
        }
    }
}

struct TestServer {
    client: CapabilityExecutionServiceClient<Channel>,
    service: CapabilityExecutionService,
    stop: Option<oneshot::Sender<()>>,
    server_task: TokioJoinHandle<Result<(), tonic::transport::Error>>,
}

impl TestServer {
    async fn start(manifest: PluginManifest, options: WorkerActivationOptions) -> Self {
        let mut registry = CapabilityRegistry::new();
        registry.register(manifest.clone()).unwrap();
        let config = CapabilityExecutionConfig {
            worker_options: options.clone(),
            ..CapabilityExecutionConfig::default()
        };
        let service = CapabilityExecutionService::new(registry, config)
            .with_provider_worker_options(
                manifest.plugin.id.clone(),
                manifest.plugin.version.clone(),
                options,
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let incoming = TcpListenerStream::new(listener);
        let (stop, receiver) = oneshot::channel();
        let server_service = service.clone();
        let server_task = tokio::spawn(async move {
            Server::builder()
                .add_service(server(server_service))
                .serve_with_incoming_shutdown(incoming, async move {
                    let _ = receiver.await;
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

fn plugins_root() -> Option<PathBuf> {
    if let Some(raw) = std::env::var_os("CYRENE_PLUGINS_WORKTREE") {
        let p = PathBuf::from(raw);
        if p.exists() {
            return Some(p);
        }
    }
    let sibling = platform_root()
        .parent()
        .map(|p| p.join("Cyrene-Plugins-Official"))
        .filter(|p| p.exists());
    if sibling.is_some() {
        return sibling;
    }
    let sibling_alt = platform_root()
        .parent()
        .map(|p| p.join("plugins"))
        .filter(|p| p.exists());
    if sibling_alt.is_some() {
        return sibling_alt;
    }
    None
}

fn provider_manifest(root: &Path) -> Option<PluginManifest> {
    let path = root.join("plugins/providers/model-api-connector/plugin.manifest.json");
    if !path.exists() {
        return None;
    }
    let content = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&content).ok()?;
    normalize_official_manifest(value).ok()
}

fn worker_options(
    root: &Path,
    upstream: &FakeUpstream,
    embeddings_supported: bool,
) -> WorkerActivationOptions {
    let python = std::env::var("CYRENE_PYTHON").unwrap_or_else(|_| "python3".to_string());
    let platform = platform_root();
    let connector = root.join("plugins/providers/model-api-connector");
    let mut environment = HashMap::new();
    environment.insert("CYRENE_CHAT_BASE_URL".to_string(), upstream.url());
    environment.insert(
        "CYRENE_CHAT_MODEL".to_string(),
        "chat-default-model".to_string(),
    );
    environment.insert("CYRENE_CHAT_TIMEOUT".to_string(), "5".to_string());
    environment.insert(
        "CYRENE_EMBEDDING_PROVIDER".to_string(),
        "openai".to_string(),
    );
    environment.insert("CYRENE_EMBEDDING_BASE_URL".to_string(), upstream.url());
    environment.insert(
        "CYRENE_EMBEDDING_MODEL".to_string(),
        "embedding-default-model".to_string(),
    );
    environment.insert(
        "CYRENE_EMBEDDINGS_SUPPORTED".to_string(),
        embeddings_supported.to_string(),
    );
    environment.insert("OPENAI_API_KEY".to_string(), "conformance-only".to_string());
    WorkerActivationOptions {
        working_dir: Some(connector.clone()),
        python_path: vec![
            platform.join("sdk/python"),
            platform.join("sdk/python/cyrene_worker_shim"),
            connector.join("src"),
        ],
        python_executable: Some(python),
        environment,
        handshake_timeout: Duration::from_secs(5),
        default_invoke_timeout: Duration::from_secs(5),
        shutdown_grace_period: Duration::from_secs(2),
        max_message_bytes: 1024 * 1024,
    }
}

fn embedding_request(model: Option<&str>) -> Request<InvokeCapabilityRequest> {
    let payload = EmbeddingsRequest {
        inputs: vec!["deterministic".to_string()],
        model: model.map(str::to_string),
    };
    let mut request = Request::new(InvokeCapabilityRequest {
        capability: MODEL_PROVIDER_CAPABILITY_ID.to_string(),
        interface_version: "1".to_string(),
        method: EMBEDDINGS_METHOD.to_string(),
        request: Some(Any {
            type_url: EMBEDDINGS_REQUEST_TYPE_URL.to_string(),
            value: payload.encode_to_vec(),
        }),
        binding_id: None,
    });
    request.set_timeout(Duration::from_secs(5));
    request
}

fn chat_request(model: Option<&str>) -> Request<InvokeCapabilityRequest> {
    let mut value = serde_json::json!({
        "messages": [{"role": "user", "content": "hello"}],
        "stream": false
    });
    if let Some(model) = model {
        value["model"] = serde_json::Value::String(model.to_string());
    }
    let mut request = Request::new(InvokeCapabilityRequest {
        capability: MODEL_PROVIDER_CAPABILITY_ID.to_string(),
        interface_version: "1".to_string(),
        method: "chat_completion".to_string(),
        request: Some(Any {
            type_url: "type.cyrene.io/tck.JsonValue".to_string(),
            value: serde_json::to_vec(&value).unwrap(),
        }),
        binding_id: None,
    });
    request.set_timeout(Duration::from_secs(5));
    request
}

fn unknown_action_request() -> Request<InvokeCapabilityRequest> {
    let mut request = chat_request(None);
    request.get_mut().method = "unknown_action".to_string();
    request
}

fn assert_embedding_success(
    response: cy_proto::capability_v1::InvokeCapabilityResponse,
    model: &str,
) {
    let Some(invoke_capability_response::Result::Response(payload)) = response.result else {
        panic!("expected typed embedding response: {response:?}");
    };
    assert_eq!(payload.type_url, EMBEDDINGS_RESPONSE_TYPE_URL);
    let decoded = EmbeddingsResponse::decode(payload.value.as_slice()).unwrap();
    let Some(embeddings_response::Result::Embeddings(batch)) = decoded.result else {
        panic!("expected embedding batch: {decoded:?}");
    };
    assert_eq!(batch.model, model);
    assert_eq!(batch.dimensions, 3);
    assert_eq!(batch.vectors[0].values, [1.0, 2.0, 3.0]);
}

async fn wait_for(flag: &AtomicBool) {
    for _ in 0..100 {
        if flag.load(Ordering::SeqCst) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        flag.load(Ordering::SeqCst),
        "upstream did not reach expected state"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_ces_provider_cancellation_chat_and_typed_method_support() {
    let Some(plugins) = plugins_root() else {
        eprintln!(
            "Skipping real_ces_provider_cancellation_chat_and_typed_method_support: CYRENE_PLUGINS_WORKTREE is not set and sibling plugins checkout was not found. This cross-repository TCK requires the Cyrene-Plugins-Official worktree."
        );
        return;
    };
    let Some(manifest) = provider_manifest(&plugins) else {
        eprintln!(
            "Skipping real_ces_provider_cancellation_chat_and_typed_method_support: plugin.manifest.json not found in plugins worktree."
        );
        return;
    };
    let upstream = FakeUpstream::start();
    let mut server =
        TestServer::start(manifest.clone(), worker_options(&plugins, &upstream, true)).await;

    let chat = server
        .client
        .invoke_capability(chat_request(Some("selected-chat-model")))
        .await
        .unwrap()
        .into_inner();
    let Some(invoke_capability_response::Result::Response(chat)) = chat.result else {
        panic!("normal chat request did not return a response");
    };
    let chat_json: serde_json::Value = serde_json::from_slice(&chat.value).unwrap();
    assert_eq!(chat_json["chunks"][0]["delta"], "real CES chat");

    let embedding = server
        .client
        .invoke_capability(embedding_request(None))
        .await
        .unwrap()
        .into_inner();
    assert_embedding_success(embedding, "embedding-default-model");

    // CES keeps unknown operations distinct from a known method that this
    // provider binding does not support: the typed category is
    // ExecutionFailure vs InvalidRequest, and each carries its stable domain
    // discriminator in the message.
    let unknown = server
        .client
        .invoke_capability(unknown_action_request())
        .await
        .unwrap()
        .into_inner();
    let Some(invoke_capability_response::Result::Error(error)) = unknown.result else {
        panic!("unknown provider action must return a typed CES error");
    };
    assert_eq!(
        error.code,
        cy_proto::capability_v1::capability_execution_error::Code::ExecutionFailure as i32
    );
    assert!(error.message.contains("UNKNOWN_OPERATION"));
    assert!(!error.message.contains("METHOD_NOT_SUPPORTED"));

    upstream
        .signals
        .embedding_started
        .store(false, Ordering::SeqCst);
    upstream
        .signals
        .block_next_embedding
        .store(true, Ordering::SeqCst);
    let mut cancelled_embedding = embedding_request(Some("cancelled-embedding-model"));
    cancelled_embedding.set_timeout(Duration::from_millis(400));
    let started = Instant::now();
    let result = server.client.invoke_capability(cancelled_embedding).await;
    let elapsed = started.elapsed();
    assert!(
        result.is_err(),
        "deadline cancellation must be transport-visible"
    );
    assert!(matches!(
        result.unwrap_err().code(),
        tonic::Code::Cancelled | tonic::Code::DeadlineExceeded
    ));
    wait_for(&upstream.signals.embedding_started).await;
    wait_for(&upstream.signals.embedding_disconnected).await;
    assert!(
        elapsed < Duration::from_secs(2),
        "cancel waited for the blocked upstream: {elapsed:?}"
    );

    let next_embedding = server
        .client
        .invoke_capability(embedding_request(Some("next-embedding-model")))
        .await
        .unwrap()
        .into_inner();
    assert_embedding_success(next_embedding, "next-embedding-model");

    upstream.signals.chat_started.store(false, Ordering::SeqCst);
    upstream
        .signals
        .block_next_chat
        .store(true, Ordering::SeqCst);
    let mut cancelled_chat = chat_request(Some("cancelled-chat-model"));
    cancelled_chat.set_timeout(Duration::from_millis(400));
    let result = server.client.invoke_capability(cancelled_chat).await;
    assert!(
        result.is_err(),
        "chat deadline cancellation must be transport-visible"
    );
    assert!(matches!(
        result.unwrap_err().code(),
        tonic::Code::Cancelled | tonic::Code::DeadlineExceeded
    ));
    wait_for(&upstream.signals.chat_started).await;
    wait_for(&upstream.signals.chat_disconnected).await;

    let next_chat = server
        .client
        .invoke_capability(chat_request(Some("next-chat-model")))
        .await
        .unwrap()
        .into_inner();
    let Some(invoke_capability_response::Result::Response(next_chat)) = next_chat.result else {
        panic!("chat worker reuse did not return a response");
    };
    let next_chat_json: serde_json::Value = serde_json::from_slice(&next_chat.value).unwrap();
    assert_eq!(next_chat_json["chunks"][0]["delta"], "real CES chat");
    server.shutdown().await;

    let mut unsupported =
        TestServer::start(manifest, worker_options(&plugins, &upstream, false)).await;
    let response = unsupported
        .client
        .invoke_capability(embedding_request(None))
        .await
        .unwrap()
        .into_inner();
    let Some(invoke_capability_response::Result::Error(error)) = response.result else {
        panic!("unsupported embedding method must return a typed CES error");
    };
    assert_eq!(
        error.code,
        cy_proto::capability_v1::capability_execution_error::Code::InvalidRequest as i32
    );
    assert!(error.message.contains("METHOD_NOT_SUPPORTED"));
    assert!(!error.message.contains("UNKNOWN_OPERATION"));
    assert!(!error.message.contains("EXECUTION_FAILURE"));
    unsupported.shutdown().await;

    assert!(upstream.signals.responses_sent.load(Ordering::SeqCst) >= 4);
}
