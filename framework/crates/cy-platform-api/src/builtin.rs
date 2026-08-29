//! 内置默认插件实现 (Built-in Plugins).
//!
//! 提供仅供开发和测试使用的非硬件基础实现：
//! [`BuiltinInMemoryStorage`]：内置纯内存产物存储适配器。

use crate::{
    ArtifactManifest, PLUGIN_API_VERSION, Plugin, PluginCapabilities, PluginError, PluginKind,
    Storage,
};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

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
