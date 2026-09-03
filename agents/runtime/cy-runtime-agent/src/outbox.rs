//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 outbox.rs                                                       │
//! │  Module: cy_runtime_agent::outbox                                   │
//! │  Role: Bounded reconnect replay and ack tracking.                   │
//! │                                                                     │
//! │  模块职责：跨 control-channel reconnect 保留未确认观测并幂等重放。       │
//! └─────────────────────────────────────────────────────────────────────┘

use std::collections::{BTreeMap, VecDeque};

use cy_proto::core_v1::{node_to_control_plane, NodeToControlPlane};

use crate::RuntimeAgentError;

const MAX_PENDING_FRAMES: usize = 1024;

#[derive(Clone)]
struct PendingFrame {
    frame_id: String,
    body: node_to_control_plane::Body,
}

/// In-memory bounded outbox. Artifact checkpoints provide restart durability.
#[derive(Default)]
pub(crate) struct AgentOutbox {
    pending: VecDeque<PendingFrame>,
    sent_by_sequence: BTreeMap<u64, String>,
    next_frame_id: u64,
    next_sequence: u64,
}

impl AgentOutbox {
    pub fn enqueue(
        &mut self,
        runtime_id: &str,
        body: node_to_control_plane::Body,
    ) -> Result<(), RuntimeAgentError> {
        if self.pending.len() >= MAX_PENDING_FRAMES {
            return Err(RuntimeAgentError::OutboxFull);
        }
        self.next_frame_id += 1;
        self.pending.push_back(PendingFrame {
            frame_id: format!("{runtime_id}-observation-{}", self.next_frame_id),
            body,
        });
        Ok(())
    }

    pub fn begin_session(&mut self) {
        self.sent_by_sequence.clear();
        self.next_sequence = 2;
    }

    pub fn acknowledge(&mut self, sequence: u64) {
        let acknowledged_ids = self
            .sent_by_sequence
            .range(..=sequence)
            .map(|(_, frame_id)| frame_id.clone())
            .collect::<Vec<_>>();
        self.sent_by_sequence.retain(|sent, _| *sent > sequence);
        self.pending
            .retain(|frame| !acknowledged_ids.contains(&frame.frame_id));
    }

    pub fn unsent_frames(&mut self, session_id: &str, control_ack: u64) -> Vec<NodeToControlPlane> {
        let already_sent = self.sent_by_sequence.values().cloned().collect::<Vec<_>>();
        let mut frames = Vec::new();
        for pending in &self.pending {
            if already_sent.contains(&pending.frame_id) {
                continue;
            }
            let sequence = self.next_sequence;
            self.next_sequence += 1;
            self.sent_by_sequence
                .insert(sequence, pending.frame_id.clone());
            frames.push(NodeToControlPlane {
                frame_id: pending.frame_id.clone(),
                sequence_number: sequence,
                session_id: session_id.to_string(),
                ack_sequence_number: control_ack,
                body: Some(pending.body.clone()),
            });
        }
        frames
    }
}

#[cfg(test)]
mod tests {
    use cy_proto::core_v1::{node_to_control_plane, RuntimeProgress};

    use super::*;

    #[test]
    fn unacknowledged_frame_replays_in_a_new_session() {
        let mut outbox = AgentOutbox::default();
        outbox
            .enqueue(
                "runtime-1",
                node_to_control_plane::Body::RuntimeProgress(RuntimeProgress::default()),
            )
            .unwrap();
        outbox.begin_session();
        let first = outbox.unsent_frames("session-1", 1);
        assert_eq!(first.len(), 1);
        outbox.begin_session();
        let replay = outbox.unsent_frames("session-2", 1);
        assert_eq!(replay.len(), 1);
        assert_eq!(replay[0].frame_id, first[0].frame_id);
        outbox.acknowledge(replay[0].sequence_number);
        assert!(outbox.unsent_frames("session-2", 2).is_empty());
    }
}
