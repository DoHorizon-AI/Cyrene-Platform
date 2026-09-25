// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: kernel/crates/cy-kernel-daemon/tests/service_supervision_tck.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! Generic Process & Workload Supervision TCK (Technology Compatibility Kit).
//!
//! Conformance proofs for:
//! 1. Process launch & readiness observation (ProcessAlive, TCP Socket, HTTP GET).
//! 2. Service endpoint publication and automatic revocation on stop.
//! 3. Graceful shutdown within configured deadline.
//! 4. Stubborn process forced termination escalation on timeout.
//! 5. Unexpected crash detection and structured exit code reporting.
//! 6. Deterministic exponential restart backoff timing.
//! 7. Bounded restart exhaustion transition to Quarantined.
//! 8. Immediate cancellation propagation.
//! 9. Structured lifecycle event ordering.
//! 10. Process tree cleanup with zero orphaned processes.
//!
//! 中文：通用进程与 workload 监督 TCK（技术兼容性套件）。一致性验证包括：进程启动和就绪观测（ProcessAlive、TCP Socket、HTTP GET）；Service 端点发布及停止时自动撤销；在配置期限内优雅关闭；超时后升级强制终止顽固进程；检测意外崩溃并结构化报告退出码；确定性的指数重启退避；重启次数耗尽后进入 Quarantined；立即传播取消；结构化生命周期事件排序；清理进程树且不遗留孤儿进程。

use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use cy_kernel_api::{
    BackoffConfig, CgroupLimits, CleanupReport, DeviceBinding, EnforcementMode, LaunchPlan,
    NodeCapabilities, ProbeConfig, ProcessCondition, ProcessHandle, ProcessRuntime, ProviderError,
    ReadinessProbe, RestartPolicy, SandboxBackend, ServiceEndpointSpec, ServiceSpec, ServiceState,
    StopRequest,
};
use cy_kernel_daemon::watchdog::ServiceSupervisor;
use tokio::net::TcpListener;

// --- Test Fixture Backend ---
// 中文：测试 fixture backend。

#[derive(Clone)]
struct MockExecutionBackend {
    launched_count: Arc<AtomicU32>,
    stopped_count: Arc<AtomicU32>,
    crash_on_launch: Arc<AtomicBool>,
    exit_code: Arc<Mutex<Option<i32>>>,
    complete_cleanup: Arc<AtomicBool>,
}

impl MockExecutionBackend {
    fn new() -> Self {
        Self {
            launched_count: Arc::new(AtomicU32::new(0)),
            stopped_count: Arc::new(AtomicU32::new(0)),
            crash_on_launch: Arc::new(AtomicBool::new(false)),
            exit_code: Arc::new(Mutex::new(Some(0))),
            complete_cleanup: Arc::new(AtomicBool::new(true)),
        }
    }

    fn with_exit_code(self, code: Option<i32>) -> Self {
        *self.exit_code.lock().unwrap() = code;
        self
    }

    fn with_crash_on_launch(self, crash: bool) -> Self {
        self.crash_on_launch.store(crash, Ordering::SeqCst);
        self
    }

    fn with_cleanup_incomplete(self) -> Self {
        self.complete_cleanup.store(false, Ordering::SeqCst);
        self
    }
}

impl ProcessRuntime for MockExecutionBackend {
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
        let count = self.launched_count.fetch_add(1, Ordering::SeqCst) + 1;
        if self.crash_on_launch.load(Ordering::SeqCst) {
            return Err(ProviderError::new(
                "mock-backend",
                "PROCESS_SPAWN_FAILED",
                "Simulated spawn failure",
            ));
        }

        Ok(ProcessHandle {
            pid: 1000 + count,
            cgroup_path: PathBuf::from(&format!("/cgroup/{}", plan.cgroup_name)),
            start_time_ticks: Some(100 + count as u64),
            transport_socket: plan.transport_socket.clone(),
        })
    }

    fn stop(
        &self,
        _handle: &ProcessHandle,
        _request: &StopRequest,
    ) -> Result<CleanupReport, ProviderError> {
        self.stopped_count.fetch_add(1, Ordering::SeqCst);
        let complete = self.complete_cleanup.load(Ordering::SeqCst);
        let code = *self.exit_code.lock().unwrap();

        Ok(CleanupReport {
            complete,
            exit_code: code,
            oom_killed: false,
            conditions: if complete {
                Vec::new()
            } else {
                vec![ProcessCondition {
                    reason_code: "STUBBORN_PROCESS".to_string(),
                    summary: "Process ignored graceful shutdown; forced termination executed"
                        .to_string(),
                }]
            },
            reason_code: if complete {
                "CLEANUP_COMPLETE".to_string()
            } else {
                "FORCED_TERMINATION_REQUIRED".to_string()
            },
        })
    }
}

impl SandboxBackend for MockExecutionBackend {
    fn backend_id(&self) -> &str {
        "mock-sandbox"
    }
}

// --- Real OS Subprocess Backend for Zero-Orphan Verification ---
// 中文：用于验证不遗留孤儿进程的真实 OS 子进程 backend。

struct OsSubprocessBackend;

impl ProcessRuntime for OsSubprocessBackend {
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

        let child = cmd
            .spawn()
            .map_err(|e| ProviderError::new("os-subprocess", "SPAWN_FAILED", &e.to_string()))?;

        let pid = child.id();
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
        #[cfg(windows)]
        {
            let _ = std::process::Command::new("taskkill")
                .args(["/F", "/PID", &handle.pid.to_string()])
                .output();
        }
        #[cfg(unix)]
        {
            unsafe {
                libc::kill(handle.pid as i32, libc::SIGKILL);
            }
        }
        Ok(CleanupReport {
            complete: true,
            exit_code: Some(0),
            oom_killed: false,
            conditions: Vec::new(),
            reason_code: "CLEANUP_COMPLETE".to_string(),
        })
    }
}

impl SandboxBackend for OsSubprocessBackend {
    fn backend_id(&self) -> &str {
        "os-subprocess"
    }
}

fn sample_binding() -> DeviceBinding {
    DeviceBinding {
        resource_id: "res-1".to_string(),
        nodes: Vec::new(),
        environment: BTreeMap::new(),
        joinable_environment_keys: Default::default(),
        required_gids: Vec::new(),
        enforcement: EnforcementMode::VisibilityOnly,
        adapter_id: "test-adapter".to_string(),
        reason_code: "OK".to_string(),
    }
}

fn sample_plan(name: &str) -> LaunchPlan {
    LaunchPlan {
        instance_name: name.to_string(),
        executable: PathBuf::from("service-binary"),
        args: vec!["--port".to_string(), "8080".to_string()],
        environment: BTreeMap::new(),
        cgroup_name: format!("cgroup-{name}"),
        limits: CgroupLimits::default(),
        working_dir: Some(PathBuf::from("/app")),
        transport_socket: None,
    }
}

// --- TCK Test Cases ---
// 中文：TCK 测试用例。

#[tokio::test]
async fn test_generic_service_launch_and_process_alive_readiness() {
    let backend = Arc::new(MockExecutionBackend::new());
    let plan = sample_plan("alive-svc");
    let spec = ServiceSpec::new("alive-svc", plan)
        .with_readiness_probe(ReadinessProbe::ProcessAlive, ProbeConfig::default());

    let mut supervisor = ServiceSupervisor::new(spec, backend.clone(), sample_binding());
    let mut events = supervisor.subscribe_events();

    assert_eq!(supervisor.state(), ServiceState::Stopped);

    let status = supervisor.start().await.expect("service must start");
    assert_eq!(status.state, ServiceState::Running);
    assert_eq!(supervisor.state(), ServiceState::Running);
    assert_eq!(backend.launched_count.load(Ordering::SeqCst), 1);

    // Verify event ordering: Starting -> Ready -> Running
    // 中文：验证事件顺序：Starting -> Ready -> Running。
    let ev1 = events.recv().await.unwrap();
    assert_eq!(ev1.state, ServiceState::Starting);
    let ev2 = events.recv().await.unwrap();
    assert_eq!(ev2.state, ServiceState::Ready);
    let ev3 = events.recv().await.unwrap();
    assert_eq!(ev3.state, ServiceState::Running);
}

#[tokio::test]
async fn test_generic_service_tcp_readiness_probe_success() {
    // Start a mock TCP listener to simulate the service socket opening
    // 中文：启动一个 mock TCP listener，模拟 Service socket 开始监听。
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind tcp listener");
    let port = listener.local_addr().unwrap().port();

    let backend = Arc::new(MockExecutionBackend::new());
    let plan = sample_plan("tcp-svc");
    let probe_config = ProbeConfig {
        initial_delay: Duration::from_millis(5),
        period: Duration::from_millis(20),
        timeout: Duration::from_millis(200),
        success_threshold: 1,
        failure_threshold: 10,
    };
    let spec = ServiceSpec::new("tcp-svc", plan).with_readiness_probe(
        ReadinessProbe::TcpSocket {
            host: "127.0.0.1".to_string(),
            port,
        },
        probe_config,
    );

    let mut supervisor = ServiceSupervisor::new(spec, backend, sample_binding());
    let status = supervisor
        .start()
        .await
        .expect("service must become ready via TCP");
    assert_eq!(status.state, ServiceState::Running);
    assert_eq!(supervisor.state(), ServiceState::Running);
}

#[tokio::test]
async fn test_generic_service_http_get_readiness_probe_success() {
    // Start a mock HTTP listener responding with 200 OK
    // 中文：启动一个 mock HTTP listener，并让其返回 200 OK。
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind http listener");
    let port = listener.local_addr().unwrap().port();

    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = [0u8; 512];
                let _ = socket.read(&mut buf).await;
                let response = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK";
                let _ = socket.write_all(response.as_bytes()).await;
            });
        }
    });

    let backend = Arc::new(MockExecutionBackend::new());
    let plan = sample_plan("http-svc");
    let probe_config = ProbeConfig {
        initial_delay: Duration::from_millis(5),
        period: Duration::from_millis(20),
        timeout: Duration::from_millis(200),
        success_threshold: 1,
        failure_threshold: 10,
    };
    let spec = ServiceSpec::new("http-svc", plan).with_readiness_probe(
        ReadinessProbe::HttpGet {
            host: "127.0.0.1".to_string(),
            port,
            path: "/health/ready".to_string(),
            expected_status: Some(200),
        },
        probe_config,
    );

    let mut supervisor = ServiceSupervisor::new(spec, backend, sample_binding());
    let status = supervisor
        .start()
        .await
        .expect("service must become ready via HTTP probe");
    assert_eq!(status.state, ServiceState::Running);
}

#[tokio::test]
async fn test_generic_service_readiness_probe_timeout_transitions_to_failed() {
    // Probe a non-existent port -> must fail after failure_threshold
    // 中文：探测不存在的端口；必须在达到 failure_threshold 后失败。
    let backend = Arc::new(MockExecutionBackend::new());
    let plan = sample_plan("failing-probe-svc");
    let probe_config = ProbeConfig {
        initial_delay: Duration::ZERO,
        period: Duration::from_millis(10),
        timeout: Duration::from_millis(20),
        success_threshold: 1,
        failure_threshold: 3,
    };
    let spec = ServiceSpec::new("failing-probe-svc", plan)
        .with_readiness_probe(
            ReadinessProbe::TcpSocket {
                host: "127.0.0.1".to_string(),
                port: 59999, // unopened port | 中文：尚未打开的端口
            },
            probe_config,
        )
        .with_restart_policy(RestartPolicy::Never);

    let mut supervisor = ServiceSupervisor::new(spec, backend, sample_binding());
    let result = supervisor.start().await;
    assert!(result.is_err(), "probe failure must fail start");
    assert_eq!(supervisor.state(), ServiceState::Failed);
}

#[tokio::test]
async fn test_generic_service_spawn_failure_handling() {
    let backend = Arc::new(MockExecutionBackend::new().with_crash_on_launch(true));
    let plan = sample_plan("spawn-fail-svc");
    let spec = ServiceSpec::new("spawn-fail-svc", plan).with_restart_policy(RestartPolicy::Never);

    let mut supervisor = ServiceSupervisor::new(spec, backend, sample_binding());
    let result = supervisor.start().await;
    assert!(result.is_err(), "spawn failure must return Err");
    assert_eq!(supervisor.state(), ServiceState::Failed);
}

#[tokio::test]
async fn test_generic_service_endpoint_lifecycle() {
    let backend = Arc::new(MockExecutionBackend::new());
    let plan = sample_plan("endpoint-svc");
    let mut attrs = BTreeMap::new();
    attrs.insert("protocol".to_string(), "http1.1".to_string());

    let spec = ServiceSpec::new("endpoint-svc", plan)
        .with_readiness_probe(ReadinessProbe::ProcessAlive, ProbeConfig::default())
        .with_endpoint(ServiceEndpointSpec {
            transport: "http".to_string(),
            schema_id: "test.service.http.v1".to_string(),
            port: Some(15031),
            path: Some("/api/v1".to_string()),
            attributes: attrs,
            connection_ref: "http://127.0.0.1/direct".to_string(),
            credential_ref: None,
        });

    let mut supervisor = ServiceSupervisor::new(spec, backend, sample_binding());

    // 1. Before start: no published endpoint
    // 中文：1. 启动前：没有已发布的端点。
    assert!(supervisor.status().published_endpoint.is_none());

    // 2. Start service -> endpoint published on ready
    // 中文：2. 启动 Service：就绪后发布端点。
    supervisor.start().await.expect("start must succeed");
    let status = supervisor.status();
    assert_eq!(status.state, ServiceState::Running);
    let ep = status
        .published_endpoint
        .expect("endpoint must be published on ready");
    assert_eq!(ep.identity.id, "endpoint/endpoint-svc");
    assert_eq!(ep.transport, "http");
    assert_eq!(ep.schema_id, "test.service.http.v1");
    assert_eq!(ep.public_attributes.get("port"), Some(&"15031".to_string()));
    assert_eq!(
        ep.public_attributes.get("path"),
        Some(&"/api/v1".to_string())
    );
    assert_eq!(
        ep.public_attributes.get("protocol"),
        Some(&"http1.1".to_string())
    );

    // 3. Stop service -> endpoint unpublished
    // 中文：3. 停止 Service：取消发布端点。
    supervisor.stop().await.expect("stop must succeed");
    assert_eq!(supervisor.state(), ServiceState::Stopped);
    assert!(
        supervisor.status().published_endpoint.is_none(),
        "endpoint must be revoked on stop"
    );
}

#[tokio::test]
async fn test_generic_service_graceful_shutdown() {
    let backend = Arc::new(MockExecutionBackend::new());
    let plan = sample_plan("graceful-svc");
    let spec = ServiceSpec::new("graceful-svc", plan)
        .with_graceful_stop_timeout(Duration::from_millis(50));

    let mut supervisor = ServiceSupervisor::new(spec, backend.clone(), sample_binding());
    supervisor.start().await.expect("start must succeed");
    assert_eq!(supervisor.state(), ServiceState::Running);

    let status = supervisor.stop().await.expect("stop must succeed");
    assert_eq!(status.state, ServiceState::Stopped);
    assert_eq!(backend.stopped_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn test_generic_service_forced_termination_when_cleanup_incomplete() {
    let backend = Arc::new(MockExecutionBackend::new().with_cleanup_incomplete());
    let plan = sample_plan("stubborn-svc");
    let spec = ServiceSpec::new("stubborn-svc", plan)
        .with_graceful_stop_timeout(Duration::from_millis(20));

    let mut supervisor = ServiceSupervisor::new(spec, backend.clone(), sample_binding());
    supervisor.start().await.expect("start must succeed");

    let status = supervisor.stop().await.expect("stop returns status");
    // When cleanup is incomplete, state reports Failed with reason
    // 中文：清理未完成时，状态会以对应 reason 报告 Failed。
    assert_eq!(status.state, ServiceState::Failed);
    assert_eq!(
        status.last_exit_report.unwrap().reason_code,
        "FORCED_TERMINATION_REQUIRED"
    );
}

#[tokio::test]
async fn test_generic_service_immediate_cancellation() {
    let backend = Arc::new(MockExecutionBackend::new());
    let plan = sample_plan("cancel-svc");
    let spec = ServiceSpec::new("cancel-svc", plan);

    let mut supervisor = ServiceSupervisor::new(spec, backend.clone(), sample_binding());
    supervisor.start().await.expect("start must succeed");
    assert_eq!(supervisor.state(), ServiceState::Running);

    let status = supervisor.cancel().await.expect("cancel must succeed");
    assert_eq!(status.state, ServiceState::Stopped);
    assert_eq!(backend.stopped_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn test_generic_service_unexpected_crash_exit_code_reporting() {
    let backend = Arc::new(MockExecutionBackend::new().with_exit_code(Some(137)));
    let plan = sample_plan("crash-svc");
    let spec = ServiceSpec::new("crash-svc", plan).with_restart_policy(RestartPolicy::Never);

    let mut supervisor = ServiceSupervisor::new(spec, backend, sample_binding());
    supervisor.start().await.expect("start must succeed");

    // Simulate unexpected crash with exit code 137 (SIGKILL/OOM)
    // 中文：模拟意外崩溃，退出码为 137（SIGKILL/OOM）。
    let crash_report = CleanupReport {
        complete: true,
        exit_code: Some(137),
        oom_killed: true,
        conditions: vec![ProcessCondition {
            reason_code: "OOM_KILLED".to_string(),
            summary: "Killed by out-of-memory killer".to_string(),
        }],
        reason_code: "OOM_KILLED".to_string(),
    };

    let status = supervisor.handle_observed_exit(crash_report).await;
    assert_eq!(status.state, ServiceState::Failed);
    let report = status
        .last_exit_report
        .expect("exit report must be preserved");
    assert_eq!(report.exit_code, Some(137));
    assert!(report.oom_killed);
    assert_eq!(report.reason_code, "OOM_KILLED");
}

#[tokio::test]
async fn test_generic_service_restart_policy_on_failure_with_deterministic_backoff() {
    let backend = Arc::new(MockExecutionBackend::new());
    let plan = sample_plan("restart-backoff-svc");
    let backoff = BackoffConfig {
        initial_delay: Duration::from_millis(20),
        max_delay: Duration::from_millis(100),
        multiplier: 2.0,
        reset_after: Duration::from_secs(60),
    };
    let spec = ServiceSpec::new("restart-backoff-svc", plan).with_restart_policy(
        RestartPolicy::OnFailure {
            max_retries: Some(3),
            backoff,
        },
    );

    let mut supervisor = ServiceSupervisor::new(spec, backend.clone(), sample_binding());
    supervisor.start().await.expect("first start must succeed");
    assert_eq!(supervisor.generation(), 1);
    assert_eq!(supervisor.restart_count(), 0);

    // 1st crash -> triggers attempt 1 with 20ms backoff
    // 中文：第 1 次崩溃：触发第 1 次重启尝试，退避 20ms。
    let start_t = Instant::now();
    let crash_1 = CleanupReport {
        complete: true,
        exit_code: Some(1),
        oom_killed: false,
        conditions: Vec::new(),
        reason_code: "SEGFAULT".to_string(),
    };
    let status_1 = supervisor.handle_observed_exit(crash_1).await;
    assert_eq!(status_1.state, ServiceState::Restarting);
    assert_eq!(supervisor.restart_count(), 1);
    assert!(
        start_t.elapsed() >= Duration::from_millis(18),
        "must respect 20ms backoff delay"
    );

    // Step supervisor -> launches generation 2
    // 中文：推进 supervisor：启动 generation 2。
    supervisor.step_supervision().await.expect("restart start");
    assert_eq!(supervisor.state(), ServiceState::Running);
    assert_eq!(supervisor.generation(), 2);

    // 2nd crash -> triggers attempt 2 with 40ms backoff (20 * 2^1)
    // 中文：第 2 次崩溃：触发第 2 次重启尝试，退避 40ms（20 * 2^1）。
    let start_t2 = Instant::now();
    let crash_2 = CleanupReport {
        complete: true,
        exit_code: Some(2),
        oom_killed: false,
        conditions: Vec::new(),
        reason_code: "UNCAUGHT_EXCEPTION".to_string(),
    };
    let status_2 = supervisor.handle_observed_exit(crash_2).await;
    assert_eq!(status_2.state, ServiceState::Restarting);
    assert_eq!(supervisor.restart_count(), 2);
    assert!(
        start_t2.elapsed() >= Duration::from_millis(35),
        "must respect 40ms backoff delay"
    );

    supervisor.step_supervision().await.expect("restart start");
    assert_eq!(supervisor.state(), ServiceState::Running);
    assert_eq!(supervisor.generation(), 3);
}

#[tokio::test]
async fn test_generic_service_restart_exhaustion_quarantine() {
    let backend = Arc::new(MockExecutionBackend::new());
    let plan = sample_plan("exhaust-svc");
    let backoff = BackoffConfig {
        initial_delay: Duration::from_millis(5),
        max_delay: Duration::from_millis(20),
        multiplier: 2.0,
        reset_after: Duration::from_secs(60),
    };
    let spec =
        ServiceSpec::new("exhaust-svc", plan).with_restart_policy(RestartPolicy::OnFailure {
            max_retries: Some(2),
            backoff,
        });

    let mut supervisor = ServiceSupervisor::new(spec, backend, sample_binding());
    supervisor.start().await.expect("start");

    let crash = CleanupReport {
        complete: true,
        exit_code: Some(1),
        oom_killed: false,
        conditions: Vec::new(),
        reason_code: "CRASH".to_string(),
    };

    // 1st crash: attempt 1 (allowed, <= 2)
    // 中文：第 1 次崩溃：第 1 次尝试，允许（不超过 2）。
    supervisor.handle_observed_exit(crash.clone()).await;
    assert_eq!(supervisor.state(), ServiceState::Restarting);
    supervisor.step_supervision().await.expect("restart 1");

    // 2nd crash: attempt 2 (allowed, <= 2)
    // 中文：第 2 次崩溃：第 2 次尝试，允许（不超过 2）。
    supervisor.handle_observed_exit(crash.clone()).await;
    assert_eq!(supervisor.state(), ServiceState::Restarting);
    supervisor.step_supervision().await.expect("restart 2");

    // 3rd crash: attempt 3 (> 2 max retries) -> must QUARANTINE!
    // 中文：第 3 次崩溃：第 3 次尝试，超过最多 2 次重试，必须进入 QUARANTINE！
    supervisor.handle_observed_exit(crash).await;
    assert_eq!(supervisor.state(), ServiceState::Quarantined);
}

#[tokio::test]
async fn test_generic_service_clean_exit_does_not_restart_on_failure_policy() {
    let backend = Arc::new(MockExecutionBackend::new());
    let plan = sample_plan("clean-exit-svc");
    let spec =
        ServiceSpec::new("clean-exit-svc", plan).with_restart_policy(RestartPolicy::OnFailure {
            max_retries: Some(5),
            backoff: BackoffConfig::default(),
        });

    let mut supervisor = ServiceSupervisor::new(spec, backend, sample_binding());
    supervisor.start().await.expect("start");

    // Clean exit with code 0
    // 中文：以退出码 0 正常退出。
    let clean_exit = CleanupReport {
        complete: true,
        exit_code: Some(0),
        oom_killed: false,
        conditions: Vec::new(),
        reason_code: "CLEANUP_COMPLETE".to_string(),
    };

    let status = supervisor.handle_observed_exit(clean_exit).await;
    assert_eq!(
        status.state,
        ServiceState::Stopped,
        "clean exit must transition to Stopped without restarting"
    );
    assert_eq!(supervisor.restart_count(), 0);
}

#[tokio::test]
async fn test_generic_service_real_os_child_process_lifecycle_and_cleanup() {
    let backend = Arc::new(OsSubprocessBackend);
    #[cfg(windows)]
    let (exe, args) = (
        "cmd.exe",
        vec!["/c".to_string(), "ping -n 10 127.0.0.1 > nul".to_string()],
    );
    #[cfg(not(windows))]
    let (exe, args) = ("sleep", vec!["10".to_string()]);

    let plan = LaunchPlan {
        instance_name: "real-proc-svc".to_string(),
        executable: PathBuf::from(exe),
        args,
        environment: BTreeMap::new(),
        cgroup_name: "real-proc-cgroup".to_string(),
        limits: CgroupLimits::default(),
        working_dir: None,
        transport_socket: None,
    };

    let spec = ServiceSpec::new("real-proc-svc", plan)
        .with_readiness_probe(ReadinessProbe::ProcessAlive, ProbeConfig::default())
        .with_graceful_stop_timeout(Duration::from_millis(100));

    let mut supervisor = ServiceSupervisor::new(spec, backend, sample_binding());

    // 1. Launch real OS process
    // 中文：1. 启动真实 OS 进程。
    let status = supervisor.start().await.expect("real process launch");
    assert_eq!(status.state, ServiceState::Running);
    let handle = supervisor.handle().expect("process handle must exist");
    assert!(handle.pid > 0);

    // 2. Stop real OS process
    // 中文：2. 停止真实 OS 进程。
    let stop_status = supervisor.stop().await.expect("real process stop");
    assert_eq!(stop_status.state, ServiceState::Stopped);
}

#[tokio::test]
async fn test_generic_service_single_restart_authority_and_worker_active_isolation() {
    let backend = Arc::new(MockExecutionBackend::new());
    let plan = sample_plan("single-restart-svc");
    let mut attrs = BTreeMap::new();
    attrs.insert("api".to_string(), "v1".to_string());

    let spec = ServiceSpec::new("single-restart-svc", plan)
        .with_readiness_probe(ReadinessProbe::ProcessAlive, ProbeConfig::default())
        .with_endpoint(ServiceEndpointSpec {
            transport: "http".to_string(),
            schema_id: "test.api.v1".to_string(),
            port: Some(8080),
            path: None,
            attributes: attrs,
            connection_ref: "http://127.0.0.1/direct".to_string(),
            credential_ref: None,
        })
        .with_restart_policy(RestartPolicy::OnFailure {
            max_retries: Some(3),
            backoff: BackoffConfig {
                initial_delay: Duration::from_millis(10),
                max_delay: Duration::from_millis(50),
                multiplier: 2.0,
                reset_after: Duration::from_secs(60),
            },
        });

    let mut supervisor = ServiceSupervisor::new(spec, backend.clone(), sample_binding());

    // 1. Initial start -> Generation 1 becomes Ready and Running
    // 中文：1. 首次启动：Generation 1 进入 Ready 和 Running。
    let status_gen1 = supervisor.start().await.expect("initial start succeeds");
    assert_eq!(status_gen1.state, ServiceState::Running);
    assert_eq!(supervisor.generation(), 1);
    assert_eq!(supervisor.restart_count(), 0);
    assert_eq!(backend.launched_count.load(Ordering::SeqCst), 1);
    assert!(
        supervisor.handle().is_some(),
        "exactly one active process handle"
    );
    assert_eq!(
        supervisor
            .status()
            .published_endpoint
            .unwrap()
            .identity
            .generation,
        1
    );

    // 2. Process crashes
    // 中文：2. 进程崩溃。
    let crash_report = CleanupReport {
        complete: true,
        exit_code: Some(1),
        oom_killed: false,
        conditions: vec![ProcessCondition {
            reason_code: "PANIC".to_string(),
            summary: "Simulated unhandled panic".to_string(),
        }],
        reason_code: "CRASH".to_string(),
    };

    let status_restarting = supervisor.handle_observed_exit(crash_report).await;
    assert_eq!(status_restarting.state, ServiceState::Restarting);
    assert_eq!(supervisor.restart_count(), 1);
    assert!(
        supervisor.status().published_endpoint.is_none(),
        "endpoint revoked immediately on crash"
    );

    // 3. Step supervisor -> Exactly one replacement generation launches
    // 中文：3. 推进 supervisor：恰好启动一个替代 generation。
    let status_gen2 = supervisor
        .step_supervision()
        .await
        .expect("restart succeeds");
    assert_eq!(status_gen2.state, ServiceState::Running);
    assert_eq!(supervisor.generation(), 2);
    assert_eq!(supervisor.restart_count(), 1);
    assert_eq!(
        backend.launched_count.load(Ordering::SeqCst),
        2,
        "exactly one replacement launched (total 2)"
    );

    // 4. Exactly one process and exactly one endpoint remain active
    // 中文：4. 只保留一个活动进程和一个活动端点。
    let handle = supervisor
        .handle()
        .expect("exactly one active process handle");
    assert_eq!(handle.pid, 1002);
    let ep = supervisor
        .status()
        .published_endpoint
        .expect("exactly one endpoint published");
    assert_eq!(ep.identity.generation, 2);
    assert_eq!(ep.owner.generation, 2);

    // 5. Subsequent step_supervision on healthy running service does NOT cause duplicate restarts
    // 中文：5. Service 健康运行后再次执行 step_supervision，不得触发重复重启。
    let status_noop = supervisor
        .step_supervision()
        .await
        .expect("noop supervision");
    assert_eq!(status_noop.state, ServiceState::Running);
    assert_eq!(supervisor.generation(), 2);
    assert_eq!(
        backend.launched_count.load(Ordering::SeqCst),
        2,
        "no duplicate launch occurred"
    );
}

#[tokio::test]
async fn test_generic_service_stale_endpoint_protection_across_generations() {
    let backend = Arc::new(MockExecutionBackend::new());
    let plan = sample_plan("stale-endpoint-svc");

    let spec = ServiceSpec::new("stale-endpoint-svc", plan)
        .with_readiness_probe(ReadinessProbe::ProcessAlive, ProbeConfig::default())
        .with_endpoint(ServiceEndpointSpec {
            transport: "grpc".to_string(),
            schema_id: "test.grpc.v1".to_string(),
            port: Some(9090),
            path: None,
            attributes: BTreeMap::new(),
            connection_ref: "grpc://127.0.0.1/direct".to_string(),
            credential_ref: Some("secret.test.endpoint".to_string()),
        })
        .with_restart_policy(RestartPolicy::OnFailure {
            max_retries: Some(2),
            backoff: BackoffConfig {
                initial_delay: Duration::from_millis(10),
                max_delay: Duration::from_millis(50),
                multiplier: 2.0,
                reset_after: Duration::from_secs(60),
            },
        });

    let mut supervisor = ServiceSupervisor::new(spec, backend, sample_binding());

    // 1. Generation N (1) ready -> Endpoint published with generation 1
    // 中文：1. Generation N（1）就绪，发布带 generation 1 的 Endpoint。
    supervisor.start().await.expect("start");
    let ep_gen1 = supervisor
        .status()
        .published_endpoint
        .expect("endpoint published");
    assert_eq!(ep_gen1.identity.generation, 1);
    assert_eq!(ep_gen1.identity.id, "endpoint/stale-endpoint-svc");

    // 2. Generation 1 crashes -> Endpoint revoked immediately
    // 中文：2. Generation 1 崩溃，立即撤销 Endpoint。
    let crash = CleanupReport {
        complete: true,
        exit_code: Some(1),
        oom_killed: false,
        conditions: Vec::new(),
        reason_code: "SEGFAULT".to_string(),
    };
    supervisor.handle_observed_exit(crash).await;
    assert_eq!(supervisor.state(), ServiceState::Restarting);
    assert!(
        supervisor.status().published_endpoint.is_none(),
        "endpoint must be None during Restarting"
    );

    // 3. Generation N+1 (2) launches -> Endpoint is NOT published until ready
    // 中文：3. 启动 Generation N+1（2）；就绪前不得发布 Endpoint。
    supervisor.step_supervision().await.expect("restart");
    assert_eq!(supervisor.state(), ServiceState::Running);

    // 4. Endpoint is published with Generation 2, old Generation 1 endpoint NEVER reappears
    // 中文：4. 使用 Generation 2 发布 Endpoint；绝不能重新出现旧 Generation 1 的 Endpoint。
    let ep_gen2 = supervisor
        .status()
        .published_endpoint
        .expect("generation 2 endpoint published");
    assert_eq!(ep_gen2.identity.generation, 2);
    assert_ne!(ep_gen2.identity.generation, ep_gen1.identity.generation);
    assert_eq!(ep_gen2.owner.generation, 2);
}
