//! 插件实例 Actor 与沙箱运行时健康前置检查辅助函数

use std::sync::Arc;

use cy_kernel_daemon::watchdog::{InstanceActor, InstanceActorState};
use cy_platform_api::PluginError;
use tokio::sync::Mutex as AsyncMutex;

/// 在发起 RPC 调用前确保实例 Actor 处于就绪状态
pub async fn prepare_instance_actor<'a>(
    plugin_id: &str,
    actor_arc: &'a Arc<AsyncMutex<InstanceActor>>,
) -> Result<tokio::sync::MutexGuard<'a, InstanceActor>, PluginError> {
    let mut actor = actor_arc.lock().await;
    if actor.state() == InstanceActorState::Quarantined {
        return Err(PluginError::Unavailable(format!(
            "Instance {} is quarantined; execution blocked",
            plugin_id
        )));
    }
    if actor.state() == InstanceActorState::Stopped || actor.state() == InstanceActorState::Starting {
        if let Err(e) = actor.start() {
            return Err(PluginError::Unavailable(format!(
                "Failed to start instance actor for {}: {}",
                plugin_id, e
            )));
        }
    }
    if actor.state() != InstanceActorState::Healthy && actor.state() != InstanceActorState::Degraded {
        return Err(PluginError::Unavailable(format!(
            "Instance {} is not healthy (state={:?})",
            plugin_id,
            actor.state()
        )));
    }
    Ok(actor)
}
