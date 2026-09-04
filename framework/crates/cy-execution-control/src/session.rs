//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 session.rs                                                      │
//! │  Module: cy_execution_control::session                              │
//! │  Role: Fenced outbound NodeControl session handles.                 │
//! │                                                                     │
//! │  模块职责：维护带 fencing 的 NodeControl 出站 session handle。        │
//! └─────────────────────────────────────────────────────────────────────┘

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use cy_proto::core_v1::{control_plane_to_node, ControlPlaneToNode, NodeRef};
use tokio::sync::{mpsc, Mutex, Notify};
use tonic::Status;

use crate::DispatchError;

pub(crate) type OutboundItem = Result<ControlPlaneToNode, Status>;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct NodeKey {
    pub node_id: String,
    pub node_epoch: u64,
}

impl From<&NodeRef> for NodeKey {
    fn from(node: &NodeRef) -> Self {
        Self {
            node_id: node.node_id.clone(),
            node_epoch: node.node_epoch,
        }
    }
}

impl NodeKey {
    pub fn to_proto(&self) -> NodeRef {
        NodeRef {
            node_id: self.node_id.clone(),
            node_epoch: self.node_epoch,
        }
    }
}

pub(crate) struct SessionHandle {
    pub session_id: String,
    sender: mpsc::Sender<OutboundItem>,
    outbound_gate: Mutex<()>,
    next_sequence: Mutex<u64>,
    acknowledged_agent_sequence: AtomicU64,
    fenced: AtomicBool,
    ready: AtomicBool,
    ready_changed: Notify,
}

impl SessionHandle {
    pub fn new(session_id: String, sender: mpsc::Sender<OutboundItem>) -> Self {
        Self {
            session_id,
            sender,
            outbound_gate: Mutex::new(()),
            next_sequence: Mutex::new(1),
            acknowledged_agent_sequence: AtomicU64::new(0),
            fenced: AtomicBool::new(false),
            ready: AtomicBool::new(false),
            ready_changed: Notify::new(),
        }
    }

    /// Acquire the same gate as outbound sends before fencing the session.
    ///
    /// This makes fencing linearizable with respect to `sender.send`: either
    /// the send completes before fencing, or it observes the fenced state and
    /// does not enqueue a stale frame.
    pub async fn fence(&self) {
        let _gate = self.outbound_gate.lock().await;
        self.fenced.store(true, Ordering::SeqCst);
        self.ready_changed.notify_waiters();
    }

    pub fn is_fenced(&self) -> bool {
        self.fenced.load(Ordering::SeqCst)
    }

    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::SeqCst)
    }

    pub fn is_usable(&self) -> bool {
        self.is_ready() && !self.is_fenced()
    }

    #[cfg(test)]
    pub fn mark_ready_for_test(&self) {
        self.ready.store(true, Ordering::SeqCst);
        self.ready_changed.notify_waiters();
    }

    pub fn acknowledge_agent_sequence(&self, sequence: u64) {
        self.acknowledged_agent_sequence
            .fetch_max(sequence, Ordering::SeqCst);
    }

    pub async fn send(&self, body: control_plane_to_node::Body) -> Result<u64, DispatchError> {
        loop {
            let ready = self.ready_changed.notified();
            if self.is_fenced() {
                return Err(DispatchError::input(
                    "SESSION_FENCED",
                    "authenticated Agent control session has been fenced",
                ));
            }
            if self.is_ready() {
                break;
            }
            ready.await;
        }
        let _gate = self.outbound_gate.lock().await;
        if self.is_fenced() {
            return Err(DispatchError::input(
                "SESSION_FENCED",
                "authenticated Agent control session has been fenced",
            ));
        }
        let sequence_number = {
            let mut next_sequence = self.next_sequence.lock().await;
            let sequence_number = *next_sequence;
            *next_sequence = next_sequence.saturating_add(1);
            sequence_number
        };
        let frame = ControlPlaneToNode {
            frame_id: format!("{}-{sequence_number}", self.session_id),
            sequence_number,
            session_id: self.session_id.clone(),
            ack_sequence_number: self.acknowledged_agent_sequence.load(Ordering::SeqCst),
            body: Some(body),
        };
        self.sender.send(Ok(frame)).await.map_err(|_| {
            DispatchError::transient(
                "AGENT_SESSION_CLOSED",
                "authenticated Agent control stream is closed",
            )
        })?;
        Ok(sequence_number)
    }

    /// Queue the one mandatory Welcome frame before making the route visible.
    /// 在 route 可见前排入唯一且必需的 Welcome 帧。
    pub async fn send_welcome(
        &self,
        body: control_plane_to_node::Body,
    ) -> Result<u64, DispatchError> {
        let _gate = self.outbound_gate.lock().await;
        if self.is_fenced() || self.is_ready() {
            return Err(DispatchError::input(
                "WELCOME_STATE_INVALID",
                "Welcome can be sent exactly once on an unfenced unready session",
            ));
        }
        let mut next_sequence = self.next_sequence.lock().await;
        if *next_sequence != 1 {
            return Err(DispatchError::input(
                "WELCOME_SEQUENCE_INVALID",
                "Welcome must be the first outbound Agent frame",
            ));
        }
        let sequence_number = *next_sequence;
        *next_sequence = next_sequence.saturating_add(1);
        let frame = ControlPlaneToNode {
            frame_id: format!("{}-{sequence_number}", self.session_id),
            sequence_number,
            session_id: self.session_id.clone(),
            ack_sequence_number: self.acknowledged_agent_sequence.load(Ordering::SeqCst),
            body: Some(body),
        };
        self.sender.send(Ok(frame)).await.map_err(|_| {
            DispatchError::transient(
                "AGENT_SESSION_CLOSED",
                "authenticated Agent control stream closed before Welcome",
            )
        })?;
        self.ready.store(true, Ordering::SeqCst);
        self.ready_changed.notify_waiters();
        Ok(sequence_number)
    }

    pub async fn close_with(&self, status: Status) {
        let _gate = self.outbound_gate.lock().await;
        self.fenced.store(true, Ordering::SeqCst);
        self.ready_changed.notify_waiters();
        let _ = self.sender.send(Err(status)).await;
    }
}

#[derive(Clone)]
pub(crate) struct HostSession {
    pub handle: std::sync::Arc<SessionHandle>,
}

#[derive(Clone)]
pub(crate) struct RuntimeSession {
    pub node: NodeKey,
    pub organization_id: String,
    pub workspace_id: String,
    pub workload_identity: cy_kernel_contract::Identity,
    pub workload_identity_expires_at_unix_ms: u64,
    pub handle: std::sync::Arc<SessionHandle>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fenced_session_rejects_outbound_frames() {
        let (sender, mut receiver) = mpsc::channel(1);
        let handle = SessionHandle::new("session-1".to_string(), sender);

        handle.fence().await;

        let result = handle
            .send(control_plane_to_node::Body::StopCommand(
                cy_proto::core_v1::StopCommand::default(),
            ))
            .await;
        assert_eq!(result.unwrap_err().reason_code, "SESSION_FENCED");
        assert!(receiver.try_recv().is_err());
    }

    #[tokio::test]
    async fn business_send_waits_until_welcome_is_first() {
        let (sender, mut receiver) = mpsc::channel(4);
        let handle = std::sync::Arc::new(SessionHandle::new("session-1".to_string(), sender));
        let blocked_handle = std::sync::Arc::clone(&handle);
        let blocked_send = tokio::spawn(async move {
            blocked_handle
                .send(control_plane_to_node::Body::StopCommand(
                    cy_proto::core_v1::StopCommand::default(),
                ))
                .await
        });
        tokio::task::yield_now().await;
        assert!(receiver.try_recv().is_err());

        handle
            .send_welcome(control_plane_to_node::Body::Welcome(
                cy_proto::core_v1::NodeWelcome::default(),
            ))
            .await
            .unwrap();
        assert_eq!(receiver.recv().await.unwrap().unwrap().sequence_number, 1);
        assert_eq!(blocked_send.await.unwrap().unwrap(), 2);
        assert_eq!(receiver.recv().await.unwrap().unwrap().sequence_number, 2);
    }

    #[tokio::test]
    async fn fencing_waits_for_in_flight_send_and_blocks_later_frames() {
        let (sender, mut receiver) = mpsc::channel(2);
        let handle = std::sync::Arc::new(SessionHandle::new("session-1".to_string(), sender));
        handle
            .send_welcome(control_plane_to_node::Body::Welcome(
                cy_proto::core_v1::NodeWelcome::default(),
            ))
            .await
            .unwrap();
        assert_eq!(receiver.recv().await.unwrap().unwrap().sequence_number, 1);

        handle
            .send(control_plane_to_node::Body::StopCommand(
                cy_proto::core_v1::StopCommand::default(),
            ))
            .await
            .unwrap();

        let held_gate = handle.outbound_gate.lock().await;
        let blocked_handle = std::sync::Arc::clone(&handle);
        let blocked_send = tokio::spawn(async move {
            blocked_handle
                .send(control_plane_to_node::Body::StopCommand(
                    cy_proto::core_v1::StopCommand::default(),
                ))
                .await
        });
        tokio::task::yield_now().await;

        let fence_handle = std::sync::Arc::clone(&handle);
        let fence = tokio::spawn(async move {
            fence_handle.fence().await;
        });
        tokio::task::yield_now().await;

        drop(held_gate);
        assert!(!fence.is_finished());
        let _ = receiver.recv().await;
        blocked_send.await.unwrap().unwrap();
        fence.await.unwrap();
        assert!(handle.is_fenced());
        assert_eq!(receiver.recv().await.unwrap().unwrap().sequence_number, 3);

        let result = handle
            .send(control_plane_to_node::Body::StopCommand(
                cy_proto::core_v1::StopCommand::default(),
            ))
            .await;
        assert_eq!(result.unwrap_err().reason_code, "SESSION_FENCED");
        assert!(receiver.try_recv().is_err());
    }
}
