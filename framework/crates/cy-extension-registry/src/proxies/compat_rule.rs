//! 远程兼容性规则代理 (Remote Compat Rule Proxy)

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cy_manifest::{HardwareManifest, ModelManifest, WhyReport, WorkloadRequest};
use cy_platform_api::{
    CompatRule, Plugin, PluginCapabilities, PluginError, PluginKind, PLUGIN_API_VERSION,
};
use cy_plugin_protocol::pb::{invoke_result, EvaluateCompatRequest, Invoke};
use cy_plugin_supervisor::PluginSupervisor;
use tokio::sync::Mutex as AsyncMutex;

use crate::helper::prepare_supervisor;

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
