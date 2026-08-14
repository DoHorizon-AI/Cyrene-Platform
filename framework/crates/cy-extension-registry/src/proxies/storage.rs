//! 远程存储代理 (Remote Storage Proxy)

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cy_kernel_daemon::watchdog::InstanceActor;
use cy_manifest::ArtifactManifest;
use cy_platform_api::{
    Plugin, PluginCapabilities, PluginError, PluginKind, Storage, PLUGIN_API_VERSION,
};
use cy_plugin_protocol::pb::{invoke_result, FetchArtifactRequest, Invoke, StoreArtifactRequest};
use tokio::sync::Mutex as AsyncMutex;

use crate::helper::prepare_instance_actor;

/// 10. Remote Storage Proxy
pub struct RemoteStorage {
    plugin_id: String,
    actor: Arc<AsyncMutex<InstanceActor>>,
    capabilities: PluginCapabilities,
}

impl RemoteStorage {
    pub fn new(
        plugin_id: impl Into<String>,
        actor: Arc<AsyncMutex<InstanceActor>>,
    ) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            actor,
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
        let mut actor = prepare_instance_actor(&self.plugin_id, &self.actor).await?;
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
        let res = actor
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
        let mut actor = prepare_instance_actor(&self.plugin_id, &self.actor).await?;
        let invoke_req = Invoke {
            extension_point: "storage".to_string(),
            method: "fetch_artifact".to_string(),
            request: Some(cy_plugin_protocol::pb::invoke::Request::FetchArtifact(
                FetchArtifactRequest {
                    artifact_id: artifact_id.to_string(),
                },
            )),
        };
        let res = actor
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
