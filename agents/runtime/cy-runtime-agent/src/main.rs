//! Container-oriented `cy-runtime-agent run -- <workload command>` entry point.

use std::env;
use std::path::PathBuf;
use std::time::Duration;

use cy_kernel_contract::Identity;
use cy_runtime_agent::{run_runtime_agent, RuntimeAgentConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    run_runtime_agent(Args::parse()?.into_config()?).await?;
    Ok(())
}

struct Args {
    control_plane: String,
    server_name: String,
    control_ca: PathBuf,
    client_certificate: PathBuf,
    client_key: PathBuf,
    artifact_ca: PathBuf,
    organization_id: String,
    workspace_id: String,
    runtime_id: String,
    runtime_generation: u64,
    enrollment_proof: String,
    resume_token: String,
    state_dir: PathBuf,
    artifact_root: PathBuf,
    workload: Vec<String>,
}

impl Args {
    fn parse() -> Result<Self, Box<dyn std::error::Error>> {
        let arguments = env::args().skip(1).collect::<Vec<_>>();
        if arguments.first().map(String::as_str) != Some("run") {
            return Err(usage().into());
        }
        let separator = arguments
            .iter()
            .position(|argument| argument == "--")
            .ok_or_else(usage)?;
        let workload = arguments[separator + 1..].to_vec();
        if workload.is_empty() {
            return Err(usage().into());
        }
        let value = |name: &str, environment: &str| -> Result<String, std::io::Error> {
            flag_value(&arguments[1..separator], name)
                .or_else(|| env::var(environment).ok().filter(|value| !value.is_empty()))
                .ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        format!("{name} or {environment} is required"),
                    )
                })
        };
        Ok(Self {
            control_plane: value("--control-plane", "CYRENE_CONTROL_PLANE_ENDPOINT")?,
            server_name: value(
                "--control-plane-server-name",
                "CYRENE_CONTROL_PLANE_SERVER_NAME",
            )?,
            control_ca: value("--control-plane-ca", "CYRENE_CONTROL_PLANE_CA")
                .map(PathBuf::from)?,
            client_certificate: value("--client-certificate", "CYRENE_AGENT_CLIENT_CERT")
                .map(PathBuf::from)?,
            client_key: value("--client-key", "CYRENE_AGENT_CLIENT_KEY").map(PathBuf::from)?,
            artifact_ca: value("--artifact-ca", "CYRENE_ARTIFACT_CA").map(PathBuf::from)?,
            organization_id: value("--organization-id", "CYRENE_ORGANIZATION_ID")?,
            workspace_id: value("--workspace-id", "CYRENE_WORKSPACE_ID")?,
            runtime_id: value("--runtime-id", "CYRENE_RUNTIME_ID")?,
            runtime_generation: value("--runtime-generation", "CYRENE_RUNTIME_GENERATION")?
                .parse()?,
            enrollment_proof: flag_value(&arguments[1..separator], "--enrollment-proof")
                .or_else(|| env::var("CYRENE_ENROLLMENT_PROOF").ok())
                .unwrap_or_default(),
            resume_token: flag_value(&arguments[1..separator], "--resume-token")
                .or_else(|| env::var("CYRENE_AGENT_RESUME_TOKEN").ok())
                .unwrap_or_default(),
            state_dir: flag_value(&arguments[1..separator], "--state-dir")
                .or_else(|| env::var("CYRENE_AGENT_STATE_DIR").ok())
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/var/lib/cyrene/runtime-agent")),
            artifact_root: flag_value(&arguments[1..separator], "--artifact-root")
                .or_else(|| env::var("CYRENE_ARTIFACT_ROOT").ok())
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/var/lib/cyrene/artifacts")),
            workload,
        })
    }

    fn into_config(self) -> Result<RuntimeAgentConfig, Box<dyn std::error::Error>> {
        let config = RuntimeAgentConfig {
            control_plane_endpoint: self.control_plane,
            control_plane_server_name: self.server_name,
            control_plane_ca: self.control_ca,
            client_certificate: self.client_certificate,
            client_key: self.client_key,
            artifact_ca: self.artifact_ca,
            organization_id: self.organization_id,
            workspace_id: self.workspace_id,
            runtime: Identity {
                id: self.runtime_id,
                generation: self.runtime_generation,
            },
            agent_version: env!("CARGO_PKG_VERSION").to_string(),
            enrollment_proof: self.enrollment_proof,
            resume_token: self.resume_token,
            state_dir: self.state_dir,
            artifact_destination_root: self.artifact_root,
            reconnect_min: Duration::from_millis(100),
            reconnect_max: Duration::from_secs(5),
            workload: self.workload,
        };
        config.validate()?;
        Ok(config)
    }
}

fn flag_value(arguments: &[String], name: &str) -> Option<String> {
    arguments
        .windows(2)
        .find_map(|pair| (pair[0] == name).then(|| pair[1].clone()))
}

fn usage() -> std::io::Error {
    std::io::Error::other(
        "usage: cy-runtime-agent run [control and identity options] -- <fixed workload command>",
    )
}
