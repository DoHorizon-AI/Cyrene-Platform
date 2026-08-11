//! 通用远程插件代理 (Generic Remote Plugin Proxy)

use std::sync::Arc;

use cy_platform_api::{Plugin, PluginCapabilities, PluginKind, PLUGIN_API_VERSION};
use cy_plugin_supervisor::PluginSupervisor;
use tokio::sync::Mutex as AsyncMutex;

/// Generic Remote Proxy for arbitrary non-extension-point plugins (services, libraries, bundles).
pub struct GenericRemotePlugin {
    plugin_id: String,
    kind: PluginKind,
    supervisor: Arc<AsyncMutex<PluginSupervisor>>,
    capabilities: PluginCapabilities,
}

impl GenericRemotePlugin {
    pub fn new(
        plugin_id: impl Into<String>,
        kind_str: &str,
        supervisor: Arc<AsyncMutex<PluginSupervisor>>,
    ) -> Self {
        let kind = match kind_str {
            "bundle" => PluginKind::Bundle,
            "service" => PluginKind::Service,
            "library" => PluginKind::Library,
            "python-package" => PluginKind::PythonPackage,
            "protocol-and-services" => PluginKind::ProtocolAndServices,
            "deployment-assets" => PluginKind::DeploymentAssets,
            _ => PluginKind::Probe,
        };
        Self {
            plugin_id: plugin_id.into(),
            kind,
            supervisor,
            capabilities: Default::default(),
        }
    }
    pub fn supervisor(&self) -> &Arc<AsyncMutex<PluginSupervisor>> {
        &self.supervisor
    }
}

impl Plugin for GenericRemotePlugin {
    fn id(&self) -> &str {
        &self.plugin_id
    }
    fn kind(&self) -> PluginKind {
        self.kind
    }
    fn api_version(&self) -> &str {
        PLUGIN_API_VERSION
    }
    fn capabilities(&self) -> &PluginCapabilities {
        &self.capabilities
    }
}
