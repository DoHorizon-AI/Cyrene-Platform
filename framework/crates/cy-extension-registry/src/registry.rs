//! 强类型扩展注册中心 (Extension Registry)

use std::collections::HashMap;
use std::sync::Arc;

use cy_platform_api::{
    CompatRule, ExecutionEngine, GatewayFilter, ModelAnalyzer, Notification, Plugin, Probe,
    Quantization, RuntimeBuilder, Storage, TrainingBackend,
};

/// 强类型插件扩展注册中心：集中管理并分发 10 大扩展点的插件实例
#[derive(Default)]
pub struct ExtensionRegistry {
    probes: HashMap<String, Arc<dyn Probe>>,
    model_analyzers: HashMap<String, Arc<dyn ModelAnalyzer>>,
    compat_rules: HashMap<String, Arc<dyn CompatRule>>,
    runtime_builders: HashMap<String, Arc<dyn RuntimeBuilder>>,
    execution_engines: HashMap<String, Arc<dyn ExecutionEngine>>,
    training_backends: HashMap<String, Arc<dyn TrainingBackend>>,
    quantizations: HashMap<String, Arc<dyn Quantization>>,
    gateway_filters: HashMap<String, Arc<dyn GatewayFilter>>,
    notifications: HashMap<String, Arc<dyn Notification>>,
    storages: HashMap<String, Arc<dyn Storage>>,

    all_plugins: HashMap<String, Arc<dyn Plugin>>,
}

impl ExtensionRegistry {
    /// 创建空的扩展注册中心
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册通用插件
    pub fn register_plugin(&mut self, plugin: Arc<dyn Plugin>) {
        self.all_plugins.insert(plugin.id().to_string(), plugin);
    }

    pub fn register_probe<T: Probe + 'static>(&mut self, plugin: Arc<T>) {
        self.all_plugins
            .insert(plugin.id().to_string(), plugin.clone() as Arc<dyn Plugin>);
        self.probes
            .insert(plugin.id().to_string(), plugin as Arc<dyn Probe>);
    }

    pub fn register_model_analyzer<T: ModelAnalyzer + 'static>(&mut self, plugin: Arc<T>) {
        self.all_plugins
            .insert(plugin.id().to_string(), plugin.clone() as Arc<dyn Plugin>);
        self.model_analyzers
            .insert(plugin.id().to_string(), plugin as Arc<dyn ModelAnalyzer>);
    }

    pub fn register_compat_rule<T: CompatRule + 'static>(&mut self, plugin: Arc<T>) {
        self.all_plugins
            .insert(plugin.id().to_string(), plugin.clone() as Arc<dyn Plugin>);
        self.compat_rules
            .insert(plugin.id().to_string(), plugin as Arc<dyn CompatRule>);
    }

    pub fn register_runtime_builder<T: RuntimeBuilder + 'static>(&mut self, plugin: Arc<T>) {
        self.all_plugins
            .insert(plugin.id().to_string(), plugin.clone() as Arc<dyn Plugin>);
        self.runtime_builders
            .insert(plugin.id().to_string(), plugin as Arc<dyn RuntimeBuilder>);
    }

    pub fn register_execution_engine<T: ExecutionEngine + 'static>(&mut self, plugin: Arc<T>) {
        self.all_plugins
            .insert(plugin.id().to_string(), plugin.clone() as Arc<dyn Plugin>);
        self.execution_engines
            .insert(plugin.id().to_string(), plugin as Arc<dyn ExecutionEngine>);
    }

    pub fn register_training_backend<T: TrainingBackend + 'static>(&mut self, plugin: Arc<T>) {
        self.all_plugins
            .insert(plugin.id().to_string(), plugin.clone() as Arc<dyn Plugin>);
        self.training_backends
            .insert(plugin.id().to_string(), plugin as Arc<dyn TrainingBackend>);
    }

    pub fn register_quantization<T: Quantization + 'static>(&mut self, plugin: Arc<T>) {
        self.all_plugins
            .insert(plugin.id().to_string(), plugin.clone() as Arc<dyn Plugin>);
        self.quantizations
            .insert(plugin.id().to_string(), plugin as Arc<dyn Quantization>);
    }

    pub fn register_gateway_filter<T: GatewayFilter + 'static>(&mut self, plugin: Arc<T>) {
        self.all_plugins
            .insert(plugin.id().to_string(), plugin.clone() as Arc<dyn Plugin>);
        self.gateway_filters
            .insert(plugin.id().to_string(), plugin as Arc<dyn GatewayFilter>);
    }

    pub fn register_notification<T: Notification + 'static>(&mut self, plugin: Arc<T>) {
        self.all_plugins
            .insert(plugin.id().to_string(), plugin.clone() as Arc<dyn Plugin>);
        self.notifications
            .insert(plugin.id().to_string(), plugin as Arc<dyn Notification>);
    }

    pub fn register_storage<T: Storage + 'static>(&mut self, plugin: Arc<T>) {
        self.all_plugins
            .insert(plugin.id().to_string(), plugin.clone() as Arc<dyn Plugin>);
        self.storages
            .insert(plugin.id().to_string(), plugin as Arc<dyn Storage>);
    }

    pub fn get_probe(&self, id: &str) -> Option<Arc<dyn Probe>> {
        self.probes.get(id).cloned()
    }

    pub fn get_model_analyzer(&self, id: &str) -> Option<Arc<dyn ModelAnalyzer>> {
        self.model_analyzers.get(id).cloned()
    }

    pub fn get_compat_rule(&self, id: &str) -> Option<Arc<dyn CompatRule>> {
        self.compat_rules.get(id).cloned()
    }

    pub fn get_runtime_builder(&self, id: &str) -> Option<Arc<dyn RuntimeBuilder>> {
        self.runtime_builders.get(id).cloned()
    }

    pub fn get_execution_engine(&self, id: &str) -> Option<Arc<dyn ExecutionEngine>> {
        self.execution_engines.get(id).cloned()
    }

    pub fn get_training_backend(&self, id: &str) -> Option<Arc<dyn TrainingBackend>> {
        self.training_backends.get(id).cloned()
    }

    pub fn get_quantization(&self, id: &str) -> Option<Arc<dyn Quantization>> {
        self.quantizations.get(id).cloned()
    }

    pub fn get_gateway_filter(&self, id: &str) -> Option<Arc<dyn GatewayFilter>> {
        self.gateway_filters.get(id).cloned()
    }

    pub fn get_notification(&self, id: &str) -> Option<Arc<dyn Notification>> {
        self.notifications.get(id).cloned()
    }

    pub fn get_storage(&self, id: &str) -> Option<Arc<dyn Storage>> {
        self.storages.get(id).cloned()
    }

    pub fn list_probes(&self) -> Vec<Arc<dyn Probe>> {
        self.probes.values().cloned().collect()
    }

    pub fn list_model_analyzers(&self) -> Vec<Arc<dyn ModelAnalyzer>> {
        self.model_analyzers.values().cloned().collect()
    }

    pub fn list_compat_rules(&self) -> Vec<Arc<dyn CompatRule>> {
        self.compat_rules.values().cloned().collect()
    }

    pub fn list_runtime_builders(&self) -> Vec<Arc<dyn RuntimeBuilder>> {
        self.runtime_builders.values().cloned().collect()
    }

    pub fn list_execution_engines(&self) -> Vec<Arc<dyn ExecutionEngine>> {
        self.execution_engines.values().cloned().collect()
    }

    pub fn list_training_backends(&self) -> Vec<Arc<dyn TrainingBackend>> {
        self.training_backends.values().cloned().collect()
    }

    pub fn list_quantizations(&self) -> Vec<Arc<dyn Quantization>> {
        self.quantizations.values().cloned().collect()
    }

    pub fn list_gateway_filters(&self) -> Vec<Arc<dyn GatewayFilter>> {
        self.gateway_filters.values().cloned().collect()
    }

    pub fn list_notifications(&self) -> Vec<Arc<dyn Notification>> {
        self.notifications.values().cloned().collect()
    }

    pub fn list_storages(&self) -> Vec<Arc<dyn Storage>> {
        self.storages.values().cloned().collect()
    }

    pub fn list_plugins(&self) -> Vec<Arc<dyn Plugin>> {
        self.all_plugins.values().cloned().collect()
    }
}
