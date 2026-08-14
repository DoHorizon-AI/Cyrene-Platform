//! Instance-level Watchdog Actor implementation.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use cy_kernel_api::{
    CleanupReport, DeviceBinding, LaunchPlan, ProcessHandle, ProviderError, SandboxBackend,
    StopRequest,
};

use crate::sandboxed_process::SandboxedProcess;

/// Lifecycle state for an active Worker Instance Actor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstanceActorState {
    Starting,
    Healthy,
    Degraded,
    Draining,
    Stopping,
    Stopped,
    Quarantined,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstanceHealthVerdict {
    Healthy,
    DeadlineExceeded,
    HeartbeatLost,
    Quarantined,
}

/// Actor representing one running instance managed by the Kernel.
pub struct InstanceActor {
    instance_id: String,
    lease_name: String,
    fence_token: u64,
    process: SandboxedProcess,
    state: InstanceActorState,
    last_heartbeat: Option<Instant>,
    heartbeat_deadline: Duration,
    crash_timestamps: Vec<Instant>,
}

impl InstanceActor {
    pub fn new(
        instance_id: impl Into<String>,
        lease_name: impl Into<String>,
        fence_token: u64,
        runtime: Arc<dyn SandboxBackend>,
        plan: LaunchPlan,
        binding: DeviceBinding,
        heartbeat_deadline: Duration,
    ) -> Self {
        Self {
            instance_id: instance_id.into(),
            lease_name: lease_name.into(),
            fence_token,
            process: SandboxedProcess::new(runtime, plan, binding),
            state: InstanceActorState::Starting,
            last_heartbeat: None,
            heartbeat_deadline,
            crash_timestamps: Vec::new(),
        }
    }

    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    pub fn lease_name(&self) -> &str {
        &self.lease_name
    }

    pub fn fence_token(&self) -> u64 {
        self.fence_token
    }

    pub fn state(&self) -> InstanceActorState {
        self.state
    }

    /// Launch the process inside the privileged sandbox.
    pub fn start(&mut self) -> Result<&ProcessHandle, ProviderError> {
        if self.state == InstanceActorState::Quarantined {
            return Err(ProviderError::new(
                "instance-watchdog",
                "INSTANCE_QUARANTINED",
                "instance is in quarantine; launch forbidden",
            ));
        }

        self.state = InstanceActorState::Starting;
        match self.process.start() {
            Ok(_) => {
                self.state = InstanceActorState::Healthy;
                self.last_heartbeat = Some(Instant::now());
                Ok(self
                    .process
                    .handle()
                    .expect("process handle must be set after successful start"))
            }
            Err(err) => {
                let err_clone = err.clone();
                self.record_crash(Instant::now());
                Err(err_clone)
            }
        }
    }
    /// Validate that the caller's fence token matches the active instance lease fence token.
    /// Rejects stale requests per ADR-HARDWARE-ADAPTER-BOUNDARY and Kernel Semantic Contract v1.
    pub fn validate_fence_token(&self, token: u64) -> Result<(), ProviderError> {
        if token != self.fence_token {
            return Err(ProviderError::new(
                "instance-watchdog",
                "FENCE_TOKEN_MISMATCH",
                &format!(
                    "stale fence token {token}; active instance lease token is {}",
                    self.fence_token
                ),
            ));
        }
        Ok(())
    }

    /// Mark the instance as Draining (rejecting new work while active tasks finish).
    pub fn drain(&mut self) {
        if self.state == InstanceActorState::Healthy || self.state == InstanceActorState::Degraded {
            self.state = InstanceActorState::Draining;
        }
    }

    /// Record a received heartbeat from the worker.
    pub fn on_heartbeat_received(&mut self, now: Instant) -> InstanceHealthVerdict {
        if self.state == InstanceActorState::Quarantined {
            return InstanceHealthVerdict::Quarantined;
        }

        self.last_heartbeat = Some(now);
        if self.state == InstanceActorState::Degraded || self.state == InstanceActorState::Starting {
            self.state = InstanceActorState::Healthy;
        }
        InstanceHealthVerdict::Healthy
    }

    /// Check if the heartbeat deadline has expired.
    pub fn check_heartbeat_deadline(&mut self, now: Instant) -> InstanceHealthVerdict {
        if self.state == InstanceActorState::Quarantined {
            return InstanceHealthVerdict::Quarantined;
        }

        if let Some(last) = self.last_heartbeat {
            if now.duration_since(last) > self.heartbeat_deadline {
                self.state = InstanceActorState::Degraded;
                return InstanceHealthVerdict::DeadlineExceeded;
            }
        } else if self.state != InstanceActorState::Starting {
            self.state = InstanceActorState::Degraded;
            return InstanceHealthVerdict::HeartbeatLost;
        }

        InstanceHealthVerdict::Healthy
    }

    /// Record a crash and evaluate quarantine condition (>3 crashes in 60s).
    pub fn record_crash(&mut self, now: Instant) -> bool {
        self.crash_timestamps.push(now);
        self.crash_timestamps
            .retain(|&t| now.duration_since(t) <= Duration::from_secs(60));

        if self.crash_timestamps.len() > 3 {
            self.state = InstanceActorState::Quarantined;
            true
        } else {
            self.state = InstanceActorState::Stopped;
            false
        }
    }

    /// Gracefully stop the sandboxed instance.
    pub fn stop(&mut self, request: &StopRequest) -> Result<&CleanupReport, ProviderError> {
        self.state = InstanceActorState::Stopping;
        let report = self.process.stop(request)?;
        if report.complete {
            if self.state != InstanceActorState::Quarantined {
                self.state = InstanceActorState::Stopped;
            }
        } else {
            self.state = InstanceActorState::Quarantined;
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cy_kernel_api::{
        CgroupLimits, CgroupTelemetry, EnforcementMode, NodeCapabilities, ProcessRuntime,
    };
    use std::{collections::BTreeMap, path::PathBuf};

    struct MockSandbox {
        should_fail: bool,
    }

    impl ProcessRuntime for MockSandbox {
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
            if self.should_fail {
                return Err(ProviderError::new("mock", "LAUNCH_FAILED", "mock fail"));
            }
            Ok(ProcessHandle {
                pid: 12345,
                cgroup_path: PathBuf::from(&format!("/cgroup/{}", plan.cgroup_name)),
                start_time_ticks: Some(100),
            })
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
                reason_code: "CLEANUP_COMPLETE".to_string(),
            })
        }

        fn telemetry(&self, _handle: &ProcessHandle) -> Result<CgroupTelemetry, ProviderError> {
            Ok(CgroupTelemetry::default())
        }
    }

    impl SandboxBackend for MockSandbox {
        fn backend_id(&self) -> &str {
            "mock-sandbox"
        }
    }

    fn sample_plan() -> LaunchPlan {
        LaunchPlan {
            instance_name: "inst-1".to_string(),
            executable: PathBuf::from("/bin/true"),
            args: Vec::new(),
            environment: BTreeMap::new(),
            cgroup_name: "instance-inst-1".to_string(),
            limits: CgroupLimits::default(),
        }
    }

    fn sample_binding() -> DeviceBinding {
        DeviceBinding {
            resource_id: "res-1".to_string(),
            nodes: Vec::new(),
            environment: BTreeMap::new(),
            required_gids: Vec::new(),
            enforcement: EnforcementMode::Soft,
            adapter_id: "test".to_string(),
            reason_code: "test".to_string(),
        }
    }

    #[test]
    fn test_actor_lifecycle_and_heartbeat_deadline() {
        let sandbox = Arc::new(MockSandbox { should_fail: false });
        let mut actor = InstanceActor::new(
            "inst-1",
            "lease-1",
            42,
            sandbox,
            sample_plan(),
            sample_binding(),
            Duration::from_secs(5),
        );

        assert_eq!(actor.state(), InstanceActorState::Starting);
        actor.start().expect("start should succeed");
        assert_eq!(actor.state(), InstanceActorState::Healthy);

        let now = Instant::now();
        // Check deadline within 5s -> Healthy
        let verdict = actor.check_heartbeat_deadline(now + Duration::from_secs(2));
        assert_eq!(verdict, InstanceHealthVerdict::Healthy);

        // Check deadline after 6s without heartbeat -> DeadlineExceeded & Degraded
        let verdict = actor.check_heartbeat_deadline(now + Duration::from_secs(6));
        assert_eq!(verdict, InstanceHealthVerdict::DeadlineExceeded);
        assert_eq!(actor.state(), InstanceActorState::Degraded);

        // Receive heartbeat -> Recover to Healthy
        let verdict = actor.on_heartbeat_received(now + Duration::from_secs(7));
        assert_eq!(verdict, InstanceHealthVerdict::Healthy);
        assert_eq!(actor.state(), InstanceActorState::Healthy);

        // Stop actor
        let stop_res = actor.stop(&StopRequest {
            grace_period: Duration::from_millis(500),
            immediate: false,
        });
        assert!(stop_res.is_ok());
        assert_eq!(actor.state(), InstanceActorState::Stopped);
    }

    #[test]
    fn test_actor_crash_loop_quarantine() {
        let sandbox = Arc::new(MockSandbox { should_fail: true });
        let mut actor = InstanceActor::new(
            "inst-crash",
            "lease-crash",
            42,
            sandbox,
            sample_plan(),
            sample_binding(),
            Duration::from_secs(5),
        );

        let base_time = Instant::now();
        assert!(!actor.record_crash(base_time));
        assert!(!actor.record_crash(base_time + Duration::from_secs(10)));
        assert!(!actor.record_crash(base_time + Duration::from_secs(20)));
        // 4th crash -> Quarantine!
        assert!(actor.record_crash(base_time + Duration::from_secs(30)));
        assert_eq!(actor.state(), InstanceActorState::Quarantined);

        // Subsequent start should fail closed
        let start_err = actor.start().unwrap_err();
        assert_eq!(start_err.reason_code, "INSTANCE_QUARANTINED");
    }

    #[test]
    fn test_actor_fence_token_validation_and_draining() {
        let sandbox = Arc::new(MockSandbox { should_fail: false });
        let mut actor = InstanceActor::new(
            "inst-fence",
            "lease-fence",
            100,
            sandbox,
            sample_plan(),
            sample_binding(),
            Duration::from_secs(5),
        );

        actor.start().expect("start should succeed");

        // Valid fence token matches active lease
        assert!(actor.validate_fence_token(100).is_ok());

        // Stale fence token is rejected
        let err = actor.validate_fence_token(99).unwrap_err();
        assert_eq!(err.reason_code, "FENCE_TOKEN_MISMATCH");

        // Drain transitions state to Draining
        actor.drain();
        assert_eq!(actor.state(), InstanceActorState::Draining);

        // Stop cleanly transitions Draining -> Stopped
        let stop_res = actor.stop(&StopRequest {
            grace_period: Duration::from_millis(500),
            immediate: false,
        });
        assert!(stop_res.is_ok());
        assert_eq!(actor.state(), InstanceActorState::Stopped);
    }
}
