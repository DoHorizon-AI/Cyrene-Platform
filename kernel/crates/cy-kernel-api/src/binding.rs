//! 设备绑定配置与环境隔离保护.

use std::collections::{BTreeMap, BTreeSet};

use crate::{capability::EnforcementMode, error::ProviderError, inventory::DeviceNode};

/// How an Adapter-owned environment key combines across resources in one Lease.
///
/// Kernel does not interpret vendor names. Adapters mark keys as joinable;
/// exclusive keys still fail closed on conflict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvironmentMerge {
    Exclusive,
    OrderedUniqueJoin,
}

/// 设备绑定配置 (Device Binding)：定义特定沙箱进程对硬件设备的访问授权与环境变量
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceBinding {
    /// 绑定的语义资源 ID
    pub resource_id: String,
    /// 需要注入沙箱的设备节点
    pub nodes: Vec<DeviceNode>,
    /// Adapter 返回、由 Kernel 受控注入的设备环境变量
    pub environment: BTreeMap<String, String>,
    /// Keys that concatenate uniquely (stable first-seen order) when merging
    /// multiple resource bindings. Empty means every key is exclusive.
    pub joinable_environment_keys: BTreeSet<String>,
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

    /// Merge N per-resource bindings into one sandbox binding.
    ///
    /// Joinable keys concatenate unique values in first-seen order. Every other
    /// environment key remains fail-closed on conflict. Kernel does not know
    /// vendor visibility variable names.
    pub fn merge_all(bindings: Vec<Self>) -> Result<Self, ProviderError> {
        let Some(first) = bindings.first().cloned() else {
            return Ok(Self {
                resource_id: "none".to_string(),
                nodes: Vec::new(),
                environment: BTreeMap::new(),
                joinable_environment_keys: BTreeSet::new(),
                required_gids: Vec::new(),
                enforcement: EnforcementMode::Unenforced,
                adapter_id: "kernel-daemon".to_string(),
                reason_code: "NO_RESOURCE_BINDING".to_string(),
            });
        };
        let mut nodes = first.nodes.clone();
        let mut environment = first.environment.clone();
        let mut joinable = first.joinable_environment_keys.clone();
        let mut required_gids = first.required_gids.clone();
        let enforcement = first.enforcement;
        let mut adapter_ids = vec![first.adapter_id.clone()];
        let mut resource_ids = ordered_unique(split_csv(&first.resource_id));
        for binding in bindings.into_iter().skip(1) {
            if binding.enforcement != enforcement {
                return Err(ProviderError::new(
                    "kernel-daemon",
                    "MIXED_RESOURCE_ENFORCEMENT",
                    "a multi-resource binding must use one enforcement mode",
                ));
            }
            if !adapter_ids.contains(&binding.adapter_id) {
                adapter_ids.push(binding.adapter_id.clone());
            }
            for resource_id in split_csv(&binding.resource_id) {
                if !resource_ids.iter().any(|existing| existing == &resource_id) {
                    resource_ids.push(resource_id);
                }
            }
            for node in binding.nodes {
                if !nodes.iter().any(|existing| existing.path == node.path) {
                    nodes.push(node);
                }
            }
            for (key, value) in binding.environment {
                let this_joinable = binding.joinable_environment_keys.contains(&key);
                let already_joinable = joinable.contains(&key);
                if environment.contains_key(&key) && this_joinable != already_joinable {
                    return Err(ProviderError::new(
                        "kernel-daemon",
                        "MIXED_ENVIRONMENT_MERGE",
                        &key,
                    ));
                }
                if this_joinable || already_joinable {
                    joinable.insert(key.clone());
                    let merged = join_unique_csv(environment.get(&key).map(String::as_str), &value);
                    environment.insert(key, merged);
                    continue;
                }
                if let Some(existing) = environment.get(&key) {
                    if existing != &value {
                        return Err(ProviderError::new(
                            "kernel-daemon",
                            "CONFLICTING_RESOURCE_ENVIRONMENT",
                            &key,
                        ));
                    }
                } else {
                    environment.insert(key, value);
                }
            }
            for key in &binding.joinable_environment_keys {
                joinable.insert(key.clone());
            }
            for gid in binding.required_gids {
                if !required_gids.contains(&gid) {
                    required_gids.push(gid);
                }
            }
        }
        adapter_ids.sort();
        Ok(Self {
            resource_id: resource_ids.join(","),
            nodes,
            environment,
            joinable_environment_keys: joinable,
            required_gids,
            enforcement,
            adapter_id: adapter_ids.join(","),
            reason_code: "RESOURCE_BINDING_CREATED_BY_UDS_ADAPTERS".to_string(),
        })
    }
}

fn split_csv(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn ordered_unique(values: Vec<String>) -> Vec<String> {
    let mut unique = Vec::new();
    for value in values {
        if !unique.iter().any(|existing| existing == &value) {
            unique.push(value);
        }
    }
    unique
}

fn join_unique_csv(existing: Option<&str>, incoming: &str) -> String {
    let mut values = existing
        .map(split_csv)
        .unwrap_or_default();
    for value in split_csv(incoming) {
        if !values.iter().any(|existing| existing == &value) {
            values.push(value);
        }
    }
    values.join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding(
        resource_id: &str,
        environment: BTreeMap<String, String>,
        joinable: &[&str],
    ) -> DeviceBinding {
        DeviceBinding {
            resource_id: resource_id.to_string(),
            nodes: Vec::new(),
            environment,
            joinable_environment_keys: joinable.iter().map(|key| (*key).to_string()).collect(),
            required_gids: Vec::new(),
            enforcement: EnforcementMode::Hard,
            adapter_id: "nvidia".to_string(),
            reason_code: "TEST".to_string(),
        }
    }

    #[test]
    fn adapter_binding_environment_is_reserved_without_vendor_knowledge() {
        let sample = binding(
            "accelerator-1",
            BTreeMap::from([("ADAPTER_VISIBLE_DEVICE".to_string(), "1".to_string())]),
            &[],
        );
        let requested =
            BTreeMap::from([("ADAPTER_VISIBLE_DEVICE".to_string(), "other".to_string())]);
        assert_eq!(
            sample
                .merge_environment(&requested)
                .unwrap_err()
                .reason_code,
            "RESERVED_ENVIRONMENT"
        );
    }

    #[test]
    fn one_resource_binding_keeps_single_visibility_value() {
        let merged = DeviceBinding::merge_all(vec![binding(
            "GPU-A",
            BTreeMap::from([("VISIBLE".to_string(), "GPU-A".to_string())]),
            &["VISIBLE"],
        )])
        .unwrap();
        assert_eq!(merged.environment["VISIBLE"], "GPU-A");
        assert_eq!(merged.resource_id, "GPU-A");
    }

    #[test]
    fn joinable_keys_concatenate_unique_values_in_stable_order() {
        let merged = DeviceBinding::merge_all(vec![
            binding(
                "GPU-A",
                BTreeMap::from([("VISIBLE".to_string(), "GPU-A".to_string())]),
                &["VISIBLE"],
            ),
            binding(
                "GPU-C",
                BTreeMap::from([("VISIBLE".to_string(), "GPU-C".to_string())]),
                &["VISIBLE"],
            ),
            binding(
                "GPU-D",
                BTreeMap::from([("VISIBLE".to_string(), "GPU-D".to_string())]),
                &["VISIBLE"],
            ),
        ])
        .unwrap();
        assert_eq!(merged.environment["VISIBLE"], "GPU-A,GPU-C,GPU-D");
        assert_eq!(merged.resource_id, "GPU-A,GPU-C,GPU-D");
    }

    #[test]
    fn duplicate_resource_identities_are_not_repeated() {
        let merged = DeviceBinding::merge_all(vec![
            binding(
                "GPU-A",
                BTreeMap::from([("VISIBLE".to_string(), "GPU-A".to_string())]),
                &["VISIBLE"],
            ),
            binding(
                "GPU-A",
                BTreeMap::from([("VISIBLE".to_string(), "GPU-A".to_string())]),
                &["VISIBLE"],
            ),
        ])
        .unwrap();
        assert_eq!(merged.environment["VISIBLE"], "GPU-A");
        assert_eq!(merged.resource_id, "GPU-A");
    }

    #[test]
    fn exclusive_environment_conflicts_still_fail_closed() {
        let error = DeviceBinding::merge_all(vec![
            binding(
                "r1",
                BTreeMap::from([("EXCLUSIVE".to_string(), "one".to_string())]),
                &[],
            ),
            binding(
                "r2",
                BTreeMap::from([("EXCLUSIVE".to_string(), "two".to_string())]),
                &[],
            ),
        ])
        .unwrap_err();
        assert_eq!(error.reason_code, "CONFLICTING_RESOURCE_ENVIRONMENT");
    }

    #[test]
    fn mixed_joinable_and_exclusive_on_the_same_key_fails_closed() {
        let error = DeviceBinding::merge_all(vec![
            binding(
                "r1",
                BTreeMap::from([("VISIBLE".to_string(), "a".to_string())]),
                &["VISIBLE"],
            ),
            binding(
                "r2",
                BTreeMap::from([("VISIBLE".to_string(), "b".to_string())]),
                &[],
            ),
        ])
        .unwrap_err();
        assert_eq!(error.reason_code, "MIXED_ENVIRONMENT_MERGE");
    }

    #[test]
    fn exclusive_then_joinable_on_the_same_key_fails_closed() {
        let error = DeviceBinding::merge_all(vec![
            binding(
                "r1",
                BTreeMap::from([("VISIBLE".to_string(), "a".to_string())]),
                &[],
            ),
            binding(
                "r2",
                BTreeMap::from([("VISIBLE".to_string(), "b".to_string())]),
                &["VISIBLE"],
            ),
        ])
        .unwrap_err();
        assert_eq!(error.reason_code, "MIXED_ENVIRONMENT_MERGE");
    }
}
