//! End-to-end integration tests connecting cy-sandbox-client with cyrene-sandboxd.

use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    os::unix::net::UnixListener,
    path::PathBuf,
    sync::Arc,
    thread,
    time::Duration,
};

use cy_kernel_api::{
    CgroupLimits, DeviceBinding, EnforcementMode, LaunchPlan, ProcessRuntime, StopRequest,
};
use cy_proto::sandbox_v1;
use cy_sandbox_client::{SandboxAdapterEndpoint, UdsSandboxAdapterClient};
use cyrene_sandboxd::{handle_request, CgroupV2Config, CgroupV2Runtime};
use prost::Message;

const MAX_FRAME_BYTES: usize = 1024 * 1024;

fn start_mock_sandboxd(
    socket_path: PathBuf,
    cgroup_root: PathBuf,
) -> (thread::JoinHandle<()>, std::sync::mpsc::Sender<()>) {
    let (stop_tx, stop_rx) = std::sync::mpsc::channel();
    let runtime = Arc::new(CgroupV2Runtime::new(CgroupV2Config {
        root: cgroup_root,
        transport_root: socket_path.parent().unwrap().join("workers"),
        device_bpf_enabled: false,
        dev_mode: false,
    }));

    let listener = UnixListener::bind(&socket_path).expect("bind socket failed");
    listener
        .set_nonblocking(true)
        .expect("set nonblocking failed");

    let handle = thread::spawn(move || {
        while stop_rx.try_recv().is_err() {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let _ = stream.set_nonblocking(false);
                    if let Ok(payload) = read_frame(&mut stream) {
                        if let Ok(request) = sandbox_v1::SandboxRequest::decode(payload.as_slice())
                        {
                            let response =
                                handle_request(runtime.as_ref(), "sandboxd-integration", request);
                            let encoded = response.encode_to_vec();
                            let _ = write_frame(&mut stream, &encoded);
                        }
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
    });

    (handle, stop_tx)
}

fn read_frame(mut reader: impl Read) -> std::io::Result<Vec<u8>> {
    let mut length = [0_u8; 4];
    reader.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    if length > MAX_FRAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "frame exceeds limit",
        ));
    }
    let mut payload = vec![0; length];
    reader.read_exact(&mut payload)?;
    Ok(payload)
}

fn write_frame(mut writer: impl Write, payload: &[u8]) -> std::io::Result<()> {
    let length = (payload.len() as u32).to_be_bytes();
    writer.write_all(&length)?;
    writer.write_all(payload)?;
    writer.flush()
}

#[test]
fn test_uds_kernel_sandbox_preflight_and_device_denial() {
    let temp_dir = tempfile::tempdir().expect("create tempdir failed");
    let socket_path = temp_dir.path().join("sandboxd.sock");
    let cgroup_root = temp_dir.path().join("cgroup-root");

    fs::create_dir_all(&cgroup_root).unwrap();
    fs::write(cgroup_root.join("cgroup.controllers"), "cpu memory pids").unwrap();
    fs::write(cgroup_root.join("cgroup.kill"), "1").unwrap();

    let (server, stop_tx) = start_mock_sandboxd(socket_path.clone(), cgroup_root.clone());

    let endpoint = SandboxAdapterEndpoint::new("sandboxd-integration", &socket_path);
    let client = UdsSandboxAdapterClient::from_endpoint(endpoint).expect("client creation failed");

    // 1. Preflight test over UDS
    let caps = client.preflight();
    assert!(
        caps.ready,
        "preflight should report ready on valid cgroup setup"
    );

    // 2. Hard device enforcement denial when device-bpf is disabled
    let plan = LaunchPlan {
        instance_name: "test-proc".to_string(),
        executable: PathBuf::from("/bin/true"),
        args: Vec::new(),
        environment: BTreeMap::new(),
        cgroup_name: "instance-test-1".to_string(),
        transport_socket: None,
        limits: CgroupLimits {
            cpu_max_millicores: Some(500),
            memory_max_bytes: Some(64 * 1024 * 1024),
            cpuset_cpus: None,
        },
    };

    let hard_binding = DeviceBinding {
        resource_id: "gpu-0".to_string(),
        nodes: Vec::new(),
        environment: BTreeMap::new(),
        required_gids: Vec::new(),
        enforcement: EnforcementMode::Hard,
        adapter_id: "nvidia".to_string(),
        reason_code: "DEVICE_BPF_REQUIRED".to_string(),
    };

    let launch_result = client.launch(&plan, &hard_binding);
    assert!(
        launch_result.is_err(),
        "hard device binding without bpf must fail closed"
    );
    let err = launch_result.unwrap_err();
    assert_eq!(err.reason_code, "HARD_ENFORCEMENT_UNAVAILABLE");

    // Stop server
    let _ = stop_tx.send(());
    let _ = server.join();
}

#[test]
fn test_uds_kernel_sandbox_soft_enforcement_roundtrip() {
    let temp_dir = tempfile::tempdir().expect("create tempdir failed");
    let socket_path = temp_dir.path().join("sandboxd.sock");
    let cgroup_root = temp_dir.path().join("cgroup-root");

    fs::create_dir_all(&cgroup_root).unwrap();
    fs::write(cgroup_root.join("cgroup.controllers"), "cpu memory pids").unwrap();
    fs::write(cgroup_root.join("cgroup.kill"), "1").unwrap();

    let (server, stop_tx) = start_mock_sandboxd(socket_path.clone(), cgroup_root.clone());

    let endpoint = SandboxAdapterEndpoint::new("sandboxd-integration", &socket_path);
    let client = UdsSandboxAdapterClient::from_endpoint(endpoint).expect("client creation failed");

    let plan = LaunchPlan {
        instance_name: "test-proc-soft".to_string(),
        executable: PathBuf::from("/bin/sleep"),
        args: vec!["10".to_string()],
        environment: BTreeMap::new(),
        cgroup_name: "instance-test-2".to_string(),
        transport_socket: None,
        limits: CgroupLimits {
            cpu_max_millicores: Some(1000),
            memory_max_bytes: Some(128 * 1024 * 1024),
            cpuset_cpus: None,
        },
    };

    let soft_binding = DeviceBinding {
        resource_id: "cpu-host".to_string(),
        nodes: Vec::new(),
        environment: BTreeMap::new(),
        required_gids: Vec::new(),
        enforcement: EnforcementMode::Soft,
        adapter_id: "hardware-adapter-linux-sys".to_string(),
        reason_code: "SYSTEM_RESOURCE_SOFT_ENFORCEMENT".to_string(),
    };

    let launch_result = client.launch(&plan, &soft_binding);
    if let Ok(handle) = launch_result {
        // Read telemetry
        let _telemetry = client.telemetry(&handle);

        // Stop process
        let stop_res = client.stop(
            &handle,
            &StopRequest {
                grace_period: Duration::from_millis(500),
                immediate: false,
            },
        );
        assert!(stop_res.is_ok(), "stopping process should succeed");
    }

    let _ = stop_tx.send(());
    let _ = server.join();
}
