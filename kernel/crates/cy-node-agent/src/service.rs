//! gRPC AgentService Server Implementation for Node Agent.
//!
//! Provides gRPC handlers for Heartbeat, Target Registration, Command Execution Stream,
//! and Journal Streaming.

use std::pin::Pin;

use cy_proto::agent_service_server::AgentService;
use cy_proto::{
    AgentCommandRequest, AgentCommandResponse, AgentHeartbeatRequest, AgentHeartbeatResponse,
    JournalEntry, JournalStreamRequest, TargetRegistrationRequest, TargetRegistrationResponse,
};
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::Stream;
use tonic::{Request, Response, Status};

use crate::journal::SharedAgentJournal;
use crate::probe::HardwareProbe;

/// Node Agent gRPC service handler.
pub struct NodeAgentService {
    pub agent_id: String,
    pub target_id: String,
    pub journal: SharedAgentJournal,
    pub probe: HardwareProbe,
}

impl NodeAgentService {
    pub fn new(agent_id: &str, target_id: &str, journal: SharedAgentJournal) -> Self {
        Self {
            agent_id: agent_id.to_string(),
            target_id: target_id.to_string(),
            journal,
            probe: HardwareProbe::new(),
        }
    }
}

#[tonic::async_trait]
impl AgentService for NodeAgentService {
    type ExecuteCommandStreamStream =
        Pin<Box<dyn Stream<Item = Result<AgentCommandResponse, Status>> + Send + 'static>>;
    type StreamJournalStream =
        Pin<Box<dyn Stream<Item = Result<JournalEntry, Status>> + Send + 'static>>;

    async fn heartbeat(
        &self,
        request: Request<AgentHeartbeatRequest>,
    ) -> Result<Response<AgentHeartbeatResponse>, Status> {
        let req = request.into_inner();
        let now = chrono::Utc::now().timestamp();

        let mut journal = self
            .journal
            .write()
            .map_err(|e| Status::internal(e.to_string()))?;
        let _ = journal.append(
            "agent.service",
            "DEBUG",
            &format!("Heartbeat received from {}", req.agent_id),
            req.metrics,
        );

        Ok(Response::new(AgentHeartbeatResponse {
            acknowledged: true,
            timestamp: now,
            command_pending: false,
            message: format!("Heartbeat acknowledged for agent {}", req.agent_id),
        }))
    }

    async fn register_target(
        &self,
        request: Request<TargetRegistrationRequest>,
    ) -> Result<Response<TargetRegistrationResponse>, Status> {
        let req = request.into_inner();

        let mut journal = self
            .journal
            .write()
            .map_err(|e| Status::internal(e.to_string()))?;
        let _ = journal.append(
            "agent.service",
            "INFO",
            &format!("RegisterTarget: hostname={}", req.hostname),
            req.labels,
        );

        Ok(Response::new(TargetRegistrationResponse {
            success: true,
            registered_target_id: req.target_id.clone(),
            assigned_cluster: "default".to_string(),
            message: format!("Target {} registered successfully", req.target_id),
        }))
    }

    async fn execute_command_stream(
        &self,
        request: Request<AgentCommandRequest>,
    ) -> Result<Response<Self::ExecuteCommandStreamStream>, Status> {
        let req = request.into_inner();
        let (tx, rx) = tokio::sync::mpsc::channel(16);

        let command_id = req.command_id.clone();
        let payload = req.payload.clone();

        tokio::spawn(async move {
            let response = AgentCommandResponse {
                command_id,
                sequence_number: 1,
                stdout_chunk: format!("Executed payload: {}\n", payload).into_bytes(),
                stderr_chunk: vec![],
                exit_code: 0,
                completed: true,
                error_message: String::new(),
            };
            let _ = tx.send(Ok(response)).await;
        });

        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }

    async fn stream_journal(
        &self,
        request: Request<JournalStreamRequest>,
    ) -> Result<Response<Self::StreamJournalStream>, Status> {
        let req = request.into_inner();
        let (tx, rx) = tokio::sync::mpsc::channel(128);

        let (history, mut broadcast_rx) = {
            let journal = self
                .journal
                .read()
                .map_err(|e| Status::internal(e.to_string()))?;
            let history = journal.query(&req);
            let rx = journal.subscribe();
            (history, rx)
        };

        tokio::spawn(async move {
            // 1. Stream historical entries
            for record in history {
                let proto_entry: JournalEntry = record.entry.into();
                if tx.send(Ok(proto_entry)).await.is_err() {
                    return;
                }
            }

            // 2. Follow live entries if requested
            if req.follow {
                while let Ok(record) = broadcast_rx.recv().await {
                    if req.since_timestamp > 0 && record.entry.timestamp < req.since_timestamp {
                        continue;
                    }
                    if !req.filter_unit.is_empty() && record.entry.source != req.filter_unit {
                        continue;
                    }

                    let proto_entry: JournalEntry = record.entry.into();
                    if tx.send(Ok(proto_entry)).await.is_err() {
                        break;
                    }
                }
            }
        });

        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::AgentJournal;
    use std::sync::{Arc, RwLock};
    use tokio_stream::StreamExt;

    #[tokio::test]
    async fn test_grpc_agent_service_handlers() {
        let journal = Arc::new(RwLock::new(AgentJournal::new("t-1", 100, None)));
        let service = NodeAgentService::new("agent-1", "t-1", journal.clone());

        // Test Heartbeat
        let hb_req = Request::new(AgentHeartbeatRequest {
            agent_id: "agent-1".into(),
            target_id: "t-1".into(),
            timestamp: 100,
            status: "HEALTHY".into(),
            metrics: std::collections::HashMap::new(),
        });

        let hb_res = service.heartbeat(hb_req).await.unwrap().into_inner();
        assert!(hb_res.acknowledged);

        // Test StreamJournal
        journal
            .write()
            .unwrap()
            .append(
                "cy-engine",
                "INFO",
                "Journal stream test line",
                std::collections::HashMap::new(),
            )
            .unwrap();

        let stream_req = Request::new(JournalStreamRequest {
            target_id: "t-1".into(),
            follow: false,
            tail_lines: 10,
            filter_unit: "cy-engine".into(),
            since_timestamp: 0,
        });

        let mut stream = service
            .stream_journal(stream_req)
            .await
            .unwrap()
            .into_inner();
        let item = stream.next().await.unwrap().unwrap();
        assert_eq!(item.message, "Journal stream test line");
    }
}
