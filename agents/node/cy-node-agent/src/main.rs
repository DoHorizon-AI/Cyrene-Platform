// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: agents/node/cy-node-agent/src/main.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! Linux Node Agent binary: outbound mTLS control plane + local Kernel UDS.

use std::{env, path::PathBuf, time::Duration};

use cy_node_agent::{run_node_agent, NodeAgentConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let log_format = std::env::var("CYRENE_LOG_FORMAT")
        .ok()
        .and_then(|f| f.parse().ok())
        .unwrap_or(cy_observability::LogFormat::Json);
    let log_level = std::env::var("CYRENE_LOG_LEVEL")
        .or_else(|_| std::env::var("RUST_LOG"))
        .unwrap_or_else(|_| "info".to_string());
    let obs_config = cy_observability::ObservabilityConfig::managed("cy-node-agent")
        .with_format(log_format)
        .with_log_level(log_level);
    let _guard = cy_observability::init_observability(obs_config).ok();

    let config = match Args::parse().and_then(|a| a.into_config().map_err(Into::into)) {
        Ok(c) => c,
        Err(err) => {
            tracing::error!(
                event.name = "platform.service.startup_failed",
                error.code = cy_observability::PlatformErrorCode::NodeConnectFailed.as_str(),
                message = "Node agent configuration error",
                error = %err,
            );
            return Err(err);
        }
    };

    tracing::info!(
        event.name = cy_observability::EVENT_SERVICE_STARTED,
        message = "Starting Cyrene Node Agent",
        node_id = %config.node_id,
    );

    tokio::select! {
        result = run_node_agent(config) => {
            if let Err(ref err) = result {
                tracing::error!(
                    event.name = "platform.service.terminated_unexpectedly",
                    error.code = cy_observability::PlatformErrorCode::NodeConnectFailed.as_str(),
                    message = "Node agent loop terminated with error",
                    error = %err,
                );
            }
            result.map_err(Into::into)
        }
        signal = tokio::signal::ctrl_c() => {
            signal?;
            tracing::info!(
                event.name = cy_observability::EVENT_SERVICE_STOPPED,
                message = "Node agent stopped by signal",
            );
            Ok(())
        }
    }
}

#[derive(Debug)]
struct Args {
    control_plane_endpoint: String,
    control_plane_server_name: String,
    control_plane_ca: PathBuf,
    client_certificate: PathBuf,
    client_key: PathBuf,
    kernel_socket: PathBuf,
    node_id: String,
    agent_version: String,
    resume_token: String,
    reconnect_min: Duration,
    reconnect_max: Duration,
}

impl Args {
    fn parse() -> Result<Self, Box<dyn std::error::Error>> {
        let mut control_plane_endpoint = env_value("CYRENE_CONTROL_PLANE_ENDPOINT");
        let mut control_plane_server_name = env_value("CYRENE_CONTROL_PLANE_SERVER_NAME");
        let mut control_plane_ca = env_value("CYRENE_CONTROL_PLANE_CA").map(PathBuf::from);
        let mut client_certificate = env_value("CYRENE_AGENT_CLIENT_CERT").map(PathBuf::from);
        let mut client_key = env_value("CYRENE_AGENT_CLIENT_KEY").map(PathBuf::from);
        let mut kernel_socket = PathBuf::from("/run/cyrene/kernel.sock");
        let mut node_id = env_value("CYRENE_NODE_ID");
        let mut agent_version = env_value("CYRENE_AGENT_VERSION")
            .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
        let mut resume_token = env_value("CYRENE_AGENT_RESUME_TOKEN").unwrap_or_default();
        let mut reconnect_min = Duration::from_secs(1);
        let mut reconnect_max = Duration::from_secs(30);
        let mut values = env::args().skip(1);

        while let Some(argument) = values.next() {
            let mut value = || {
                values.next().ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        format!("{argument} requires a value"),
                    )
                })
            };
            match argument.as_str() {
                "--control-plane" => control_plane_endpoint = Some(value()?),
                "--control-plane-server-name" => control_plane_server_name = Some(value()?),
                "--control-plane-ca" => control_plane_ca = Some(PathBuf::from(value()?)),
                "--client-certificate" => client_certificate = Some(PathBuf::from(value()?)),
                "--client-key" => client_key = Some(PathBuf::from(value()?)),
                "--kernel-socket" => kernel_socket = PathBuf::from(value()?),
                "--node-id" => node_id = Some(value()?),
                "--agent-version" => agent_version = value()?,
                "--resume-token" => resume_token = value()?,
                "--reconnect-min-ms" => reconnect_min = Duration::from_millis(value()?.parse()?),
                "--reconnect-max-ms" => reconnect_max = Duration::from_millis(value()?.parse()?),
                "--help" | "-h" => return Err(usage().into()),
                _ => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        format!("unknown argument: {argument}"),
                    )
                    .into())
                }
            }
        }

        Ok(Self {
            control_plane_endpoint: required(control_plane_endpoint, "control-plane endpoint")?,
            control_plane_server_name: required(
                control_plane_server_name,
                "control-plane server name",
            )?,
            control_plane_ca: required(control_plane_ca, "control-plane CA")?,
            client_certificate: required(client_certificate, "client certificate")?,
            client_key: required(client_key, "client key")?,
            kernel_socket,
            node_id: required(node_id, "node id")?,
            agent_version,
            resume_token,
            reconnect_min,
            reconnect_max,
        })
    }

    fn into_config(self) -> Result<NodeAgentConfig, Box<dyn std::error::Error>> {
        let config = NodeAgentConfig {
            control_plane_endpoint: self.control_plane_endpoint,
            control_plane_server_name: self.control_plane_server_name,
            control_plane_ca: self.control_plane_ca,
            client_certificate: self.client_certificate,
            client_key: self.client_key,
            kernel_socket: self.kernel_socket,
            node_id: self.node_id,
            agent_version: self.agent_version,
            min_protocol_version: 1,
            max_protocol_version: 1,
            resume_token: self.resume_token,
            reconnect_min: self.reconnect_min,
            reconnect_max: self.reconnect_max,
        };
        config.validate()?;
        Ok(config)
    }
}

fn env_value(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.is_empty())
}

fn required<T>(value: Option<T>, name: &str) -> Result<T, std::io::Error> {
    value.ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{name} is required; supply the argument or its CYRENE_* environment value"),
        )
    })
}

fn usage() -> std::io::Error {
    std::io::Error::other(
        "usage: cy-node-agent --control-plane https://host:port --control-plane-server-name DNS_NAME --control-plane-ca /absolute/ca.pem --client-certificate /absolute/client.pem --client-key /absolute/client.key --node-id NODE_ID [--kernel-socket /run/cyrene/kernel.sock] [--agent-version VERSION] [--resume-token TOKEN] [--reconnect-min-ms N] [--reconnect-max-ms N]",
    )
}
