//! Generic Service and Workload Supervision Specification.
//!
//! Provides Product-neutral definitions for hosting long-running services,
//! including lifecycle states, readiness probes, deterministic restart
//! backoff policies, endpoint bindings, and structured lifecycle events.
//!
//! # Architectural Invariant (ADR-010):
//!
//! `ServiceSpec`, `ServiceState`, `ServiceStatus`, and `ServiceEvent` are
//! **daemon-level process supervision & orchestration abstractions**.
//! They are **NOT** authoritative Kernel semantic entities.
//!
//! - **NO** `ServiceId` resource in `cy-kernel-contract`.
//! - **NO** `ServiceRepository`, `ServiceLedger`, or `Service` persistence in Kernel authority.
//! - **NO** independent authoritative `Service` event stream in the authority ledger.
//!
//! Service workloads compose existing Kernel primitives: [`LaunchPlan`],
//! [`crate::ProcessRuntime`], [`crate::SandboxBackend`], [`CleanupReport`], and [`semantic::Endpoint`].

use std::{collections::BTreeMap, time::Duration};

use cy_kernel_contract as semantic;

use crate::{launch::LaunchPlan, runtime::CleanupReport};

/// Bounded exponential backoff configuration for process restarts.
#[derive(Debug, Clone, PartialEq)]
pub struct BackoffConfig {
    /// Initial backoff delay before the first restart attempt.
    pub initial_delay: Duration,
    /// Maximum backoff ceiling.
    pub max_delay: Duration,
    /// Multiplier factor applied on successive restart attempts.
    pub multiplier: f64,
    /// Duration of continuous healthy execution required to reset the restart counter.
    pub reset_after: Duration,
}

impl Default for BackoffConfig {
    fn default() -> Self {
        Self {
            initial_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(30),
            multiplier: 2.0,
            reset_after: Duration::from_secs(60),
        }
    }
}

impl BackoffConfig {
    /// Create an exponential backoff configuration with explicit parameters.
    pub fn exponential(initial_delay: Duration, max_delay: Duration) -> Self {
        Self {
            initial_delay,
            max_delay,
            multiplier: 2.0,
            reset_after: Duration::from_secs(60),
        }
    }

    /// Compute the deterministic backoff delay for a given 1-based attempt index.
    pub fn compute_delay(&self, attempt: u32) -> Duration {
        if attempt == 0 {
            return Duration::ZERO;
        }
        let exp = attempt.saturating_sub(1);
        let factor = self.multiplier.powi(exp as i32);
        let delay_nanos = (self.initial_delay.as_nanos() as f64 * factor) as u128;
        let delay = Duration::from_nanos(delay_nanos.min(u64::MAX as u128) as u64);
        delay.min(self.max_delay)
    }
}

/// Service restart policy controlling supervisor behavior on process exit.
#[derive(Debug, Clone, PartialEq)]
pub enum RestartPolicy {
    /// Never restart the service process once it exits or fails.
    Never,
    /// Always restart the service regardless of exit code (clean exit or crash).
    Always {
        /// Optional ceiling on consecutive restart attempts.
        max_retries: Option<u32>,
        /// Exponential backoff policy.
        backoff: BackoffConfig,
    },
    /// Restart only when the process exits unexpectedly or with a non-zero exit code.
    OnFailure {
        /// Optional ceiling on consecutive restart attempts.
        max_retries: Option<u32>,
        /// Exponential backoff policy.
        backoff: BackoffConfig,
    },
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self::OnFailure {
            max_retries: Some(5),
            backoff: BackoffConfig::default(),
        }
    }
}

/// Generic readiness probe mechanism to determine when a service is ready to accept traffic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadinessProbe {
    /// Process is alive and running in the OS / sandbox.
    ProcessAlive,
    /// TCP connection can be established to the specified host and port.
    TcpSocket { host: String, port: u16 },
    /// HTTP GET request returns an acceptable status code (default 200..=299).
    HttpGet {
        host: String,
        port: u16,
        path: String,
        expected_status: Option<u16>,
    },
    /// WorkerControl bidirectional handshake (Hello/HelloAck) completed.
    WorkerControl,
}

/// Timing and retry parameters for readiness probe execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeConfig {
    /// Delay before the first probe attempt after process launch.
    pub initial_delay: Duration,
    /// Period between subsequent probe checks.
    pub period: Duration,
    /// Timeout for each individual probe check.
    pub timeout: Duration,
    /// Number of consecutive successes required to declare readiness.
    pub success_threshold: u32,
    /// Number of consecutive failures before declaring readiness probe failure.
    pub failure_threshold: u32,
}

impl Default for ProbeConfig {
    fn default() -> Self {
        Self {
            initial_delay: Duration::from_millis(10),
            period: Duration::from_millis(50),
            timeout: Duration::from_millis(500),
            success_threshold: 1,
            failure_threshold: 60, // allows up to 3.0s for slow runtimes
        }
    }
}

/// Specification for an endpoint exposed by the supervised service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceEndpointSpec {
    /// Endpoint transport mechanism (e.g. "http", "grpc", "tcp", "uds").
    pub transport: String,
    /// Schema ID describing the API contract.
    pub schema_id: String,
    /// Optional listening port.
    pub port: Option<u16>,
    /// Optional base path or socket path.
    pub path: Option<String>,
    /// Generic public metadata attributes.
    pub attributes: BTreeMap<String, String>,
}

/// Complete declarative specification for a generic long-running service workload.
#[derive(Debug, Clone, PartialEq)]
pub struct ServiceSpec {
    /// Unique service instance name.
    pub name: String,
    /// Concrete process launch plan.
    pub plan: LaunchPlan,
    /// Optional readiness probe configuration.
    pub readiness_probe: Option<(ReadinessProbe, ProbeConfig)>,
    /// Restart and failure policy.
    pub restart_policy: RestartPolicy,
    /// Maximum grace period to wait for graceful stop before SIGKILL escalation.
    pub graceful_stop_timeout: Duration,
    /// Optional service endpoint specification.
    pub endpoint: Option<ServiceEndpointSpec>,
}

impl ServiceSpec {
    pub fn new(name: impl Into<String>, plan: LaunchPlan) -> Self {
        Self {
            name: name.into(),
            plan,
            readiness_probe: Some((ReadinessProbe::ProcessAlive, ProbeConfig::default())),
            restart_policy: RestartPolicy::default(),
            graceful_stop_timeout: Duration::from_secs(5),
            endpoint: None,
        }
    }

    pub fn with_readiness_probe(mut self, probe: ReadinessProbe, config: ProbeConfig) -> Self {
        self.readiness_probe = Some((probe, config));
        self
    }

    pub fn with_restart_policy(mut self, policy: RestartPolicy) -> Self {
        self.restart_policy = policy;
        self
    }

    pub fn with_graceful_stop_timeout(mut self, timeout: Duration) -> Self {
        self.graceful_stop_timeout = timeout;
        self
    }

    pub fn with_endpoint(mut self, endpoint: ServiceEndpointSpec) -> Self {
        self.endpoint = Some(endpoint);
        self
    }
}

/// Observable lifecycle state of a supervised service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceState {
    /// Process has been launched and is waiting to satisfy readiness probes.
    Starting,
    /// Readiness probe succeeded; endpoints published.
    Ready,
    /// Service is actively executing workload.
    Running,
    /// Graceful stop requested; waiting for process exit within timeout.
    Stopping,
    /// Process exited cleanly and stopped.
    Stopped,
    /// Process terminated unexpectedly or failed readiness checks.
    Failed,
    /// Process is in backoff delay before being restarted.
    Restarting,
    /// Restart budget exhausted or consecutive crash limit reached.
    Quarantined,
}

impl ServiceState {
    /// Returns true if the service is in a terminal non-running state.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Stopped | Self::Failed | Self::Quarantined)
    }

    /// Returns true if the service is actively running or making progress.
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            Self::Starting | Self::Ready | Self::Running | Self::Restarting
        )
    }

    /// Convert state to stable canonical reason code.
    pub fn as_reason_code(&self) -> &'static str {
        match self {
            Self::Starting => "SERVICE_STARTING",
            Self::Ready => "SERVICE_READY",
            Self::Running => "SERVICE_RUNNING",
            Self::Stopping => "SERVICE_STOPPING",
            Self::Stopped => "SERVICE_STOPPED",
            Self::Failed => "SERVICE_FAILED",
            Self::Restarting => "SERVICE_RESTARTING",
            Self::Quarantined => "SERVICE_QUARANTINED",
        }
    }
}

/// Instantaneous status snapshot of a supervised service.
#[derive(Debug, Clone, PartialEq)]
pub struct ServiceStatus {
    pub name: String,
    pub state: ServiceState,
    pub restart_count: u32,
    pub last_exit_report: Option<CleanupReport>,
    pub published_endpoint: Option<semantic::Endpoint>,
    pub last_error: Option<String>,
}

/// Immutable structured lifecycle event published by the service supervisor.
#[derive(Debug, Clone, PartialEq)]
pub struct ServiceEvent {
    pub service_name: String,
    pub state: ServiceState,
    pub timestamp_unix_ms: u64,
    pub reason_code: String,
    pub message: String,
}
