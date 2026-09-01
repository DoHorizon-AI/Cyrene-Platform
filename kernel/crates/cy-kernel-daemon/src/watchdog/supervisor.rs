//! Generic Service and Workload Supervisor Implementation.
//!
//! Provides deterministic lifecycle management, asynchronous readiness probing,
//! bounded exponential backoff restarts, graceful shutdown deadlines, forced
//! termination escalation, and endpoint lifecycle integration for generic services.
//!
//! # Architectural Invariant (ADR-010):
//!
//! `ServiceSupervisor` is a **daemon-level generic orchestration abstraction**.
//! It is **NOT** an authoritative Kernel semantic domain entity. It drives
//! existing Kernel primitives ([`cy_kernel_api::LaunchPlan`], [`SandboxBackend`],
//! [`CleanupReport`], and [`semantic::Endpoint`]).

use std::{
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use cy_kernel_api::{
    semantic, BackoffConfig, CleanupReport, DeviceBinding, ProcessHandle, ProviderError,
    ReadinessProbe, RestartPolicy, SandboxBackend, ServiceEvent, ServiceSpec, ServiceState,
    ServiceStatus, StopRequest,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::broadcast,
};

use crate::sandboxed_process::SandboxedProcess;

const EVENT_BUFFER_CAPACITY: usize = 128;

/// Generic service workload supervisor driving process execution.
pub struct ServiceSupervisor {
    spec: ServiceSpec,
    runtime: Arc<dyn SandboxBackend>,
    binding: DeviceBinding,
    process: Option<SandboxedProcess>,
    state: ServiceState,
    generation: u64,
    restart_count: u32,
    last_launch_at: Option<Instant>,
    last_exit_report: Option<CleanupReport>,
    published_endpoint: Option<semantic::Endpoint>,
    last_error: Option<String>,
    event_tx: broadcast::Sender<ServiceEvent>,
}

impl ServiceSupervisor {
    /// Create a new generic ServiceSupervisor for the given specification and runtime backend.
    pub fn new(
        spec: ServiceSpec,
        runtime: Arc<dyn SandboxBackend>,
        binding: DeviceBinding,
    ) -> Self {
        let (event_tx, _) = broadcast::channel(EVENT_BUFFER_CAPACITY);
        Self {
            spec,
            runtime,
            binding,
            process: None,
            state: ServiceState::Stopped,
            generation: 0,
            restart_count: 0,
            last_launch_at: None,
            last_exit_report: None,
            published_endpoint: None,
            last_error: None,
            event_tx,
        }
    }

    /// Read current service specification.
    pub fn spec(&self) -> &ServiceSpec {
        &self.spec
    }

    /// Read current observable service state.
    pub fn state(&self) -> ServiceState {
        self.state
    }

    /// Read current restart count.
    pub fn restart_count(&self) -> u32 {
        self.restart_count
    }

    /// Read current generation number.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Read current active process handle if running.
    pub fn handle(&self) -> Option<&ProcessHandle> {
        self.process.as_ref().and_then(|p| p.handle())
    }

    /// Read current status snapshot.
    pub fn status(&self) -> ServiceStatus {
        ServiceStatus {
            name: self.spec.name.clone(),
            state: self.state,
            restart_count: self.restart_count,
            last_exit_report: self.last_exit_report.clone(),
            published_endpoint: self.published_endpoint.clone(),
            last_error: self.last_error.clone(),
        }
    }

    /// Subscribe to structured service lifecycle events.
    pub fn subscribe_events(&self) -> broadcast::Receiver<ServiceEvent> {
        self.event_tx.subscribe()
    }

    /// Start the service process and execute readiness probing.
    pub async fn start(&mut self) -> Result<ServiceStatus, ProviderError> {
        if self.state.is_active() && self.state != ServiceState::Restarting {
            return Ok(self.status());
        }

        if let Err(err) = self.spawn_process() {
            self.last_error = Some(err.to_string());
            self.transition_state(
                ServiceState::Failed,
                "PROCESS_SPAWN_FAILED",
                &format!("Process spawn failed: {err}"),
            );
            return Err(err);
        }
        self.transition_state(
            ServiceState::Starting,
            "PROCESS_SPAWNED",
            "Service process spawned",
        );

        match self.probe_readiness().await {
            Ok(()) => {
                self.transition_state(
                    ServiceState::Ready,
                    "READINESS_PROBE_PASSED",
                    "Service readiness probe succeeded",
                );
                self.publish_endpoint_if_configured();
                self.transition_state(
                    ServiceState::Running,
                    "SERVICE_RUNNING",
                    "Service is actively running",
                );
                Ok(self.status())
            }
            Err(err) => {
                self.last_error = Some(err.to_string());
                self.transition_state(
                    ServiceState::Failed,
                    "READINESS_PROBE_FAILED",
                    &format!("Readiness probe failed: {err}"),
                );
                // Attempt cleanup of failed start
                let _ = self.cleanup_current_process(Duration::from_secs(1));
                self.handle_exit_policy(false).await;
                Err(err)
            }
        }
    }

    /// Request graceful stop and await process cleanup within configured timeout.
    pub async fn stop(&mut self) -> Result<ServiceStatus, ProviderError> {
        if self.state.is_terminal() && self.state != ServiceState::Restarting {
            return Ok(self.status());
        }

        self.transition_state(
            ServiceState::Stopping,
            "STOP_REQUESTED",
            "Graceful service stop requested",
        );
        self.unpublish_endpoint();

        let timeout = self.spec.graceful_stop_timeout;
        let report = self.cleanup_current_process(timeout)?;

        if report.complete {
            self.transition_state(
                ServiceState::Stopped,
                "CLEANUP_COMPLETE",
                "Service stopped cleanly",
            );
        } else {
            self.transition_state(
                ServiceState::Failed,
                "CLEANUP_INCOMPLETE",
                &format!("Service cleanup incomplete: {}", report.reason_code),
            );
        }
        Ok(self.status())
    }

    /// Immediately cancel / kill the service process.
    pub async fn cancel(&mut self) -> Result<ServiceStatus, ProviderError> {
        self.transition_state(
            ServiceState::Stopping,
            "CANCEL_REQUESTED",
            "Immediate cancellation requested",
        );
        self.unpublish_endpoint();
        let report = self.cleanup_current_process(Duration::ZERO)?;
        self.transition_state(
            ServiceState::Stopped,
            "CANCELLED",
            &format!("Service cancelled: {}", report.reason_code),
        );
        Ok(self.status())
    }

    /// Supervise the service continuously: handles crash detection and automatic restarts.
    pub async fn step_supervision(&mut self) -> Result<ServiceStatus, ProviderError> {
        // 1. Reset restart counter if healthy window exceeded
        if let Some(launch_at) = self.last_launch_at {
            if self.state == ServiceState::Running {
                let reset_after = match &self.spec.restart_policy {
                    RestartPolicy::Always { backoff, .. }
                    | RestartPolicy::OnFailure { backoff, .. } => backoff.reset_after,
                    RestartPolicy::Never => Duration::from_secs(60),
                };
                if launch_at.elapsed() >= reset_after && self.restart_count > 0 {
                    self.restart_count = 0;
                }
            }
        }

        // 2. If restarting in backoff, attempt next start
        if self.state == ServiceState::Restarting {
            return self.start().await;
        }

        Ok(self.status())
    }

    /// Explicitly notify the supervisor of an observed unexpected process exit.
    pub async fn handle_observed_exit(&mut self, exit_report: CleanupReport) -> ServiceStatus {
        self.unpublish_endpoint();
        self.last_exit_report = Some(exit_report.clone());
        let clean = exit_report.exit_code == Some(0) && !exit_report.oom_killed;

        if clean {
            self.transition_state(
                ServiceState::Stopped,
                "CLEAN_EXIT",
                "Process exited cleanly with status 0",
            );
        } else {
            self.transition_state(
                ServiceState::Failed,
                &exit_report.reason_code,
                &format!(
                    "Process crashed (exit_code={:?}, oom_killed={})",
                    exit_report.exit_code, exit_report.oom_killed
                ),
            );
        }

        self.handle_exit_policy(clean).await;
        self.status()
    }

    // --- Internal Helpers ---

    fn spawn_process(&mut self) -> Result<(), ProviderError> {
        self.generation = self.generation.wrapping_add(1).max(1);
        let mut plan = self.spec.plan.clone();
        plan.instance_name = format!("{}-{}", self.spec.name, self.generation);
        let mut process = SandboxedProcess::new(self.runtime.clone(), plan, self.binding.clone());
        process.start()?;
        self.process = Some(process);
        self.last_launch_at = Some(Instant::now());
        Ok(())
    }

    fn cleanup_current_process(
        &mut self,
        grace_period: Duration,
    ) -> Result<CleanupReport, ProviderError> {
        if let Some(mut process) = self.process.take() {
            let immediate = grace_period == Duration::ZERO;
            let report = process
                .stop(&StopRequest {
                    grace_period,
                    immediate,
                })
                .map(Clone::clone)?;
            self.last_exit_report = Some(report.clone());
            Ok(report)
        } else {
            let report = CleanupReport {
                complete: true,
                exit_code: None,
                oom_killed: false,
                conditions: Vec::new(),
                reason_code: "NOT_RUNNING".to_string(),
            };
            self.last_exit_report = Some(report.clone());
            Ok(report)
        }
    }

    async fn probe_readiness(&mut self) -> Result<(), ProviderError> {
        let Some((probe, config)) = &self.spec.readiness_probe else {
            return Ok(());
        };

        if config.initial_delay > Duration::ZERO {
            tokio::time::sleep(config.initial_delay).await;
        }

        let mut consecutive_successes = 0_u32;
        let mut consecutive_failures = 0_u32;

        while consecutive_failures < config.failure_threshold {
            let probe_result = match probe {
                ReadinessProbe::ProcessAlive => {
                    if let Some(process) = self.process.as_ref() {
                        if process.handle().is_some() {
                            Ok(())
                        } else {
                            Err("process handle missing".to_string())
                        }
                    } else {
                        Err("process not initialized".to_string())
                    }
                }
                ReadinessProbe::TcpSocket { host, port } => {
                    Self::probe_tcp(host, *port, config.timeout).await
                }
                ReadinessProbe::HttpGet {
                    host,
                    port,
                    path,
                    expected_status,
                } => Self::probe_http(host, *port, path, *expected_status, config.timeout).await,
                ReadinessProbe::WorkerControl => {
                    // Handled via WorkerControl protocol
                    Ok(())
                }
            };

            match probe_result {
                Ok(()) => {
                    consecutive_successes += 1;
                    consecutive_failures = 0;
                    if consecutive_successes >= config.success_threshold {
                        return Ok(());
                    }
                }
                Err(_err) => {
                    consecutive_failures += 1;
                    consecutive_successes = 0;
                }
            }

            tokio::time::sleep(config.period).await;
        }

        Err(ProviderError::new(
            "service-supervisor",
            "READINESS_TIMEOUT",
            &format!(
                "readiness probe for service {} failed after {} attempts",
                self.spec.name, config.failure_threshold
            ),
        ))
    }

    async fn probe_tcp(host: &str, port: u16, timeout: Duration) -> Result<(), String> {
        let addr = format!("{host}:{port}");
        tokio::time::timeout(timeout, TcpStream::connect(&addr))
            .await
            .map_err(|_| "tcp connect timed out".to_string())?
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    async fn probe_http(
        host: &str,
        port: u16,
        path: &str,
        expected_status: Option<u16>,
        timeout: Duration,
    ) -> Result<(), String> {
        let addr = format!("{host}:{port}");
        let mut stream = tokio::time::timeout(timeout, TcpStream::connect(&addr))
            .await
            .map_err(|_| "http connect timed out".to_string())?
            .map_err(|e| e.to_string())?;

        let request = format!(
            "GET {} HTTP/1.1\r\nHost: {}:{}\r\nConnection: close\r\nUser-Agent: Cyrene-Supervisor/1.0\r\n\r\n",
            path, host, port
        );
        stream
            .write_all(request.as_bytes())
            .await
            .map_err(|e| e.to_string())?;

        let mut buffer = [0u8; 512];
        let bytes_read = tokio::time::timeout(timeout, stream.read(&mut buffer))
            .await
            .map_err(|_| "http read timed out".to_string())?
            .map_err(|e| e.to_string())?;

        let response = String::from_utf8_lossy(&buffer[..bytes_read]);
        let first_line = response.lines().next().ok_or("empty http response")?;
        let parts: Vec<&str> = first_line.split_whitespace().collect();
        if parts.len() < 2 {
            return Err("malformed http response status line".to_string());
        }
        let status_code: u16 = parts[1]
            .parse()
            .map_err(|_| "invalid http status code in response".to_string())?;

        if let Some(expected) = expected_status {
            if status_code == expected {
                Ok(())
            } else {
                Err(format!(
                    "unexpected http status code: {status_code} (expected {expected})"
                ))
            }
        } else if (200..=299).contains(&status_code) {
            Ok(())
        } else {
            Err(format!("http status code is not 2xx: {status_code}"))
        }
    }

    async fn handle_exit_policy(&mut self, clean_exit: bool) {
        let policy = self.spec.restart_policy.clone();
        match policy {
            RestartPolicy::Never => {
                // No restart
            }
            RestartPolicy::OnFailure {
                max_retries,
                backoff,
            } => {
                if !clean_exit {
                    self.execute_restart_or_quarantine(max_retries, &backoff)
                        .await;
                }
            }
            RestartPolicy::Always {
                max_retries,
                backoff,
            } => {
                self.execute_restart_or_quarantine(max_retries, &backoff)
                    .await;
            }
        }
    }

    async fn execute_restart_or_quarantine(
        &mut self,
        max_retries: Option<u32>,
        backoff: &BackoffConfig,
    ) {
        let attempt = self.restart_count + 1;
        if let Some(max) = max_retries {
            if attempt > max {
                self.transition_state(
                    ServiceState::Quarantined,
                    "RESTART_EXHAUSTED",
                    &format!(
                        "Maximum restart limit ({max}) exhausted for service {}",
                        self.spec.name
                    ),
                );
                return;
            }
        }

        self.restart_count = attempt;
        let delay = backoff.compute_delay(attempt);
        self.transition_state(
            ServiceState::Restarting,
            "RESTART_BACKOFF",
            &format!(
                "Restart attempt {} for service {} scheduled after {:?}",
                attempt, self.spec.name, delay
            ),
        );

        tokio::time::sleep(delay).await;
    }

    fn publish_endpoint_if_configured(&mut self) {
        if let Some(endpoint_spec) = &self.spec.endpoint {
            let mut attrs = endpoint_spec.attributes.clone();
            if let Some(port) = endpoint_spec.port {
                attrs.insert("port".to_string(), port.to_string());
            }
            if let Some(path) = &endpoint_spec.path {
                attrs.insert("path".to_string(), path.clone());
            }
            let endpoint = semantic::Endpoint {
                identity: semantic::Identity {
                    id: format!("endpoint/{}", self.spec.name),
                    generation: self.generation,
                },
                provider: semantic::Identity {
                    id: "provider/kernel-supervisor".to_string(),
                    generation: 1,
                },
                owner: semantic::Identity {
                    id: format!("service/{}", self.spec.name),
                    generation: self.generation,
                },
                transport: endpoint_spec.transport.clone(),
                schema_id: endpoint_spec.schema_id.clone(),
                capabilities: Vec::new(),
                public_attributes: attrs,
            };
            self.published_endpoint = Some(endpoint);
        }
    }

    fn unpublish_endpoint(&mut self) {
        self.published_endpoint = None;
    }

    fn transition_state(&mut self, state: ServiceState, reason_code: &str, message: &str) {
        self.state = state;
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let event = ServiceEvent {
            service_name: self.spec.name.clone(),
            state,
            timestamp_unix_ms: now_ms,
            reason_code: reason_code.to_string(),
            message: message.to_string(),
        };
        let _ = self.event_tx.send(event);
    }
}
