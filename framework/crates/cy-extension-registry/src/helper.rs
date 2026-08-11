//! 插件监管器与运行时健康前置检查辅助函数

use std::sync::Arc;
use std::time::Duration;

use cy_platform_api::PluginError;
use cy_plugin_supervisor::{PluginRuntimeState, PluginSupervisor};
use tokio::sync::Mutex as AsyncMutex;

/// 在发起 RPC 调用前确保插件监管器与子进程就绪
pub async fn prepare_supervisor<'a>(
    plugin_id: &str,
    sup_arc: &'a Arc<AsyncMutex<PluginSupervisor>>,
) -> Result<tokio::sync::MutexGuard<'a, PluginSupervisor>, PluginError> {
    let mut sup = sup_arc.lock().await;
    if let Err(e) = sup.ensure_healthy(Duration::from_secs(5)).await {
        return Err(PluginError::Unavailable(format!(
            "Failed to ensure plugin {} is healthy: {}",
            plugin_id, e
        )));
    }
    if sup.state() != PluginRuntimeState::Healthy {
        return Err(PluginError::Unavailable(format!(
            "Plugin {} is not healthy (state={:?})",
            plugin_id,
            sup.state()
        )));
    }
    Ok(sup)
}
