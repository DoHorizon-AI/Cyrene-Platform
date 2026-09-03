// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: kernel/crates/cy-kernel-daemon/src/watchdog/instance_actor.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! Instance-level Watchdog Actor implementation.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use cy_kernel_api::{
    CleanupReport, DeviceBinding, LaunchPlan, ProcessHandle, ProviderError, RuntimeProcessEvidence,
    SandboxBackend, StopRequest,
};
use tokio::sync::{mpsc, oneshot, Mutex as AsyncMutex};

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

/// Protocol-neutral command sent to the framework-owned worker transport.
#[derive(Debug)]
pub enum WorkerTransportCommand {
    Request(WorkerTransportRequest),
    Cancel {
        cancel_request_id: String,
        target_request_id: String,
        generation: u64,
        fence_token: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerCancelAck {
    pub target_request_id: String,
}

type PendingCancellation = (String, oneshot::Sender<WorkerCancelAck>);
type PendingCancellations = Arc<Mutex<HashMap<String, PendingCancellation>>>;

#[derive(Debug)]
pub struct WorkerTransportRequest {
    pub request_id: String,
    pub generation: u64,
    pub fence_token: u64,
    /// Encoded worker-control Envelope. The Kernel keeps this opaque.
    pub payload: Vec<u8>,
}

#[derive(Debug)]
pub struct WorkerTransportResponse {
    pub request_id: String,
    pub generation: u64,
    pub fence_token: u64,
    pub payload: Vec<u8>,
}

#[derive(Clone)]
pub struct WorkerTransportDispatcher {
    generation: u64,
    fence_token: u64,
    pending_requests: Arc<AsyncMutex<HashMap<String, oneshot::Sender<WorkerTransportResponse>>>>,
    pending_cancellations: PendingCancellations,
}

impl WorkerTransportDispatcher {
    pub async fn dispatch(&self, response: WorkerTransportResponse) -> bool {
        if response.generation != self.generation || response.fence_token != self.fence_token {
            return false;
        }
        let Some(reply) = self
            .pending_requests
            .lock()
            .await
            .remove(&response.request_id)
        else {
            return false;
        };
        reply.send(response).is_ok()
    }

    pub fn dispatch_cancel_ack(&self, cancel_request_id: &str, target_request_id: &str) -> bool {
        let mut pending = self
            .pending_cancellations
            .lock()
            .expect("pending cancellation lock poisoned");
        let Some((target, _)) = pending.get(cancel_request_id) else {
            return false;
        };
        if target != target_request_id {
            return false;
        }
        let (_, reply) = pending
            .remove(cancel_request_id)
            .expect("pending cancellation existed");
        reply
            .send(WorkerCancelAck {
                target_request_id: target_request_id.to_string(),
            })
            .is_ok()
    }
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
    generation: u64,
    consecutive_timeouts: u32,
    transport_tx: Option<mpsc::Sender<WorkerTransportCommand>>,
    pending_requests: Arc<AsyncMutex<HashMap<String, oneshot::Sender<WorkerTransportResponse>>>>,
    pending_cancellations: PendingCancellations,
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
            generation: 0,
            consecutive_timeouts: 0,
            transport_tx: None,
            pending_requests: Arc::new(AsyncMutex::new(HashMap::new())),
            pending_cancellations: Arc::new(Mutex::new(HashMap::new())),
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

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn consecutive_timeouts(&self) -> u32 {
        self.consecutive_timeouts
    }

    /// Read-only access to the last observed heartbeat timestamp. Used by the
    /// kernel watchdog scan loop without requiring `&mut` borrow.
    pub fn last_heartbeat(&self) -> Option<Instant> {
        self.last_heartbeat
    }

    /// Read-only access to the configured heartbeat deadline. Used by the
    /// kernel watchdog scan loop to detect overdue instances.
    pub fn heartbeat_deadline(&self) -> Duration {
        self.heartbeat_deadline
    }

    /// Attach a communication channel to the sandboxed worker.
    pub fn attach_transport_channel(&mut self, tx: mpsc::Sender<WorkerTransportCommand>) {
        self.transport_tx = Some(tx);
        self.consecutive_timeouts = 0;
        if self.state == InstanceActorState::Starting || self.state == InstanceActorState::Stopped {
            self.state = InstanceActorState::Healthy;
        }
    }

    pub fn transport_attached(&self) -> bool {
        self.transport_tx.is_some()
    }

    pub fn transport_socket(&self) -> Option<PathBuf> {
        self.process.transport_socket().map(PathBuf::from)
    }

    /// Access the pending request map for incoming response routing.
    pub async fn dispatch_transport_response(&self, response: WorkerTransportResponse) -> bool {
        self.transport_dispatcher().dispatch(response).await
    }

    pub fn transport_dispatcher(&self) -> WorkerTransportDispatcher {
        WorkerTransportDispatcher {
            generation: self.generation,
            fence_token: self.fence_token,
            pending_requests: self.pending_requests.clone(),
            pending_cancellations: self.pending_cancellations.clone(),
        }
    }

    /// Detach a failed transport only if it still belongs to the active
    /// generation and fence. Dropping pending senders wakes blocked invokes;
    /// an invoke waiting on one records the crash when it observes closure.
    pub async fn mark_transport_lost(&mut self, generation: u64, fence_token: u64) -> bool {
        if generation != self.generation
            || fence_token != self.fence_token
            || self.transport_tx.take().is_none()
        {
            return false;
        }

        let had_pending_requests = {
            let mut pending = self.pending_requests.lock().await;
            let had_pending_requests = !pending.is_empty();
            pending.clear();
            had_pending_requests
        };
        if !had_pending_requests
            && !matches!(
                self.state,
                InstanceActorState::Stopping
                    | InstanceActorState::Stopped
                    | InstanceActorState::Quarantined
            )
        {
            self.record_crash(Instant::now());
        }
        true
    }

    /// Invoke an extension point RPC on the running sandboxed worker.
    pub async fn invoke_raw(
        &mut self,
        payload: Vec<u8>,
        timeout: Duration,
    ) -> Result<Vec<u8>, ProviderError> {
        self.send_request(uuid::Uuid::new_v4().to_string(), payload, timeout)
            .await
    }

    /// Send one opaque worker-control Envelope through the instance transport.
    ///
    /// The Kernel owns correlation, timeout, cancellation, generation and
    /// fence enforcement, but does not decode the worker protocol.
    pub async fn send_request(
        &mut self,
        request_id: String,
        payload: Vec<u8>,
        timeout: Duration,
    ) -> Result<Vec<u8>, ProviderError> {
        if request_id.is_empty() {
            return Err(ProviderError::new(
                "instance-watchdog",
                "INVALID_REQUEST_ID",
                "worker request id must not be empty",
            ));
        }

        if self.state == InstanceActorState::Quarantined {
            return Err(ProviderError::new(
                "instance-watchdog",
                "INSTANCE_QUARANTINED",
                "instance is quarantined; invoke forbidden",
            ));
        }
        if self.state == InstanceActorState::Draining {
            return Err(ProviderError::new(
                "instance-watchdog",
                "INSTANCE_DRAINING",
                "instance is draining; new invokes rejected",
            ));
        }

        let tx = self
            .transport_tx
            .as_ref()
            .ok_or_else(|| {
                ProviderError::new(
                    "instance-watchdog",
                    "TRANSPORT_UNAVAILABLE",
                    "instance transport channel not attached",
                )
            })?
            .clone();

        let req_id = request_id;
        let (reply_tx, reply_rx) = oneshot::channel();
        self.pending_requests
            .lock()
            .await
            .insert(req_id.clone(), reply_tx);

        if tx
            .send(WorkerTransportCommand::Request(WorkerTransportRequest {
                request_id: req_id.clone(),
                generation: self.generation,
                fence_token: self.fence_token,
                payload,
            }))
            .await
            .is_err()
        {
            self.pending_requests.lock().await.remove(&req_id);
            self.transport_tx = None;
            self.record_crash(Instant::now());
            return Err(ProviderError::new(
                "instance-watchdog",
                "TRANSPORT_SEND_FAILED",
                "failed to send envelope to instance transport",
            ));
        }

        match tokio::time::timeout(timeout, reply_rx).await {
            Ok(Ok(response)) if response.request_id == req_id => {
                self.consecutive_timeouts = 0;
                Ok(response.payload)
            }
            Ok(Ok(_)) | Ok(Err(_)) => {
                self.consecutive_timeouts = 0;
                self.transport_tx = None;
                self.record_crash(Instant::now());
                Err(ProviderError::new(
                    "instance-watchdog",
                    "INSTANCE_CRASHED",
                    "instance channel closed or invalid response",
                ))
            }
            Err(_) => {
                self.pending_requests.lock().await.remove(&req_id);
                let cancel_send = tokio::time::timeout(
                    Duration::from_millis(100),
                    tx.send(WorkerTransportCommand::Cancel {
                        cancel_request_id: format!("cancel-{req_id}"),
                        target_request_id: req_id.clone(),
                        generation: self.generation,
                        fence_token: self.fence_token,
                    }),
                )
                .await;
                self.consecutive_timeouts = self.consecutive_timeouts.saturating_add(1);
                if cancel_send.is_err()
                    || cancel_send.as_ref().is_ok_and(|res| res.is_err())
                    || self.consecutive_timeouts >= 3
                {
                    self.transport_tx = None;
                    self.record_crash(Instant::now());
                }
                Err(ProviderError::new(
                    "instance-watchdog",
                    "INVOKE_TIMEOUT",
                    "invoke request timed out",
                ))
            }
        }
    }

    /// Delivers one correlated cancellation request. Its reply is only a
    /// receipt; authority still waits for the executor's terminal report.
    pub fn request_cancel(
        &mut self,
        target_request_id: String,
    ) -> Result<oneshot::Receiver<WorkerCancelAck>, ProviderError> {
        let tx = self.transport_tx.as_ref().ok_or_else(|| {
            ProviderError::new(
                "instance-watchdog",
                "TRANSPORT_UNAVAILABLE",
                "instance transport channel not attached",
            )
        })?;
        let cancel_request_id = format!("cancel-{}", uuid::Uuid::new_v4());
        let (reply, receiver) = oneshot::channel();
        self.pending_cancellations
            .lock()
            .expect("pending cancellation lock poisoned")
            .insert(
                cancel_request_id.clone(),
                (target_request_id.clone(), reply),
            );
        if let Err(error) = tx.try_send(WorkerTransportCommand::Cancel {
            cancel_request_id: cancel_request_id.clone(),
            target_request_id,
            generation: self.generation,
            fence_token: self.fence_token,
        }) {
            self.pending_cancellations
                .lock()
                .expect("pending cancellation lock poisoned")
                .remove(&cancel_request_id);
            return Err(ProviderError::new(
                "instance-watchdog",
                "CANCEL_DELIVERY_FAILED",
                &format!("worker cancellation channel is unavailable or saturated: {error}"),
            ));
        }
        Ok(receiver)
    }

    pub fn cancel(&mut self, target_request_id: String) -> Result<(), ProviderError> {
        self.request_cancel(target_request_id).map(drop)
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

        let restarting_after_failure =
            self.state == InstanceActorState::Stopped && self.process.handle().is_some();
        self.state = InstanceActorState::Starting;
        if restarting_after_failure
            && self
                .process
                .stop(&StopRequest {
                    grace_period: Duration::from_millis(500),
                    immediate: true,
                })
                .map(|report| !report.complete)
                .unwrap_or(true)
        {
            self.state = InstanceActorState::Quarantined;
            return Err(ProviderError::new(
                "instance-watchdog",
                "RESTART_CLEANUP_FAILED",
                "previous worker process could not be completely cleaned up",
            ));
        }
        match self.process.start() {
            Ok(_) => {
                self.generation = self.generation.wrapping_add(1).max(1);
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

    /// Returns the post-launch identity that must be durably journaled before
    /// the worker becomes visible outside this Kernel process.
    pub fn recovery_evidence(&self) -> Result<RuntimeProcessEvidence, ProviderError> {
        self.process
            .handle()
            .ok_or_else(|| {
                ProviderError::new(
                    "instance-watchdog",
                    "RECOVERY_EVIDENCE_UNAVAILABLE",
                    "worker process has not started",
                )
            })?
            .recovery_evidence()
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
        if self.state == InstanceActorState::Degraded || self.state == InstanceActorState::Starting
        {
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
        self.transport_tx.take();
        let report = match self.process.stop(request) {
            Ok(report) => report,
            Err(error) => {
                self.state = InstanceActorState::Quarantined;
                return Err(error);
            }
        };
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

/// Test-only helper: construct a no-op `InstanceActor` for struct-level unit tests.
#[cfg(any(test, feature = "test-utils"))]
impl InstanceActor {
    pub fn new_for_test() -> Self {
        Self::new_for_test_inner(None)
    }

    pub fn new_for_test_with_transport(socket: impl Into<PathBuf>) -> Self {
        Self::new_for_test_inner(Some(socket.into()))
    }

    fn new_for_test_inner(transport_socket: Option<PathBuf>) -> Self {
        use cy_kernel_api::{
            CgroupLimits, CgroupTelemetry, DeviceBinding, EnforcementMode, LaunchPlan,
            NodeCapabilities, ProcessHandle, ProcessRuntime, ProviderError, SandboxBackend,
            StopRequest,
        };
        use std::{collections::BTreeMap, path::PathBuf};

        struct NoopSandbox {
            transport_socket: Option<PathBuf>,
        }
        impl ProcessRuntime for NoopSandbox {
            fn preflight(&self) -> NodeCapabilities {
                NodeCapabilities {
                    ready: true,
                    facts: Vec::new(),
                    enforcement: Vec::new(),
                }
            }
            fn launch(
                &self,
                _plan: &LaunchPlan,
                _binding: &DeviceBinding,
            ) -> Result<ProcessHandle, ProviderError> {
                Ok(ProcessHandle {
                    pid: 99999,
                    cgroup_path: PathBuf::from("/dev/null"),
                    start_time_ticks: Some(1),
                    transport_socket: self.transport_socket.clone(),
                })
            }
            fn stop(
                &self,
                _handle: &ProcessHandle,
                _request: &StopRequest,
            ) -> Result<cy_kernel_api::CleanupReport, ProviderError> {
                Ok(cy_kernel_api::CleanupReport {
                    complete: true,
                    exit_code: Some(0),
                    oom_killed: false,
                    conditions: Vec::new(),
                    reason_code: "NOOP".to_string(),
                })
            }
            fn telemetry(&self, _handle: &ProcessHandle) -> Result<CgroupTelemetry, ProviderError> {
                Ok(CgroupTelemetry::default())
            }
        }
        impl SandboxBackend for NoopSandbox {
            fn backend_id(&self) -> &str {
                "noop-test"
            }
        }

        Self::new(
            "test-instance",
            "test-lease",
            0,
            Arc::new(NoopSandbox { transport_socket }),
            LaunchPlan {
                instance_name: "test".to_string(),
                executable: PathBuf::from("/bin/true"),
                args: Vec::new(),
                environment: BTreeMap::new(),
                cgroup_name: "test".to_string(),
                limits: CgroupLimits::default(),
                working_dir: None,
                transport_socket: None,
            },
            DeviceBinding {
                resource_id: "test".to_string(),
                nodes: Vec::new(),
                environment: BTreeMap::new(),
                joinable_environment_keys: Default::default(),
                required_gids: Vec::new(),
                enforcement: EnforcementMode::Soft,
                adapter_id: "test".to_string(),
                reason_code: "test".to_string(),
            },
            Duration::from_secs(30),
        )
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
                transport_socket: None,
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
            working_dir: None,
            transport_socket: None,
        }
    }

    fn sample_binding() -> DeviceBinding {
        DeviceBinding {
            resource_id: "res-1".to_string(),
            nodes: Vec::new(),
            environment: BTreeMap::new(),
            joinable_environment_keys: Default::default(),
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

    #[tokio::test]
    async fn transport_loss_detaches_only_the_active_generation() {
        let mut actor = InstanceActor::new_for_test();
        actor.start().expect("start should succeed");
        let (tx, _rx) = mpsc::channel(1);
        actor.attach_transport_channel(tx);
        assert_eq!(actor.generation(), 1);

        assert!(!actor.mark_transport_lost(0, 0).await);
        assert!(actor.transport_attached());
        assert!(actor.mark_transport_lost(1, 0).await);
        assert!(!actor.transport_attached());
        assert_eq!(actor.state(), InstanceActorState::Stopped);
        assert!(!actor.mark_transport_lost(1, 0).await);
    }

    #[tokio::test]
    async fn single_timeout_preserves_transport_and_tracks_consecutive() {
        let mut actor = InstanceActor::new_for_test();
        actor.start().expect("start should succeed");
        let (tx, mut rx) = mpsc::channel(10);
        actor.attach_transport_channel(tx);

        // Spawn mock transport receiver that ignores requests (simulating timeout)
        tokio::spawn(async move {
            while let Some(cmd) = rx.recv().await {
                // Do not reply, just drain command (e.g. Cancel)
                let _ = cmd;
            }
        });

        // 1st timeout: fails with INVOKE_TIMEOUT, consecutive_timeouts = 1, transport STILL attached
        let err1 = actor
            .invoke_raw(vec![1, 2, 3], Duration::from_millis(20))
            .await
            .unwrap_err();
        assert_eq!(err1.reason_code, "INVOKE_TIMEOUT");
        assert_eq!(actor.consecutive_timeouts(), 1);
        assert!(
            actor.transport_attached(),
            "transport must remain attached on 1st timeout"
        );

        // 2nd timeout: consecutive_timeouts = 2, transport STILL attached
        let err2 = actor
            .invoke_raw(vec![1, 2, 3], Duration::from_millis(20))
            .await
            .unwrap_err();
        assert_eq!(err2.reason_code, "INVOKE_TIMEOUT");
        assert_eq!(actor.consecutive_timeouts(), 2);
        assert!(
            actor.transport_attached(),
            "transport must remain attached on 2nd timeout"
        );

        // 3rd timeout: threshold reached -> transport detached and crash recorded
        let err3 = actor
            .invoke_raw(vec![1, 2, 3], Duration::from_millis(20))
            .await
            .unwrap_err();
        assert_eq!(err3.reason_code, "INVOKE_TIMEOUT");
        assert_eq!(actor.consecutive_timeouts(), 3);
        assert!(
            !actor.transport_attached(),
            "transport must detach after 3rd consecutive timeout"
        );
    }
}
