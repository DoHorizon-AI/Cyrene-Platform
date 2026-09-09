use std::{
    collections::BTreeMap,
    io::{BufRead, Write},
    path::PathBuf,
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    ActivationRequest, BindingId, FilesystemPackageRuntime, InstallationId, PackageId,
    PackageRuntimeError, PackageSource, PackageVersion,
};

const CONTROL_PROTOCOL_VERSION: &str = "cy-package-runtime.control.v1";
const MAX_CONTROL_LINE_BYTES: usize = 1024 * 1024;

/// One request on the node-local JSON-lines control channel.
///
/// Paths and Plugin environment values are an internal Platform adapter seam;
/// Product APIs must project only stable package, installation, binding and
/// runtime facts.
#[derive(Debug, Deserialize)]
pub struct ControlRequest {
    pub request_id: String,
    #[serde(flatten)]
    pub command: ControlCommand,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum ControlCommand {
    Authority,
    Inspect {
        descriptor_path: PathBuf,
        archive_path: PathBuf,
    },
    Verify {
        descriptor_path: PathBuf,
        archive_path: PathBuf,
    },
    Install {
        descriptor_path: PathBuf,
        archive_path: PathBuf,
    },
    InstallOffline {
        package_id: String,
        package_version: String,
    },
    GetInstallation {
        installation_id: String,
    },
    ListInstallations,
    Activate {
        binding_id: String,
        installation_id: String,
        #[serde(default)]
        environment: BTreeMap<String, String>,
    },
    RecoverBinding {
        binding_id: String,
        #[serde(default)]
        environment: BTreeMap<String, String>,
    },
    Deactivate {
        binding_id: String,
    },
    RuntimeStatus {
        binding_id: String,
    },
    Upgrade {
        binding_id: String,
        installation_id: String,
        #[serde(default)]
        environment: BTreeMap<String, String>,
    },
    Rollback {
        binding_id: String,
        #[serde(default)]
        environment: BTreeMap<String, String>,
    },
    RemoveBindingReference {
        binding_id: String,
    },
    Uninstall {
        installation_id: String,
    },
    Cleanup,
    OrphanRuntimeCount,
    Shutdown,
}

#[derive(Debug, Serialize)]
pub struct ControlResponse {
    pub request_id: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ControlError>,
}

#[derive(Debug, Serialize)]
pub struct ControlError {
    pub code: String,
    pub message: String,
    pub remediation: String,
}

/// Long-lived owner of package lifecycle and supervised Plugin processes.
pub struct PackageRuntimeControlServer {
    runtime: FilesystemPackageRuntime,
}

impl PackageRuntimeControlServer {
    pub fn new(runtime: FilesystemPackageRuntime) -> Self {
        Self { runtime }
    }

    pub fn run(
        &self,
        input: impl BufRead,
        mut output: impl Write,
    ) -> Result<(), PackageRuntimeError> {
        for line in input.lines() {
            let line = line.map_err(control_io_error("CONTROL_READ_FAILED"))?;
            if line.trim().is_empty() {
                continue;
            }
            if line.len() > MAX_CONTROL_LINE_BYTES {
                self.write_response(
                    &mut output,
                    &failure_response(
                        String::new(),
                        PackageRuntimeError::new(
                            "CONTROL_REQUEST_TOO_LARGE",
                            "control request exceeds the one MiB limit",
                        ),
                    ),
                )?;
                continue;
            }
            let request = match serde_json::from_str::<ControlRequest>(&line) {
                Ok(request) => request,
                Err(error) => {
                    self.write_response(
                        &mut output,
                        &failure_response(
                            String::new(),
                            PackageRuntimeError::new(
                                "CONTROL_REQUEST_INVALID",
                                format!("invalid control request: {error}"),
                            ),
                        ),
                    )?;
                    continue;
                }
            };
            let shutdown = matches!(&request.command, ControlCommand::Shutdown);
            let response = match self.dispatch(request.command) {
                Ok(result) => ControlResponse {
                    request_id: request.request_id,
                    ok: true,
                    result: Some(result),
                    error: None,
                },
                Err(error) => failure_response(request.request_id, error),
            };
            self.write_response(&mut output, &response)?;
            if shutdown {
                break;
            }
        }
        Ok(())
    }

    fn dispatch(&self, command: ControlCommand) -> Result<Value, PackageRuntimeError> {
        match command {
            ControlCommand::Authority => Ok(json!({
                "authority": "platform_package_runtime",
                "protocol_version": CONTROL_PROTOCOL_VERSION,
            })),
            ControlCommand::Inspect {
                descriptor_path,
                archive_path,
            } => to_value(
                self.runtime
                    .inspect(&source(descriptor_path, archive_path))?,
            ),
            ControlCommand::Verify {
                descriptor_path,
                archive_path,
            } => {
                let verified = self
                    .runtime
                    .verify(&source(descriptor_path, archive_path))?;
                to_value(json!({
                    "inspection": verified.inspection,
                    "evidence": verified.evidence,
                }))
            }
            ControlCommand::Install {
                descriptor_path,
                archive_path,
            } => to_value(
                self.runtime
                    .install(&source(descriptor_path, archive_path))?,
            ),
            ControlCommand::InstallOffline {
                package_id,
                package_version,
            } => to_value(self.runtime.install_offline(
                &PackageId::new(package_id)?,
                &PackageVersion::new(package_version)?,
            )?),
            ControlCommand::GetInstallation { installation_id } => to_value(
                self.runtime
                    .get_installation(&InstallationId::new(installation_id)?)?,
            ),
            ControlCommand::ListInstallations => to_value(self.runtime.list_installations()?),
            ControlCommand::Activate {
                binding_id,
                installation_id,
                environment,
            } => to_value(self.runtime.activate(ActivationRequest {
                binding_id: BindingId::new(binding_id)?,
                installation_id: InstallationId::new(installation_id)?,
                environment,
            })?),
            ControlCommand::RecoverBinding {
                binding_id,
                environment,
            } => to_value(
                self.runtime
                    .recover_binding(&BindingId::new(binding_id)?, environment)?,
            ),
            ControlCommand::Deactivate { binding_id } => {
                to_value(self.runtime.deactivate(&BindingId::new(binding_id)?)?)
            }
            ControlCommand::RuntimeStatus { binding_id } => {
                to_value(self.runtime.runtime_status(&BindingId::new(binding_id)?)?)
            }
            ControlCommand::Upgrade {
                binding_id,
                installation_id,
                environment,
            } => to_value(self.runtime.upgrade(
                &BindingId::new(binding_id)?,
                &InstallationId::new(installation_id)?,
                environment,
            )?),
            ControlCommand::Rollback {
                binding_id,
                environment,
            } => to_value(
                self.runtime
                    .rollback(&BindingId::new(binding_id)?, environment)?,
            ),
            ControlCommand::RemoveBindingReference { binding_id } => {
                self.runtime
                    .remove_binding_reference(&BindingId::new(binding_id)?)?;
                Ok(json!({ "removed": true }))
            }
            ControlCommand::Uninstall { installation_id } => {
                self.runtime
                    .uninstall(&InstallationId::new(installation_id)?)?;
                Ok(json!({ "uninstalled": true }))
            }
            ControlCommand::Cleanup => to_value(self.runtime.cleanup()?),
            ControlCommand::OrphanRuntimeCount => {
                Ok(json!({ "orphan_runtime_count": self.runtime.orphan_runtime_count()? }))
            }
            ControlCommand::Shutdown => Ok(json!({ "shutdown": true })),
        }
    }

    fn write_response(
        &self,
        output: &mut impl Write,
        response: &ControlResponse,
    ) -> Result<(), PackageRuntimeError> {
        serde_json::to_writer(&mut *output, response).map_err(json_error)?;
        output
            .write_all(b"\n")
            .map_err(control_io_error("CONTROL_WRITE_FAILED"))?;
        output
            .flush()
            .map_err(control_io_error("CONTROL_WRITE_FAILED"))
    }
}

fn source(descriptor_path: PathBuf, archive_path: PathBuf) -> PackageSource {
    PackageSource {
        descriptor_path,
        archive_path,
    }
}

fn to_value(value: impl Serialize) -> Result<Value, PackageRuntimeError> {
    serde_json::to_value(value).map_err(json_error)
}

fn failure_response(request_id: String, error: PackageRuntimeError) -> ControlResponse {
    ControlResponse {
        request_id,
        ok: false,
        result: None,
        error: Some(ControlError {
            remediation: remediation(&error.code).to_string(),
            code: error.code,
            message: error.message,
        }),
    }
}

fn remediation(code: &str) -> &'static str {
    match code {
        "INSTALLATION_REFERENCED" | "BINDING_RUNNING" => {
            "disable and deactivate Product bindings, then remove their references"
        }
        "OFFLINE_PACKAGE_UNAVAILABLE" => {
            "install and verify this exact official package once while its artifact is available"
        }
        "ARCHIVE_CORRUPT" | "ARTIFACT_CORRUPT" | "CACHE_CORRUPT" | "DEPENDENCY_LOCK_CORRUPT" => {
            "discard the source and obtain the official package and descriptor again"
        }
        "PLUGIN_RUNTIME_UNAVAILABLE" | "PLUGIN_RUNTIME_EXITED" => {
            "inspect Plugin runtime diagnostics and retry activation"
        }
        _ => "inspect the structured code and Platform package runtime diagnostics",
    }
}

fn json_error(error: serde_json::Error) -> PackageRuntimeError {
    PackageRuntimeError::new("CONTROL_JSON_FAILED", error.to_string())
}

fn control_io_error(code: &'static str) -> impl FnOnce(std::io::Error) -> PackageRuntimeError {
    move |error| PackageRuntimeError::new(code, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_contract_distinguishes_all_runtime_identities() {
        let request: ControlRequest = serde_json::from_value(json!({
            "request_id": "request-1",
            "operation": "activate",
            "binding_id": "binding-main",
            "installation_id": "install-a",
            "environment": {"TOKEN": "secret"}
        }))
        .unwrap();
        let ControlCommand::Activate {
            binding_id,
            installation_id,
            environment,
        } = request.command
        else {
            panic!("wrong command");
        };
        assert_eq!(binding_id, "binding-main");
        assert_eq!(installation_id, "install-a");
        assert_eq!(environment["TOKEN"], "secret");
    }

    #[test]
    fn error_response_has_structured_remediation() {
        let response = failure_response(
            "request-2".to_string(),
            PackageRuntimeError::new("INSTALLATION_REFERENCED", "still referenced"),
        );
        let value = serde_json::to_value(response).unwrap();
        assert_eq!(value["ok"], false);
        assert_eq!(value["error"]["code"], "INSTALLATION_REFERENCED");
        assert!(
            value["error"]["remediation"]
                .as_str()
                .unwrap()
                .contains("bindings")
        );
    }
}
