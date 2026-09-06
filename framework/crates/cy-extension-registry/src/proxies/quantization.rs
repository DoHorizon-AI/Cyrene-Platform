// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: framework/crates/cy-extension-registry/src/proxies/quantization.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! 远程量化代理 (Remote Quantization Proxy)

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cy_kernel_daemon::watchdog::InstanceActor;
use cy_manifest::{ArtifactManifest, ModelManifest, WeightPrecision};
use cy_platform_api::{
    Plugin, PluginCapabilities, PluginError, PluginKind, Quantization, PLUGIN_API_VERSION,
};
use cy_plugin_protocol::pb::{invoke_result, Invoke, QuantizeModelRequest};
use tokio::sync::Mutex as AsyncMutex;

use crate::helper::{invoke_actor, prepare_instance_actor};

/// 7. Remote Quantization Proxy
pub struct RemoteQuantization {
    plugin_id: String,
    actor: Arc<AsyncMutex<InstanceActor>>,
    capabilities: PluginCapabilities,
}

impl RemoteQuantization {
    pub fn new(plugin_id: impl Into<String>, actor: Arc<AsyncMutex<InstanceActor>>) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            actor,
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
        let mut actor = prepare_instance_actor(&self.plugin_id, &self.actor).await?;
        let model_json =
            serde_json::to_string(model).map_err(|e| PluginError::Execution(e.to_string()))?;
        let prec_str = format!("{:?}", target_precision);

        let invoke_req = Invoke {
            extension_point: "quantization".to_string(),
            method: "quantize_model".to_string(),
            payload: Vec::new(),
            payload_type_url: String::new(),
            stream_results: false,
            request: Some(cy_plugin_protocol::pb::invoke::Request::QuantizeModel(
                QuantizeModelRequest {
                    model_manifest_json: model_json,
                    target_precision: prec_str,
                },
            )),
        };
        let res = invoke_actor(&mut actor, invoke_req, Duration::from_secs(60)).await?;
        if let Some(invoke_result::Response::QuantizeModel(resp)) = res.response {
            return serde_json::from_str(&resp.artifact_manifest_json)
                .map_err(|e| PluginError::Execution(e.to_string()));
        }
        Err(PluginError::Execution(
            "Invalid quantize_model response payload".to_string(),
        ))
    }
}
