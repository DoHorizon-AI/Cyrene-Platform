// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: framework/crates/cy-extension-registry/src/proxies/execution_engine.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! 远程执行引擎代理 (Remote Execution Engine Proxy)

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cy_kernel_daemon::watchdog::InstanceActor;
use cy_manifest::{ModelManifest, RuntimeManifest};
use cy_platform_api::{
    ExecutionEngine, Plugin, PluginCapabilities, PluginError, PluginKind, PLUGIN_API_VERSION,
};
use cy_plugin_protocol::pb::{invoke_result, ExecuteInferenceRequest, Invoke};
use tokio::sync::Mutex as AsyncMutex;

use crate::helper::{invoke_actor, prepare_instance_actor};

/// 5. Remote Execution Engine Proxy
pub struct RemoteExecutionEngine {
    plugin_id: String,
    actor: Arc<AsyncMutex<InstanceActor>>,
    capabilities: PluginCapabilities,
}

impl RemoteExecutionEngine {
    pub fn new(plugin_id: impl Into<String>, actor: Arc<AsyncMutex<InstanceActor>>) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            actor,
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
        let mut actor = prepare_instance_actor(&self.plugin_id, &self.actor).await?;
        let runtime_json =
            serde_json::to_string(runtime).map_err(|e| PluginError::Execution(e.to_string()))?;
        let model_json =
            serde_json::to_string(model).map_err(|e| PluginError::Execution(e.to_string()))?;

        let invoke_req = Invoke {
            extension_point: "execution-engine".to_string(),
            method: "execute_inference".to_string(),
            payload: Vec::new(),
            payload_type_url: String::new(),
            request: Some(cy_plugin_protocol::pb::invoke::Request::ExecuteInference(
                ExecuteInferenceRequest {
                    runtime_manifest_json: runtime_json,
                    model_manifest_json: model_json,
                    prompt: prompt.to_string(),
                    streaming: false,
                },
            )),
        };
        let res = invoke_actor(&mut actor, invoke_req, Duration::from_secs(30)).await?;
        if let Some(invoke_result::Response::ExecuteInference(resp)) = res.response {
            return Ok(resp.output_text);
        }
        Err(PluginError::Execution(
            "Invalid execute_inference response payload".to_string(),
        ))
    }
}
