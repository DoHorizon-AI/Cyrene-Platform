// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: kernel/crates/cy-kernel-daemon/src/tests/service_supervision_wire.rs
// ║ Module: CYRENE Platform
// ║ Role: Service supervision wire input validation regression tests.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Service supervision 线协议输入校验回归测试。
// ╚══════════════════════════════════════════════════════════════════════╝

use cy_proto::core_v1::{self, readiness_probe::Probe, restart_policy::Policy};
use prost_types::Duration as ProtoDuration;
use tonic::Code;

use crate::ServiceSupervisionManager;

fn service_spec() -> core_v1::ServiceSpec {
    core_v1::ServiceSpec {
        name: "test-service".to_string(),
        executable: "/bin/true".to_string(),
        ..Default::default()
    }
}

fn spec_with_duration(field: usize, duration: ProtoDuration) -> core_v1::ServiceSpec {
    let mut spec = service_spec();
    match field {
        0..=2 => {
            spec.readiness_probe = Some(core_v1::ReadinessProbe {
                probe: Some(Probe::ProcessAlive(core_v1::ProcessAliveProbe {})),
            });
            let mut config = core_v1::ProbeConfig::default();
            match field {
                0 => config.initial_delay = Some(duration),
                1 => config.period = Some(duration),
                _ => config.timeout = Some(duration),
            }
            spec.probe_config = Some(config);
        }
        3..=8 => {
            let mut backoff = core_v1::BackoffConfig::default();
            match field % 3 {
                0 => backoff.initial_delay = Some(duration),
                1 => backoff.max_delay = Some(duration),
                _ => backoff.reset_after = Some(duration),
            }
            let policy = if field <= 5 {
                Policy::OnFailure(core_v1::RestartPolicyOnFailure {
                    backoff: Some(backoff),
                    ..Default::default()
                })
            } else {
                Policy::Always(core_v1::RestartPolicyAlways {
                    backoff: Some(backoff),
                    ..Default::default()
                })
            };
            spec.restart_policy = Some(core_v1::RestartPolicy {
                policy: Some(policy),
            });
        }
        9 => spec.graceful_stop_timeout = Some(duration),
        _ => unreachable!("unknown duration field"),
    }
    spec
}

#[test]
fn rejects_negative_seconds_in_all_service_duration_fields() {
    for field in 0..10 {
        let spec = spec_with_duration(
            field,
            ProtoDuration {
                seconds: -1,
                nanos: 0,
            },
        );
        let error = ServiceSupervisionManager::proto_spec_to_domain(spec).unwrap_err();
        assert_eq!(error.code(), Code::InvalidArgument, "field {field}");
    }
}

#[test]
fn rejects_other_invalid_protobuf_durations() {
    for duration in [
        ProtoDuration {
            seconds: 0,
            nanos: -1,
        },
        ProtoDuration {
            seconds: 0,
            nanos: 1_000_000_000,
        },
        ProtoDuration {
            seconds: 315_576_000_001,
            nanos: 0,
        },
    ] {
        let error =
            ServiceSupervisionManager::proto_spec_to_domain(spec_with_duration(9, duration))
                .unwrap_err();
        assert_eq!(error.code(), Code::InvalidArgument);
    }
}

#[test]
fn rejects_out_of_range_probe_and_endpoint_fields() {
    for field in 0..4 {
        let mut spec = service_spec();
        match field {
            0 => {
                spec.readiness_probe = Some(core_v1::ReadinessProbe {
                    probe: Some(Probe::TcpSocket(core_v1::TcpSocketProbe {
                        port: 70_000,
                        ..Default::default()
                    })),
                });
            }
            1 | 2 => {
                spec.readiness_probe = Some(core_v1::ReadinessProbe {
                    probe: Some(Probe::HttpGet(core_v1::HttpGetProbe {
                        port: if field == 1 { 70_000 } else { 80 },
                        expected_status: (field == 2).then_some(65_736),
                        ..Default::default()
                    })),
                });
            }
            _ => {
                spec.endpoint = Some(core_v1::ServiceEndpointSpec {
                    port: Some(70_000),
                    ..Default::default()
                });
            }
        }
        let error = ServiceSupervisionManager::proto_spec_to_domain(spec).unwrap_err();
        assert_eq!(error.code(), Code::InvalidArgument, "field {field}");
    }
}

#[test]
fn retains_valid_boundary_values() {
    let mut spec = spec_with_duration(
        9,
        ProtoDuration {
            seconds: 1,
            nanos: 999_999_999,
        },
    );
    spec.endpoint = Some(core_v1::ServiceEndpointSpec {
        port: Some(u16::MAX as u32),
        ..Default::default()
    });
    let domain = ServiceSupervisionManager::proto_spec_to_domain(spec).unwrap();
    assert_eq!(domain.graceful_stop_timeout.as_secs(), 1);
    assert_eq!(domain.graceful_stop_timeout.subsec_nanos(), 999_999_999);
    assert_eq!(domain.endpoint.unwrap().port, Some(u16::MAX));
}
