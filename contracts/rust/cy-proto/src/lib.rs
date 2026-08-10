//! 自动生成的 CYRENE Core v1 平台核心网络协议与 gRPC 类型。
//!
//! 【协议契约源头】
//! 本模块中的代码是在编译期由 `build.rs` 通过 `tonic-build` 编译 `contracts/proto/cyrene/core/v1/`
//! 下的 Protobuf 文件生成的。
//! Proto 文件是整个平台的单一契约事实来源（Single Source of Truth），包含：
//! - 节点问候与握手协议 ([`core_v1::NodeHello`], [`core_v1::NodeWelcome`])
//! - 硬件加速卡拓扑与设备信息 ([`core_v1::AcceleratorDevice`])
//! - 资源租约管理与围栏令牌 ([`core_v1::ReserveResourcesRequest`], [`core_v1::ResourceLease`])
//! - 插件进程生命周期与清理状态 ([`core_v1::PluginProcess`])
//! - 控制面下发指令与心跳流 ([`core_v1::KernelCommand`], [`core_v1::ReportHeartbeatResponse`])

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
    }
}

/// 简写别名：便于外部代码直接引用 `cy_proto::core_v1::*`。
pub use cyrene::core::v1 as core_v1;

#[cfg(test)]
mod tests {
    use super::core_v1;
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
}
