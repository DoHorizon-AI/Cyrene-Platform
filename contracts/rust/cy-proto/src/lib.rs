//! Generated CYRENE protocol types.
//!
//! The schema is compiled from `contracts/proto/ai_service.proto` and
//! `contracts/proto/agent_service.proto` by `build.rs` (tonic-build) and included here.
//! `contracts/proto/*.proto` are
//! the single contract source; do not hand-edit generated code.

/// Generated types under their proto package path (`cy.llm`).
pub mod cy {
    pub mod llm {
        tonic::include_proto!("cy.llm");
    }
}

/// Convenience re-export of every generated message, enum, client and server
/// type so downstream crates can `use cy_proto::*;` or `use cy_proto::llm::*;`.
pub use cy::llm;
pub use cy::llm::*;

pub mod ai_service_client {
    pub use crate::cy::llm::ai_inference_client::AiInferenceClient as AiServiceClient;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_proto_types() {
        let req = AgentHeartbeatRequest {
            agent_id: "agent-1".into(),
            target_id: "target-1".into(),
            timestamp: 123456789,
            status: "HEALTHY".into(),
            metrics: std::collections::HashMap::new(),
        };
        assert_eq!(req.agent_id, "agent-1");

        let reg = TargetRegistrationRequest {
            target_id: "t-100".into(),
            hostname: "node1".into(),
            ip_address: "192.168.1.10".into(),
            arch: "x86_64".into(),
            os: "linux".into(),
            capabilities: vec!["cuda".into()],
            labels: std::collections::HashMap::new(),
            agent_version: "1.0.0".into(),
        };
        assert_eq!(reg.target_id, "t-100");

        let cmd = AgentCommandRequest {
            command_id: "cmd-1".into(),
            target_id: "t-100".into(),
            command_type: "exec".into(),
            payload: "echo hi".into(),
            timeout_seconds: 30,
            env: std::collections::HashMap::new(),
        };
        assert_eq!(cmd.command_id, "cmd-1");

        let journal = JournalStreamRequest {
            target_id: "t-100".into(),
            follow: true,
            tail_lines: 100,
            filter_unit: "cy-engine".into(),
            since_timestamp: 0,
        };
        assert_eq!(journal.target_id, "t-100");
    }
}
