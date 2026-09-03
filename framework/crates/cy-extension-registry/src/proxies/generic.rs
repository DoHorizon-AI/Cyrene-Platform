// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: framework/crates/cy-extension-registry/src/proxies/generic.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! 通用远程插件代理 (Generic Remote Plugin Proxy)

use std::sync::Arc;

use cy_kernel_daemon::watchdog::InstanceActor;
use cy_platform_api::{Plugin, PluginCapabilities, PluginKind, PLUGIN_API_VERSION};
use tokio::sync::Mutex as AsyncMutex;

/// Generic Remote Proxy for arbitrary non-extension-point plugins (services, libraries, bundles).
pub struct GenericRemotePlugin {
    plugin_id: String,
    kind: PluginKind,
    actor: Arc<AsyncMutex<InstanceActor>>,
    capabilities: PluginCapabilities,
}

impl GenericRemotePlugin {
    pub fn new(
        plugin_id: impl Into<String>,
        kind_str: &str,
        actor: Arc<AsyncMutex<InstanceActor>>,
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
            actor,
            capabilities: Default::default(),
        }
    }
    pub fn actor(&self) -> &Arc<AsyncMutex<InstanceActor>> {
        &self.actor
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
