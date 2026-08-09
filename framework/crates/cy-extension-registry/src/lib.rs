use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cy_manifest::{
    ArtifactManifest, CheckpointMetadata, HardwareManifest, ModelManifest, RuntimeManifest,
    TrainingRevision, VramEstimate, WeightPrecision, WhyReport, WorkloadRequest,
};

use cy_platform_api::{
    CompatRule, ExecutionEngine, GatewayFilter, ModelAnalyzer, Notification, Plugin,
    PluginCapabilities, PluginError, PluginKind, Probe, Quantization, RuntimeBuilder, Storage,
    TrainingBackend, PLUGIN_API_VERSION,
};

use cy_plugin_protocol::pb::{
    invoke_result, AnalyzeModelRequest, BuildRuntimeRequest, DetectHardwareRequest,
    EvaluateCompatRequest, ExecuteInferenceRequest, FetchArtifactRequest, FilterRequest, Invoke,
    QuantizeModelRequest, RunTrainingStepRequest, SendNotificationRequest, StoreArtifactRequest,
};
use cy_plugin_supervisor::{PluginRuntimeState, PluginSupervisor};
use tokio::sync::Mutex as AsyncMutex;

// ---------------------------------------------------------------------------
// Helper macro/function for ensuring supervisor readiness before invoke
// ---------------------------------------------------------------------------

async fn prepare_supervisor<'a>(
    plugin_id: &str,
    sup_arc: &'a Arc<AsyncMutex<PluginSupervisor>>,
) -> Result<tokio::sync::MutexGuard<'a, PluginSupervisor>, PluginError> {
    let mut sup = sup_arc.lock().await;
    if let Err(e) = sup.ensure_healthy(Duration::from_secs(5)).await {
        return Err(PluginError::Unavailable(format!(
            "Failed to ensure plugin {} is healthy: {}",
            plugin_id, e
        )));
    }
    if sup.state() != PluginRuntimeState::Healthy {
        return Err(PluginError::Unavailable(format!(
            "Plugin {} is not healthy (state={:?})",
            plugin_id,
            sup.state()
        )));
    }
    Ok(sup)
}

// ---------------------------------------------------------------------------
// Typed Remote Proxies for All 10 Extension Points
// ---------------------------------------------------------------------------

/// 1. Remote Probe Proxy
pub struct RemoteProbe {
    plugin_id: String,
    supervisor: Arc<AsyncMutex<PluginSupervisor>>,
    capabilities: PluginCapabilities,
}

impl RemoteProbe {
    pub fn new(
        plugin_id: impl Into<String>,
        supervisor: Arc<AsyncMutex<PluginSupervisor>>,
    ) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            supervisor,
            capabilities: Default::default(),
        }
    }
    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }
}

impl Plugin for RemoteProbe {
    fn id(&self) -> &str {
        &self.plugin_id
    }
    fn kind(&self) -> PluginKind {
        PluginKind::Probe
    }
    fn api_version(&self) -> &str {
        PLUGIN_API_VERSION
    }
    fn capabilities(&self) -> &PluginCapabilities {
        &self.capabilities
    }
}

#[async_trait]
impl Probe for RemoteProbe {
    async fn detect_hardware(&self) -> Result<HardwareManifest, PluginError> {
        let mut sup = prepare_supervisor(&self.plugin_id, &self.supervisor).await?;
        let invoke_req = Invoke {
            extension_point: "probe".to_string(),
            method: "detect_hardware".to_string(),
            request: Some(cy_plugin_protocol::pb::invoke::Request::DetectHardware(
                DetectHardwareRequest {},
            )),
        };
        let res = sup
            .invoke(invoke_req, Duration::from_secs(10))
            .await
            .map_err(|e| PluginError::Execution(e.to_string()))?;
        if let Some(invoke_result::Response::DetectHardware(resp)) = res.response {
            return serde_json::from_str(&resp.hardware_manifest_json)
                .map_err(|e| PluginError::Execution(e.to_string()));
        }
        Err(PluginError::Execution(
            "Invalid detect_hardware response payload".to_string(),
        ))
    }
}

/// 2. Remote Model Analyzer Proxy
pub struct RemoteModelAnalyzer {
    plugin_id: String,
    supervisor: Arc<AsyncMutex<PluginSupervisor>>,
    capabilities: PluginCapabilities,
}

impl RemoteModelAnalyzer {
    pub fn new(
        plugin_id: impl Into<String>,
        supervisor: Arc<AsyncMutex<PluginSupervisor>>,
    ) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            supervisor,
            capabilities: Default::default(),
        }
    }
}

impl Plugin for RemoteModelAnalyzer {
    fn id(&self) -> &str {
        &self.plugin_id
    }
    fn kind(&self) -> PluginKind {
        PluginKind::ModelAnalyzer
    }
    fn api_version(&self) -> &str {
        PLUGIN_API_VERSION
    }
    fn capabilities(&self) -> &PluginCapabilities {
        &self.capabilities
    }
}

#[async_trait]
impl ModelAnalyzer for RemoteModelAnalyzer {
    async fn analyze_model(
        &self,
        model: &ModelManifest,
        workload: &WorkloadRequest,
    ) -> Result<VramEstimate, PluginError> {
        let mut sup = prepare_supervisor(&self.plugin_id, &self.supervisor).await?;
        let model_json =
            serde_json::to_string(model).map_err(|e| PluginError::Execution(e.to_string()))?;
        let workload_json =
            serde_json::to_string(workload).map_err(|e| PluginError::Execution(e.to_string()))?;

        let invoke_req = Invoke {
            extension_point: "model-analyzer".to_string(),
            method: "analyze_model".to_string(),
            request: Some(cy_plugin_protocol::pb::invoke::Request::AnalyzeModel(
                AnalyzeModelRequest {
                    model_manifest_json: model_json,
                    workload_request_json: workload_json,
                },
            )),
        };
        let res = sup
            .invoke(invoke_req, Duration::from_secs(15))
            .await
            .map_err(|e| PluginError::Execution(e.to_string()))?;
        if let Some(invoke_result::Response::AnalyzeModel(resp)) = res.response {
            return Ok(VramEstimate {
                train_gb: resp.train_gb,
                infer_gb: resp.infer_gb,
            });
        }
        Err(PluginError::Execution(
            "Invalid analyze_model response payload".to_string(),
        ))
    }
}

/// 3. Remote Compat Rule Proxy
pub struct RemoteCompatRule {
    plugin_id: String,
    supervisor: Arc<AsyncMutex<PluginSupervisor>>,
    capabilities: PluginCapabilities,
}

impl RemoteCompatRule {
    pub fn new(
        plugin_id: impl Into<String>,
        supervisor: Arc<AsyncMutex<PluginSupervisor>>,
    ) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            supervisor,
            capabilities: Default::default(),
        }
    }
}

impl Plugin for RemoteCompatRule {
    fn id(&self) -> &str {
        &self.plugin_id
    }
    fn kind(&self) -> PluginKind {
        PluginKind::CompatRule
    }
    fn api_version(&self) -> &str {
        PLUGIN_API_VERSION
    }
    fn capabilities(&self) -> &PluginCapabilities {
        &self.capabilities
    }
}

#[async_trait]
impl CompatRule for RemoteCompatRule {
    async fn evaluate_compatibility(
        &self,
        hardware: &HardwareManifest,
        model: &ModelManifest,
        workload: &WorkloadRequest,
    ) -> Result<WhyReport, PluginError> {
        let mut sup = prepare_supervisor(&self.plugin_id, &self.supervisor).await?;
        let hw_json =
            serde_json::to_string(hardware).map_err(|e| PluginError::Execution(e.to_string()))?;
        let model_json =
            serde_json::to_string(model).map_err(|e| PluginError::Execution(e.to_string()))?;
        let workload_json =
            serde_json::to_string(workload).map_err(|e| PluginError::Execution(e.to_string()))?;

        let invoke_req = Invoke {
            extension_point: "compat-rule".to_string(),
            method: "evaluate_compatibility".to_string(),
            request: Some(cy_plugin_protocol::pb::invoke::Request::EvaluateCompat(
                EvaluateCompatRequest {
                    hardware_manifest_json: hw_json,
                    model_manifest_json: model_json,
                    workload_request_json: workload_json,
                },
            )),
        };
        let res = sup
            .invoke(invoke_req, Duration::from_secs(15))
            .await
            .map_err(|e| PluginError::Execution(e.to_string()))?;
        if let Some(invoke_result::Response::EvaluateCompat(resp)) = res.response {
            return serde_json::from_str(&resp.why_report_json)
                .map_err(|e| PluginError::Execution(e.to_string()));
        }
        Err(PluginError::Execution(
            "Invalid evaluate_compatibility response payload".to_string(),
        ))
    }
}

/// 4. Remote Runtime Builder Proxy
pub struct RemoteRuntimeBuilder {
    plugin_id: String,
    supervisor: Arc<AsyncMutex<PluginSupervisor>>,
    capabilities: PluginCapabilities,
}

impl RemoteRuntimeBuilder {
    pub fn new(
        plugin_id: impl Into<String>,
        supervisor: Arc<AsyncMutex<PluginSupervisor>>,
    ) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            supervisor,
            capabilities: Default::default(),
        }
    }
}

impl Plugin for RemoteRuntimeBuilder {
    fn id(&self) -> &str {
        &self.plugin_id
    }
    fn kind(&self) -> PluginKind {
        PluginKind::RuntimeBuilder
    }
    fn api_version(&self) -> &str {
        PLUGIN_API_VERSION
    }
    fn capabilities(&self) -> &PluginCapabilities {
        &self.capabilities
    }
}

#[async_trait]
impl RuntimeBuilder for RemoteRuntimeBuilder {
    async fn build_runtime(
        &self,
        workload: &WorkloadRequest,
        hardware: &HardwareManifest,
        model: &ModelManifest,
    ) -> Result<RuntimeManifest, PluginError> {
        let mut sup = prepare_supervisor(&self.plugin_id, &self.supervisor).await?;
        let workload_json =
            serde_json::to_string(workload).map_err(|e| PluginError::Execution(e.to_string()))?;
        let hw_json =
            serde_json::to_string(hardware).map_err(|e| PluginError::Execution(e.to_string()))?;
        let model_json =
            serde_json::to_string(model).map_err(|e| PluginError::Execution(e.to_string()))?;

        let invoke_req = Invoke {
            extension_point: "runtime-builder".to_string(),
            method: "build_runtime".to_string(),
            request: Some(cy_plugin_protocol::pb::invoke::Request::BuildRuntime(
                BuildRuntimeRequest {
                    workload_request_json: workload_json,
                    hardware_manifest_json: hw_json,
                    model_manifest_json: model_json,
                },
            )),
        };
        let res = sup
            .invoke(invoke_req, Duration::from_secs(30))
            .await
            .map_err(|e| PluginError::Execution(e.to_string()))?;
        if let Some(invoke_result::Response::BuildRuntime(resp)) = res.response {
            return serde_json::from_str(&resp.runtime_manifest_json)
                .map_err(|e| PluginError::Execution(e.to_string()));
        }
        Err(PluginError::Execution(
            "Invalid build_runtime response payload".to_string(),
        ))
    }
}

/// 5. Remote Execution Engine Proxy
pub struct RemoteExecutionEngine {
    plugin_id: String,
    supervisor: Arc<AsyncMutex<PluginSupervisor>>,
    capabilities: PluginCapabilities,
}

impl RemoteExecutionEngine {
    pub fn new(
        plugin_id: impl Into<String>,
        supervisor: Arc<AsyncMutex<PluginSupervisor>>,
    ) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            supervisor,
            capabilities: Default::default(),
        }
    }
    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }
}

impl Plugin for RemoteExecutionEngine {
    fn id(&self) -> &str {
        &self.plugin_id
    }
    fn kind(&self) -> PluginKind {
        PluginKind::ExecutionEngine
    }
    fn api_version(&self) -> &str {
        PLUGIN_API_VERSION
    }
    fn capabilities(&self) -> &PluginCapabilities {
        &self.capabilities
    }
}

#[async_trait]
impl ExecutionEngine for RemoteExecutionEngine {
    async fn execute_inference(
        &self,
        runtime: &RuntimeManifest,
        model: &ModelManifest,
        prompt: &str,
    ) -> Result<String, PluginError> {
        let mut sup = prepare_supervisor(&self.plugin_id, &self.supervisor).await?;
        let runtime_json =
            serde_json::to_string(runtime).map_err(|e| PluginError::Execution(e.to_string()))?;
        let model_json =
            serde_json::to_string(model).map_err(|e| PluginError::Execution(e.to_string()))?;

        let invoke_req = Invoke {
            extension_point: "execution-engine".to_string(),
            method: "execute_inference".to_string(),
            request: Some(cy_plugin_protocol::pb::invoke::Request::ExecuteInference(
                ExecuteInferenceRequest {
                    runtime_manifest_json: runtime_json,
                    model_manifest_json: model_json,
                    prompt: prompt.to_string(),
                    streaming: false,
                },
            )),
        };
        let res = sup
            .invoke(invoke_req, Duration::from_secs(30))
            .await
            .map_err(|e| PluginError::Execution(e.to_string()))?;
        if let Some(invoke_result::Response::ExecuteInference(resp)) = res.response {
            return Ok(resp.output_text);
        }
        Err(PluginError::Execution(
            "Invalid execute_inference response payload".to_string(),
        ))
    }
}

/// 6. Remote Training Backend Proxy
pub struct RemoteTrainingBackend {
    plugin_id: String,
    supervisor: Arc<AsyncMutex<PluginSupervisor>>,
    capabilities: PluginCapabilities,
}

impl RemoteTrainingBackend {
    pub fn new(
        plugin_id: impl Into<String>,
        supervisor: Arc<AsyncMutex<PluginSupervisor>>,
    ) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            supervisor,
            capabilities: Default::default(),
        }
    }
}

impl Plugin for RemoteTrainingBackend {
    fn id(&self) -> &str {
        &self.plugin_id
    }
    fn kind(&self) -> PluginKind {
        PluginKind::TrainingBackend
    }
    fn api_version(&self) -> &str {
        PLUGIN_API_VERSION
    }
    fn capabilities(&self) -> &PluginCapabilities {
        &self.capabilities
    }
}

#[async_trait]
impl TrainingBackend for RemoteTrainingBackend {
    async fn run_training_step(
        &self,
        runtime: &RuntimeManifest,
        revision: &TrainingRevision,
    ) -> Result<CheckpointMetadata, PluginError> {
        let mut sup = prepare_supervisor(&self.plugin_id, &self.supervisor).await?;
        let runtime_json =
            serde_json::to_string(runtime).map_err(|e| PluginError::Execution(e.to_string()))?;
        let revision_json =
            serde_json::to_string(revision).map_err(|e| PluginError::Execution(e.to_string()))?;

        let invoke_req = Invoke {
            extension_point: "training-backend".to_string(),
            method: "run_training_step".to_string(),
            request: Some(cy_plugin_protocol::pb::invoke::Request::RunTrainingStep(
                RunTrainingStepRequest {
                    runtime_manifest_json: runtime_json,
                    training_revision_json: revision_json,
                },
            )),
        };
        let res = sup
            .invoke(invoke_req, Duration::from_secs(60))
            .await
            .map_err(|e| PluginError::Execution(e.to_string()))?;
        if let Some(invoke_result::Response::RunTrainingStep(resp)) = res.response {
            return serde_json::from_str(&resp.checkpoint_metadata_json)
                .map_err(|e| PluginError::Execution(e.to_string()));
        }
        Err(PluginError::Execution(
            "Invalid run_training_step response payload".to_string(),
        ))
    }
}

/// 7. Remote Quantization Proxy
pub struct RemoteQuantization {
    plugin_id: String,
    supervisor: Arc<AsyncMutex<PluginSupervisor>>,
    capabilities: PluginCapabilities,
}

impl RemoteQuantization {
    pub fn new(
        plugin_id: impl Into<String>,
        supervisor: Arc<AsyncMutex<PluginSupervisor>>,
    ) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            supervisor,
            capabilities: Default::default(),
        }
    }
}

impl Plugin for RemoteQuantization {
    fn id(&self) -> &str {
        &self.plugin_id
    }
    fn kind(&self) -> PluginKind {
        PluginKind::Quantization
    }
    fn api_version(&self) -> &str {
        PLUGIN_API_VERSION
    }
    fn capabilities(&self) -> &PluginCapabilities {
        &self.capabilities
    }
}

#[async_trait]
impl Quantization for RemoteQuantization {
    async fn quantize_model(
        &self,
        model: &ModelManifest,
        target_precision: WeightPrecision,
    ) -> Result<ArtifactManifest, PluginError> {
        let mut sup = prepare_supervisor(&self.plugin_id, &self.supervisor).await?;
        let model_json =
            serde_json::to_string(model).map_err(|e| PluginError::Execution(e.to_string()))?;
        let prec_str = format!("{:?}", target_precision);

        let invoke_req = Invoke {
            extension_point: "quantization".to_string(),
            method: "quantize_model".to_string(),
            request: Some(cy_plugin_protocol::pb::invoke::Request::QuantizeModel(
                QuantizeModelRequest {
                    model_manifest_json: model_json,
                    target_precision: prec_str,
                },
            )),
        };
        let res = sup
            .invoke(invoke_req, Duration::from_secs(60))
            .await
            .map_err(|e| PluginError::Execution(e.to_string()))?;
        if let Some(invoke_result::Response::QuantizeModel(resp)) = res.response {
            return serde_json::from_str(&resp.artifact_manifest_json)
                .map_err(|e| PluginError::Execution(e.to_string()));
        }
        Err(PluginError::Execution(
            "Invalid quantize_model response payload".to_string(),
        ))
    }
}

/// 8. Remote Gateway Filter Proxy
pub struct RemoteGatewayFilter {
    plugin_id: String,
    supervisor: Arc<AsyncMutex<PluginSupervisor>>,
    capabilities: PluginCapabilities,
}

impl RemoteGatewayFilter {
    pub fn new(
        plugin_id: impl Into<String>,
        supervisor: Arc<AsyncMutex<PluginSupervisor>>,
    ) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            supervisor,
            capabilities: Default::default(),
        }
    }
}

impl Plugin for RemoteGatewayFilter {
    fn id(&self) -> &str {
        &self.plugin_id
    }
    fn kind(&self) -> PluginKind {
        PluginKind::GatewayFilter
    }
    fn api_version(&self) -> &str {
        PLUGIN_API_VERSION
    }
    fn capabilities(&self) -> &PluginCapabilities {
        &self.capabilities
    }
}

#[async_trait]
impl GatewayFilter for RemoteGatewayFilter {
    async fn filter_request(
        &self,
        headers: &HashMap<String, String>,
        body: &str,
    ) -> Result<bool, PluginError> {
        let mut sup = prepare_supervisor(&self.plugin_id, &self.supervisor).await?;
        let invoke_req = Invoke {
            extension_point: "gateway-filter".to_string(),
            method: "filter_request".to_string(),
            request: Some(cy_plugin_protocol::pb::invoke::Request::FilterRequestData(
                FilterRequest {
                    headers: headers.clone(),
                    body: body.to_string(),
                },
            )),
        };
        let res = sup
            .invoke(invoke_req, Duration::from_secs(10))
            .await
            .map_err(|e| PluginError::Execution(e.to_string()))?;
        if let Some(invoke_result::Response::FilterResponseData(resp)) = res.response {
            return Ok(resp.allow);
        }
        Err(PluginError::Execution(
            "Invalid filter_request response payload".to_string(),
        ))
    }
}

/// 9. Remote Notification Proxy
pub struct RemoteNotification {
    plugin_id: String,
    supervisor: Arc<AsyncMutex<PluginSupervisor>>,
    capabilities: PluginCapabilities,
}

impl RemoteNotification {
    pub fn new(
        plugin_id: impl Into<String>,
        supervisor: Arc<AsyncMutex<PluginSupervisor>>,
    ) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            supervisor,
            capabilities: Default::default(),
        }
    }
}

impl Plugin for RemoteNotification {
    fn id(&self) -> &str {
        &self.plugin_id
    }
    fn kind(&self) -> PluginKind {
        PluginKind::Notification
    }
    fn api_version(&self) -> &str {
        PLUGIN_API_VERSION
    }
    fn capabilities(&self) -> &PluginCapabilities {
        &self.capabilities
    }
}

#[async_trait]
impl Notification for RemoteNotification {
    async fn send_notification(
        &self,
        topic: &str,
        message: &str,
        level: &str,
    ) -> Result<(), PluginError> {
        let mut sup = prepare_supervisor(&self.plugin_id, &self.supervisor).await?;
        let invoke_req = Invoke {
            extension_point: "notification".to_string(),
            method: "send_notification".to_string(),
            request: Some(cy_plugin_protocol::pb::invoke::Request::SendNotification(
                SendNotificationRequest {
                    topic: topic.to_string(),
                    message: message.to_string(),
                    level: level.to_string(),
                },
            )),
        };
        let _ = sup
            .invoke(invoke_req, Duration::from_secs(10))
            .await
            .map_err(|e| PluginError::Execution(e.to_string()))?;
        Ok(())
    }
}

/// 10. Remote Storage Proxy
pub struct RemoteStorage {
    plugin_id: String,
    supervisor: Arc<AsyncMutex<PluginSupervisor>>,
    capabilities: PluginCapabilities,
}

impl RemoteStorage {
    pub fn new(
        plugin_id: impl Into<String>,
        supervisor: Arc<AsyncMutex<PluginSupervisor>>,
    ) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            supervisor,
            capabilities: Default::default(),
        }
    }
}

impl Plugin for RemoteStorage {
    fn id(&self) -> &str {
        &self.plugin_id
    }
    fn kind(&self) -> PluginKind {
        PluginKind::Storage
    }
    fn api_version(&self) -> &str {
        PLUGIN_API_VERSION
    }
    fn capabilities(&self) -> &PluginCapabilities {
        &self.capabilities
    }
}

#[async_trait]
impl Storage for RemoteStorage {
    async fn store_artifact(
        &self,
        artifact: &ArtifactManifest,
        data: &[u8],
    ) -> Result<String, PluginError> {
        let mut sup = prepare_supervisor(&self.plugin_id, &self.supervisor).await?;
        let artifact_json =
            serde_json::to_string(artifact).map_err(|e| PluginError::Execution(e.to_string()))?;

        let invoke_req = Invoke {
            extension_point: "storage".to_string(),
            method: "store_artifact".to_string(),
            request: Some(cy_plugin_protocol::pb::invoke::Request::StoreArtifact(
                StoreArtifactRequest {
                    artifact_manifest_json: artifact_json,
                    data: data.to_vec(),
                },
            )),
        };
        let res = sup
            .invoke(invoke_req, Duration::from_secs(30))
            .await
            .map_err(|e| PluginError::Execution(e.to_string()))?;
        if let Some(invoke_result::Response::StoreArtifact(resp)) = res.response {
            return Ok(resp.artifact_id);
        }
        Err(PluginError::Execution(
            "Invalid store_artifact response payload".to_string(),
        ))
    }

    async fn fetch_artifact(&self, artifact_id: &str) -> Result<Vec<u8>, PluginError> {
        let mut sup = prepare_supervisor(&self.plugin_id, &self.supervisor).await?;
        let invoke_req = Invoke {
            extension_point: "storage".to_string(),
            method: "fetch_artifact".to_string(),
            request: Some(cy_plugin_protocol::pb::invoke::Request::FetchArtifact(
                FetchArtifactRequest {
                    artifact_id: artifact_id.to_string(),
                },
            )),
        };
        let res = sup
            .invoke(invoke_req, Duration::from_secs(30))
            .await
            .map_err(|e| PluginError::Execution(e.to_string()))?;
        if let Some(invoke_result::Response::FetchArtifact(resp)) = res.response {
            return Ok(resp.data);
        }
        Err(PluginError::Execution(
            "Invalid fetch_artifact response payload".to_string(),
        ))
    }
}

/// Generic Remote Proxy for arbitrary non-extension-point plugins (services, libraries, bundles).
pub struct GenericRemotePlugin {
    plugin_id: String,
    kind: PluginKind,
    supervisor: Arc<AsyncMutex<PluginSupervisor>>,
    capabilities: PluginCapabilities,
}

impl GenericRemotePlugin {
    pub fn new(
        plugin_id: impl Into<String>,
        kind_str: &str,
        supervisor: Arc<AsyncMutex<PluginSupervisor>>,
    ) -> Self {
        let kind = match kind_str {
            "bundle" => PluginKind::Bundle,
            "service" => PluginKind::Service,
            "library" => PluginKind::Library,
            "python-package" => PluginKind::PythonPackage,
            "protocol-and-services" => PluginKind::ProtocolAndServices,
            "deployment-assets" => PluginKind::DeploymentAssets,
            _ => PluginKind::Probe,
        };
        Self {
            plugin_id: plugin_id.into(),
            kind,
            supervisor,
            capabilities: Default::default(),
        }
    }
    pub fn supervisor(&self) -> &Arc<AsyncMutex<PluginSupervisor>> {
        &self.supervisor
    }
}

impl Plugin for GenericRemotePlugin {
    fn id(&self) -> &str {
        &self.plugin_id
    }
    fn kind(&self) -> PluginKind {
        self.kind
    }
    fn api_version(&self) -> &str {
        PLUGIN_API_VERSION
    }
    fn capabilities(&self) -> &PluginCapabilities {
        &self.capabilities
    }
}

// ---------------------------------------------------------------------------
// Typed Extension Registry
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct ExtensionRegistry {
    probes: HashMap<String, Arc<dyn Probe>>,
    model_analyzers: HashMap<String, Arc<dyn ModelAnalyzer>>,
    compat_rules: HashMap<String, Arc<dyn CompatRule>>,
    runtime_builders: HashMap<String, Arc<dyn RuntimeBuilder>>,
    execution_engines: HashMap<String, Arc<dyn ExecutionEngine>>,
    training_backends: HashMap<String, Arc<dyn TrainingBackend>>,
    quantizations: HashMap<String, Arc<dyn Quantization>>,
    gateway_filters: HashMap<String, Arc<dyn GatewayFilter>>,
    notifications: HashMap<String, Arc<dyn Notification>>,
    storages: HashMap<String, Arc<dyn Storage>>,

    all_plugins: HashMap<String, Arc<dyn Plugin>>,
}

impl ExtensionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_plugin(&mut self, plugin: Arc<dyn Plugin>) {
        self.all_plugins.insert(plugin.id().to_string(), plugin);
    }

    pub fn register_probe<T: Probe + 'static>(&mut self, plugin: Arc<T>) {
        self.all_plugins
            .insert(plugin.id().to_string(), plugin.clone() as Arc<dyn Plugin>);
        self.probes
            .insert(plugin.id().to_string(), plugin as Arc<dyn Probe>);
    }

    pub fn register_model_analyzer<T: ModelAnalyzer + 'static>(&mut self, plugin: Arc<T>) {
        self.all_plugins
            .insert(plugin.id().to_string(), plugin.clone() as Arc<dyn Plugin>);
        self.model_analyzers
            .insert(plugin.id().to_string(), plugin as Arc<dyn ModelAnalyzer>);
    }

    pub fn register_compat_rule<T: CompatRule + 'static>(&mut self, plugin: Arc<T>) {
        self.all_plugins
            .insert(plugin.id().to_string(), plugin.clone() as Arc<dyn Plugin>);
        self.compat_rules
            .insert(plugin.id().to_string(), plugin as Arc<dyn CompatRule>);
    }

    pub fn register_runtime_builder<T: RuntimeBuilder + 'static>(&mut self, plugin: Arc<T>) {
        self.all_plugins
            .insert(plugin.id().to_string(), plugin.clone() as Arc<dyn Plugin>);
        self.runtime_builders
            .insert(plugin.id().to_string(), plugin as Arc<dyn RuntimeBuilder>);
    }

    pub fn register_execution_engine<T: ExecutionEngine + 'static>(&mut self, plugin: Arc<T>) {
        self.all_plugins
            .insert(plugin.id().to_string(), plugin.clone() as Arc<dyn Plugin>);
        self.execution_engines
            .insert(plugin.id().to_string(), plugin as Arc<dyn ExecutionEngine>);
    }

    pub fn register_training_backend<T: TrainingBackend + 'static>(&mut self, plugin: Arc<T>) {
        self.all_plugins
            .insert(plugin.id().to_string(), plugin.clone() as Arc<dyn Plugin>);
        self.training_backends
            .insert(plugin.id().to_string(), plugin as Arc<dyn TrainingBackend>);
    }

    pub fn register_quantization<T: Quantization + 'static>(&mut self, plugin: Arc<T>) {
        self.all_plugins
            .insert(plugin.id().to_string(), plugin.clone() as Arc<dyn Plugin>);
        self.quantizations
            .insert(plugin.id().to_string(), plugin as Arc<dyn Quantization>);
    }

    pub fn register_gateway_filter<T: GatewayFilter + 'static>(&mut self, plugin: Arc<T>) {
        self.all_plugins
            .insert(plugin.id().to_string(), plugin.clone() as Arc<dyn Plugin>);
        self.gateway_filters
            .insert(plugin.id().to_string(), plugin as Arc<dyn GatewayFilter>);
    }

    pub fn register_notification<T: Notification + 'static>(&mut self, plugin: Arc<T>) {
        self.all_plugins
            .insert(plugin.id().to_string(), plugin.clone() as Arc<dyn Plugin>);
        self.notifications
            .insert(plugin.id().to_string(), plugin as Arc<dyn Notification>);
    }

    pub fn register_storage<T: Storage + 'static>(&mut self, plugin: Arc<T>) {
        self.all_plugins
            .insert(plugin.id().to_string(), plugin.clone() as Arc<dyn Plugin>);
        self.storages
            .insert(plugin.id().to_string(), plugin as Arc<dyn Storage>);
    }

    pub fn get_probe(&self, id: &str) -> Option<Arc<dyn Probe>> {
        self.probes.get(id).cloned()
    }

    pub fn get_model_analyzer(&self, id: &str) -> Option<Arc<dyn ModelAnalyzer>> {
        self.model_analyzers.get(id).cloned()
    }

    pub fn get_compat_rule(&self, id: &str) -> Option<Arc<dyn CompatRule>> {
        self.compat_rules.get(id).cloned()
    }

    pub fn get_runtime_builder(&self, id: &str) -> Option<Arc<dyn RuntimeBuilder>> {
        self.runtime_builders.get(id).cloned()
    }

    pub fn get_execution_engine(&self, id: &str) -> Option<Arc<dyn ExecutionEngine>> {
        self.execution_engines.get(id).cloned()
    }

    pub fn get_training_backend(&self, id: &str) -> Option<Arc<dyn TrainingBackend>> {
        self.training_backends.get(id).cloned()
    }

    pub fn get_quantization(&self, id: &str) -> Option<Arc<dyn Quantization>> {
        self.quantizations.get(id).cloned()
    }

    pub fn get_gateway_filter(&self, id: &str) -> Option<Arc<dyn GatewayFilter>> {
        self.gateway_filters.get(id).cloned()
    }

    pub fn get_notification(&self, id: &str) -> Option<Arc<dyn Notification>> {
        self.notifications.get(id).cloned()
    }

    pub fn get_storage(&self, id: &str) -> Option<Arc<dyn Storage>> {
        self.storages.get(id).cloned()
    }

    pub fn list_probes(&self) -> Vec<Arc<dyn Probe>> {
        self.probes.values().cloned().collect()
    }

    pub fn list_model_analyzers(&self) -> Vec<Arc<dyn ModelAnalyzer>> {
        self.model_analyzers.values().cloned().collect()
    }

    pub fn list_compat_rules(&self) -> Vec<Arc<dyn CompatRule>> {
        self.compat_rules.values().cloned().collect()
    }

    pub fn list_runtime_builders(&self) -> Vec<Arc<dyn RuntimeBuilder>> {
        self.runtime_builders.values().cloned().collect()
    }

    pub fn list_execution_engines(&self) -> Vec<Arc<dyn ExecutionEngine>> {
        self.execution_engines.values().cloned().collect()
    }

    pub fn list_training_backends(&self) -> Vec<Arc<dyn TrainingBackend>> {
        self.training_backends.values().cloned().collect()
    }

    pub fn list_quantizations(&self) -> Vec<Arc<dyn Quantization>> {
        self.quantizations.values().cloned().collect()
    }

    pub fn list_gateway_filters(&self) -> Vec<Arc<dyn GatewayFilter>> {
        self.gateway_filters.values().cloned().collect()
    }

    pub fn list_notifications(&self) -> Vec<Arc<dyn Notification>> {
        self.notifications.values().cloned().collect()
    }

    pub fn list_storages(&self) -> Vec<Arc<dyn Storage>> {
        self.storages.values().cloned().collect()
    }

    pub fn list_plugins(&self) -> Vec<Arc<dyn Plugin>> {
        self.all_plugins.values().cloned().collect()
    }
}
