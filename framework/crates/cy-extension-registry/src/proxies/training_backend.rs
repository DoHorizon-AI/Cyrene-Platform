//! 远程训练后端代理 (Remote Training Backend Proxy)

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cy_kernel_daemon::watchdog::InstanceActor;
use cy_manifest::{CheckpointMetadata, RuntimeManifest, TrainingRevision};
use cy_platform_api::{
    Plugin, PluginCapabilities, PluginError, PluginKind, TrainingBackend, PLUGIN_API_VERSION,
};
use cy_plugin_protocol::pb::{invoke_result, Invoke, RunTrainingStepRequest};
use tokio::sync::Mutex as AsyncMutex;

use crate::helper::{invoke_actor, prepare_instance_actor};

/// 6. Remote Training Backend Proxy
pub struct RemoteTrainingBackend {
    plugin_id: String,
    actor: Arc<AsyncMutex<InstanceActor>>,
    capabilities: PluginCapabilities,
}

impl RemoteTrainingBackend {
    pub fn new(plugin_id: impl Into<String>, actor: Arc<AsyncMutex<InstanceActor>>) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            actor,
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
        let mut actor = prepare_instance_actor(&self.plugin_id, &self.actor).await?;
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
        let res = invoke_actor(&mut actor, invoke_req, Duration::from_secs(60)).await?;
        if let Some(invoke_result::Response::RunTrainingStep(resp)) = res.response {
            return serde_json::from_str(&resp.checkpoint_metadata_json)
                .map_err(|e| PluginError::Execution(e.to_string()));
        }
        Err(PluginError::Execution(
            "Invalid run_training_step response payload".to_string(),
        ))
    }
}
