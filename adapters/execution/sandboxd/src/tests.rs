//! Unit tests for cgroup v2 runtime, cleanup, telemetry, and device filtering.

use std::{collections::BTreeMap, fs, fs::File, path::PathBuf};

use cy_kernel_api::{DeviceBinding, DeviceMapper, EnforcementMode, ProcessHandle, ProcessRuntime};

#[cfg(target_os = "linux")]
use crate::bpf::{
    build_device_filter_program, DeviceRule, BPF_ALU64_MOV_K, BPF_DEVCG_DEV_CHAR, BPF_JMP_EXIT,
};
use crate::{bpf::LinuxDeviceMapper, config::CgroupV2Config, runtime::CgroupV2Runtime};

fn config(root: PathBuf) -> CgroupV2Config {
    CgroupV2Config {
        root,
        device_bpf_enabled: false,
    }
}

#[test]
fn preflight_requires_cgroup_v2_and_cgroup_kill() {
    let root = std::env::temp_dir().join(format!("cyrene-cgroup-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("cgroup.controllers"), "cpu memory pids").unwrap();
    File::create(root.join("cgroup.kill")).unwrap();
    assert!(CgroupV2Runtime::new(config(root.clone())).preflight().ready);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn owned_cleanup_never_selects_non_instance_children() {
    let root = std::env::temp_dir().join(format!("cyrene-cleanup-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("foreign")).unwrap();
    fs::create_dir_all(root.join("instance-stale")).unwrap();
    File::create(root.join("instance-stale/cgroup.procs")).unwrap();
    File::create(root.join("instance-stale/cgroup.kill")).unwrap();
    // A normal directory cannot emulate cgroupfs: its virtual control
    // files remain ordinary files, so removal must fail closed. The key
    // invariant is that the unrelated sibling is never selected.
    let error = CgroupV2Runtime::new(config(root.clone()))
        .cleanup_owned_instances()
        .unwrap_err();
    assert_eq!(error.reason_code, "OWNED_CGROUP_CLEANUP_INCOMPLETE");
    assert!(root.join("foreign").exists());
    assert!(root.join("instance-stale").exists());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn telemetry_uses_kernel_files_without_estimation() {
    let root = std::env::temp_dir().join(format!("cyrene-telemetry-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("memory.current"), "42\n").unwrap();
    fs::write(root.join("memory.events.local"), "oom_kill 7\n").unwrap();
    fs::write(
        root.join("cpu.stat"),
        "usage_usec 11\nuser_usec 8\nsystem_usec 3\n",
    )
    .unwrap();
    let telemetry = CgroupV2Runtime::new(config(root.clone())).telemetry(&ProcessHandle {
        pid: 1,
        cgroup_path: root.clone(),
        start_time_ticks: None,
    });
    assert_eq!(telemetry.memory_current_bytes, Some(42));
    assert_eq!(telemetry.cpu_usage_usec, Some(11));
    assert_eq!(telemetry.oom_kill_count, 7);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn hard_device_request_fails_closed_when_disabled() {
    let mapper = LinuxDeviceMapper {
        device_bpf_enabled: false,
    };
    let binding = DeviceBinding {
        resource_id: "GPU-0".to_string(),
        nodes: Vec::new(),
        environment: BTreeMap::new(),
        required_gids: Vec::new(),
        enforcement: EnforcementMode::Hard,
        adapter_id: "test".to_string(),
        reason_code: "test".to_string(),
    };
    assert_eq!(
        mapper
            .enforce(&binding, EnforcementMode::Hard)
            .unwrap_err()
            .reason_code,
        "HARD_ENFORCEMENT_UNAVAILABLE"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn device_filter_program_is_default_deny_and_has_allow_path() {
    let program = build_device_filter_program(&[DeviceRule {
        kind: BPF_DEVCG_DEV_CHAR,
        major: 195,
        minor: 0,
    }]);
    assert_eq!(program.last().unwrap().code, BPF_JMP_EXIT);
    assert_eq!(program[program.len() - 2].imm, 0);
    assert!(program
        .iter()
        .any(|instruction| instruction.code == BPF_ALU64_MOV_K && instruction.imm == 1));
}
