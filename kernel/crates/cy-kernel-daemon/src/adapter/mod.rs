//! KernelServiceAdapter 定义、构造器与后台监控线程。

pub(crate) mod events;
pub(crate) mod operations;
pub(crate) mod worker;

use std::{
    collections::{HashMap, VecDeque},
    ops::Deref,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};

use cy_kernel_api::{
    InstalledPluginResolver, NoopRuntimeJournal, ProviderError, ResourceLease, RuntimeJournalEvent,
    RuntimeJournalSink,
};
use cy_proto::{core_v1, core_v2};
use tokio::sync::broadcast;
use tonic::Status;

use crate::{
    authority::{AuthorityRuntime, LocalKernelAuthority},
    daemon::KernelDaemon,
    session::WorkerHeartbeatConfig,
};

pub(crate) const OPERATION_EVENT_HISTORY_CAPACITY: usize = 256;
pub(crate) const OPERATION_EVENT_SUBSCRIBER_CAPACITY: usize = 64;

/// Core v1 KernelService 到真实资源管理器与 SandboxBackend 的最小服务适配层。
#[derive(Clone)]
pub struct KernelServiceAdapter {
    pub(crate) authority: Arc<LocalKernelAuthority>,
    pub(crate) operations: Arc<Mutex<HashMap<String, core_v1::Operation>>>,
    pub(crate) operation_events: Arc<Mutex<VecDeque<core_v1::OperationEvent>>>,
    pub(crate) operation_event_sender: broadcast::Sender<core_v1::OperationEvent>,
    pub(crate) next_event_sequence: Arc<AtomicU64>,
    pub(crate) adapter_available: Arc<AtomicBool>,
    pub(crate) adapter_poll_interval: Duration,
}

impl KernelServiceAdapter {
    pub fn new(daemon: Arc<KernelDaemon>, resolver: Arc<dyn InstalledPluginResolver>) -> Self {
        let (operation_event_sender, _) = broadcast::channel(OPERATION_EVENT_HISTORY_CAPACITY);
        Self {
            authority: Arc::new(LocalKernelAuthority::new(
                daemon,
                resolver,
                Arc::new(NoopRuntimeJournal),
            )),
            operations: Arc::new(Mutex::new(HashMap::new())),
            operation_events: Arc::new(Mutex::new(VecDeque::with_capacity(
                OPERATION_EVENT_HISTORY_CAPACITY,
            ))),
            operation_event_sender,
            next_event_sequence: Arc::new(AtomicU64::new(1)),
            adapter_available: Arc::new(AtomicBool::new(true)),
            adapter_poll_interval: Duration::from_secs(5),
        }
    }

    pub fn with_worker_heartbeat(mut self, heartbeat: WorkerHeartbeatConfig) -> Self {
        Arc::get_mut(&mut self.authority)
            .expect("authority cannot be reconfigured after adapter cloning")
            .set_worker_heartbeat(heartbeat);
        self
    }

    pub fn with_adapter_poll_interval(mut self, interval: Duration) -> Self {
        self.adapter_poll_interval = interval;
        self
    }

    pub fn with_runtime_journal(mut self, runtime_journal: Arc<dyn RuntimeJournalSink>) -> Self {
        Arc::get_mut(&mut self.authority)
            .expect("authority cannot be reconfigured after adapter cloning")
            .set_runtime_journal(runtime_journal);
        self
    }

    pub fn authority(&self) -> Arc<LocalKernelAuthority> {
        Arc::clone(&self.authority)
    }

    pub fn server(&self) -> core_v1::kernel_service_server::KernelServiceServer<Self> {
        core_v1::kernel_service_server::KernelServiceServer::new(self.clone())
    }

    /// Canonical, transport-neutral semantic action projection. It shares the
    /// same local UDS authorization boundary and does not expose legacy
    /// installation or vendor compatibility data.
    pub fn authority_server(
        &self,
    ) -> core_v1::kernel_authority_service_server::KernelAuthorityServiceServer<Self> {
        core_v1::kernel_authority_service_server::KernelAuthorityServiceServer::new(self.clone())
    }

    /// Core v2 authority projection shares the authenticated authority UDS
    /// listener but requires a namespace in every stateful request.
    pub fn authority_v2_server(
        &self,
    ) -> core_v2::kernel_authority_service_server::KernelAuthorityServiceServer<Self> {
        core_v2::kernel_authority_service_server::KernelAuthorityServiceServer::new(self.clone())
    }

    pub fn lifecycle_server(
        &self,
    ) -> core_v1::plugin_lifecycle_service_server::PluginLifecycleServiceServer<Self> {
        core_v1::plugin_lifecycle_service_server::PluginLifecycleServiceServer::new(self.clone())
    }

    /// Canonical Worker liveness/drain channel. It is registered only on the
    /// Worker UDS listener, separate from `KernelAuthorityService`.
    pub fn worker_control_server(
        &self,
    ) -> core_v1::worker_control_service_server::WorkerControlServiceServer<Self> {
        core_v1::worker_control_service_server::WorkerControlServiceServer::new(self.clone())
    }

    /// Starts the bounded watchdog loop. A missing heartbeat executes the same
    /// SIGTERM -> cgroup.kill cleanup path as an explicit termination.
    pub fn start_watchdog(&self) -> thread::JoinHandle<()> {
        let adapter = self.clone();
        thread::spawn(move || loop {
            thread::sleep(adapter.heartbeat.interval.min(Duration::from_secs(1)));
            adapter.enforce_heartbeat_deadlines();
        })
    }

    /// Continuously refreshes adapter facts independently of allocation paths.
    /// The state only transitions when a fact source disconnects/expires or
    /// subsequently recovers, so callers receive actionable DEGRADED evidence
    /// without unbounded event noise.
    pub fn start_adapter_monitor(&self) -> thread::JoinHandle<()> {
        let adapter = self.clone();
        thread::spawn(move || loop {
            thread::sleep(adapter.adapter_poll_interval);
            let result = adapter.daemon.refresh_inventory_facts();
            let ready = result.is_ok();
            let previous = adapter.adapter_available.swap(ready, Ordering::Relaxed);
            match (previous, ready) {
                (true, false) => {
                    let error = result.expect_err("adapter result is known to be an error");
                    adapter.publish_runtime_event(
                        core_v1::RuntimeEventType::AdapterDegraded,
                        "hardware-adapters",
                        error.reason_code,
                        error.message,
                    );
                }
                (false, true) => adapter.publish_runtime_event(
                    core_v1::RuntimeEventType::KernelReconciled,
                    "hardware-adapters",
                    "ADAPTER_RECOVERED",
                    "hardware adapter facts are current again",
                ),
                _ => {}
            }
        })
    }

    pub(crate) fn validate_node(&self, node: Option<&core_v1::NodeRef>) -> Result<(), Status> {
        let Some(node) = node else {
            return Ok(());
        };
        if node.node_id != self.daemon.node_id
            || (node.node_epoch != 0 && node.node_epoch != self.daemon.node_epoch)
        {
            return Err(Status::failed_precondition(
                "request targets another node epoch",
            ));
        }
        Ok(())
    }

    pub(crate) fn release_owned_lease(
        &self,
        owned_lease: bool,
        lease: &ResourceLease,
    ) -> Result<(), ProviderError> {
        if !owned_lease {
            return Ok(());
        }
        let lease_ref = core_v1::ResourceLeaseRef {
            lease_name: lease.name.clone(),
            fence_token: lease.fence_token,
        };
        self.record_runtime(
            RuntimeJournalEvent::LeaseReleaseStarted,
            None,
            Some(&lease_ref),
            "LEASE_RELEASE_STARTED",
        )?;
        let releasing = self.daemon.begin_release(&lease.name, lease.fence_token)?;
        if let Err(error) = self.record_runtime(
            RuntimeJournalEvent::LeaseReleased,
            None,
            Some(&lease_ref),
            "LEASE_RELEASED",
        ) {
            let _ = self.daemon.fail_release(&releasing.name, lease.fence_token);
            return Err(error);
        }
        if let Err(error) = self.daemon.complete_release(&lease.name, lease.fence_token) {
            let _ = self.daemon.fail_release(&releasing.name, lease.fence_token);
            return Err(error);
        }
        Ok(())
    }
}

impl Deref for KernelServiceAdapter {
    type Target = AuthorityRuntime;

    fn deref(&self) -> &Self::Target {
        &self.authority.runtime
    }
}
