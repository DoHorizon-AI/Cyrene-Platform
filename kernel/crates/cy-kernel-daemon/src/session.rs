//! Worker and managed process session types.

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use cy_kernel_api::semantic;
use cy_proto::core_v1;
use tokio::sync::mpsc;
use tonic::Status;

use crate::sandboxed_process::SandboxedProcess;

/// Worker heartbeat policy injected into every managed worker process.
#[derive(Debug, Clone)]
pub struct WorkerHeartbeatConfig {
    pub socket_path: PathBuf,
    pub interval: Duration,
    pub timeout: Duration,
    pub graceful_stop: Duration,
    /// Maximum time reserved for a Worker protocol-level ShutdownAck before
    /// sandboxd begins SIGTERM -> cgroup.kill -> reap.
    pub shutdown_ack_timeout: Duration,
}

impl Default for WorkerHeartbeatConfig {
    fn default() -> Self {
        Self {
            socket_path: PathBuf::from("/run/cyrene/kernel.sock"),
            interval: Duration::from_secs(5),
            timeout: Duration::from_secs(20),
            graceful_stop: Duration::from_secs(10),
            shutdown_ack_timeout: Duration::from_secs(3),
        }
    }
}

pub(crate) struct ManagedProcess {
    pub(crate) instance: SandboxedProcess,
    pub(crate) lease: Option<core_v1::ResourceLeaseRef>,
    pub(crate) semantic_worker: Option<semantic::Worker>,
    pub(crate) plugin: core_v1::InstalledPluginRef,
    pub(crate) generation: u64,
    pub(crate) accepted_sequence: u64,
    pub(crate) last_heartbeat: Instant,
    pub(crate) last_heartbeat_at: Option<prost_types::Timestamp>,
    pub(crate) runtime_state: i32,
    pub(crate) health: Option<core_v1::HealthReport>,
    pub(crate) restart_count: u32,
    pub(crate) watchdog_triggered: bool,
    pub(crate) control: Option<WorkerControlSession>,
    pub(crate) semantic_control: Option<SemanticWorkerControlSession>,
    pub(crate) pending_shutdown: Option<PendingWorkerShutdown>,
}

pub(crate) type WorkerControlSender = mpsc::Sender<Result<core_v1::KernelToWorker, Status>>;

pub(crate) struct WorkerControlSession {
    pub(crate) connection_id: u64,
    pub(crate) outbound: WorkerControlSender,
}

pub(crate) type SemanticWorkerControlSender =
    mpsc::Sender<Result<core_v1::KernelToWorkerControl, Status>>;

/// The canonical worker channel intentionally carries no authority operation.
/// A connected process may prove liveness and acknowledge a drain request, but
/// it cannot acquire a lease or publish an Endpoint through this socket.
pub(crate) struct SemanticWorkerControlSession {
    pub(crate) connection_id: u64,
    pub(crate) outbound: SemanticWorkerControlSender,
}

pub(crate) struct PendingWorkerShutdown {
    pub(crate) shutdown_id: String,
    pub(crate) acknowledged: bool,
    pub(crate) drained: bool,
}
