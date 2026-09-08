// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: framework/crates/cy-capability-execution-service/src/binding_config.rs ║
// ║ Module: CYRENE Platform                                            ║
// ║ Role: Generic configured-binding process wiring.                    ║
// ║                                                                      ║
// ║ 模块：CYRENE Platform                                                ║
// ║ 职责：通用配置 binding 的进程组装。                                  ║
// ╚══════════════════════════════════════════════════════════════════════╝
//! Shared configuration helpers for the capability execution service.
//!
//! This module is deliberately generic: a configured binding only names an
//! immutable manifest release and supplies worker environment values. It does
//! not select a Product, resolve a replica, or introduce another registry.

use std::{
    collections::{HashMap, HashSet},
    fmt, fs,
    path::Path,
};

use cy_manifest::PluginManifest;
use cy_platform_api::{
    CapabilityBinding, CapabilityRegistry, CapabilityResolutionError, WorkerActivationOptions,
    normalize_repository_manifest,
};

use crate::{CapabilityExecutionConfig, CapabilityExecutionService};

/// One stable configured binding and the environment used by its worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfiguredBinding {
    pub id: String,
    pub environment: HashMap<String, String>,
}

/// Configuration-file errors are kept separate from registry-resolution
/// errors so malformed process configuration fails before the service starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigurationError {
    message: String,
}

impl ConfigurationError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for ConfigurationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message.fmt(formatter)
    }
}

impl std::error::Error for ConfigurationError {}

impl From<std::io::Error> for ConfigurationError {
    fn from(error: std::io::Error) -> Self {
        Self::new(error.to_string())
    }
}

impl From<serde_json::Error> for ConfigurationError {
    fn from(error: serde_json::Error) -> Self {
        Self::new(error.to_string())
    }
}

impl From<String> for ConfigurationError {
    fn from(message: String) -> Self {
        Self::new(message)
    }
}

/// Load either a Platform manifest or a repository plugin manifest.
pub fn load_manifest(path: &Path) -> Result<PluginManifest, ConfigurationError> {
    let value: serde_json::Value = serde_json::from_str(&fs::read_to_string(path)?)?;
    if value.get("plugin").is_some() {
        Ok(serde_json::from_value(value)?)
    } else {
        normalize_repository_manifest(value).map_err(ConfigurationError::from)
    }
}

/// Load and validate the generic binding configuration JSON array.
pub fn load_bindings(path: &Path) -> Result<Vec<ConfiguredBinding>, ConfigurationError> {
    let value: serde_json::Value = serde_json::from_str(&fs::read_to_string(path)?)?;
    let entries = value
        .as_array()
        .ok_or_else(|| ConfigurationError::new("--bindings must contain a JSON array"))?;
    let mut bindings = Vec::with_capacity(entries.len());
    let mut ids = HashSet::with_capacity(entries.len());
    for entry in entries {
        let object = entry.as_object().ok_or_else(|| {
            ConfigurationError::new("each configured binding must be a JSON object")
        })?;
        let id = object
            .get("id")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| {
                ConfigurationError::new("each configured binding requires a non-empty id")
            })?
            .to_string();
        if !ids.insert(id.clone()) {
            return Err(ConfigurationError::new(format!(
                "duplicate configured binding id: {id}"
            )));
        }
        let environment = object
            .get("environment")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| {
                ConfigurationError::new(format!("binding {id} requires an environment object"))
            })?
            .iter()
            .map(|(key, value)| {
                value
                    .as_str()
                    .map(|value| (key.clone(), value.to_string()))
                    .ok_or_else(|| {
                        ConfigurationError::new(format!(
                            "binding {id} environment value {key} must be a string"
                        ))
                    })
            })
            .collect::<Result<HashMap<_, _>, _>>()?;
        bindings.push(ConfiguredBinding { id, environment });
    }
    if bindings.is_empty() {
        return Err(ConfigurationError::new(
            "--bindings must contain at least one configured binding",
        ));
    }
    Ok(bindings)
}

/// Build the one canonical registry/service pair used by the binary and the
/// configured-binding example.
pub fn build_service(
    manifest: PluginManifest,
    base_worker_options: WorkerActivationOptions,
    event_buffer_capacity: usize,
    bindings: Option<Vec<ConfiguredBinding>>,
) -> Result<CapabilityExecutionService, CapabilityResolutionError> {
    let mut registry = CapabilityRegistry::new();
    registry.register(manifest.clone())?;

    if let Some(configured_bindings) = bindings.as_deref() {
        for binding in configured_bindings {
            let registered = CapabilityBinding::new(
                binding.id.clone(),
                manifest.plugin.id.clone(),
                manifest.plugin.version.clone(),
            )
            .map_err(CapabilityResolutionError::InvalidBinding)?;
            registry.register_binding(registered)?;
        }
    }

    let config = CapabilityExecutionConfig {
        worker_options: base_worker_options.clone(),
        application_event_buffer_capacity: event_buffer_capacity,
        ..CapabilityExecutionConfig::default()
    };
    let mut service = CapabilityExecutionService::new(registry, config);
    match bindings {
        Some(configured_bindings) => {
            for binding in configured_bindings {
                let options = WorkerActivationOptions {
                    environment: binding.environment,
                    ..base_worker_options.clone()
                };
                service = service.with_binding_worker_options(binding.id, options);
            }
        }
        None => {
            service = service.with_provider_worker_options(
                manifest.plugin.id,
                manifest.plugin.version,
                base_worker_options,
            );
        }
    }
    Ok(service)
}

#[cfg(test)]
mod tests {
    use std::{env, fs, path::PathBuf};

    use uuid::Uuid;

    use super::*;

    struct TemporaryConfig(PathBuf);

    impl TemporaryConfig {
        fn new(contents: &str) -> Self {
            let path = env::temp_dir().join(format!(
                "cyrene-capability-binding-test-{}.json",
                Uuid::new_v4()
            ));
            fs::write(&path, contents).expect("write temporary binding config");
            Self(path)
        }
    }

    impl Drop for TemporaryConfig {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    #[test]
    fn loads_unique_binding_ids_and_string_environment() {
        let config = TemporaryConfig::new(
            r#"[
                {"id":" main ","environment":{"INSTANCE":"main","EMPTY":""}},
                {"id":"secondary","environment":{}}
            ]"#,
        );

        let bindings = load_bindings(&config.0).expect("valid binding configuration");

        assert_eq!(bindings[0].id, "main");
        assert_eq!(bindings[0].environment["INSTANCE"], "main");
        assert_eq!(bindings[0].environment["EMPTY"], "");
        assert_eq!(bindings[1].id, "secondary");
    }

    #[test]
    fn rejects_duplicate_binding_ids_after_trimming() {
        let config = TemporaryConfig::new(
            r#"[
                {"id":"main","environment":{}},
                {"id":" main ","environment":{}}
            ]"#,
        );

        let error = load_bindings(&config.0).expect_err("duplicate binding IDs must fail");

        assert_eq!(error.to_string(), "duplicate configured binding id: main");
    }

    #[test]
    fn rejects_non_string_environment_values() {
        let config = TemporaryConfig::new(r#"[{"id":"main","environment":{"PORT":1234}}]"#);

        let error = load_bindings(&config.0).expect_err("non-string environment must fail");

        assert_eq!(
            error.to_string(),
            "binding main environment value PORT must be a string"
        );
    }

    #[test]
    fn rejects_empty_binding_ids_and_empty_configuration() {
        let empty_id = TemporaryConfig::new(r#"[{"id":"  ","environment":{}}]"#);
        let error = load_bindings(&empty_id.0).expect_err("empty binding ID must fail");
        assert_eq!(
            error.to_string(),
            "each configured binding requires a non-empty id"
        );

        let empty = TemporaryConfig::new("[]");
        let error = load_bindings(&empty.0).expect_err("empty configuration must fail");
        assert_eq!(
            error.to_string(),
            "--bindings must contain at least one configured binding"
        );
    }
}
