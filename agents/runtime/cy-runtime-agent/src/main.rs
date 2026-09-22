//! Container-oriented `cy-runtime-agent run -- <workload command>` entry point.

use std::env;
use std::path::PathBuf;
use std::time::Duration;

use cy_kernel_contract::Identity;
use cy_proto::core_v1::NodeRef;
use cy_runtime_agent::{run_runtime_agent, RuntimeAgentConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let log_format = std::env::var("CYRENE_LOG_FORMAT")
        .ok()
        .and_then(|f| f.parse().ok())
        .unwrap_or(cy_observability::LogFormat::Json);
    let log_level = std::env::var("CYRENE_LOG_LEVEL")
        .or_else(|_| std::env::var("RUST_LOG"))
        .unwrap_or_else(|_| "info".to_string());
    let obs_config = cy_observability::ObservabilityConfig::managed("cy-runtime-agent")
        .with_format(log_format)
        .with_log_level(log_level);
    let _guard = cy_observability::init_observability(obs_config).ok();

    let config = match Args::parse().and_then(Args::into_config) {
        Ok(c) => c,
        Err(err) => {
            tracing::error!(
                event.name = "platform.service.startup_failed",
                error.code = cy_observability::PlatformErrorCode::NodeConnectFailed.as_str(),
                message = "Runtime agent configuration parsing failed",
                error = %err,
            );
            return Err(err);
        }
    };

    tracing::info!(
        event.name = cy_observability::EVENT_SERVICE_STARTED,
        message = "Starting Cyrene Runtime Agent",
        node_id = %config.node.node_id,
        runtime_id = %config.runtime.id,
    );

    let result = run_runtime_agent(config).await;
    match result {
        Ok(()) => {
            tracing::info!(
                event.name = cy_observability::EVENT_SERVICE_STOPPED,
                message = "Runtime agent finished successfully",
            );
            Ok(())
        }
        Err(ref err) => {
            tracing::error!(
                event.name = "platform.service.terminated_unexpectedly",
                error.code = cy_observability::PlatformErrorCode::NodeConnectFailed.as_str(),
                message = "Runtime agent terminated with error",
                error = %err,
            );
            result.map_err(Into::into)
        }
    }
}

struct Args {
    control_plane: String,
    server_name: String,
    control_ca: PathBuf,
    client_certificate: PathBuf,
    client_key: PathBuf,
    artifact_ca: PathBuf,
    artifact_ticket_key: Option<PathBuf>,
    organization_id: String,
    workspace_id: String,
    node_id: String,
    node_epoch: u64,
    node_type: String,
    persistent: bool,
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
            artifact_ticket_key: flag_value(&arguments[1..separator], "--artifact-ticket-key")
                .or_else(|| env::var("CYRENE_ARTIFACT_TICKET_KEY").ok())
                .filter(|value| !value.is_empty())
                .map(PathBuf::from),
            organization_id: value("--organization-id", "CYRENE_ORGANIZATION_ID")?,
            workspace_id: value("--workspace-id", "CYRENE_WORKSPACE_ID")?,
            node_id: value("--node-id", "CYRENE_NODE_ID")?,
            node_epoch: value("--node-epoch", "CYRENE_NODE_EPOCH")?.parse()?,
            node_type: value("--node-type", "CYRENE_NODE_TYPE")?,
            persistent: parse_bool(&value("--persistent", "CYRENE_NODE_PERSISTENT")?)?,
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
            artifact_ticket_key: self.artifact_ticket_key,
            organization_id: self.organization_id,
            workspace_id: self.workspace_id,
            node: NodeRef {
                node_id: self.node_id,
                node_epoch: self.node_epoch,
            },
            node_type: self.node_type,
            persistent: self.persistent,
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

fn parse_bool(value: &str) -> Result<bool, std::io::Error> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "persistent must be explicitly true or false",
        )),
    }
}

fn usage() -> std::io::Error {
    std::io::Error::other(
        "usage: cy-runtime-agent run [control and identity options] -- <fixed workload command>",
    )
}
