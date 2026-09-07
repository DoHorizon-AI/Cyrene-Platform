// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: framework/crates/cy-extension-registry/src/proxies/gateway_filter.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! 远程网关过滤器代理 (Remote Gateway Filter Proxy)

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cy_kernel_daemon::watchdog::InstanceActor;
use cy_platform_api::{
    GatewayFilter, Plugin, PluginCapabilities, PluginError, PluginKind, PLUGIN_API_VERSION,
};
use cy_plugin_protocol::pb::{invoke_result, FilterRequest, Invoke};
use tokio::sync::Mutex as AsyncMutex;

use crate::helper::{invoke_actor, prepare_instance_actor};

/// 8. Remote Gateway Filter Proxy
pub struct RemoteGatewayFilter {
    plugin_id: String,
    actor: Arc<AsyncMutex<InstanceActor>>,
    capabilities: PluginCapabilities,
}

impl RemoteGatewayFilter {
    pub fn new(plugin_id: impl Into<String>, actor: Arc<AsyncMutex<InstanceActor>>) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            actor,
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
        let mut actor = prepare_instance_actor(&self.plugin_id, &self.actor).await?;
        let invoke_req = Invoke {
            extension_point: "gateway-filter".to_string(),
            method: "filter_request".to_string(),
            payload: Vec::new(),
            payload_type_url: String::new(),
            stream_results: false,
            request: Some(cy_plugin_protocol::pb::invoke::Request::FilterRequestData(
                FilterRequest {
                    headers: headers.clone(),
                    body: body.to_string(),
                },
            )),
        };
        let res = invoke_actor(&mut actor, invoke_req, Duration::from_secs(10)).await?;
        if let Some(invoke_result::Response::FilterResponseData(resp)) = res.response {
            return Ok(resp.allow);
        }
        Err(PluginError::Execution(
            "Invalid filter_request response payload".to_string(),
        ))
    }
}
