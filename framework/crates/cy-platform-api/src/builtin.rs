//! 内置默认插件实现 (Built-in Plugins).
//!
//! 提供系统默认开箱即用的基础实现：
//! 1. [`BuiltinSystemProbe`]：内置硬件探针默认实现（返回静态硬件清单骨架）；
//! 2. [`BuiltinInMemoryStorage`]：内置纯内存产物存储适配器（用于单机开发、测试与原型验证）。

use crate::{
    ArtifactManifest, PLUGIN_API_VERSION, Plugin, PluginCapabilities, PluginError, PluginKind,
    Probe, Storage,
};
use async_trait::async_trait;
use cy_manifest::{CpuInfo, GpuInfo, HardwareManifest, Interconnect, OsInfo, PrecisionSupport};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// 内置系统硬件探针插件
pub struct BuiltinSystemProbe {
    /// 声明的能力集
    capabilities: PluginCapabilities,
}

impl Default for BuiltinSystemProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl BuiltinSystemProbe {
    /// 创建内置硬件探针实例
    pub fn new() -> Self {
        Self {
            capabilities: PluginCapabilities::default(),
        }
    }
}

impl Plugin for BuiltinSystemProbe {
    fn id(&self) -> &str {
        "com.cy.builtin.probe"
    }

    fn kind(&self) -> PluginKind {
        PluginKind::Probe
    }

    fn api_version(&self) -> &str {
        PLUGIN_API_VERSION
    }

    fn capabilities(&self) -> &PluginCapabilities {
        &self.capabilities
    }
}

#[async_trait]
impl Probe for BuiltinSystemProbe {
    async fn detect_hardware(&self) -> Result<HardwareManifest, PluginError> {
        Ok(HardwareManifest {
            os: OsInfo {
                name: "Linux".to_string(),
                kernel: "5.15.0".to_string(),
                glibc: "2.31".to_string(),
            },
            cpu: CpuInfo {
                arch: "x86_64".to_string(),
                cores: 16,
                threads: Some(32),
            },
            memory_gb: 64.0,
            disk_gb: 1024.0,
            gpus: vec![GpuInfo {
                model: "NVIDIA RTX 4090".to_string(),
                count: 1,
                vram_gb: 24.0,
                compute_capability: "8.9".to_string(),
            }],
            driver_version: "535.104.05".to_string(),
            cuda_max_supported: "12.2".to_string(),
            interconnect: Interconnect {
                nvlink: false,
                pcie_gen: Some(4),
            },
            precision_support: PrecisionSupport {
                bf16: true,
                fp16: true,
                fp8: true,
            },
        })
    }
}

/// 内置纯内存产物存储适配器（用于单机本地测试与临时缓存）
pub struct BuiltinInMemoryStorage {
    /// 声明的能力集
    capabilities: PluginCapabilities,
    /// 内存键值存储 (`artifact_id -> binary_bytes`)
    store: Arc<Mutex<HashMap<String, Vec<u8>>>>,
}

impl Default for BuiltinInMemoryStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl BuiltinInMemoryStorage {
    /// 创建内存存储插件实例
    pub fn new() -> Self {
        Self {
            capabilities: PluginCapabilities::default(),
            store: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl Plugin for BuiltinInMemoryStorage {
    fn id(&self) -> &str {
        "com.cy.builtin.storage.memory"
    }

    fn kind(&self) -> PluginKind {
        PluginKind::Storage
    }

    fn api_version(&self) -> &str {
        PLUGIN_API_VERSION
    }

    fn capabilities(&self) -> &PluginCapabilities {
        &self.capabilities
    }
}

#[async_trait]
impl Storage for BuiltinInMemoryStorage {
    async fn store_artifact(
        &self,
        artifact: &ArtifactManifest,
        data: &[u8],
    ) -> Result<String, PluginError> {
        let artifact_id = artifact
            .artifact_id
            .clone()
            .unwrap_or_else(|| "default_id".to_string());
        let mut store = self.store.lock().unwrap();
        store.insert(artifact_id.clone(), data.to_vec());
        Ok(artifact_id)
    }

    async fn fetch_artifact(&self, artifact_id: &str) -> Result<Vec<u8>, PluginError> {
        let store = self.store.lock().unwrap();
        if let Some(data) = store.get(artifact_id) {
            Ok(data.clone())
        } else {
            Err(PluginError::Execution(format!(
                "Artifact {} not found",
                artifact_id
            )))
        }
    }
}
