// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: adapters/hardware/linux_sys/src/tests.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! Unit and contract tests for cyrene-linux-sys-adapter.

use std::fs;

use cy_kernel_api::{
    semantic::ResourceState, EnforcementMode, HostInventoryProvider, ResourceProvider,
};
use cy_proto::hardware_v1;

use crate::{
    handle_request, parse_cpuinfo, parse_meminfo, peer::client_peer_credentials_allowed,
    LinuxSystemProvider, PROTOCOL_VERSION,
};

const SAMPLE_CPUINFO: &str = r#"
processor	: 0
vendor_id	: GenuineIntel
cpu family	: 6
model		: 158
model name	: Intel(R) Core(TM) i7-8700K CPU @ 3.70GHz
stepping	: 10
microcode	: 0xde
cpu MHz		: 3700.000
cache size	: 12288 KB
physical id	: 0
siblings	: 12
core id		: 0
cpu cores	: 6
flags		: fpu vme de pse tsc msr pae mce cx8 apic sep mtrr pge mca cmov pat pse36 clflush dts acpi mmx fxsr sse sse2 ss ht tm pbe syscall nx pdpe1gb rdtscp lm constant_tsc art arch_perfmon pebs bts rep_good nopl xtopology nonstop_tsc cpuid aperfmperf pni pclmulqdq dtes64 monitor ds_cpl vmx smx est tm2 ssse3 sdbg fma cx16 xtpr pdcm pcid sse4_1 sse4_2 x2apic movbe popcnt tsc_deadline_timer aes xsave avx f16c rdrand lahf_lm abm 3dnowprefetch cpuid_fault epb invpcid_single pti ssbd ibrs ibpb stibp tpr_shadow vnmi flexpriority ept vpid ept_ad fsgsbase tsc_adjust bmi1 hle avx2 smep bmi2 erms invpcid rtm mpx rdseed adx smap clflushopt intel_pt xsaveopt xsavec xgetbv1 xsaves dtherm ida arat pln pts hwp hwp_notify hwp_act_window hwp_epp md_clear flush_l1d arch_capabilities

processor	: 1
vendor_id	: GenuineIntel
cpu family	: 6
model		: 158
model name	: Intel(R) Core(TM) i7-8700K CPU @ 3.70GHz
stepping	: 10
microcode	: 0xde
cpu MHz		: 3700.000
cache size	: 12288 KB
physical id	: 0
siblings	: 12
core id		: 1
cpu cores	: 6
flags		: fpu vme de pse tsc msr pae mce cx8 apic sep mtrr pge mca cmov pat pse36 clflush dts acpi mmx fxsr sse sse2 ss ht tm pbe syscall nx pdpe1gb rdtscp lm constant_tsc art arch_perfmon pebs bts rep_good nopl xtopology nonstop_tsc cpuid aperfmperf pni pclmulqdq dtes64 monitor ds_cpl vmx smx est tm2 ssse3 sdbg fma cx16 xtpr pdcm pcid sse4_1 sse4_2 x2apic movbe popcnt tsc_deadline_timer aes xsave avx f16c rdrand lahf_lm abm 3dnowprefetch cpuid_fault epb invpcid_single pti ssbd ibrs ibpb stibp tpr_shadow vnmi flexpriority ept vpid ept_ad fsgsbase tsc_adjust bmi1 hle avx2 smep bmi2 erms invpcid rtm mpx rdseed adx smap clflushopt intel_pt xsaveopt xsavec xgetbv1 xsaves dtherm ida arat pln pts hwp hwp_notify hwp_act_window hwp_epp md_clear flush_l1d arch_capabilities
"#;

const SAMPLE_MEMINFO: &str = r#"
MemTotal:       32768000 kB
MemFree:         8192000 kB
MemAvailable:   24576000 kB
Buffers:         1024000 kB
Cached:          5120000 kB
SwapCached:            0 kB
Active:         12000000 kB
Inactive:        6000000 kB
SwapTotal:       8388608 kB
SwapFree:        8388608 kB
"#;

#[test]
fn cpuinfo_parser_extracts_cores_and_model() {
    let cpu = parse_cpuinfo(SAMPLE_CPUINFO).expect("cpuinfo parse should succeed");
    assert_eq!(cpu.model_name, "Intel(R) Core(TM) i7-8700K CPU @ 3.70GHz");
    assert_eq!(cpu.logical_cores, 2);
    assert_eq!(cpu.physical_cores, 2);
    assert!(cpu.flags.contains(&"avx2".to_string()));
    assert!(cpu.flags.contains(&"sse4_2".to_string()));
}

#[test]
fn meminfo_parser_extracts_memory_and_swap() {
    let mem = parse_meminfo(SAMPLE_MEMINFO).expect("meminfo parse should succeed");
    assert_eq!(mem.total_bytes, 32768000 * 1024);
    assert_eq!(mem.available_bytes, 24576000 * 1024);
    assert_eq!(mem.swap_total_bytes, 8388608 * 1024);
    assert_eq!(mem.swap_free_bytes, 8388608 * 1024);
}

#[test]
fn provider_probes_inventory_from_mock_filesystem() {
    let temp_dir = tempfile::tempdir().expect("tempdir create failed");
    let procfs = temp_dir.path().join("proc");
    let sysfs = temp_dir.path().join("sys");
    let cgroup = temp_dir.path().join("cgroup");

    fs::create_dir_all(&procfs).unwrap();
    fs::create_dir_all(&sysfs).unwrap();
    fs::create_dir_all(&cgroup).unwrap();

    fs::write(procfs.join("cpuinfo"), SAMPLE_CPUINFO).unwrap();
    fs::write(procfs.join("meminfo"), SAMPLE_MEMINFO).unwrap();
    fs::write(cgroup.join("cgroup.controllers"), "cpu memory pids").unwrap();
    fs::write(cgroup.join("cgroup.kill"), "1").unwrap();

    let provider = LinuxSystemProvider::new("hardware-adapter-linux-sys")
        .with_procfs_root(&procfs)
        .with_sysfs_root(&sysfs)
        .with_cgroup_root(&cgroup);

    let inventory = provider
        .probe_inventory()
        .expect("probe inventory should succeed");

    assert_eq!(inventory.generation, 1);
    assert_eq!(inventory.resources.len(), 2);

    let cpu_res = inventory
        .resources
        .iter()
        .find(|r| r.resource_class == "compute.cpu")
        .expect("cpu resource must be present");
    assert_eq!(cpu_res.identity.id, "cpu-host");
    assert_eq!(cpu_res.capacity.get("cores").unwrap().value, 2);
    assert_eq!(cpu_res.state, ResourceState::Ready);

    let ram_res = inventory
        .resources
        .iter()
        .find(|r| r.resource_class == "memory.ram")
        .expect("ram resource must be present");
    assert_eq!(ram_res.identity.id, "ram-host");
    assert_eq!(
        ram_res.capacity.get("capacity_bytes").unwrap().value,
        32768000 * 1024
    );

    assert!(inventory.capabilities.ready);
    assert!(inventory
        .capabilities
        .facts
        .iter()
        .any(|f| f.name == "cgroup-v2" && f.available));
    assert!(inventory
        .capabilities
        .facts
        .iter()
        .any(|f| f.name == "cpu-controller" && f.available));

    let binding = provider
        .create_binding(cpu_res)
        .expect("binding should succeed");
    assert_eq!(binding.resource_id, "cpu-host");
    assert_eq!(binding.enforcement, EnforcementMode::Soft);
}

#[test]
#[allow(deprecated)]
fn protocol_request_handling_returns_valid_inventory_and_binding() {
    let temp_dir = tempfile::tempdir().expect("tempdir create failed");
    let procfs = temp_dir.path().join("proc");
    fs::create_dir_all(&procfs).unwrap();
    fs::write(procfs.join("cpuinfo"), SAMPLE_CPUINFO).unwrap();
    fs::write(procfs.join("meminfo"), SAMPLE_MEMINFO).unwrap();

    let provider = LinuxSystemProvider::new("hardware-adapter-linux-sys").with_procfs_root(&procfs);

    // Test GetInventory request
    let req = hardware_v1::AdapterRequest {
        protocol_version: PROTOCOL_VERSION,
        body: Some(hardware_v1::adapter_request::Body::GetInventory(
            hardware_v1::GetInventoryRequest {
                node_id: "node-1".to_string(),
                known_generation: 0,
            },
        )),
    };

    let resp = handle_request(&provider, req);
    assert_eq!(resp.protocol_version, PROTOCOL_VERSION);
    assert_eq!(resp.adapter_id, "hardware-adapter-linux-sys");

    let inventory = match resp.body {
        Some(hardware_v1::adapter_response::Body::Inventory(inv)) => inv,
        other => panic!("expected Inventory body, got {other:?}"),
    };
    assert_eq!(inventory.resources.len(), 2);

    // Test CreateBinding request
    let bind_req = hardware_v1::AdapterRequest {
        protocol_version: PROTOCOL_VERSION,
        body: Some(hardware_v1::adapter_request::Body::CreateBinding(
            hardware_v1::CreateBindingRequest {
                device_id: "cpu-host".to_string(),
                expected_inventory_generation: inventory.generation,
                resource: None,
            },
        )),
    };

    let bind_resp = handle_request(&provider, bind_req);
    match bind_resp.body {
        Some(hardware_v1::adapter_response::Body::Binding(binding)) => {
            assert_eq!(binding.device_id, "cpu-host");
        }
        other => panic!("expected Binding body, got {other:?}"),
    }
}

#[test]
#[allow(deprecated)]
fn binding_rejects_stale_inventory_or_resource_generation() {
    let temp_dir = tempfile::tempdir().expect("tempdir create failed");
    let procfs = temp_dir.path().join("proc");
    fs::create_dir_all(&procfs).unwrap();
    fs::write(procfs.join("cpuinfo"), SAMPLE_CPUINFO).unwrap();
    fs::write(procfs.join("meminfo"), SAMPLE_MEMINFO).unwrap();

    let provider = LinuxSystemProvider::new("hardware-adapter-linux-sys").with_procfs_root(&procfs);

    // 1. Stale inventory generation
    let stale_inv_req = hardware_v1::AdapterRequest {
        protocol_version: PROTOCOL_VERSION,
        body: Some(hardware_v1::adapter_request::Body::CreateBinding(
            hardware_v1::CreateBindingRequest {
                device_id: "cpu-host".to_string(),
                expected_inventory_generation: 999, // stale!
                resource: None,
            },
        )),
    };
    let resp = handle_request(&provider, stale_inv_req);
    match resp.body {
        Some(hardware_v1::adapter_response::Body::Error(err)) => {
            assert_eq!(err.reason_code, "INVENTORY_GENERATION_STALE");
        }
        other => panic!("expected Error body, got {other:?}"),
    }

    // 2. Stale resource generation
    let stale_res_req = hardware_v1::AdapterRequest {
        protocol_version: PROTOCOL_VERSION,
        body: Some(hardware_v1::adapter_request::Body::CreateBinding(
            hardware_v1::CreateBindingRequest {
                device_id: "cpu-host".to_string(),
                expected_inventory_generation: 1,
                resource: Some(cy_proto::semantic_v1::Identity {
                    id: "cpu-host".to_string(),
                    generation: 999, // stale resource generation!
                }),
            },
        )),
    };
    let resp = handle_request(&provider, stale_res_req);
    match resp.body {
        Some(hardware_v1::adapter_response::Body::Error(err)) => {
            assert_eq!(err.reason_code, "RESOURCE_GENERATION_STALE");
        }
        other => panic!("expected Error body, got {other:?}"),
    }

    // 3. Absent resource
    let absent_req = hardware_v1::AdapterRequest {
        protocol_version: PROTOCOL_VERSION,
        body: Some(hardware_v1::adapter_request::Body::CreateBinding(
            hardware_v1::CreateBindingRequest {
                device_id: "non-existent-device".to_string(),
                expected_inventory_generation: 1,
                resource: None,
            },
        )),
    };
    let resp = handle_request(&provider, absent_req);
    match resp.body {
        Some(hardware_v1::adapter_response::Body::Error(err)) => {
            assert_eq!(err.reason_code, "RESOURCE_NOT_FOUND");
        }
        other => panic!("expected Error body, got {other:?}"),
    }
}

#[test]
fn peer_credential_policy_allows_matching_and_rejects_mismatch() {
    // Neither configured -> allow all
    assert!(client_peer_credentials_allowed(1000, 2000, None, None));

    // Only UID configured
    assert!(client_peer_credentials_allowed(
        1000,
        9999,
        Some(1000),
        None
    ));
    assert!(!client_peer_credentials_allowed(
        1001,
        2000,
        Some(1000),
        None
    ));

    // Only GID configured
    assert!(client_peer_credentials_allowed(
        9999,
        2000,
        None,
        Some(2000)
    ));
    assert!(!client_peer_credentials_allowed(
        1000,
        2001,
        None,
        Some(2000)
    ));

    // Both UID and GID configured (AND semantics)
    assert!(client_peer_credentials_allowed(
        1000,
        2000,
        Some(1000),
        Some(2000)
    ));
    assert!(!client_peer_credentials_allowed(
        1001,
        2000,
        Some(1000),
        Some(2000)
    ));
    assert!(!client_peer_credentials_allowed(
        1000,
        2001,
        Some(1000),
        Some(2000)
    ));
    assert!(!client_peer_credentials_allowed(
        1001,
        2001,
        Some(1000),
        Some(2000)
    ));
}
