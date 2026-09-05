//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 node.rs                                                         │
//! │  Module: cy_execution_fabric::node                                  │
//! │  Role: Node persistence and session lifecycle projection.           │
//! │                                                                     │
//! │  模块职责：实现 Node 持久性与控制 session 的生命周期投影。               │
//! └─────────────────────────────────────────────────────────────────────┘

use cy_proto::core_v1::{ExecutionAgentHello, ExecutionNodeDescriptor, NodeLifecycleState};

use crate::FabricContractError;

/// Control-plane projection for one existing [`cy_proto::core_v1::NodeRef`].
///
/// This is not a second Node registry: it holds lifecycle facts around the
/// canonical NodeRef carried by the existing wire contract.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeLifecycleProjection {
    descriptor: ExecutionNodeDescriptor,
    state: NodeLifecycleState,
    session_id: Option<String>,
}

impl NodeLifecycleProjection {
    /// Establish the first authenticated session for a Node identity.
    pub fn enroll(
        hello: &ExecutionAgentHello,
        session_id: impl Into<String>,
    ) -> Result<Self, FabricContractError> {
        let descriptor = hello.node.clone().ok_or_else(|| {
            FabricContractError::new("NODE_IDENTITY_REQUIRED", "Node descriptor is required")
        })?;
        validate_node_descriptor(&descriptor)?;
        let session_id = session_id.into();
        if session_id.is_empty() {
            return Err(FabricContractError::new(
                "NODE_SESSION_REQUIRED",
                "Node session identity is required",
            ));
        }
        Ok(Self {
            descriptor,
            state: NodeLifecycleState::Online,
            session_id: Some(session_id),
        })
    }

    pub fn descriptor(&self) -> &ExecutionNodeDescriptor {
        &self.descriptor
    }

    pub fn state(&self) -> NodeLifecycleState {
        self.state
    }

    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// Attach a new authenticated session to the same logical Node.
    pub fn reconnect(
        &mut self,
        descriptor: &ExecutionNodeDescriptor,
        session_id: impl Into<String>,
    ) -> Result<(), FabricContractError> {
        validate_node_descriptor(descriptor)?;
        let expected = self.descriptor.node.as_ref().expect("validated descriptor");
        let received = descriptor.node.as_ref().expect("validated descriptor");
        if received.node_id != expected.node_id
            || received.node_epoch < expected.node_epoch
            || descriptor.node_type != self.descriptor.node_type
            || descriptor.persistent != self.descriptor.persistent
        {
            return Err(FabricContractError::new(
                "NODE_IDENTITY_MISMATCH",
                "reconnect changed Node identity or immutable lifecycle attributes",
            ));
        }
        if matches!(
            self.state,
            NodeLifecycleState::Lost | NodeLifecycleState::Terminated
        ) && !self.persistent()
        {
            return Err(FabricContractError::new(
                "EPHEMERAL_NODE_CLOSED",
                "an ephemeral Node cannot reconnect after its lifecycle is closed",
            ));
        }
        let session_id = session_id.into();
        if session_id.is_empty() || self.session_id.as_deref() == Some(&session_id) {
            return Err(FabricContractError::new(
                "NODE_SESSION_INVALID",
                "reconnect requires a new non-empty NodeSessionId",
            ));
        }
        self.descriptor = descriptor.clone();
        self.state = NodeLifecycleState::Online;
        self.session_id = Some(session_id);
        Ok(())
    }

    /// Record transport loss without equating a missing heartbeat with a crash.
    pub fn disconnect(&mut self, canonical_lease_active: bool) {
        self.session_id = None;
        self.state = if self.persistent() || canonical_lease_active {
            NodeLifecycleState::Offline
        } else {
            NodeLifecycleState::Lost
        };
    }

    /// Apply canonical Lease expiry after the connection has disappeared.
    pub fn lease_expired(&mut self) {
        self.session_id = None;
        self.state = if self.persistent() {
            NodeLifecycleState::Offline
        } else {
            NodeLifecycleState::Lost
        };
    }

    /// Close an ephemeral Node after Provider or Control confirms termination.
    pub fn terminate(&mut self) -> Result<(), FabricContractError> {
        if self.persistent() {
            return Err(FabricContractError::new(
                "PERSISTENT_NODE_RETAINED",
                "persistent Node identity is retained while offline",
            ));
        }
        self.session_id = None;
        self.state = NodeLifecycleState::Terminated;
        Ok(())
    }

    pub fn persistent(&self) -> bool {
        self.descriptor.persistent == Some(true)
    }
}

pub(crate) fn validate_node_descriptor(
    descriptor: &ExecutionNodeDescriptor,
) -> Result<(), FabricContractError> {
    let node = descriptor
        .node
        .as_ref()
        .ok_or_else(|| FabricContractError::new("NODE_IDENTITY_REQUIRED", "NodeRef is required"))?;
    if node.node_id.is_empty() || node.node_epoch == 0 || descriptor.node_type.is_empty() {
        return Err(FabricContractError::new(
            "NODE_IDENTITY_INVALID",
            "NodeId, positive Node epoch, and node_type are required",
        ));
    }
    if descriptor.persistent.is_none() {
        return Err(FabricContractError::new(
            "NODE_PERSISTENCE_REQUIRED",
            "persistent must be explicitly true or false",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use cy_proto::core_v1::{
        ExecutionAttachmentType, PersistenceClass, RestartCapability, RuntimeRef,
    };
    use cy_proto::semantic_v1::Identity;

    use super::*;

    fn hello(node_id: &str, persistent: bool) -> ExecutionAgentHello {
        ExecutionAgentHello {
            runtime: Some(RuntimeRef {
                identity: Some(Identity {
                    id: "runtime-1".to_string(),
                    generation: 1,
                }),
            }),
            scope: None,
            attachment_type: ExecutionAttachmentType::ContainerAgent as i32,
            persistence_class: if persistent {
                PersistenceClass::Persistent as i32
            } else {
                PersistenceClass::Ephemeral as i32
            },
            agent_version: "test".to_string(),
            min_protocol_version: 2,
            max_protocol_version: 2,
            resume_token: "resume".to_string(),
            capabilities: vec![],
            enrollment_proof: String::new(),
            node: Some(ExecutionNodeDescriptor {
                node: Some(cy_proto::core_v1::NodeRef {
                    node_id: node_id.to_string(),
                    node_epoch: 1,
                }),
                node_type: "container".to_string(),
                persistent: Some(persistent),
            }),
            restart_capability: RestartCapability::None as i32,
        }
    }

    #[test]
    fn persistent_node_reconnects_with_same_node_id_and_new_session() {
        let agent_hello = hello("node-persistent", true);
        let mut node = NodeLifecycleProjection::enroll(&agent_hello, "session-1").unwrap();
        node.disconnect(false);
        assert_eq!(node.state(), NodeLifecycleState::Offline);
        node.reconnect(agent_hello.node.as_ref().unwrap(), "session-2")
            .unwrap();
        assert_eq!(node.state(), NodeLifecycleState::Online);
        assert_eq!(node.session_id(), Some("session-2"));
    }

    #[test]
    fn ephemeral_node_closes_after_loss_and_requires_replacement_identity() {
        let agent_hello = hello("node-ephemeral", false);
        let mut node = NodeLifecycleProjection::enroll(&agent_hello, "session-1").unwrap();
        node.disconnect(false);
        assert_eq!(node.state(), NodeLifecycleState::Lost);
        node.terminate().unwrap();
        assert_eq!(node.state(), NodeLifecycleState::Terminated);
        assert_eq!(
            node.reconnect(agent_hello.node.as_ref().unwrap(), "session-2")
                .unwrap_err()
                .reason_code,
            "EPHEMERAL_NODE_CLOSED"
        );
        assert!(
            NodeLifecycleProjection::enroll(&hello("node-replacement", false), "session-3").is_ok()
        );
    }
}
