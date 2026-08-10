//! Generated CYRENE protocol types.
//!
//! The schema is compiled from `contracts/proto/` by `build.rs` (tonic-build)
//! and included here. Proto files are the single contract source; do not
//! hand-edit generated code.

pub mod google {
    pub mod rpc {
        tonic::include_proto!("google.rpc");
    }
}

/// Generated types under their proto package path (`cy.llm`).
pub mod cy {
    pub mod llm {
        tonic::include_proto!("cy.llm");
    }
}

/// Core v1 generated types under their proto package path.
pub mod cyrene {
    pub mod core {
        pub mod v1 {
            tonic::include_proto!("cyrene.core.v1");
        }
    }
}

/// Convenience re-export of every generated message, enum, client and server
/// type so downstream crates can `use cy_proto::*;` or `use cy_proto::llm::*;`.
pub use cy::llm;
pub use cy::llm::*;
pub use cyrene::core::v1 as core_v1;

pub mod ai_service_client {
    pub use crate::cy::llm::ai_inference_client::AiInferenceClient as AiServiceClient;
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;

    fn fixture_bytes(name: &str) -> Vec<u8> {
        let manifest = include_str!("../../../fixtures/core/v1/manifest.json");
        let marker = format!("\"name\": \"{name}\"");
        let start = manifest
            .find(&marker)
            .unwrap_or_else(|| panic!("fixture {name} not found"));
        let entry = &manifest[start..];
        let wire_marker = "\"wire_hex\": \"";
        let wire_start = entry.find(wire_marker).unwrap() + wire_marker.len();
        let wire_end = entry[wire_start..].find('"').unwrap() + wire_start;
        let hex = &entry[wire_start..wire_end];
        hex.as_bytes()
            .chunks(2)
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect()
    }

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

    #[test]
    fn core_v1_fixtures_round_trip() {
        let hello = core_v1::NodeHello::decode(fixture_bytes("node_hello").as_slice()).unwrap();
        assert_eq!(hello.node.unwrap().node_id, "node-1");

        let welcome =
            core_v1::NodeWelcome::decode(fixture_bytes("node_welcome").as_slice()).unwrap();
        assert_eq!(welcome.selected_protocol_version, 1);

        let reserve =
            core_v1::ReserveResourcesRequest::decode(fixture_bytes("reserve_resources").as_slice())
                .unwrap();
        assert_eq!(reserve.node.unwrap().node_epoch, 7);
        assert_eq!(
            reserve
                .requirements
                .unwrap()
                .cpu
                .unwrap()
                .request_millicores,
            1000
        );

        let release =
            core_v1::ReleaseResourcesRequest::decode(fixture_bytes("release_resources").as_slice())
                .unwrap();
        assert_eq!(release.lease.unwrap().fence_token, 9);

        let launch = core_v1::LaunchPluginRequest::decode(
            fixture_bytes("inline_resource_claim_launch").as_slice(),
        )
        .unwrap();
        assert_eq!(launch.plugin.unwrap().plugin_id, "plugin-a");
        assert!(matches!(
            launch.allocation,
            Some(core_v1::launch_plugin_request::Allocation::ResourceClaim(_))
        ));

        let success =
            core_v1::Operation::decode(fixture_bytes("operation_success").as_slice()).unwrap();
        assert_eq!(success.state, core_v1::OperationState::Succeeded as i32);
        assert!(matches!(
            success.outcome,
            Some(core_v1::operation::Outcome::Result(_))
        ));

        let failure =
            core_v1::Operation::decode(fixture_bytes("operation_failure").as_slice()).unwrap();
        assert_eq!(failure.state, core_v1::OperationState::Failed as i32);
        assert!(matches!(
            failure.outcome,
            Some(core_v1::operation::Outcome::Error(_))
        ));

        let stale = core_v1::ReportHeartbeatResponse::decode(
            fixture_bytes("heartbeat_stale_generation").as_slice(),
        )
        .unwrap();
        assert_eq!(
            stale.disposition,
            core_v1::HeartbeatDisposition::StaleGeneration as i32
        );
    }
}
