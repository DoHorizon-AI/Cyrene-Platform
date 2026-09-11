use std::{
    collections::{BTreeSet, HashMap},
    io::{BufRead, BufReader, Read},
    path::{Component, Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use cy_manifest::{ExecutionMode, PluginManifest, Runtime};
use serde::Deserialize;

use crate::{BindingId, InstallationId, PackageRuntimeError, RuntimeGeneration, RuntimeState};

const READY_LINE_MAX_BYTES: usize = 16 * 1024;

/// Generic process options for one Plugin-owned service runtime.
#[derive(Debug, Clone)]
pub struct ServiceActivationOptions {
    pub working_dir: Option<PathBuf>,
    pub runtime_executable: Option<String>,
    pub environment: HashMap<String, String>,
    pub startup_timeout: Duration,
    pub shutdown_grace_period: Duration,
}

impl Default for ServiceActivationOptions {
    fn default() -> Self {
        Self {
            working_dir: None,
            runtime_executable: None,
            environment: HashMap::new(),
            startup_timeout: Duration::from_secs(5),
            shutdown_grace_period: Duration::from_secs(2),
        }
    }
}

/// Process authority consumed by the package lifecycle.
///
/// Implementations supervise Plugin processes and publish opaque connection
/// descriptors. Capability payloads never enter this interface.
pub trait PluginServiceSupervisor: Send {
    fn activate(
        &mut self,
        binding_id: &BindingId,
        installation_id: &InstallationId,
        manifest: &PluginManifest,
        options: ServiceActivationOptions,
        generation: RuntimeGeneration,
    ) -> Result<String, PackageRuntimeError>;

    fn deactivate(&mut self, binding_id: &BindingId) -> Result<(), PackageRuntimeError>;

    fn status(&mut self, binding_id: &BindingId) -> Result<RuntimeState, PackageRuntimeError>;

    fn connection_ref(
        &mut self,
        binding_id: &BindingId,
    ) -> Result<Option<String>, PackageRuntimeError>;

    fn active_bindings(&mut self) -> Result<BTreeSet<BindingId>, PackageRuntimeError>;
}

struct SupervisedService {
    installation_id: InstallationId,
    generation: RuntimeGeneration,
    child: Child,
    connection_ref: String,
    shutdown_grace_period: Duration,
}

/// Starts a package-owned process and supervises only its lifecycle and
/// readiness descriptor.
pub struct ProcessPluginServiceSupervisor {
    services: HashMap<BindingId, SupervisedService>,
}

impl ProcessPluginServiceSupervisor {
    pub fn new() -> Self {
        Self {
            services: HashMap::new(),
        }
    }
}

impl Default for ProcessPluginServiceSupervisor {
    fn default() -> Self {
        Self::new()
    }
}

impl PluginServiceSupervisor for ProcessPluginServiceSupervisor {
    fn activate(
        &mut self,
        binding_id: &BindingId,
        installation_id: &InstallationId,
        manifest: &PluginManifest,
        options: ServiceActivationOptions,
        generation: RuntimeGeneration,
    ) -> Result<String, PackageRuntimeError> {
        self.deactivate(binding_id)?;
        let (capability, interface_version) = direct_contract(manifest)?;
        let launch = manifest.plugin.launch.as_ref().ok_or_else(|| {
            PackageRuntimeError::new(
                "PLUGIN_LAUNCH_MISSING",
                format!(
                    "Plugin {} has no package launch command",
                    manifest.plugin.id
                ),
            )
        })?;
        let executable = resolve_executable(launch.executable.as_str(), &options)?;

        let mut command = Command::new(executable);
        command
            .env_clear()
            .args(&launch.args)
            .args(["--capability", capability])
            .args(["--interface-version", interface_version])
            .args(["--listen", "127.0.0.1:0"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        if let Some(working_dir) = &options.working_dir {
            command.current_dir(working_dir);
        }
        for (name, value) in &options.environment {
            command.env(name, value);
        }

        let mut child = command.spawn().map_err(|error| {
            PackageRuntimeError::new(
                "PLUGIN_RUNTIME_UNAVAILABLE",
                format!("could not start Plugin package process: {error}"),
            )
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            PackageRuntimeError::new(
                "PLUGIN_RUNTIME_UNAVAILABLE",
                "Plugin runtime stdout was not available for readiness",
            )
        })?;
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name(format!("plugin-ready-{binding_id}"))
            .spawn(move || {
                let mut reader = BufReader::new(stdout);
                let mut line = String::new();
                let result = (&mut reader)
                    .take((READY_LINE_MAX_BYTES + 1) as u64)
                    .read_line(&mut line)
                    .map(|_| line);
                let _ = ready_tx.send(result);
                let _ = std::io::copy(&mut reader, &mut std::io::sink());
            })
            .map_err(|error| {
                let _ = child.kill();
                PackageRuntimeError::new(
                    "PLUGIN_RUNTIME_UNAVAILABLE",
                    format!("could not start Plugin readiness reader: {error}"),
                )
            })?;

        let ready_line = match ready_rx.recv_timeout(options.startup_timeout) {
            Ok(Ok(line)) => line,
            Ok(Err(error)) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(PackageRuntimeError::new(
                    "PLUGIN_READINESS_FAILED",
                    format!("could not read Plugin readiness: {error}"),
                ));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(PackageRuntimeError::new(
                    "PLUGIN_READINESS_TIMEOUT",
                    "Plugin runtime did not publish readiness before its deadline",
                ));
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(PackageRuntimeError::new(
                    "PLUGIN_READINESS_FAILED",
                    "Plugin readiness channel closed before a descriptor was published",
                ));
            }
        };
        let connection_ref = parse_readiness(&ready_line, capability, interface_version)
            .inspect_err(|_| {
                let _ = child.kill();
                let _ = child.wait();
            })?;
        if child.try_wait().map_err(process_poll_error)?.is_some() {
            return Err(PackageRuntimeError::new(
                "PLUGIN_RUNTIME_EXITED",
                "Plugin runtime exited immediately after publishing readiness",
            ));
        }

        self.services.insert(
            binding_id.clone(),
            SupervisedService {
                installation_id: installation_id.clone(),
                generation,
                child,
                connection_ref: connection_ref.clone(),
                shutdown_grace_period: options.shutdown_grace_period,
            },
        );
        Ok(connection_ref)
    }

    fn deactivate(&mut self, binding_id: &BindingId) -> Result<(), PackageRuntimeError> {
        if let Some(mut service) = self.services.remove(binding_id) {
            terminate(&mut service.child, service.shutdown_grace_period)?;
        }
        Ok(())
    }

    fn status(&mut self, binding_id: &BindingId) -> Result<RuntimeState, PackageRuntimeError> {
        let Some(service) = self.services.get_mut(binding_id) else {
            return Ok(RuntimeState::Stopped);
        };
        let _ = (&service.installation_id, service.generation);
        service
            .child
            .try_wait()
            .map_err(process_poll_error)
            .map(|status| {
                if status.is_none() {
                    RuntimeState::Running
                } else {
                    RuntimeState::Failed
                }
            })
    }

    fn connection_ref(
        &mut self,
        binding_id: &BindingId,
    ) -> Result<Option<String>, PackageRuntimeError> {
        if self.status(binding_id)? != RuntimeState::Running {
            return Ok(None);
        }
        Ok(self
            .services
            .get(binding_id)
            .map(|service| service.connection_ref.clone()))
    }

    fn active_bindings(&mut self) -> Result<BTreeSet<BindingId>, PackageRuntimeError> {
        let ids = self.services.keys().cloned().collect::<Vec<_>>();
        let mut active = BTreeSet::new();
        for binding_id in ids {
            if self.status(&binding_id)? == RuntimeState::Running {
                active.insert(binding_id);
            }
        }
        Ok(active)
    }
}

fn resolve_executable(
    requested: &str,
    options: &ServiceActivationOptions,
) -> Result<PathBuf, PackageRuntimeError> {
    if requested == "prepared-runtime" {
        return options
            .runtime_executable
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| {
                PackageRuntimeError::new(
                    "PLUGIN_RUNTIME_UNAVAILABLE",
                    "package requested a prepared runtime executable but none was produced",
                )
            });
    }

    let relative = Path::new(requested);
    if requested.trim().is_empty()
        || relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir
                    | Component::RootDir
                    | Component::Prefix(_)
                    | Component::CurDir
            )
        })
    {
        return Err(PackageRuntimeError::new(
            "PLUGIN_LAUNCH_INVALID",
            "package launch executable must be prepared-runtime or a safe package-relative path",
        ));
    }
    let root = options.working_dir.as_ref().ok_or_else(|| {
        PackageRuntimeError::new(
            "PLUGIN_LAUNCH_INVALID",
            "a package-relative launch executable requires a package root",
        )
    })?;
    let executable = root.join(relative);
    if !executable.is_file() {
        return Err(PackageRuntimeError::new(
            "PLUGIN_RUNTIME_UNAVAILABLE",
            format!(
                "package launch executable does not exist: {}",
                executable.display()
            ),
        ));
    }
    Ok(executable)
}

impl Drop for ProcessPluginServiceSupervisor {
    fn drop(&mut self) {
        for (_, mut service) in self.services.drain() {
            let _ = terminate(&mut service.child, service.shutdown_grace_period);
        }
    }
}

#[derive(Debug, Deserialize)]
struct ReadyDescriptor {
    event: String,
    connection_ref: String,
    capability: String,
    interface_version: String,
}

fn direct_contract(manifest: &PluginManifest) -> Result<(&str, &str), PackageRuntimeError> {
    if manifest.plugin.runtime != Some(Runtime::Service) {
        return Err(PackageRuntimeError::new(
            "PLUGIN_RUNTIME_UNSUPPORTED",
            "Package activation requires a Plugin-owned service runtime",
        ));
    }
    let descriptors = manifest
        .capability_descriptors
        .iter()
        .filter(|descriptor| descriptor.supports_mode(ExecutionMode::Service))
        .collect::<Vec<_>>();
    if descriptors.len() != 1 {
        return Err(PackageRuntimeError::new(
            "PLUGIN_SERVICE_AMBIGUOUS",
            "One binding must resolve to exactly one service capability and interface version",
        ));
    }
    Ok((
        descriptors[0].id.id.as_str(),
        descriptors[0].interface_version.version.as_str(),
    ))
}

fn parse_readiness(
    line: &str,
    expected_capability: &str,
    expected_interface_version: &str,
) -> Result<String, PackageRuntimeError> {
    if line.len() > READY_LINE_MAX_BYTES {
        return Err(PackageRuntimeError::new(
            "PLUGIN_READINESS_INVALID",
            "Plugin readiness descriptor exceeds 16 KiB",
        ));
    }
    let ready: ReadyDescriptor = serde_json::from_str(line).map_err(|error| {
        PackageRuntimeError::new(
            "PLUGIN_READINESS_INVALID",
            format!("Plugin readiness descriptor is invalid JSON: {error}"),
        )
    })?;
    if ready.event != "direct_plugin_ready"
        || ready.capability != expected_capability
        || ready.interface_version != expected_interface_version
    {
        return Err(PackageRuntimeError::new(
            "PLUGIN_READINESS_MISMATCH",
            "Plugin readiness does not match the activated service contract",
        ));
    }
    let connection_ref = ready.connection_ref.trim();
    if connection_ref.is_empty()
        || connection_ref.len() > 2048
        || connection_ref.chars().any(char::is_control)
    {
        return Err(PackageRuntimeError::new(
            "PLUGIN_CONNECTION_REF_INVALID",
            "Plugin readiness contains an invalid connection reference",
        ));
    }
    Ok(connection_ref.to_string())
}

fn process_poll_error(error: std::io::Error) -> PackageRuntimeError {
    PackageRuntimeError::new(
        "PLUGIN_RUNTIME_UNAVAILABLE",
        format!("could not poll Plugin runtime process: {error}"),
    )
}

fn terminate(child: &mut Child, grace: Duration) -> Result<(), PackageRuntimeError> {
    if child.try_wait().map_err(process_poll_error)?.is_some() {
        return Ok(());
    }
    #[cfg(unix)]
    {
        let _ = Command::new("kill")
            .arg("-TERM")
            .arg(child.id().to_string())
            .status();
        let deadline = Instant::now() + grace;
        while Instant::now() < deadline {
            if child.try_wait().map_err(process_poll_error)?.is_some() {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
    child.kill().map_err(|error| {
        PackageRuntimeError::new(
            "PLUGIN_SHUTDOWN_FAILED",
            format!("could not terminate Plugin runtime: {error}"),
        )
    })?;
    child.wait().map_err(|error| {
        PackageRuntimeError::new(
            "PLUGIN_SHUTDOWN_FAILED",
            format!("could not reap Plugin runtime: {error}"),
        )
    })?;
    Ok(())
}
