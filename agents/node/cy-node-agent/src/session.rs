// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: agents/node/cy-node-agent/src/session.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! Core v1 node-control session state.
//!
//! This is deliberately a transport-free state layer. P2 owns the network
//! listener, mTLS/UDS, command admission, and runtime execution.

use cy_proto::core_v1::{
    control_plane_to_node, node_to_control_plane, ControlPlaneToNode, KernelCommand, NodeHeartbeat,
    NodeHello, NodeRef, NodeToControlPlane, NodeWelcome,
};
use cy_proto::semantic_v1::ContractRevision;
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum NodeControlSessionError {
    #[error("node-control session is not awaiting a welcome frame")]
    WelcomeOutOfOrder,
    #[error("node-control frame did not contain a welcome")]
    MissingWelcome,
    #[error("selected protocol version {selected} is outside [{min}, {max}]")]
    UnsupportedProtocol { selected: u32, min: u32, max: u32 },
    #[error("node-control session has not been welcomed")]
    NotEstablished,
    #[error("node-control frame did not contain a kernel command")]
    MissingCommand,
    #[error("node-control welcome did not establish a non-empty session id")]
    MissingSessionId,
    #[error("node-control frame belongs to another session")]
    SessionFenced,
    #[error("node-control frame sequence {received} is not newer than {accepted}")]
    StaleControlFrame { received: u64, accepted: u64 },
    #[error("kernel command id is required")]
    MissingCommandId,
    #[error("node-control welcome did not select the offered semantic contract")]
    UnsupportedSemanticContract,
}

#[derive(Debug, Clone, PartialEq)]
enum SessionState {
    AwaitingWelcome,
    Established {
        session_id: String,
        desired_generation: u64,
        last_control_sequence: u64,
        selected_contract: ContractRevision,
    },
}

/// Minimal state machine for the outbound Core v1 `Connect` stream.
#[derive(Debug, Clone)]
pub struct NodeControlSession {
    node: NodeRef,
    agent_version: String,
    min_protocol_version: u32,
    max_protocol_version: u32,
    resume_token: String,
    next_sequence: u64,
    state: SessionState,
}

impl NodeControlSession {
    pub fn new(
        node_id: impl Into<String>,
        node_epoch: u64,
        agent_version: impl Into<String>,
        min_protocol_version: u32,
        max_protocol_version: u32,
        resume_token: impl Into<String>,
    ) -> Self {
        Self {
            node: NodeRef {
                node_id: node_id.into(),
                node_epoch,
            },
            agent_version: agent_version.into(),
            min_protocol_version,
            max_protocol_version,
            resume_token: resume_token.into(),
            next_sequence: 1,
            state: SessionState::AwaitingWelcome,
        }
    }

    /// Build the first outbound frame. No heartbeat or command is valid before it.
    pub fn hello(&mut self) -> NodeToControlPlane {
        self.frame(node_to_control_plane::Body::Hello(NodeHello {
            node: Some(self.node.clone()),
            agent_version: self.agent_version.clone(),
            min_protocol_version: self.min_protocol_version,
            max_protocol_version: self.max_protocol_version,
            resume_token: self.resume_token.clone(),
            offered_contracts: vec![semantic_contract_revision()],
        }))
    }

    pub fn accept_welcome(
        &mut self,
        frame: ControlPlaneToNode,
    ) -> Result<NodeWelcome, NodeControlSessionError> {
        if !matches!(self.state, SessionState::AwaitingWelcome) {
            return Err(NodeControlSessionError::WelcomeOutOfOrder);
        }

        if frame.sequence_number == 0 {
            return Err(NodeControlSessionError::WelcomeOutOfOrder);
        }

        let Some(control_plane_to_node::Body::Welcome(welcome)) = frame.body.as_ref() else {
            return Err(NodeControlSessionError::MissingWelcome);
        };

        if welcome.selected_protocol_version < self.min_protocol_version
            || welcome.selected_protocol_version > self.max_protocol_version
        {
            return Err(NodeControlSessionError::UnsupportedProtocol {
                selected: welcome.selected_protocol_version,
                min: self.min_protocol_version,
                max: self.max_protocol_version,
            });
        }

        if welcome.session_id.is_empty() || frame.session_id != welcome.session_id {
            return Err(NodeControlSessionError::MissingSessionId);
        }
        let selected_contract = welcome
            .selected_contract
            .clone()
            .filter(is_supported_semantic_contract)
            .ok_or(NodeControlSessionError::UnsupportedSemanticContract)?;
        if !welcome.resume_token.is_empty() {
            self.resume_token = welcome.resume_token.clone();
        }

        self.state = SessionState::Established {
            session_id: welcome.session_id.clone(),
            desired_generation: welcome.desired_generation,
            last_control_sequence: frame.sequence_number,
            selected_contract,
        };
        Ok(welcome.clone())
    }

    /// Build the next outbound heartbeat after the welcome has been accepted.
    pub fn heartbeat(
        &mut self,
        observed_generation: u64,
    ) -> Result<NodeToControlPlane, NodeControlSessionError> {
        if !matches!(self.state, SessionState::Established { .. }) {
            return Err(NodeControlSessionError::NotEstablished);
        }

        Ok(
            self.frame(node_to_control_plane::Body::Heartbeat(NodeHeartbeat {
                node: Some(self.node.clone()),
                observed_generation,
                observed_at: None,
            })),
        )
    }

    /// Validate and extract a command frame without executing it.
    pub fn accept_command(
        &mut self,
        frame: ControlPlaneToNode,
    ) -> Result<KernelCommand, NodeControlSessionError> {
        let (session_id, accepted) = match &self.state {
            SessionState::Established {
                session_id,
                last_control_sequence,
                ..
            } => (session_id.clone(), *last_control_sequence),
            SessionState::AwaitingWelcome => return Err(NodeControlSessionError::NotEstablished),
        };
        if frame.session_id != session_id {
            return Err(NodeControlSessionError::SessionFenced);
        }
        if frame.sequence_number <= accepted {
            return Err(NodeControlSessionError::StaleControlFrame {
                received: frame.sequence_number,
                accepted,
            });
        }
        let command = match frame.body {
            Some(control_plane_to_node::Body::Command(command))
                if !command.command_id.is_empty() =>
            {
                command
            }
            Some(control_plane_to_node::Body::Command(_)) => {
                return Err(NodeControlSessionError::MissingCommandId)
            }
            _ => return Err(NodeControlSessionError::MissingCommand),
        };
        if let SessionState::Established {
            last_control_sequence,
            ..
        } = &mut self.state
        {
            *last_control_sequence = frame.sequence_number;
        }
        Ok(command)
    }

    pub fn session_id(&self) -> Option<&str> {
        match &self.state {
            SessionState::Established { session_id, .. } => Some(session_id),
            SessionState::AwaitingWelcome => None,
        }
    }

    pub fn desired_generation(&self) -> Option<u64> {
        match self.state {
            SessionState::Established {
                desired_generation, ..
            } => Some(desired_generation),
            SessionState::AwaitingWelcome => None,
        }
    }

    pub fn resume_token(&self) -> &str {
        &self.resume_token
    }

    pub fn is_established(&self) -> bool {
        matches!(self.state, SessionState::Established { .. })
    }

    pub fn selected_contract(&self) -> Option<&ContractRevision> {
        match &self.state {
            SessionState::Established {
                selected_contract, ..
            } => Some(selected_contract),
            SessionState::AwaitingWelcome => None,
        }
    }

    /// Wrap a completed typed Kernel command so that it is bound to the
    /// currently accepted control-plane session.
    pub fn command_result(
        &mut self,
        result: cy_proto::core_v1::KernelCommandResult,
    ) -> Result<NodeToControlPlane, NodeControlSessionError> {
        if !self.is_established() {
            return Err(NodeControlSessionError::NotEstablished);
        }
        Ok(self.frame(node_to_control_plane::Body::CommandResult(result)))
    }

    fn frame(&mut self, body: node_to_control_plane::Body) -> NodeToControlPlane {
        let sequence_number = self.next_sequence;
        self.next_sequence += 1;
        NodeToControlPlane {
            frame_id: format!("{}-{}", self.node.node_id, sequence_number),
            sequence_number,
            session_id: self.session_id().unwrap_or_default().to_string(),
            body: Some(body),
        }
    }
}

fn semantic_contract_revision() -> ContractRevision {
    ContractRevision {
        contract_id: "cyrene.kernel.semantic".to_string(),
        major: 1,
        minor: 0,
    }
}

fn is_supported_semantic_contract(revision: &ContractRevision) -> bool {
    revision.contract_id == "cyrene.kernel.semantic" && revision.major == 1 && revision.minor == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn welcome_frame() -> ControlPlaneToNode {
        ControlPlaneToNode {
            frame_id: "welcome-1".into(),
            sequence_number: 1,
            body: Some(control_plane_to_node::Body::Welcome(NodeWelcome {
                session_id: "session-1".into(),
                selected_protocol_version: 1,
                desired_generation: 3,
                heartbeat_interval: None,
                server_time: None,
                resume_token: "resume-2".into(),
                selected_contract: Some(semantic_contract_revision()),
            })),
            session_id: "session-1".into(),
        }
    }

    #[test]
    fn connect_starts_with_hello_then_accepts_welcome() {
        let mut session = NodeControlSession::new("node-1", 7, "0.1.0", 1, 1, "resume-1");
        let hello = session.hello();

        assert_eq!(hello.sequence_number, 1);
        assert!(matches!(
            hello.body,
            Some(node_to_control_plane::Body::Hello(NodeHello { .. }))
        ));
        assert_eq!(session.session_id(), None);
        let Some(node_to_control_plane::Body::Hello(hello)) = hello.body else {
            panic!("first node-control frame must be hello");
        };
        assert_eq!(hello.offered_contracts, vec![semantic_contract_revision()]);

        let welcome = session.accept_welcome(welcome_frame()).unwrap();

        assert_eq!(welcome.session_id, "session-1");
        assert_eq!(session.session_id(), Some("session-1"));
        assert_eq!(session.desired_generation(), Some(3));
        assert_eq!(session.resume_token(), "resume-2");
        assert_eq!(
            session.selected_contract(),
            Some(&semantic_contract_revision())
        );
    }

    #[test]
    fn heartbeat_and_command_require_established_session() {
        let mut session = NodeControlSession::new("node-1", 7, "0.1.0", 1, 1, "");
        assert_eq!(
            session.heartbeat(3).unwrap_err(),
            NodeControlSessionError::NotEstablished
        );

        session.hello();
        session.accept_welcome(welcome_frame()).unwrap();

        let heartbeat = session.heartbeat(4).unwrap();
        assert_eq!(heartbeat.sequence_number, 2);
        assert!(matches!(
            heartbeat.body,
            Some(node_to_control_plane::Body::Heartbeat(NodeHeartbeat {
                observed_generation: 4,
                ..
            }))
        ));

        let command = session
            .accept_command(ControlPlaneToNode {
                frame_id: "command-1".into(),
                sequence_number: 2,
                session_id: "session-1".into(),
                body: Some(control_plane_to_node::Body::Command(KernelCommand {
                    command_id: "op-1".into(),
                    request: None,
                })),
            })
            .unwrap();
        assert_eq!(command.command_id, "op-1");
    }

    #[test]
    fn session_fences_old_or_replayed_commands() {
        let mut session = NodeControlSession::new("node-1", 7, "0.1.0", 1, 1, "");
        session.hello();
        session.accept_welcome(welcome_frame()).unwrap();

        let stale = session.accept_command(ControlPlaneToNode {
            frame_id: "replay".into(),
            sequence_number: 1,
            session_id: "session-1".into(),
            body: Some(control_plane_to_node::Body::Command(KernelCommand {
                command_id: "op-1".into(),
                request: None,
            })),
        });
        assert!(matches!(
            stale,
            Err(NodeControlSessionError::StaleControlFrame { .. })
        ));

        let fenced = session.accept_command(ControlPlaneToNode {
            frame_id: "old-session".into(),
            sequence_number: 2,
            session_id: "session-old".into(),
            body: Some(control_plane_to_node::Body::Command(KernelCommand {
                command_id: "op-2".into(),
                request: None,
            })),
        });
        assert_eq!(fenced.unwrap_err(), NodeControlSessionError::SessionFenced);
    }
}
