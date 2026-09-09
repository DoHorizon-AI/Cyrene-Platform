// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: contracts/rust/cy-proto/src/lib.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! 自动生成的 CYRENE Core v1 平台核心网络协议与 gRPC 类型。
//!
//! 【协议契约源头】
//! 本模块中的代码是在编译期由 `build.rs` 通过 `tonic-build` 编译 `contracts/proto/cyrene/core/v1/`
//! 下的 Protobuf 文件生成的。
//! Proto 文件是整个平台的单一契约事实来源（Single Source of Truth），包含：
//! - 节点问候与握手协议 ([`core_v1::NodeHello`], [`core_v1::NodeWelcome`])
//! - 通用资源与能力事实 ([`semantic_v1::Resource`], [`semantic_v1::Capability`])
//! - 资源租约管理与围栏令牌 ([`core_v1::AcquireLeaseRequest`], [`semantic_v1::Lease`])
//! - 插件进程生命周期与清理状态 ([`core_v1::PluginProcess`])
//! - 控制面下发指令与心跳流 ([`core_v1::KernelCommand`], [`core_v1::ReportHeartbeatResponse`])

// Prost owns the generated enum representation; wire compatibility takes
// precedence over hand-boxing generated variants in this projection crate.
#![allow(clippy::large_enum_variant)]

pub mod google {
    pub mod rpc {
        tonic::include_proto!("google.rpc");
    }
}

/// 自动生成的 Core v1 消息、枚举、客户端及服务器端定义模块。
pub mod cyrene {
    pub mod core {
        pub mod v1 {
            tonic::include_proto!("cyrene.core.v1");
        }

        pub mod v2 {
            tonic::include_proto!("cyrene.core.v2");
        }
    }

    pub mod capability {
        pub mod v1 {
            tonic::include_proto!("cyrene.capability.v1");
        }
    }

    pub mod hardware {
        pub mod v1 {
            tonic::include_proto!("cyrene.hardware.v1");
        }
    }

    pub mod sandbox {
        pub mod v1 {
            tonic::include_proto!("cyrene.sandbox.v1");
        }
    }

    pub mod semantic {
        pub mod v1 {
            tonic::include_proto!("cyrene.semantic.v1");
        }
    }

    pub mod provider {
        pub mod v1 {
            tonic::include_proto!("cyrene.provider.v1");
        }
    }

    pub mod workspace {
        pub mod v1 {
            tonic::include_proto!("cyrene.workspace.v1");
        }
    }
}

/// Product-facing language-neutral capability execution service.
pub use cyrene::capability::v1 as capability_v1;
/// 简写别名：便于外部代码直接引用 `cy_proto::core_v1::*`。
pub use cyrene::core::v1 as core_v1;
/// Core v2 authority projection with explicit namespace scope.
pub use cyrene::core::v2 as core_v2;
/// Versioned local protocol between the Kernel and external hardware adapters.
pub use cyrene::hardware::v1 as hardware_v1;
/// Dedicated local Provider lifecycle and reconciliation projection.
pub use cyrene::provider::v1 as provider_v1;
/// Versioned local protocol between the Kernel and the external Sandbox Adapter Host.
pub use cyrene::sandbox::v1 as sandbox_v1;
/// Transport projection of the Kernel Semantic Contract v1 nouns.
pub use cyrene::semantic::v1 as semantic_v1;
/// Transport-neutral Workspace discovery and relay API projection.
pub use cyrene::workspace::v1 as workspace_v1;

#[cfg(test)]
#[allow(deprecated)]
mod tests {
    use super::{core_v1, hardware_v1, sandbox_v1, semantic_v1};
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
    fn core_v1_fixtures_round_trip() {
        let hello_bytes = fixture_bytes("node_hello");
        let hello = core_v1::NodeHello::decode(hello_bytes.as_slice()).unwrap();
        assert_eq!(hello.encode_to_vec(), hello_bytes);
        assert_eq!(hello.node.unwrap().node_id, "node-1");

        let welcome_bytes = fixture_bytes("node_welcome");
        let welcome = core_v1::NodeWelcome::decode(welcome_bytes.as_slice()).unwrap();
        assert_eq!(welcome.encode_to_vec(), welcome_bytes);
        assert_eq!(welcome.selected_protocol_version, 1);

        let reserve_bytes = fixture_bytes("reserve_resources");
        let reserve = core_v1::ReserveResourcesRequest::decode(reserve_bytes.as_slice()).unwrap();
        assert_eq!(reserve.encode_to_vec(), reserve_bytes);
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

        let release_bytes = fixture_bytes("release_resources");
        let release = core_v1::ReleaseResourcesRequest::decode(release_bytes.as_slice()).unwrap();
        assert_eq!(release.encode_to_vec(), release_bytes);
        assert_eq!(release.lease.unwrap().fence_token, 9);

        let launch_bytes = fixture_bytes("inline_resource_claim_launch");
        let launch = core_v1::LaunchPluginRequest::decode(launch_bytes.as_slice()).unwrap();
        assert_eq!(launch.encode_to_vec(), launch_bytes);
        assert_eq!(launch.plugin.unwrap().plugin_id, "plugin-a");
        assert!(matches!(
            launch.allocation,
            Some(core_v1::launch_plugin_request::Allocation::ResourceClaim(_))
        ));

        let success_bytes = fixture_bytes("operation_success");
        let success = core_v1::Operation::decode(success_bytes.as_slice()).unwrap();
        assert_eq!(success.encode_to_vec(), success_bytes);
        assert_eq!(success.state, core_v1::OperationState::Succeeded as i32);
        assert!(matches!(
            success.outcome,
            Some(core_v1::operation::Outcome::Result(_))
        ));

        let failure_bytes = fixture_bytes("operation_failure");
        let failure = core_v1::Operation::decode(failure_bytes.as_slice()).unwrap();
        assert_eq!(failure.encode_to_vec(), failure_bytes);
        assert_eq!(failure.state, core_v1::OperationState::Failed as i32);
        assert!(matches!(
            failure.outcome,
            Some(core_v1::operation::Outcome::Error(_))
        ));

        let stale_bytes = fixture_bytes("heartbeat_stale_generation");
        let stale = core_v1::ReportHeartbeatResponse::decode(stale_bytes.as_slice()).unwrap();
        assert_eq!(stale.encode_to_vec(), stale_bytes);
        assert_eq!(
            stale.disposition,
            core_v1::HeartbeatDisposition::StaleGeneration as i32
        );

        let device_bytes = fixture_bytes("accelerator_topology");
        let device = core_v1::AcceleratorDevice::decode(device_bytes.as_slice()).unwrap();
        assert_eq!(device.encode_to_vec(), device_bytes);
        assert_eq!(device.device_id, "gpu-0");
        assert_eq!(device.numa_node, Some(1));
        assert_eq!(device.links[0].peer_device_id, "gpu-1");
        assert_eq!(
            device.links[0].link_type,
            core_v1::AcceleratorLinkType::Nvlink as i32
        );

        let lease_bytes = fixture_bytes("lease_inventory_generation");
        let lease = core_v1::ResourceLease::decode(lease_bytes.as_slice()).unwrap();
        assert_eq!(lease.encode_to_vec(), lease_bytes);
        assert_eq!(lease.fence_token, 9);
        assert_eq!(lease.inventory_generation, 42);

        let process_bytes = fixture_bytes("process_cleanup_stuck");
        let process = core_v1::PluginProcess::decode(process_bytes.as_slice()).unwrap();
        assert_eq!(process.encode_to_vec(), process_bytes);
        assert_eq!(
            process.cleanup_state,
            core_v1::ProcessCleanupState::Stuck as i32
        );
        assert_eq!(process.conditions[0].reason_code, "REAP_TIMEOUT");
    }

    #[test]
    fn worker_control_tck_fixtures_round_trip() {
        let hello =
            core_v1::WorkerToKernel::decode(fixture_bytes("worker_control_hello").as_slice())
                .unwrap();
        assert!(matches!(
            hello.body,
            Some(core_v1::worker_to_kernel::Body::Hello(core_v1::WorkerHello {
                ref plugin_instance_name,
                generation: 7,
                protocol_version: 1,
            })) if plugin_instance_name == "worker-1"
        ));

        let heartbeat =
            core_v1::WorkerToKernel::decode(fixture_bytes("worker_control_heartbeat").as_slice())
                .unwrap();
        assert!(matches!(
            heartbeat.body,
            Some(core_v1::worker_to_kernel::Body::Heartbeat(
                core_v1::WorkerHeartbeat {
                    generation: 7,
                    sequence_number: 1,
                    runtime_state: 5,
                    ..
                }
            ))
        ));

        let shutdown_ack = core_v1::WorkerToKernel::decode(
            fixture_bytes("worker_control_shutdown_ack").as_slice(),
        )
        .unwrap();
        assert!(matches!(
            shutdown_ack.body,
            Some(core_v1::worker_to_kernel::Body::ShutdownAck(core_v1::WorkerShutdownAck {
                ref shutdown_id,
                generation: 7,
                drained: true,
                ..
            })) if shutdown_id == "shutdown-1"
        ));

        let welcome =
            core_v1::KernelToWorker::decode(fixture_bytes("worker_control_welcome").as_slice())
                .unwrap();
        assert!(matches!(
            welcome.body,
            Some(core_v1::kernel_to_worker::Body::Welcome(
                core_v1::WorkerWelcome {
                    desired_state: 1,
                    desired_generation: 7,
                    next_heartbeat_after: Some(prost_types::Duration {
                        seconds: 5,
                        nanos: 0
                    }),
                }
            ))
        ));

        let heartbeat_ack = core_v1::KernelToWorker::decode(
            fixture_bytes("worker_control_heartbeat_ack").as_slice(),
        )
        .unwrap();
        assert!(matches!(
            heartbeat_ack.body,
            Some(core_v1::kernel_to_worker::Body::HeartbeatAck(
                core_v1::WorkerHeartbeatAck {
                    disposition: 1,
                    accepted_sequence_number: 1,
                    desired_state: 1,
                    desired_generation: 7,
                }
            ))
        ));

        let shutdown =
            core_v1::KernelToWorker::decode(fixture_bytes("worker_control_shutdown").as_slice())
                .unwrap();
        assert!(matches!(
            shutdown.body,
            Some(core_v1::kernel_to_worker::Body::Shutdown(core_v1::WorkerShutdown {
                ref shutdown_id,
                mode: 1,
                ack_deadline: Some(prost_types::Duration { seconds: 3, nanos: 0 }),
                ref reason_code,
            })) if shutdown_id == "shutdown-1" && reason_code == "TERMINATE_REQUESTED"
        ));
    }

    #[test]
    fn core_v1_does_not_expose_process_or_large_artifact_inputs() {
        let core_dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../proto/cyrene/core/v1");
        for entry in std::fs::read_dir(core_dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|value| value.to_str()) != Some("proto") {
                continue;
            }
            let source = std::fs::read_to_string(path).unwrap().to_ascii_lowercase();
            for forbidden in [
                " shell ",
                " argv ",
                " env ",
                " model ",
                " dataset ",
                " checkpoint ",
                " stdout ",
                " stderr ",
                " payload ",
            ] {
                assert!(
                    !source.contains(forbidden),
                    "forbidden field token: {forbidden}"
                );
            }
        }
    }

    #[test]
    fn hardware_adapter_request_round_trips() {
        let request = hardware_v1::AdapterRequest {
            protocol_version: 1,
            body: Some(hardware_v1::adapter_request::Body::CreateBinding(
                hardware_v1::CreateBindingRequest {
                    device_id: "device-1".to_string(),
                    expected_inventory_generation: 42,
                    resource: None,
                },
            )),
        };
        let encoded = request.encode_to_vec();
        let decoded = hardware_v1::AdapterRequest::decode(encoded.as_slice()).unwrap();
        assert_eq!(decoded.protocol_version, 1);
        assert!(matches!(
            decoded.body,
            Some(hardware_v1::adapter_request::Body::CreateBinding(binding))
                if binding.device_id == "device-1" && binding.expected_inventory_generation == 42
        ));
    }

    #[test]
    fn sandbox_adapter_request_round_trips() {
        let request = sandbox_v1::SandboxRequest {
            protocol_version: 1,
            body: Some(sandbox_v1::sandbox_request::Body::Preflight(
                sandbox_v1::SandboxPreflightRequest {},
            )),
        };
        let bytes = request.encode_to_vec();
        let decoded = sandbox_v1::SandboxRequest::decode(bytes.as_slice()).unwrap();
        assert_eq!(decoded.protocol_version, 1);
        assert!(matches!(
            decoded.body,
            Some(sandbox_v1::sandbox_request::Body::Preflight(_))
        ));
    }

    #[test]
    fn semantic_contract_projection_round_trips_generic_resources() {
        let resource = semantic_v1::Resource {
            identity: Some(semantic_v1::Identity {
                id: "resource-1".to_string(),
                generation: 7,
            }),
            provider: Some(semantic_v1::Identity {
                id: "provider-1".to_string(),
                generation: 3,
            }),
            resource_class: "accelerator".to_string(),
            capabilities: vec![semantic_v1::Capability {
                id: "accelerator.compute".to_string(),
                revision: 1,
                properties: Default::default(),
            }],
            capacity: Default::default(),
            attributes: Default::default(),
            state: semantic_v1::ResourceState::Ready as i32,
            reason_code: "ready".to_string(),
            summary: "resource is ready".to_string(),
            links: Vec::new(),
        };
        let encoded = resource.encode_to_vec();
        let decoded = semantic_v1::Resource::decode(encoded.as_slice()).unwrap();
        assert_eq!(decoded, resource);
        assert_eq!(decoded.resource_class, "accelerator");
    }
}
