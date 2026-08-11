//! 远程通知代理 (Remote Notification Proxy)

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cy_platform_api::{
    Notification, Plugin, PluginCapabilities, PluginError, PluginKind, PLUGIN_API_VERSION,
};
use cy_plugin_protocol::pb::{Invoke, SendNotificationRequest};
use cy_plugin_supervisor::PluginSupervisor;
use tokio::sync::Mutex as AsyncMutex;

use crate::helper::prepare_supervisor;

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
