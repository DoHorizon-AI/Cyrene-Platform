// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: framework/crates/cy-extension-registry/src/proxies/model_analyzer.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! 远程模型分析器代理 (Remote Model Analyzer Proxy)

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cy_kernel_daemon::watchdog::InstanceActor;
use cy_manifest::{ModelManifest, VramEstimate, WorkloadRequest};
use cy_platform_api::{
    ModelAnalyzer, Plugin, PluginCapabilities, PluginError, PluginKind, PLUGIN_API_VERSION,
};
use cy_plugin_protocol::pb::{invoke_result, AnalyzeModelRequest, Invoke};
use tokio::sync::Mutex as AsyncMutex;

use crate::helper::{invoke_actor, prepare_instance_actor};

/// 2. Remote Model Analyzer Proxy
pub struct RemoteModelAnalyzer {
    plugin_id: String,
    actor: Arc<AsyncMutex<InstanceActor>>,
    capabilities: PluginCapabilities,
}

impl RemoteModelAnalyzer {
    pub fn new(plugin_id: impl Into<String>, actor: Arc<AsyncMutex<InstanceActor>>) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            actor,
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
        let mut actor = prepare_instance_actor(&self.plugin_id, &self.actor).await?;
        let model_json =
            serde_json::to_string(model).map_err(|e| PluginError::Execution(e.to_string()))?;
        let workload_json =
            serde_json::to_string(workload).map_err(|e| PluginError::Execution(e.to_string()))?;

        let invoke_req = Invoke {
            extension_point: "model-analyzer".to_string(),
            method: "analyze_model".to_string(),
            payload: Vec::new(),
            payload_type_url: String::new(),
            stream_results: false,
            request: Some(cy_plugin_protocol::pb::invoke::Request::AnalyzeModel(
                AnalyzeModelRequest {
                    model_manifest_json: model_json,
                    workload_request_json: workload_json,
                },
            )),
        };
        let res = invoke_actor(&mut actor, invoke_req, Duration::from_secs(15)).await?;
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
