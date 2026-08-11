//! 设备绑定配置与环境隔离保护.

use std::collections::BTreeMap;

use crate::{capability::EnforcementMode, error::ProviderError, inventory::DeviceNode};

/// 设备绑定配置 (Device Binding)：定义特定沙箱进程对硬件设备的访问授权与环境变量
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceBinding {
    /// 绑定的语义资源 ID
    pub resource_id: String,
    /// 需要注入沙箱的设备节点
    pub nodes: Vec<DeviceNode>,
    /// Adapter 返回、由 Kernel 受控注入的设备环境变量
    pub environment: BTreeMap<String, String>,
    /// 访问该设备必需的系统用户组 GID 列表
    pub required_gids: Vec<u32>,
    /// 强制执行模式
    pub enforcement: EnforcementMode,
    /// 执行绑定的适配器标识
    pub adapter_id: String,
    /// 绑定决策原因代码
    pub reason_code: String,
}

impl DeviceBinding {
    /// 合并用户自定义环境变量与内核设备绑定环境变量。
    ///
    /// # 安全保护（核心约束）
    /// 严格禁止插件/用户代码覆盖 Adapter 已为设备绑定声明的环境变量。
    /// 一旦发现冲突立即报错，防止越权访问未分配资源。
    pub fn merge_environment(
        &self,
        requested: &BTreeMap<String, String>,
    ) -> Result<BTreeMap<String, String>, ProviderError> {
        if requested
            .keys()
            .any(|key| self.environment.contains_key(key))
        {
            return Err(ProviderError::new(
                &self.adapter_id,
                "RESERVED_ENVIRONMENT",
                "plugin attempted to override an Adapter-owned device variable",
            ));
        }

        let mut environment = requested.clone();
        environment.extend(self.environment.clone());
        Ok(environment)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_binding_environment_is_reserved_without_vendor_knowledge() {
        let binding = DeviceBinding {
            resource_id: "accelerator-1".to_string(),
            nodes: Vec::new(),
            environment: BTreeMap::from([("ADAPTER_VISIBLE_DEVICE".to_string(), "1".to_string())]),
            required_gids: Vec::new(),
            enforcement: EnforcementMode::VisibilityOnly,
            adapter_id: "test-adapter".to_string(),
            reason_code: "TEST".to_string(),
        };
        let requested =
            BTreeMap::from([("ADAPTER_VISIBLE_DEVICE".to_string(), "other".to_string())]);
        assert_eq!(
            binding
                .merge_environment(&requested)
                .unwrap_err()
                .reason_code,
            "RESERVED_ENVIRONMENT"
        );
    }
}
