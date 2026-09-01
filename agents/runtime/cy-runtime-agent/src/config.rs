//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 config.rs                                                       │
//! │  Module: cy_runtime_agent::config                                   │
//! │  Role: Runtime Agent immutable launch configuration.                │
//! │                                                                     │
//! │  模块职责：校验控制面、身份、证书、路径与固定 workload 命令。             │
//! └─────────────────────────────────────────────────────────────────────┘

use std::path::PathBuf;
use std::time::Duration;

use cy_kernel_contract::Identity;

use crate::RuntimeAgentError;

/// Immutable launch-time configuration. Control messages cannot replace the command.
#[derive(Debug, Clone)]
pub struct RuntimeAgentConfig {
    pub control_plane_endpoint: String,
    pub control_plane_server_name: String,
    pub control_plane_ca: PathBuf,
    pub client_certificate: PathBuf,
    pub client_key: PathBuf,
    pub artifact_ca: PathBuf,
    pub organization_id: String,
    pub workspace_id: String,
    pub runtime: Identity,
    pub agent_version: String,
    pub enrollment_proof: String,
    pub resume_token: String,
    pub state_dir: PathBuf,
    pub artifact_destination_root: PathBuf,
    pub reconnect_min: Duration,
    pub reconnect_max: Duration,
    pub workload: Vec<String>,
}

impl RuntimeAgentConfig {
    pub fn validate(&self) -> Result<(), RuntimeAgentError> {
        self.runtime.validate().map_err(|error| {
            RuntimeAgentError::Configuration(format!("{}: {}", error.reason_code, error.message))
        })?;
        if !self.control_plane_endpoint.starts_with("https://")
            || self.control_plane_server_name.is_empty()
        {
            return Err(RuntimeAgentError::Configuration(
                "control-plane endpoint must use HTTPS with an explicit server name".to_string(),
            ));
        }
        if self.organization_id.is_empty()
            || self.workspace_id.is_empty()
            || self.agent_version.is_empty()
            || (self.enrollment_proof.is_empty() && self.resume_token.is_empty())
        {
            return Err(RuntimeAgentError::Configuration(
                "organization, workspace, agent version, and enrollment or resume proof are required"
                    .to_string(),
            ));
        }
        if self.workload.is_empty() || self.workload[0].is_empty() {
            return Err(RuntimeAgentError::Configuration(
                "a preconfigured workload command is required after --".to_string(),
            ));
        }
        if !self.state_dir.is_absolute() || !self.artifact_destination_root.is_absolute() {
            return Err(RuntimeAgentError::Configuration(
                "state and Artifact destination roots must be absolute".to_string(),
            ));
        }
        if self.reconnect_min.is_zero()
            || self.reconnect_max.is_zero()
            || self.reconnect_min > self.reconnect_max
        {
            return Err(RuntimeAgentError::Configuration(
                "reconnect bounds must be non-zero and ordered".to_string(),
            ));
        }
        Ok(())
    }
}
