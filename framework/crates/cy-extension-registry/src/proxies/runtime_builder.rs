//! 远程运行时构建器代理 (Remote Runtime Builder Proxy)

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cy_manifest::{HardwareManifest, ModelManifest, RuntimeManifest, WorkloadRequest};
use cy_platform_api::{
    Plugin, PluginCapabilities, PluginError, PluginKind, RuntimeBuilder, PLUGIN_API_VERSION,
};
use cy_plugin_protocol::pb::{invoke_result, BuildRuntimeRequest, Invoke};
use cy_plugin_supervisor::PluginSupervisor;
use tokio::sync::Mutex as AsyncMutex;

use crate::helper::prepare_supervisor;

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
