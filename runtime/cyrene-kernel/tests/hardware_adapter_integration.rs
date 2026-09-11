// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: runtime/cyrene-kernel/tests/hardware_adapter_integration.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! Kernel integration tests for hardware adapter discovery and binding.
//!
//! Kernel 硬件适配器发现与 binding 集成测试。
#![cfg(unix)]

use std::{
    io::{Read, Write},
    os::unix::net::UnixListener,
    sync::Arc,
    thread,
    time::Duration,
};

use cy_adapter_client::{HardwareAdapterEndpoint, PeerCredentialExpectation};
use cy_kernel_api::{
    semantic, CgroupLimits, CleanupReport, DeviceBinding, LaunchPlan, LeaseState, NodeCapabilities,
    ProcessHandle, ProcessRuntime, ProviderError, ResourceRequest, SandboxBackend, StopRequest,
};
use cy_kernel_daemon::KernelDaemon;
use cy_proto::hardware_v1;
use cy_resource_manager::InMemoryResourceManager;
use prost::Message;
use tempfile::tempdir;

struct MockSandbox {
    ready: bool,
}

impl ProcessRuntime for MockSandbox {
    fn preflight(&self) -> NodeCapabilities {
        NodeCapabilities {
            ready: self.ready,
            facts: Vec::new(),
            enforcement: Vec::new(),
        }
    }

    fn launch(
        &self,
        _plan: &LaunchPlan,
        _binding: &DeviceBinding,
    ) -> Result<ProcessHandle, ProviderError> {
        Err(ProviderError::new(
            "mock-sandbox",
            "UNSUPPORTED",
            "not supported in test",
        ))
    }

    fn stop(
        &self,
        _handle: &ProcessHandle,
        _request: &StopRequest,
    ) -> Result<CleanupReport, ProviderError> {
        Ok(CleanupReport {
            complete: true,
            exit_code: Some(0),
            oom_killed: false,
            conditions: Vec::new(),
            reason_code: "STOPPED".to_string(),
        })
    }
}

impl SandboxBackend for MockSandbox {
    fn backend_id(&self) -> &str {
        "mock-sandbox"
    }
}

fn write_frame(mut writer: impl Write, payload: &[u8]) -> std::io::Result<()> {
    writer.write_all(&(payload.len() as u32).to_be_bytes())?;
    writer.write_all(payload)
}

fn read_frame(mut reader: impl Read) -> std::io::Result<Vec<u8>> {
    let mut length = [0_u8; 4];
    reader.read_exact(&mut length)?;
    let mut payload = vec![0; u32::from_be_bytes(length) as usize];
    reader.read_exact(&mut payload)?;
    Ok(payload)
}

fn serve_linux_adapter(
    listener: UnixListener,
    provider: cyrene_linux_sys_adapter::LinuxSystemProvider,
) {
    for mut stream in listener.incoming().flatten() {
        while let Ok(frame) = read_frame(&mut stream) {
            if let Ok(req) = hardware_v1::AdapterRequest::decode(&frame[..]) {
                let resp = cyrene_linux_sys_adapter::handle_request(&provider, req);
                let mut out = Vec::new();
                resp.encode(&mut out).unwrap();
                if write_frame(&mut stream, &out).is_err() {
                    break;
                }
            } else {
                break;
            }
        }
    }
}

fn serve_nvidia_adapter(
    listener: UnixListener,
    provider: cyrene_nvidia_adapter::discovery::NvidiaSmiProvider,
) {
    for mut stream in listener.incoming().flatten() {
        while let Ok(frame) = read_frame(&mut stream) {
            if let Ok(req) = hardware_v1::AdapterRequest::decode(&frame[..]) {
                let resp = cyrene_nvidia_adapter::handle_request(&provider, req);
                let mut out = Vec::new();
                resp.encode(&mut out).unwrap();
                if write_frame(&mut stream, &out).is_err() {
                    break;
                }
            } else {
                break;
            }
        }
    }
}

struct MockNvidiaRunner;

impl cyrene_nvidia_adapter::discovery::CommandRunner for MockNvidiaRunner {
    fn run(
        &self,
        _executable: &std::path::Path,
        args: &[String],
    ) -> Result<cyrene_nvidia_adapter::discovery::CommandOutput, ProviderError> {
        if args.iter().any(|a| a.contains("topo")) {
            Ok(cyrene_nvidia_adapter::discovery::CommandOutput {
                status: 0,
                stdout: "GPU0\tGPU1\nGPU0\tX\tNV4\nGPU1\tNV4\tX\n".to_string(),
                stderr: String::new(),
            })
        } else {
            Ok(cyrene_nvidia_adapter::discovery::CommandOutput {
                status: 0,
                stdout: "0, GPU-11111111-2222-3333-4444-555555555555, NVIDIA A100-SXM4-80GB, 0000:01:00.0, 81920, 80000\n".to_string(),
                stderr: String::new(),
            })
        }
    }
}

#[test]
fn test_kernel_daemon_dual_hardware_adapters_bootstrap_and_leasing(
) -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let linux_sys_socket = dir.path().join("linux_sys.sock");
    let nvidia_socket = dir.path().join("nvidia.sock");

    // 1. Bind and serve linux_sys adapter on UDS
    let linux_listener = UnixListener::bind(&linux_sys_socket)?;
    let linux_provider = cyrene_linux_sys_adapter::LinuxSystemProvider::new("linux-sys");
    thread::spawn(move || {
        serve_linux_adapter(linux_listener, linux_provider);
    });

    // 2. Bind and serve nvidia adapter on UDS
    let nvidia_listener = UnixListener::bind(&nvidia_socket)?;
    let nvidia_provider = cyrene_nvidia_adapter::discovery::NvidiaSmiProvider::new("nvidia-smi")
        .with_runner(Arc::new(MockNvidiaRunner));
    thread::spawn(move || {
        serve_nvidia_adapter(nvidia_listener, nvidia_provider);
    });

    // Give servers a brief moment to accept connections
    thread::sleep(Duration::from_millis(50));

    // 3. Configure HardwareAdapterEndpoints for KernelDaemon
    let linux_endpoint = HardwareAdapterEndpoint::new("linux-sys", &linux_sys_socket)
        .with_peer_credentials(PeerCredentialExpectation::default());

    let nvidia_endpoint = HardwareAdapterEndpoint::new("nvidia-smi", &nvidia_socket)
        .with_peer_credentials(PeerCredentialExpectation::default());

    let sandbox = Arc::new(MockSandbox { ready: true });
    let resources = Arc::new(InMemoryResourceManager::with_next_fence_token(
        "node-test-1",
        Vec::new(),
        1,
    ));

    // 4. Construct KernelDaemon with dual hardware adapters
    let daemon = Arc::new(KernelDaemon::with_hardware_adapters(
        vec![linux_endpoint, nvidia_endpoint],
        resources.clone(),
        sandbox,
        "node-test-1",
        1,
    )?);

    // 5. Verify Preflight Check Passes
    let inv_result = daemon.inventory();
    println!("Inventory probe result: {inv_result:?}");
    let sandbox_preflight = daemon.preflight_ready();
    println!("Preflight ready: {sandbox_preflight}");
    assert!(
        daemon.preflight_ready(),
        "Daemon preflight should pass with live adapters, inventory was: {inv_result:?}"
    );

    // 6. Bootstrap Initial Inventory Facts
    let initial_snapshot = daemon.refresh_inventory_facts()?;
    assert!(
        initial_snapshot.generation >= 1,
        "Inventory generation should be positive"
    );
    assert!(
        !initial_snapshot.resources.is_empty(),
        "Linux sys adapter should probe CPU/RAM resources"
    );

    let cpu_resources: Vec<_> = initial_snapshot
        .resources
        .iter()
        .filter(|r| r.resource_class == "compute.cpu" || r.resource_class.contains("cpu"))
        .collect();
    assert!(
        !cpu_resources.is_empty(),
        "Should discover host CPU resources from linux_sys adapter"
    );

    let mem_resources: Vec<_> = initial_snapshot
        .resources
        .iter()
        .filter(|r| r.resource_class == "memory.ram" || r.resource_class.contains("memory"))
        .collect();
    assert!(
        !mem_resources.is_empty(),
        "Should discover host Memory resources from linux_sys adapter"
    );

    // 7. Verify KernelCapabilities API reports the aggregated inventory
    let capabilities = daemon.get_kernel_capabilities()?;
    assert!(
        capabilities.inventory_generation >= initial_snapshot.generation,
        "capabilities are a later observation and may advance the aggregate generation"
    );
    assert_eq!(
        capabilities.resources.len(),
        initial_snapshot.resources.len()
    );

    // 8. Test Lease Acquisition against discovered Linux resource
    let target_resource = &initial_snapshot.resources[0];
    let request = ResourceRequest {
        lease_name: "test-workload-lease-1".to_string(),
        expected_inventory_generation: initial_snapshot.generation,
        holder: semantic::Identity {
            id: "worker-1".to_string(),
            generation: 1,
        },
        query: semantic::ResourceQuery {
            resource_class: target_resource.resource_class.clone(),
            count: 1,
            required_capabilities: Vec::new(),
            minimum_capacity: std::collections::BTreeMap::new(),
        },
        expires_at_unix_ms: Some(9999999999999),
        limits: CgroupLimits::default(),
    };

    let lease = daemon.acquire_lease(request)?;
    assert_eq!(lease.name, "test-workload-lease-1");
    assert_eq!(lease.fence_token, 1);
    assert_eq!(lease.state, LeaseState::Active);

    // 9. Release Lease after its (empty) physical cleanup is confirmed.
    daemon.begin_release("test-workload-lease-1", 1)?;
    daemon.complete_release("test-workload-lease-1", 1)?;
    let released_lease = daemon.lease("test-workload-lease-1")?;
    assert_eq!(released_lease.state, LeaseState::Released);

    // 10. Stale-generation rejection assertion (ADR Gate 4)
    let stale_request = ResourceRequest {
        lease_name: "test-stale-lease".to_string(),
        expected_inventory_generation: initial_snapshot.generation + 999, // Stale generation
        holder: semantic::Identity {
            id: "worker-1".to_string(),
            generation: 1,
        },
        query: semantic::ResourceQuery {
            resource_class: target_resource.resource_class.clone(),
            count: 1,
            required_capabilities: Vec::new(),
            minimum_capacity: std::collections::BTreeMap::new(),
        },
        expires_at_unix_ms: Some(9999999999999),
        limits: CgroupLimits::default(),
    };
    let stale_err = daemon.acquire_lease(stale_request).unwrap_err();
    assert_eq!(stale_err.reason_code, "STALE_INVENTORY_GENERATION");

    Ok(())
}

#[test]
fn test_kernel_daemon_rejects_corrupted_adapter_frame_fail_closed(
) -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let bad_socket = dir.path().join("bad_adapter.sock");

    // Start a server that writes garbage protobuf bytes
    let bad_listener = UnixListener::bind(&bad_socket)?;
    thread::spawn(move || {
        for mut stream in bad_listener.incoming().flatten() {
            // Read client request
            let _ = read_frame(&mut stream);
            // Return corrupted payload (e.g. invalid protobuf bytes)
            let garbage = vec![0xFF, 0xFF, 0xFF, 0xFF];
            let _ = write_frame(&mut stream, &garbage);
        }
    });

    thread::sleep(Duration::from_millis(50));

    let bad_endpoint = HardwareAdapterEndpoint::new("bad-adapter", &bad_socket)
        .with_peer_credentials(PeerCredentialExpectation::default());

    let sandbox = Arc::new(MockSandbox { ready: true });
    let resources = Arc::new(InMemoryResourceManager::with_next_fence_token(
        "node-bad",
        Vec::new(),
        1,
    ));

    let daemon = KernelDaemon::with_hardware_adapters(
        vec![bad_endpoint],
        resources,
        sandbox,
        "node-bad",
        1,
    )?;

    // Probing a corrupted adapter must fail-closed (return ProviderError) and not panic
    let probe_result = daemon.inventory();
    assert!(
        probe_result.is_err(),
        "Corrupted frame must return Err (fail-closed)"
    );
    let err = probe_result.unwrap_err();
    assert_eq!(err.adapter_id, "hardware-adapter-registry");
    assert_eq!(err.reason_code, "ADAPTER_UNAVAILABLE");
    assert!(err.message.contains("bad-adapter"));

    Ok(())
}
