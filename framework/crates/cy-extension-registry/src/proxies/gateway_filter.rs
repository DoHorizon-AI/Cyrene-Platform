//! 远程网关过滤器代理 (Remote Gateway Filter Proxy)

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cy_platform_api::{
    GatewayFilter, Plugin, PluginCapabilities, PluginError, PluginKind, PLUGIN_API_VERSION,
};
use cy_plugin_protocol::pb::{invoke_result, FilterRequest, Invoke};
use cy_plugin_supervisor::PluginSupervisor;
use tokio::sync::Mutex as AsyncMutex;

use crate::helper::prepare_supervisor;

/// 8. Remote Gateway Filter Proxy
pub struct RemoteGatewayFilter {
    plugin_id: String,
    supervisor: Arc<AsyncMutex<PluginSupervisor>>,
    capabilities: PluginCapabilities,
}

impl RemoteGatewayFilter {
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

impl Plugin for RemoteGatewayFilter {
    fn id(&self) -> &str {
        &self.plugin_id
    }
    fn kind(&self) -> PluginKind {
        PluginKind::GatewayFilter
    }
    fn api_version(&self) -> &str {
        PLUGIN_API_VERSION
    }
    fn capabilities(&self) -> &PluginCapabilities {
        &self.capabilities
    }
}

#[async_trait]
impl GatewayFilter for RemoteGatewayFilter {
    async fn filter_request(
        &self,
        headers: &HashMap<String, String>,
        body: &str,
    ) -> Result<bool, PluginError> {
        let mut sup = prepare_supervisor(&self.plugin_id, &self.supervisor).await?;
        let invoke_req = Invoke {
            extension_point: "gateway-filter".to_string(),
            method: "filter_request".to_string(),
            request: Some(cy_plugin_protocol::pb::invoke::Request::FilterRequestData(
                FilterRequest {
                    headers: headers.clone(),
                    body: body.to_string(),
                },
            )),
        };
        let res = sup
            .invoke(invoke_req, Duration::from_secs(10))
            .await
            .map_err(|e| PluginError::Execution(e.to_string()))?;
        if let Some(invoke_result::Response::FilterResponseData(resp)) = res.response {
            return Ok(resp.allow);
        }
        Err(PluginError::Execution(
            "Invalid filter_request response payload".to_string(),
        ))
    }
}
