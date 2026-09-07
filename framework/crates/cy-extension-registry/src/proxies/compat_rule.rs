// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: framework/crates/cy-extension-registry/src/proxies/compat_rule.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! 远程兼容性规则代理 (Remote Compat Rule Proxy)

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cy_kernel_daemon::watchdog::InstanceActor;
use cy_manifest::{HardwareManifest, ModelManifest, WhyReport, WorkloadRequest};
use cy_platform_api::{
    CompatRule, Plugin, PluginCapabilities, PluginError, PluginKind, PLUGIN_API_VERSION,
};
use cy_plugin_protocol::pb::{invoke_result, EvaluateCompatRequest, Invoke};
use tokio::sync::Mutex as AsyncMutex;

use crate::helper::{invoke_actor, prepare_instance_actor};

/// 3. Remote Compat Rule Proxy
pub struct RemoteCompatRule {
    plugin_id: String,
    actor: Arc<AsyncMutex<InstanceActor>>,
    capabilities: PluginCapabilities,
}

impl RemoteCompatRule {
    pub fn new(plugin_id: impl Into<String>, actor: Arc<AsyncMutex<InstanceActor>>) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            actor,
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
        let mut actor = prepare_instance_actor(&self.plugin_id, &self.actor).await?;
        let hw_json =
            serde_json::to_string(hardware).map_err(|e| PluginError::Execution(e.to_string()))?;
        let model_json =
            serde_json::to_string(model).map_err(|e| PluginError::Execution(e.to_string()))?;
        let workload_json =
            serde_json::to_string(workload).map_err(|e| PluginError::Execution(e.to_string()))?;

        let invoke_req = Invoke {
            extension_point: "compat-rule".to_string(),
            method: "evaluate_compatibility".to_string(),
            payload: Vec::new(),
            payload_type_url: String::new(),
            stream_results: false,
            request: Some(cy_plugin_protocol::pb::invoke::Request::EvaluateCompat(
                EvaluateCompatRequest {
                    hardware_manifest_json: hw_json,
                    model_manifest_json: model_json,
                    workload_request_json: workload_json,
                },
            )),
        };
        let res = invoke_actor(&mut actor, invoke_req, Duration::from_secs(15)).await?;
        if let Some(invoke_result::Response::EvaluateCompat(resp)) = res.response {
            return serde_json::from_str(&resp.why_report_json)
                .map_err(|e| PluginError::Execution(e.to_string()));
        }
        Err(PluginError::Execution(
            "Invalid evaluate_compatibility response payload".to_string(),
        ))
    }
}
