//! Real Cross-Repository End-to-End Test: Platform Service Supervision of AstrBot .NET Host.
//!
//! Proves that the canonical AstrBot .NET Host (AstrBot.DotNetHost.dll) is reliably
//! supervised by the generic Cyrene Platform ServiceSupervisor:
//! 1. Launch canonical .NET host process.
//! 2. Starting -> HTTP readiness probe (/health/live) -> Ready -> Running.
//! 3. Endpoint publication with Generation 1.
//! 4. Real HTTP request to /health/live & /health/version.
//! 5. Unexpected child process crash simulation.
//! 6. Immediate endpoint revocation.
//! 7. Deterministic backoff -> Generation 2 launch & readiness.
//! 8. Endpoint publication with Generation 2.
//! 9. Real HTTP request to Generation 2 host.
//! 10. Graceful shutdown -> Stopped state -> Zero orphan guarantee.

use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use cy_kernel_api::{
    BackoffConfig, CleanupReport, DeviceBinding, LaunchPlan, NodeCapabilities,
    ProbeConfig, ProcessHandle, ProcessRuntime, ProviderError, ReadinessProbe,
    RestartPolicy, SandboxBackend, ServiceEndpointSpec, ServiceSpec, ServiceState,
    StopRequest,
};
use cy_kernel_daemon::watchdog::ServiceSupervisor;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

fn find_astrbot_dll() -> (PathBuf, PathBuf) {
    let candidates = [
        PathBuf::from("../services/cyrene-astrbot-rev/src/AstrBot.DotNetHost/bin/Debug/net10.0/AstrBot.DotNetHost.dll"),
        PathBuf::from("../../services/cyrene-astrbot-rev/src/AstrBot.DotNetHost/bin/Debug/net10.0/AstrBot.DotNetHost.dll"),
        PathBuf::from(r"C:\Users\Baiji\DHDev\Cyrene\services\cyrene-astrbot-rev\src\AstrBot.DotNetHost\bin\Debug\net10.0\AstrBot.DotNetHost.dll"),
    ];

    for candidate in &candidates {
        if candidate.exists() {
            let working_dir = candidate.parent().unwrap().to_path_buf();
            return (candidate.clone(), working_dir);
        }
    }

    panic!("AstrBot.DotNetHost.dll not found in candidate paths. Ensure `dotnet build` was run.");
}

async fn get_ephemeral_port() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind ephemeral port");
    let port = listener.local_addr().expect("local addr").port();
    drop(listener);
    tokio::time::sleep(Duration::from_millis(50)).await;
    port
}

struct RealAstrBotProcessBackend {
    launched_pids: Arc<Mutex<Vec<u32>>>,
}

impl RealAstrBotProcessBackend {
    fn new() -> Self {
        Self {
            launched_pids: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn kill_active_process(&self, pid: u32) {
        #[cfg(windows)]
        {
            let _ = std::process::Command::new("taskkill")
                .args(["/F", "/T", "/PID", &pid.to_string()])
                .output();
        }
        #[cfg(not(windows))]
        {
            let _ = std::process::Command::new("kill")
                .args(["-9", &pid.to_string()])
                .output();
        }
    }
}

impl ProcessRuntime for RealAstrBotProcessBackend {
    fn preflight(&self) -> NodeCapabilities {
        NodeCapabilities {
            ready: true,
            facts: Vec::new(),
            enforcement: Vec::new(),
        }
    }

    fn launch(
        &self,
        plan: &LaunchPlan,
        _binding: &DeviceBinding,
    ) -> Result<ProcessHandle, ProviderError> {
        let mut cmd = std::process::Command::new(&plan.executable);
        cmd.args(&plan.args);
        cmd.envs(&plan.environment);
        if let Some(dir) = &plan.working_dir {
            cmd.current_dir(dir);
        }
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());

        let child = cmd.spawn().map_err(|e| {
            ProviderError::new("astrbot-proc-backend", "SPAWN_FAILED", &e.to_string())
        })?;

        let pid = child.id();
        self.launched_pids.lock().unwrap().push(pid);

        Ok(ProcessHandle {
            pid,
            cgroup_path: PathBuf::from(&format!("/proc/{pid}")),
            start_time_ticks: Some(1),
            transport_socket: None,
        })
    }

    fn stop(
        &self,
        handle: &ProcessHandle,
        _request: &StopRequest,
    ) -> Result<CleanupReport, ProviderError> {
        self.kill_active_process(handle.pid);
        Ok(CleanupReport {
            complete: true,
            exit_code: Some(0),
            oom_killed: false,
            conditions: Vec::new(),
            reason_code: "STOPPED".to_string(),
        })
    }
}

impl SandboxBackend for RealAstrBotProcessBackend {
    fn backend_id(&self) -> &str {
        "astrbot-real-process-sandbox"
    }
}

async fn http_get_text(host: &str, port: u16, path: &str) -> Result<(u16, String), Box<dyn std::error::Error>> {
    let mut stream = TcpStream::connect(format!("{host}:{port}")).await?;
    let request = format!("GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await?;

    let mut response_buf = Vec::new();
    stream.read_to_end(&mut response_buf).await?;
    let response_str = String::from_utf8_lossy(&response_buf).to_string();

    let status_code = if let Some(first_line) = response_str.lines().next() {
        let parts: Vec<&str> = first_line.split_whitespace().collect();
        if parts.len() >= 2 {
            parts[1].parse::<u16>().unwrap_or(0)
        } else {
            0
        }
    } else {
        0
    };

    Ok((status_code, response_str))
}

#[tokio::test]
async fn test_platform_supervised_astrbot_dotnet_host_e2e_lifecycle() {
    let (astrbot_dll, working_dir) = find_astrbot_dll();
    let port = get_ephemeral_port().await;

    let mut env = BTreeMap::new();
    env.insert("ASPNETCORE_URLS".into(), format!("http://127.0.0.1:{port}"));
    env.insert("ASPNETCORE_ENVIRONMENT".into(), "Development".into());
    env.insert("Runtime__Mode".into(), "Development".into());
    env.insert("DOTNET_NOLOGO".into(), "1".into());
    env.insert("DOTNET_CLI_TELEMETRY_OPTOUT".into(), "1".into());
    env.insert("Logging__LogLevel__Default".into(), "Warning".into());
    env.insert("ASTRBOT_BUILD_SHA".into(), "e2e-sha-proof-v1".into());

    let plan = LaunchPlan {
        instance_name: "astrbot-host-instance".into(),
        executable: PathBuf::from("dotnet"),
        args: vec![astrbot_dll.to_string_lossy().to_string()],
        environment: env,
        cgroup_name: "service-astrbot-rev".into(),
        limits: Default::default(),
        working_dir: Some(working_dir),
        transport_socket: None,
    };

    let spec = ServiceSpec::new("cyrene.service.astrbot-rev", plan)
        .with_readiness_probe(
            ReadinessProbe::HttpGet {
                host: "127.0.0.1".into(),
                port,
                path: "/health/live".into(),
                expected_status: Some(200),
            },
            ProbeConfig {
                initial_delay: Duration::from_millis(200),
                period: Duration::from_millis(150),
                timeout: Duration::from_millis(1500),
                success_threshold: 1,
                failure_threshold: 40,
            },
        )
        .with_restart_policy(RestartPolicy::OnFailure {
            max_retries: Some(3),
            backoff: BackoffConfig {
                initial_delay: Duration::from_millis(200),
                max_delay: Duration::from_secs(3),
                multiplier: 2.0,
                reset_after: Duration::from_secs(60),
            },
        })
        .with_graceful_stop_timeout(Duration::from_secs(5))
        .with_endpoint(ServiceEndpointSpec {
            transport: "http".into(),
            schema_id: "astrbot.host.http.compatibility.v1".into(),
            port: Some(port),
            path: Some("/".into()),
            attributes: [
                ("service".into(), "AstrBot.DotNetHost".into()),
                ("runtime".into(), ".NET 10 LTS".into()),
            ]
            .into_iter()
            .collect(),
        });

    let backend = Arc::new(RealAstrBotProcessBackend::new());
    let binding = DeviceBinding {
        resource_id: "res-astrbot".to_string(),
        nodes: Vec::new(),
        environment: BTreeMap::new(),
        joinable_environment_keys: Default::default(),
        required_gids: Vec::new(),
        enforcement: cy_kernel_api::EnforcementMode::VisibilityOnly,
        adapter_id: "astrbot-adapter".to_string(),
        reason_code: "OK".to_string(),
    };
    let mut supervisor = ServiceSupervisor::new(spec, backend.clone(), binding);

    // ==========================================
    // 1. Launch Generation 1 & Probe Readiness
    // ==========================================
    let initial_status = supervisor.start().await.expect("start supervisor");
    assert_eq!(initial_status.state, ServiceState::Running, "supervisor.start() must evaluate readiness and reach Running");
    assert_eq!(supervisor.generation(), 1);

    // ==========================================
    // 2. Verify Endpoint Publication (Gen 1)
    // ==========================================
    let ep1 = supervisor.status().published_endpoint.expect("generation 1 endpoint published");
    assert_eq!(ep1.identity.generation, 1);
    assert_eq!(ep1.identity.id, "endpoint/cyrene.service.astrbot-rev");

    // ==========================================
    // 3. Real HTTP Request Path to Running Host
    // ==========================================
    let (status_live, body_live) = http_get_text("127.0.0.1", port, "/health/live").await.expect("HTTP GET /health/live");
    assert_eq!(status_live, 200, "expected HTTP 200 from /health/live");
    assert!(body_live.contains("AstrBot.DotNetHost"), "expected body to mention AstrBot.DotNetHost");

    let (status_ver, body_ver) = http_get_text("127.0.0.1", port, "/health/version").await.expect("HTTP GET /health/version");
    assert_eq!(status_ver, 200, "expected HTTP 200 from /health/version");
    assert!(body_ver.contains("e2e-sha-proof-v1"), "expected body to contain buildSha");

    // ==========================================
    // 4. Simulate Unexpected Process Crash
    // ==========================================
    let gen1_pid = *backend.launched_pids.lock().unwrap().first().expect("gen1 pid");
    backend.kill_active_process(gen1_pid);
    tokio::time::sleep(Duration::from_millis(400)).await;

    // Simulate exit observation: supervisor observes exit, revokes endpoint, enters Restarting
    let crash_report = CleanupReport {
        complete: true,
        exit_code: Some(137),
        oom_killed: false,
        conditions: Vec::new(),
        reason_code: "PROCESS_TERMINATED".to_string(),
    };
    supervisor.handle_observed_exit(crash_report).await;
    assert_eq!(supervisor.state(), ServiceState::Restarting);
    assert!(supervisor.status().published_endpoint.is_none(), "endpoint must be revoked immediately upon crash");

    // ==========================================
    // 5. Backoff -> Generation 2 Launch & Ready
    // ==========================================
    tokio::time::sleep(Duration::from_millis(300)).await;
    supervisor.step_supervision().await.expect("step restart backoff");
    assert_eq!(supervisor.generation(), 2);

    // Wait for Generation 2 to reach Running via /health/live
    let mut gen2_ready = false;
    for _ in 0..50 {
        supervisor.step_supervision().await.expect("step gen2");
        if supervisor.state() == ServiceState::Running {
            gen2_ready = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(gen2_ready, "Generation 2 AstrBot host failed to reach Running state");

    let ep2 = supervisor.status().published_endpoint.expect("generation 2 endpoint published");
    assert_eq!(ep2.identity.generation, 2);
    assert_ne!(ep2.identity.generation, ep1.identity.generation);

    // Verify Generation 2 handles real HTTP requests
    let (status_gen2, body_gen2) = http_get_text("127.0.0.1", port, "/health/live").await.expect("HTTP GET gen2");
    assert_eq!(status_gen2, 200);
    assert!(body_gen2.contains("AstrBot.DotNetHost"));

    // ==========================================
    // 6. Graceful Stop & Zero Orphan Verification
    // ==========================================
    let final_status = supervisor.stop().await.expect("graceful stop");
    assert_eq!(final_status.state, ServiceState::Stopped);
    assert!(supervisor.status().published_endpoint.is_none());

    // Give OS brief moment to clean socket/process
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Verify HTTP connection is closed
    let probe_after_stop = TcpStream::connect(format!("127.0.0.1:{port}")).await;
    assert!(probe_after_stop.is_err(), "port must be closed after supervisor stop");
}
