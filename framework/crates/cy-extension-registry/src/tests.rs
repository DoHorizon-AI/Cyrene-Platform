// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: framework/crates/cy-extension-registry/src/tests.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! Unit tests for cy-extension-registry

use std::collections::HashMap;
use std::sync::Arc;
#[cfg(unix)]
use std::time::Duration;

use async_trait::async_trait;
use cy_kernel_daemon::watchdog::InstanceActor;
use cy_manifest::{
    ArtifactKind, ArtifactManifest, CheckpointMetadata, Confidence, CpuInfo, Decision,
    HardwareManifest, HardwareProfile, Interconnect, Lineage, ModelManifest, OsInfo,
    PrecisionSupport, RuntimeManifest, TrainingRevision, TrainingStrategy, ValidationLevel,
    Verdict, VramEstimate, WeightPrecision, WhyReport, Workload, WorkloadRequest,
};
use cy_platform_api::{
    CompatRule, ExecutionEngine, GatewayFilter, ModelAnalyzer, Notification, Plugin,
    PluginCapabilities, PluginError, PluginKind, Probe, Quantization, RuntimeBuilder, Storage,
    TrainingBackend, PLUGIN_API_VERSION,
};
use tokio::sync::Mutex as AsyncMutex;

use crate::*;

// ---------------------------------------------------------------------------
// Mock Plugins for Unit Testing
// ---------------------------------------------------------------------------

struct DummyProbe {
    id: String,
    caps: PluginCapabilities,
}
impl Plugin for DummyProbe {
    fn id(&self) -> &str {
        &self.id
    }
    fn kind(&self) -> PluginKind {
        PluginKind::Probe
    }
    fn api_version(&self) -> &str {
        PLUGIN_API_VERSION
    }
    fn capabilities(&self) -> &PluginCapabilities {
        &self.caps
    }
}
#[async_trait]
impl Probe for DummyProbe {
    async fn detect_hardware(&self) -> Result<HardwareManifest, PluginError> {
        Ok(HardwareManifest {
            os: OsInfo {
                name: "linux".to_string(),
                kernel: "5.15.0".to_string(),
                glibc: "2.35".to_string(),
            },
            cpu: CpuInfo {
                arch: "x86_64".to_string(),
                cores: 8,
                threads: Some(16),
            },
            memory_gb: 32.0,
            disk_gb: 500.0,
            gpus: vec![],
            driver_version: "535.104.05".to_string(),
            cuda_max_supported: "12.2".to_string(),
            interconnect: Interconnect {
                nvlink: false,
                pcie_gen: Some(4),
            },
            precision_support: PrecisionSupport {
                bf16: true,
                fp16: true,
                fp8: false,
            },
        })
    }
}

struct DummyModelAnalyzer {
    id: String,
    caps: PluginCapabilities,
}
impl Plugin for DummyModelAnalyzer {
    fn id(&self) -> &str {
        &self.id
    }
    fn kind(&self) -> PluginKind {
        PluginKind::ModelAnalyzer
    }
    fn api_version(&self) -> &str {
        PLUGIN_API_VERSION
    }
    fn capabilities(&self) -> &PluginCapabilities {
        &self.caps
    }
}
#[async_trait]
impl ModelAnalyzer for DummyModelAnalyzer {
    async fn analyze_model(
        &self,
        _model: &ModelManifest,
        _workload: &WorkloadRequest,
    ) -> Result<VramEstimate, PluginError> {
        Ok(VramEstimate {
            train_gb: 24.0,
            infer_gb: 12.0,
        })
    }
}

struct DummyCompatRule {
    id: String,
    caps: PluginCapabilities,
}
impl Plugin for DummyCompatRule {
    fn id(&self) -> &str {
        &self.id
    }
    fn kind(&self) -> PluginKind {
        PluginKind::CompatRule
    }
    fn api_version(&self) -> &str {
        PLUGIN_API_VERSION
    }
    fn capabilities(&self) -> &PluginCapabilities {
        &self.caps
    }
}
#[async_trait]
impl CompatRule for DummyCompatRule {
    async fn evaluate_compatibility(
        &self,
        _hardware: &HardwareManifest,
        _model: &ModelManifest,
        _workload: &WorkloadRequest,
    ) -> Result<WhyReport, PluginError> {
        Ok(WhyReport {
            decisions: vec![Decision {
                subject: "compat_check".to_string(),
                verdict: Verdict::Chosen,
                rationale: "All checks passed".to_string(),
                evidence: vec![],
                confidence: Confidence::High,
                evidence_level: ValidationLevel::Declared,
            }],
            summary: Some("Compatible".to_string()),
        })
    }
}

struct DummyRuntimeBuilder {
    id: String,
    caps: PluginCapabilities,
}
impl Plugin for DummyRuntimeBuilder {
    fn id(&self) -> &str {
        &self.id
    }
    fn kind(&self) -> PluginKind {
        PluginKind::RuntimeBuilder
    }
    fn api_version(&self) -> &str {
        PLUGIN_API_VERSION
    }
    fn capabilities(&self) -> &PluginCapabilities {
        &self.caps
    }
}
#[async_trait]
impl RuntimeBuilder for DummyRuntimeBuilder {
    async fn build_runtime(
        &self,
        _workload: &WorkloadRequest,
        _hardware: &HardwareManifest,
        _model: &ModelManifest,
    ) -> Result<RuntimeManifest, PluginError> {
        Ok(RuntimeManifest {
            runtime_id: None,
            workload: Workload::Serve,
            hardware_profile: HardwareProfile {
                gpu_model: "test-gpu".to_string(),
                gpu_count: 1,
                vram_gb: 8.0,
                driver_version: "0.0.0".to_string(),
                cuda_max_supported: "12.0".to_string(),
            },
            python: "3.11".to_string(),
            cuda_runtime: "12.0".to_string(),
            torch: "2.0.0".to_string(),
            frameworks: Default::default(),
            precision: WeightPrecision::Fp16,
            training_strategy: TrainingStrategy::None,
            base_image_digest: "sha256:dummy".to_string(),
            validation_level: ValidationLevel::Declared,
        })
    }
}

struct DummyExecutionEngine {
    id: String,
    caps: PluginCapabilities,
}
impl Plugin for DummyExecutionEngine {
    fn id(&self) -> &str {
        &self.id
    }
    fn kind(&self) -> PluginKind {
        PluginKind::ExecutionEngine
    }
    fn api_version(&self) -> &str {
        PLUGIN_API_VERSION
    }
    fn capabilities(&self) -> &PluginCapabilities {
        &self.caps
    }
}
#[async_trait]
impl ExecutionEngine for DummyExecutionEngine {
    async fn execute_inference(
        &self,
        _runtime: &RuntimeManifest,
        _model: &ModelManifest,
        prompt: &str,
    ) -> Result<String, PluginError> {
        Ok(format!("Echo: {}", prompt))
    }
}

struct DummyTrainingBackend {
    id: String,
    caps: PluginCapabilities,
}
impl Plugin for DummyTrainingBackend {
    fn id(&self) -> &str {
        &self.id
    }
    fn kind(&self) -> PluginKind {
        PluginKind::TrainingBackend
    }
    fn api_version(&self) -> &str {
        PLUGIN_API_VERSION
    }
    fn capabilities(&self) -> &PluginCapabilities {
        &self.caps
    }
}
#[async_trait]
impl TrainingBackend for DummyTrainingBackend {
    async fn run_training_step(
        &self,
        _runtime: &RuntimeManifest,
        _revision: &TrainingRevision,
    ) -> Result<CheckpointMetadata, PluginError> {
        Ok(CheckpointMetadata {
            checkpoint_id: Some("ckpt-1".to_string()),
            run_id: "run-1".to_string(),
            step: 100,
            epoch: Some(1),
            path: Some("s3://bucket/ckpt-1".to_string()),
            digest: "sha256:dummy".to_string(),
            metrics: Default::default(),
            revision_id: None,
            size_bytes: Some(1024),
        })
    }
}

struct DummyQuantization {
    id: String,
    caps: PluginCapabilities,
}
impl Plugin for DummyQuantization {
    fn id(&self) -> &str {
        &self.id
    }
    fn kind(&self) -> PluginKind {
        PluginKind::Quantization
    }
    fn api_version(&self) -> &str {
        PLUGIN_API_VERSION
    }
    fn capabilities(&self) -> &PluginCapabilities {
        &self.caps
    }
}
#[async_trait]
impl Quantization for DummyQuantization {
    async fn quantize_model(
        &self,
        _model: &ModelManifest,
        _target_precision: WeightPrecision,
    ) -> Result<ArtifactManifest, PluginError> {
        Ok(ArtifactManifest {
            schema_version: None,
            artifact_id: Some("art-1".to_string()),
            kind: ArtifactKind::Model,
            source: Some("s3://bucket/art-1".to_string()),
            integrity: "sha256:dummy-sha".to_string(),
            size_bytes: Some(1024 * 1024),
            lineage: Lineage {
                base_model_revision: None,
                dataset_digest: None,
                runtime_id: None,
                revision_chain: vec![],
                checkpoint_id: None,
            },
            ref_count: Some(1),
        })
    }
}

struct DummyGatewayFilter {
    id: String,
    caps: PluginCapabilities,
}
impl Plugin for DummyGatewayFilter {
    fn id(&self) -> &str {
        &self.id
    }
    fn kind(&self) -> PluginKind {
        PluginKind::GatewayFilter
    }
    fn api_version(&self) -> &str {
        PLUGIN_API_VERSION
    }
    fn capabilities(&self) -> &PluginCapabilities {
        &self.caps
    }
}
#[async_trait]
impl GatewayFilter for DummyGatewayFilter {
    async fn filter_request(
        &self,
        _headers: &HashMap<String, String>,
        _body: &str,
    ) -> Result<bool, PluginError> {
        Ok(true)
    }
}

struct DummyNotification {
    id: String,
    caps: PluginCapabilities,
}
impl Plugin for DummyNotification {
    fn id(&self) -> &str {
        &self.id
    }
    fn kind(&self) -> PluginKind {
        PluginKind::Notification
    }
    fn api_version(&self) -> &str {
        PLUGIN_API_VERSION
    }
    fn capabilities(&self) -> &PluginCapabilities {
        &self.caps
    }
}
#[async_trait]
impl Notification for DummyNotification {
    async fn send_notification(
        &self,
        _topic: &str,
        _message: &str,
        _level: &str,
    ) -> Result<(), PluginError> {
        Ok(())
    }
}

struct DummyStorage {
    id: String,
    caps: PluginCapabilities,
}
impl Plugin for DummyStorage {
    fn id(&self) -> &str {
        &self.id
    }
    fn kind(&self) -> PluginKind {
        PluginKind::Storage
    }
    fn api_version(&self) -> &str {
        PLUGIN_API_VERSION
    }
    fn capabilities(&self) -> &PluginCapabilities {
        &self.caps
    }
}
#[async_trait]
impl Storage for DummyStorage {
    async fn store_artifact(
        &self,
        _artifact: &ArtifactManifest,
        _data: &[u8],
    ) -> Result<String, PluginError> {
        Ok("stored-id".to_string())
    }

    async fn fetch_artifact(&self, _artifact_id: &str) -> Result<Vec<u8>, PluginError> {
        Ok(vec![1, 2, 3, 4])
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn test_extension_registry_empty() {
    let registry = ExtensionRegistry::new();
    assert!(registry.list_probes().is_empty());
    assert!(registry.list_model_analyzers().is_empty());
    assert!(registry.list_compat_rules().is_empty());
    assert!(registry.list_runtime_builders().is_empty());
    assert!(registry.list_execution_engines().is_empty());
    assert!(registry.list_training_backends().is_empty());
    assert!(registry.list_quantizations().is_empty());
    assert!(registry.list_gateway_filters().is_empty());
    assert!(registry.list_notifications().is_empty());
    assert!(registry.list_storages().is_empty());
    assert!(registry.list_plugins().is_empty());

    assert!(registry.get_probe("none").is_none());
    assert!(registry.get_model_analyzer("none").is_none());
    assert!(registry.get_compat_rule("none").is_none());
    assert!(registry.get_runtime_builder("none").is_none());
    assert!(registry.get_execution_engine("none").is_none());
    assert!(registry.get_training_backend("none").is_none());
    assert!(registry.get_quantization("none").is_none());
    assert!(registry.get_gateway_filter("none").is_none());
    assert!(registry.get_notification("none").is_none());
    assert!(registry.get_storage("none").is_none());
}

#[tokio::test]
async fn test_extension_registry_all_extension_points() {
    let mut registry = ExtensionRegistry::new();

    // 1. Probe
    let probe = Arc::new(DummyProbe {
        id: "probe-1".to_string(),
        caps: Default::default(),
    });
    registry.register_probe(probe);
    assert_eq!(registry.list_probes().len(), 1);
    let fetched = registry.get_probe("probe-1").unwrap();
    let hw = fetched.detect_hardware().await.unwrap();
    assert_eq!(hw.cpu.cores, 8);

    // 2. Model Analyzer
    let analyzer = Arc::new(DummyModelAnalyzer {
        id: "analyzer-1".to_string(),
        caps: Default::default(),
    });
    registry.register_model_analyzer(analyzer);
    assert_eq!(registry.list_model_analyzers().len(), 1);
    assert!(registry.get_model_analyzer("analyzer-1").is_some());

    // 3. Compat Rule
    let compat = Arc::new(DummyCompatRule {
        id: "compat-1".to_string(),
        caps: Default::default(),
    });
    registry.register_compat_rule(compat);
    assert_eq!(registry.list_compat_rules().len(), 1);
    assert!(registry.get_compat_rule("compat-1").is_some());

    // 4. Runtime Builder
    let builder = Arc::new(DummyRuntimeBuilder {
        id: "builder-1".to_string(),
        caps: Default::default(),
    });
    registry.register_runtime_builder(builder);
    assert_eq!(registry.list_runtime_builders().len(), 1);
    assert!(registry.get_runtime_builder("builder-1").is_some());

    // 5. Execution Engine
    let engine = Arc::new(DummyExecutionEngine {
        id: "engine-1".to_string(),
        caps: Default::default(),
    });
    registry.register_execution_engine(engine);
    assert_eq!(registry.list_execution_engines().len(), 1);
    assert!(registry.get_execution_engine("engine-1").is_some());

    // 6. Training Backend
    let backend = Arc::new(DummyTrainingBackend {
        id: "backend-1".to_string(),
        caps: Default::default(),
    });
    registry.register_training_backend(backend);
    assert_eq!(registry.list_training_backends().len(), 1);
    assert!(registry.get_training_backend("backend-1").is_some());

    // 7. Quantization
    let quant = Arc::new(DummyQuantization {
        id: "quant-1".to_string(),
        caps: Default::default(),
    });
    registry.register_quantization(quant);
    assert_eq!(registry.list_quantizations().len(), 1);
    assert!(registry.get_quantization("quant-1").is_some());

    // 8. Gateway Filter
    let filter = Arc::new(DummyGatewayFilter {
        id: "filter-1".to_string(),
        caps: Default::default(),
    });
    registry.register_gateway_filter(filter);
    assert_eq!(registry.list_gateway_filters().len(), 1);
    assert!(registry.get_gateway_filter("filter-1").is_some());

    // 9. Notification
    let notif = Arc::new(DummyNotification {
        id: "notif-1".to_string(),
        caps: Default::default(),
    });
    registry.register_notification(notif);
    assert_eq!(registry.list_notifications().len(), 1);
    assert!(registry.get_notification("notif-1").is_some());

    // 10. Storage
    let storage = Arc::new(DummyStorage {
        id: "storage-1".to_string(),
        caps: Default::default(),
    });
    registry.register_storage(storage);
    assert_eq!(registry.list_storages().len(), 1);
    assert!(registry.get_storage("storage-1").is_some());

    // Check all plugins list
    assert_eq!(registry.list_plugins().len(), 10);

    // Register generic plugin
    let generic_probe = Arc::new(DummyProbe {
        id: "generic-probe".to_string(),
        caps: Default::default(),
    });
    registry.register_plugin(generic_probe);
    assert_eq!(registry.list_plugins().len(), 11);
}

#[test]
fn test_generic_remote_plugin_kind_parsing() {
    // InstanceActor requires a real SandboxedProcess; we test kind-parsing
    // without touching the actor's I/O by using the type-parse branch only.
    // (actor() accessor is available but we don't call any async methods here)
    fn make_actor() -> Arc<AsyncMutex<InstanceActor>> {
        Arc::new(AsyncMutex::new(InstanceActor::new_for_test()))
    }

    let bundle = GenericRemotePlugin::new("b1", "bundle", make_actor());
    assert_eq!(bundle.kind(), PluginKind::Bundle);
    assert_eq!(bundle.id(), "b1");
    assert_eq!(bundle.api_version(), PLUGIN_API_VERSION);
    assert!(bundle.actor().try_lock().is_ok());

    let service = GenericRemotePlugin::new("s1", "service", make_actor());
    assert_eq!(service.kind(), PluginKind::Service);

    let library = GenericRemotePlugin::new("l1", "library", make_actor());
    assert_eq!(library.kind(), PluginKind::Library);

    let py_pkg = GenericRemotePlugin::new("p1", "python-package", make_actor());
    assert_eq!(py_pkg.kind(), PluginKind::PythonPackage);

    let proto_srv = GenericRemotePlugin::new("ps1", "protocol-and-services", make_actor());
    assert_eq!(proto_srv.kind(), PluginKind::ProtocolAndServices);

    let deploy = GenericRemotePlugin::new("d1", "deployment-assets", make_actor());
    assert_eq!(deploy.kind(), PluginKind::DeploymentAssets);

    let fallback = GenericRemotePlugin::new("f1", "unknown-kind", make_actor());
    assert_eq!(fallback.kind(), PluginKind::Probe);
}

#[test]
fn test_proxy_type_aliases_compatibility() {
    fn assert_same_type<T1, T2>() {}

    assert_same_type::<RemoteProbe, ProbeProxy>();
    assert_same_type::<RemoteModelAnalyzer, ModelAnalyzerProxy>();
    assert_same_type::<RemoteCompatRule, CompatRuleProxy>();
    assert_same_type::<RemoteRuntimeBuilder, RuntimeBuilderProxy>();
    assert_same_type::<RemoteExecutionEngine, ExecutionEngineProxy>();
    assert_same_type::<RemoteTrainingBackend, TrainingBackendProxy>();
    assert_same_type::<RemoteQuantization, QuantizationProxy>();
    assert_same_type::<RemoteGatewayFilter, GatewayFilterProxy>();
    assert_same_type::<RemoteNotification, NotificationProxy>();
    assert_same_type::<RemoteStorage, StorageProxy>();
    assert_same_type::<GenericRemotePlugin, GenericPluginProxy>();
}

#[cfg(unix)]
#[tokio::test]
async fn remote_notification_uses_sandbox_worker_socket_and_correlates_responses() {
    use cy_plugin_protocol::{
        envelope::Payload,
        pb::{invoke_result, Envelope, HelloAck, InvokeResult, SendNotificationResponse},
        CURRENT_PROTOCOL_VERSION,
    };
    use tokio::net::UnixListener;

    let socket = std::env::temp_dir().join(format!("cyrene-ext-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket).expect("bind worker transport socket");
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept worker transport");
        let (mut reader, mut writer) = stream.into_split();
        let mut buffer = bytes::BytesMut::with_capacity(4096);
        let hello = crate::transport::read_frame(&mut reader, &mut buffer)
            .await
            .expect("read Hello")
            .expect("Hello frame");
        assert!(matches!(hello.payload, Some(Payload::Hello(_))));
        let hello_ack = Envelope {
            request_id: hello.request_id.clone(),
            trace_id: hello.trace_id.clone(),
            plugin_id: hello.plugin_id.clone(),
            protocol_version: CURRENT_PROTOCOL_VERSION,
            deadline_ms: 0,
            sequence_number: 1,
            generation: hello.generation,
            fence_token: hello.fence_token,
            payload: Some(Payload::HelloAck(HelloAck {
                selected_protocol_version: CURRENT_PROTOCOL_VERSION,
                plugin_id: hello.plugin_id.clone(),
                plugin_version: "1.0.0".to_string(),
                api_version: "1".to_string(),
                declared_capabilities: Vec::new(),
                metrics: HashMap::new(),
                capabilities_json: "{}".to_string(),
            })),
        };
        crate::transport::write_frame(&mut writer, &hello_ack)
            .await
            .expect("write HelloAck");
        let request = crate::transport::read_frame(&mut reader, &mut buffer)
            .await
            .expect("read invoke")
            .expect("invoke frame");
        assert_eq!(request.plugin_id, "test-instance");
        assert_eq!(request.generation, 1);
        assert_eq!(request.fence_token, 0);
        let wrong = Envelope {
            request_id: "not-the-request".to_string(),
            trace_id: request.trace_id.clone(),
            plugin_id: request.plugin_id.clone(),
            protocol_version: CURRENT_PROTOCOL_VERSION,
            deadline_ms: 0,
            sequence_number: 2,
            generation: request.generation,
            fence_token: request.fence_token,
            payload: Some(Payload::InvokeResult(InvokeResult {
                payload: Vec::new(),
                payload_type_url: String::new(),
                response: Some(invoke_result::Response::SendNotification(
                    SendNotificationResponse { success: true },
                )),
            })),
        };
        crate::transport::write_frame(&mut writer, &wrong)
            .await
            .expect("write unmatched response");
        let response = Envelope {
            request_id: request.request_id,
            trace_id: request.trace_id,
            plugin_id: request.plugin_id,
            protocol_version: CURRENT_PROTOCOL_VERSION,
            deadline_ms: 0,
            sequence_number: 3,
            generation: request.generation,
            fence_token: 0,
            payload: Some(Payload::InvokeResult(InvokeResult {
                payload: Vec::new(),
                payload_type_url: String::new(),
                response: Some(invoke_result::Response::SendNotification(
                    SendNotificationResponse { success: true },
                )),
            })),
        };
        crate::transport::write_frame(&mut writer, &response)
            .await
            .expect("write correlated response");
    });

    let actor = Arc::new(AsyncMutex::new(InstanceActor::new_for_test_with_transport(
        &socket,
    )));
    let notification = RemoteNotification::new("test-instance", actor);
    notification
        .send_notification("deploy", "ok", "info")
        .await
        .expect("remote notification should complete over worker socket");
    server.await.expect("worker transport server task");
    let _ = std::fs::remove_file(socket);
}

#[cfg(unix)]
#[tokio::test]
async fn invoke_timeout_sends_protocol_cancel_before_actor_fails_closed() {
    use cy_plugin_protocol::envelope::Payload;
    use cy_plugin_protocol::pb::{Envelope, Invoke};
    use cy_plugin_protocol::CURRENT_PROTOCOL_VERSION;
    use tokio::net::UnixListener;

    let socket = std::env::temp_dir().join(format!(
        "cyrene-extension-timeout-{}.sock",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket).expect("bind worker transport socket");
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept worker transport");
        let (mut reader, mut writer) = stream.into_split();
        let mut buffer = bytes::BytesMut::with_capacity(4096);
        let hello = crate::transport::read_frame(&mut reader, &mut buffer)
            .await
            .expect("read Hello")
            .expect("Hello frame");
        let hello_ack = Envelope {
            request_id: hello.request_id.clone(),
            trace_id: hello.trace_id.clone(),
            plugin_id: hello.plugin_id.clone(),
            protocol_version: CURRENT_PROTOCOL_VERSION,
            deadline_ms: 0,
            sequence_number: 1,
            generation: hello.generation,
            fence_token: hello.fence_token,
            payload: Some(Payload::HelloAck(cy_plugin_protocol::pb::HelloAck {
                selected_protocol_version: CURRENT_PROTOCOL_VERSION,
                plugin_id: hello.plugin_id.clone(),
                plugin_version: "1.0.0".to_string(),
                api_version: "1".to_string(),
                declared_capabilities: Vec::new(),
                metrics: HashMap::new(),
                capabilities_json: "{}".to_string(),
            })),
        };
        crate::transport::write_frame(&mut writer, &hello_ack)
            .await
            .expect("write HelloAck");
        let invoke = crate::transport::read_frame(&mut reader, &mut buffer)
            .await
            .expect("read invoke")
            .expect("invoke frame");
        assert!(matches!(invoke.payload, Some(Payload::Invoke(_))));
        let cancel = crate::transport::read_frame(&mut reader, &mut buffer)
            .await
            .expect("read cancellation")
            .expect("cancel frame");
        match cancel.payload {
            Some(Payload::Cancel(cancel)) => {
                assert_eq!(cancel.target_request_id, invoke.request_id)
            }
            other => panic!("expected protocol cancellation, got {other:?}"),
        }
    });

    let actor = Arc::new(AsyncMutex::new(InstanceActor::new_for_test_with_transport(
        &socket,
    )));
    let mut actor = crate::helper::prepare_instance_actor("test-instance", &actor)
        .await
        .expect("attach transport");
    let mut client = WorkerControlClient::new(&mut actor, "test-instance");
    let error = client
        .invoke_message(
            Invoke {
                extension_point: "test".to_string(),
                method: "timeout".to_string(),
                payload: Vec::new(),
                request: None,
            },
            Duration::from_millis(25),
        )
        .await
        .expect_err("invoke must time out");
    assert!(matches!(error, WorkerControlError::Timeout));
    server.await.expect("worker cancellation server task");
    let _ = std::fs::remove_file(socket);
}
