// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: agents/node/cy-node-agent/src/daemon.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! Deployable outbound Node Agent runtime.
//!
//! The Agent owns the remote mTLS connection and forwards type-safe Core v1
//! commands over the protected local Kernel UDS. It is intentionally outside
//! the Kernel process: a control-plane outage, certificate rotation, or Agent
//! restart cannot place network parsing or reconnect policy in Kernel memory.

use std::{fs, path::PathBuf, time::Duration};

use cy_proto::core_v1::{
    node_control_service_client::NodeControlServiceClient, ControlPlaneToNode, NodeRef,
    NodeToControlPlane,
};
use thiserror::Error;
use tokio::{sync::mpsc, time};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{
    transport::{Certificate, Channel, ClientTlsConfig, Endpoint, Identity},
    Request,
};

use crate::{
    NodeCommandBridge, NodeControlSession, NodeControlSessionError, UdsKernelCommandExecutor,
};

#[derive(Debug, Clone)]
pub struct NodeAgentConfig {
    pub control_plane_endpoint: String,
    pub control_plane_server_name: String,
    pub control_plane_ca: PathBuf,
    pub client_certificate: PathBuf,
    pub client_key: PathBuf,
    pub kernel_socket: PathBuf,
    pub node_id: String,
    pub agent_version: String,
    pub min_protocol_version: u32,
    pub max_protocol_version: u32,
    pub resume_token: String,
    pub reconnect_min: Duration,
    pub reconnect_max: Duration,
}

#[derive(Debug, Error)]
pub enum NodeAgentError {
    #[error("Node Agent configuration is invalid: {0}")]
    Configuration(String),
    #[error("failed to read mTLS material from {path}: {source}")]
    CredentialRead {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("mTLS material at {0} is empty")]
    EmptyCredential(PathBuf),
    #[error("control-plane transport failed: {0}")]
    Transport(String),
    #[error("local Kernel bridge failed: {0}")]
    LocalKernel(String),
    #[error("node-control session rejected a frame: {0}")]
    Session(#[from] NodeControlSessionError),
}

impl NodeAgentConfig {
    pub fn validate(&self) -> Result<(), NodeAgentError> {
        if !self.control_plane_endpoint.starts_with("https://") {
            return Err(NodeAgentError::Configuration(
                "control-plane endpoint must use https:// for mTLS".to_string(),
            ));
        }
        if self.control_plane_server_name.is_empty() || self.node_id.is_empty() {
            return Err(NodeAgentError::Configuration(
                "control-plane server name and node id are required".to_string(),
            ));
        }
        if self.min_protocol_version == 0 || self.min_protocol_version > self.max_protocol_version {
            return Err(NodeAgentError::Configuration(
                "protocol version range is invalid".to_string(),
            ));
        }
        if self.reconnect_min.is_zero()
            || self.reconnect_max.is_zero()
            || self.reconnect_min > self.reconnect_max
        {
            return Err(NodeAgentError::Configuration(
                "reconnect bounds must be non-zero and ordered".to_string(),
            ));
        }
        UdsKernelCommandExecutor::new(self.kernel_socket.clone())
            .map_err(|error| NodeAgentError::Configuration(error.to_string()))?;
        Ok(())
    }

    fn tls_config(&self) -> Result<ClientTlsConfig, NodeAgentError> {
        let ca = read_credential(&self.control_plane_ca, false)?;
        let certificate = read_credential(&self.client_certificate, false)?;
        let key = read_credential(&self.client_key, true)?;
        Ok(ClientTlsConfig::new()
            .domain_name(self.control_plane_server_name.clone())
            .ca_certificate(Certificate::from_pem(ca))
            .identity(Identity::from_pem(certificate, key)))
    }
}

/// Runs a single Agent forever. Every reconnect rereads the local Kernel's
/// NodeRef. If a Kernel restart changed the epoch, the previous resume cursor
/// is discarded and the control plane sees a fenced new node epoch.
// ════════════════════════════════════════════════════════════════════════════
// 🔧 FUNCTION: run_node_agent
//
//   Maintains the outbound node session and reconnects from fresh local Kernel
//   identity evidence, fencing the previous epoch when it changes.
//
//   维护出站节点会话，并依据本地 Kernel 的最新身份事实重连；节点纪元变化时，
//   丢弃旧游标并围栏之前的会话。
// ════════════════════════════════════════════════════════════════════════════
pub async fn run_node_agent(config: NodeAgentConfig) -> Result<(), NodeAgentError> {
    config.validate()?;
    let kernel = UdsKernelCommandExecutor::new(config.kernel_socket.clone())
        .map_err(|error| NodeAgentError::Configuration(error.to_string()))?;
    let bridge = NodeCommandBridge::new(kernel.clone());
    let mut resume_token = config.resume_token.clone();
    let mut observed_node: Option<NodeRef> = None;
    let mut delay = config.reconnect_min;
    let mut retry_count: u64 = 0;
    let mut first_failure_time: Option<std::time::Instant> = None;
    let mut last_warn_time: Option<std::time::Instant> = None;
    let mut connected_before = false;

    loop {
        let node = match kernel.discover_node().await {
            Ok(node) => node,
            Err(error) => {
                let is_first = first_failure_time.is_none();
                let first = first_failure_time.get_or_insert_with(std::time::Instant::now);
                retry_count += 1;
                let now = std::time::Instant::now();
                if is_first
                    || last_warn_time
                        .is_none_or(|t| now.duration_since(t) >= Duration::from_secs(10))
                {
                    last_warn_time = Some(now);
                    tracing::warn!(
                        event.name = "platform.node.reconnecting",
                        error.code = "PLATFORM.KERNEL.UNKNOWN_ERROR",
                        node_id = %config.node_id,
                        retries = retry_count,
                        elapsed_ms = now.duration_since(*first).as_millis() as u64,
                        error = %error,
                        message = "Local Kernel discovery failed; retrying with backoff",
                    );
                }
                time::sleep(delay).await;
                delay = backoff(delay, config.reconnect_max);
                if matches!(error, crate::LocalKernelError::RelativeSocketPath) {
                    return Err(NodeAgentError::LocalKernel(error.to_string()));
                }
                continue;
            }
        };
        if node.node_id != config.node_id {
            return Err(NodeAgentError::Configuration(format!(
                "configured node id {} does not match local Kernel node id {}",
                config.node_id, node.node_id
            )));
        }
        if observed_node
            .as_ref()
            .is_some_and(|previous| previous.node_epoch != node.node_epoch)
        {
            resume_token.clear();
        }
        observed_node = Some(node.clone());

        match connect_once(&config, &bridge, node, &resume_token).await {
            Ok(next_resume_token) => {
                if retry_count > 0 || first_failure_time.is_some() {
                    let elapsed_ms = first_failure_time
                        .map(|t| t.elapsed().as_millis() as u64)
                        .unwrap_or(0);
                    tracing::info!(
                        event.name = "platform.node.reconnected",
                        node_id = %config.node_id,
                        retries = retry_count,
                        elapsed_ms = elapsed_ms,
                        message = "Node successfully reconnected to control plane",
                    );
                }
                connected_before = true;
                retry_count = 0;
                first_failure_time = None;
                last_warn_time = None;
                resume_token = next_resume_token;
                delay = config.reconnect_min;
            }
            Err(error) => {
                let is_first = first_failure_time.is_none();
                let first = first_failure_time.get_or_insert_with(std::time::Instant::now);
                retry_count += 1;
                let now = std::time::Instant::now();
                if is_first {
                    last_warn_time = Some(now);
                    if connected_before {
                        tracing::warn!(
                            event.name = "platform.node.disconnected",
                            error.code = "PLATFORM.NODE.DISCONNECTED",
                            node_id = %config.node_id,
                            error = %error,
                            message = "Node disconnected from control plane; entering reconnect loop",
                        );
                    } else {
                        tracing::warn!(
                            event.name = "platform.node.reconnecting",
                            error.code = "PLATFORM.NODE.CONNECT_FAILED",
                            node_id = %config.node_id,
                            retries = retry_count,
                            error = %error,
                            message = "Initial connection to control plane failed; initiating reconnect loop",
                        );
                    }
                } else if last_warn_time
                    .is_none_or(|t| now.duration_since(t) >= Duration::from_secs(10))
                {
                    last_warn_time = Some(now);
                    tracing::warn!(
                        event.name = "platform.node.reconnecting",
                        error.code = "PLATFORM.NODE.CONNECT_FAILED",
                        node_id = %config.node_id,
                        retries = retry_count,
                        elapsed_ms = now.duration_since(*first).as_millis() as u64,
                        error = %error,
                        message = "Still attempting to reconnect to control plane",
                    );
                }
                time::sleep(delay).await;
                delay = backoff(delay, config.reconnect_max);
            }
        }
    }
}

async fn connect_once(
    config: &NodeAgentConfig,
    bridge: &NodeCommandBridge<UdsKernelCommandExecutor>,
    node: NodeRef,
    resume_token: &str,
) -> Result<String, NodeAgentError> {
    let channel = control_plane_channel(config).await?;
    let mut client = NodeControlServiceClient::new(channel);
    let mut session = NodeControlSession::new(
        node.node_id.clone(),
        node.node_epoch,
        config.agent_version.clone(),
        config.min_protocol_version,
        config.max_protocol_version,
        resume_token,
    );
    let (outbound, inbound) = mpsc::channel::<NodeToControlPlane>(32);
    outbound.send(session.hello()).await.map_err(|_| {
        NodeAgentError::Transport("control-plane request stream closed".to_string())
    })?;
    let mut inbound = client
        .connect(Request::new(ReceiverStream::new(inbound)))
        .await
        .map_err(transport_status)?
        .into_inner();

    let welcome = inbound
        .message()
        .await
        .map_err(transport_status)?
        .ok_or_else(|| {
            NodeAgentError::Transport("control-plane closed before welcome".to_string())
        })?;
    let welcome = session.accept_welcome(welcome)?;
    tracing::info!(
        event.name = "platform.node.connected",
        node_id = %node.node_id,
        node_epoch = node.node_epoch,
        message = "Connected to control plane and established session",
    );
    let heartbeat_every = proto_duration(welcome.heartbeat_interval.as_ref())
        .unwrap_or_else(|| Duration::from_secs(5));
    let mut heartbeat = time::interval(heartbeat_every);
    heartbeat.set_missed_tick_behavior(time::MissedTickBehavior::Delay);
    heartbeat.tick().await;

    loop {
        tokio::select! {
            frame = inbound.message() => match frame.map_err(transport_status)? {
                Some(frame) => forward_control_frame(&outbound, bridge, &mut session, frame).await?,
                None => {
                    tracing::info!(
                        event.name = "platform.node.disconnected",
                        node_id = %node.node_id,
                        message = "Control plane closed inbound stream cleanly",
                    );
                    return Ok(session.resume_token().to_string());
                }
            },
            _ = heartbeat.tick() => {
                let observed_generation = session.desired_generation().unwrap_or_default();
                send_frame(&outbound, session.heartbeat(observed_generation)?).await?;
            }
        }
    }
}

async fn forward_control_frame(
    outbound: &mpsc::Sender<NodeToControlPlane>,
    bridge: &NodeCommandBridge<UdsKernelCommandExecutor>,
    session: &mut NodeControlSession,
    frame: ControlPlaneToNode,
) -> Result<(), NodeAgentError> {
    let result = bridge.forward(session, frame).await?;
    send_frame(outbound, result).await
}

async fn send_frame(
    outbound: &mpsc::Sender<NodeToControlPlane>,
    frame: NodeToControlPlane,
) -> Result<(), NodeAgentError> {
    outbound
        .send(frame)
        .await
        .map_err(|_| NodeAgentError::Transport("control-plane request stream closed".to_string()))
}

async fn control_plane_channel(config: &NodeAgentConfig) -> Result<Channel, NodeAgentError> {
    Endpoint::from_shared(config.control_plane_endpoint.clone())
        .map_err(|error| NodeAgentError::Configuration(error.to_string()))?
        .tls_config(config.tls_config()?)
        .map_err(|error| NodeAgentError::Configuration(error.to_string()))?
        .connect()
        .await
        .map_err(|error| NodeAgentError::Transport(error.to_string()))
}

fn read_credential(path: &PathBuf, private_key: bool) -> Result<Vec<u8>, NodeAgentError> {
    if private_key {
        validate_private_key_permissions(path)?;
    }
    let bytes = fs::read(path).map_err(|source| NodeAgentError::CredentialRead {
        path: path.clone(),
        source,
    })?;
    if bytes.is_empty() {
        return Err(NodeAgentError::EmptyCredential(path.clone()));
    }
    Ok(bytes)
}

#[cfg(unix)]
fn validate_private_key_permissions(path: &PathBuf) -> Result<(), NodeAgentError> {
    use std::os::unix::fs::PermissionsExt;

    let mode = fs::metadata(path)
        .map_err(|source| NodeAgentError::CredentialRead {
            path: path.clone(),
            source,
        })?
        .permissions()
        .mode();
    if mode & 0o077 != 0 {
        return Err(NodeAgentError::Configuration(format!(
            "client key {} must not be readable by group or other users",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_private_key_permissions(_path: &PathBuf) -> Result<(), NodeAgentError> {
    Ok(())
}

fn proto_duration(value: Option<&prost_types::Duration>) -> Option<Duration> {
    let value = value?;
    if value.seconds < 0 || value.nanos < 0 {
        return None;
    }
    let seconds = u64::try_from(value.seconds).ok()?;
    let nanos = u32::try_from(value.nanos).ok()?;
    if nanos >= 1_000_000_000 {
        return None;
    }
    (!Duration::new(seconds, nanos).is_zero()).then_some(Duration::new(seconds, nanos))
}

fn transport_status(status: tonic::Status) -> NodeAgentError {
    NodeAgentError::Transport(format!("{}: {}", status.code(), status.message()))
}

fn backoff(current: Duration, maximum: Duration) -> Duration {
    current.checked_mul(2).unwrap_or(maximum).min(maximum)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> NodeAgentConfig {
        NodeAgentConfig {
            control_plane_endpoint: "https://control.example.test".to_string(),
            control_plane_server_name: "control.example.test".to_string(),
            control_plane_ca: PathBuf::from("/etc/cyrene/ca.pem"),
            client_certificate: PathBuf::from("/etc/cyrene/client.pem"),
            client_key: PathBuf::from("/etc/cyrene/client.key"),
            kernel_socket: PathBuf::from("/run/cyrene/kernel.sock"),
            node_id: "node-1".to_string(),
            agent_version: "test".to_string(),
            min_protocol_version: 1,
            max_protocol_version: 1,
            resume_token: String::new(),
            reconnect_min: Duration::from_secs(1),
            reconnect_max: Duration::from_secs(4),
        }
    }

    #[test]
    fn config_requires_https_and_safe_reconnect_bounds() {
        let mut value = config();
        value.control_plane_endpoint = "http://control.example.test".to_string();
        assert!(matches!(
            value.validate(),
            Err(NodeAgentError::Configuration(_))
        ));

        let mut value = config();
        value.reconnect_min = Duration::from_secs(5);
        assert!(matches!(
            value.validate(),
            Err(NodeAgentError::Configuration(_))
        ));
    }

    #[test]
    fn backoff_is_capped() {
        assert_eq!(
            backoff(Duration::from_secs(2), Duration::from_secs(5)),
            Duration::from_secs(4)
        );
        assert_eq!(
            backoff(Duration::from_secs(4), Duration::from_secs(5)),
            Duration::from_secs(5)
        );
    }
}
