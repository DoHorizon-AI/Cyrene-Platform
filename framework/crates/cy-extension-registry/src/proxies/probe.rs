//! 远程硬件探针代理 (Remote Probe Proxy)

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cy_kernel_daemon::watchdog::InstanceActor;
use cy_manifest::HardwareManifest;
use cy_platform_api::{
    Plugin, PluginCapabilities, PluginError, PluginKind, Probe, PLUGIN_API_VERSION,
};
use cy_plugin_protocol::pb::{invoke_result, DetectHardwareRequest, Invoke};
use tokio::sync::Mutex as AsyncMutex;

use crate::helper::{invoke_actor, prepare_instance_actor};

/// 1. 远程硬件探针代理
pub struct RemoteProbe {
    plugin_id: String,
    actor: Arc<AsyncMutex<InstanceActor>>,
    capabilities: PluginCapabilities,
}

impl RemoteProbe {
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
        let mut actor = prepare_instance_actor(&self.plugin_id, &self.actor).await?;
        let invoke_req = Invoke {
            extension_point: "probe".to_string(),
            method: "detect_hardware".to_string(),
            request: Some(cy_plugin_protocol::pb::invoke::Request::DetectHardware(
                DetectHardwareRequest {},
            )),
        };
        let res = invoke_actor(&mut actor, invoke_req, Duration::from_secs(10)).await?;
        if let Some(invoke_result::Response::DetectHardware(resp)) = res.response {
            return serde_json::from_str(&resp.hardware_manifest_json)
                .map_err(|e| PluginError::Execution(e.to_string()));
        }
        Err(PluginError::Execution(
            "Invalid detect_hardware response payload".to_string(),
        ))
    }
}
