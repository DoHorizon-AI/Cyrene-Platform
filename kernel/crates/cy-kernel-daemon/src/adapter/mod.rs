// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: kernel/crates/cy-kernel-daemon/src/adapter/mod.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! KernelServiceAdapter 定义、构造器与后台监控线程。

pub(crate) mod events;
pub(crate) mod operations;
pub(crate) mod worker;

use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    ops::Deref,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};

use cy_adapter_client::HardwareAdapterObservation;
use cy_kernel_api::{
    semantic, AuthorityCallContext, DurableEventStore, InstalledPluginResolver,
    KernelProviderAuthority, NamespaceId, NoopRuntimeJournal, ProviderError, ResourceLease,
    RuntimeJournalEvent, RuntimeJournalSink, StopRequest,
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

#[derive(Debug)]
struct HardwareProviderTracker {
    session_generation: u64,
    last_snapshot_generation: Option<u64>,
    last_sampled_at_unix_ms: Option<u64>,
    publication_generation: u64,
    state: Option<semantic::ProviderState>,
}

impl HardwareProviderTracker {
    fn new(kernel_epoch: u64) -> Self {
        Self {
            session_generation: kernel_epoch.max(1),
            last_snapshot_generation: None,
            last_sampled_at_unix_ms: None,
            publication_generation: 0,
            state: None,
        }
    }

    fn advance_session(&mut self) -> Result<(), ProviderError> {
        self.session_generation = self.session_generation.checked_add(1).ok_or_else(|| {
            ProviderError::new(
                "kernel-hardware-provider",
                "PROVIDER_SESSION_GENERATION_EXHAUSTED",
                "hardware provider session generation cannot advance",
            )
        })?;
        self.last_snapshot_generation = None;
        self.last_sampled_at_unix_ms = None;
        self.publication_generation = 0;
        Ok(())
    }
}

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
    hardware_providers: Arc<Mutex<BTreeMap<String, HardwareProviderTracker>>>,
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
            hardware_providers: Arc::new(Mutex::new(BTreeMap::new())),
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

    pub fn with_event_store(mut self, event_store: Arc<dyn DurableEventStore>) -> Self {
        Arc::get_mut(&mut self.authority)
            .expect("authority cannot be reconfigured after adapter cloning")
            .set_event_store(event_store);
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

    /// Provider lifecycle/reconciliation is exposed only on its authenticated
    /// UDS listener, never on the client authority or Worker sockets.
    pub fn provider_server(
        &self,
    ) -> cy_proto::provider_v1::kernel_provider_service_server::KernelProviderServiceServer<Self>
    {
        cy_proto::provider_v1::kernel_provider_service_server::KernelProviderServiceServer::new(
            self.clone(),
        )
    }

    /// Starts the bounded watchdog loop. A missing heartbeat executes the same
    /// SIGTERM -> cgroup.kill cleanup path as an explicit termination.
    pub fn start_watchdog(&self) -> thread::JoinHandle<()> {
        let adapter = self.clone();
        thread::spawn(move || loop {
            thread::sleep(adapter.heartbeat.interval.min(Duration::from_secs(1)));
            adapter.enforce_heartbeat_deadlines();
            adapter.enforce_lease_expiry();
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
            let result = adapter.sync_hardware_provider_facts();
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

    /// Registers, publishes, and reconciles one resource-only Provider for
    /// every configured hardware adapter. Allocation still consumes the
    /// registry aggregate; these Provider snapshots never inherit its
    /// generation.
    pub fn sync_hardware_provider_facts(&self) -> Result<(), ProviderError> {
        let Some(observations) = self.daemon.hardware_adapter_observations() else {
            return self.daemon.refresh_inventory_facts().map(|_| ());
        };
        let has_unavailable_adapter = observations.values().any(Result::is_err);
        for (adapter_id, observation) in &observations {
            match observation {
                Ok(observation) => self.publish_hardware_provider(adapter_id, observation)?,
                Err(error) => self.publish_unavailable_hardware_provider(adapter_id, error)?,
            }
        }
        if let Err(error) = self
            .daemon
            .refresh_hardware_inventory_observations(&observations)
        {
            if !has_unavailable_adapter {
                return Err(error);
            }
        }
        Ok(())
    }

    fn publish_hardware_provider(
        &self,
        adapter_id: &str,
        observation: &HardwareAdapterObservation,
    ) -> Result<(), ProviderError> {
        let state = if observation.snapshot.capabilities.ready {
            semantic::ProviderState::Ready
        } else {
            semantic::ProviderState::Degraded
        };
        let (identity, publish_snapshot, publication_generation) = {
            let mut providers = self
                .hardware_providers
                .lock()
                .expect("hardware provider tracker lock poisoned");
            let tracker = providers
                .entry(adapter_id.to_string())
                .or_insert_with(|| HardwareProviderTracker::new(self.daemon.node_epoch));
            let snapshot_regressed = tracker
                .last_snapshot_generation
                .is_some_and(|generation| observation.snapshot.generation < generation);
            let state_requires_new_session =
                matches!(tracker.state, Some(semantic::ProviderState::Unavailable))
                    || matches!(
                        tracker.state,
                        Some(previous)
                            if previous != semantic::ProviderState::Unavailable && previous != state
                    );
            if snapshot_regressed || state_requires_new_session {
                tracker.advance_session()?;
            }
            let publish_snapshot = tracker
                .last_snapshot_generation
                .is_none_or(|generation| observation.snapshot.generation > generation)
                || tracker
                    .last_sampled_at_unix_ms
                    .is_none_or(|sampled| observation.sampled_at_unix_ms > sampled);
            if publish_snapshot {
                // A new actual sample refreshes liveness even when hardware facts are unchanged.
                // 新采样刷新有效期;重复缓存采样不能伪造持续在线。
                tracker.publication_generation = tracker
                    .publication_generation
                    .checked_add(1)
                    .ok_or_else(|| {
                        ProviderError::new(
                            "kernel-hardware-provider",
                            "PROVIDER_SNAPSHOT_GENERATION_EXHAUSTED",
                            "hardware snapshot publication generation cannot advance",
                        )
                    })?
                    .max(observation.snapshot.generation);
                tracker.last_snapshot_generation = Some(observation.snapshot.generation);
                tracker.last_sampled_at_unix_ms = Some(observation.sampled_at_unix_ms);
            }
            tracker.state = Some(state);
            (
                semantic::Identity {
                    id: adapter_id.to_string(),
                    generation: tracker.session_generation,
                },
                publish_snapshot,
                tracker.publication_generation,
            )
        };
        let provider = semantic::Provider {
            identity: identity.clone(),
            state,
            capabilities: Vec::new(),
        };
        let context = self.hardware_provider_context(adapter_id, &identity);
        let principal = self.hardware_provider_principal();
        self.authority
            .register_resource_facts_provider(&context, &principal, provider)
            .map_err(Self::hardware_provider_error)?;
        if publish_snapshot {
            let mut snapshot = self.hardware_provider_snapshot(identity.clone(), observation);
            snapshot.snapshot_generation = publication_generation;
            self.authority
                .publish_inventory(&context, &principal, snapshot)
                .map_err(Self::hardware_provider_error)?;
        }
        self.authority
            .reconcile_provider(&context, &principal, &identity)
            .map_err(Self::hardware_provider_error)?;
        Ok(())
    }

    fn publish_unavailable_hardware_provider(
        &self,
        adapter_id: &str,
        _error: &ProviderError,
    ) -> Result<(), ProviderError> {
        let identity = {
            let mut providers = self
                .hardware_providers
                .lock()
                .expect("hardware provider tracker lock poisoned");
            let tracker = providers
                .entry(adapter_id.to_string())
                .or_insert_with(|| HardwareProviderTracker::new(self.daemon.node_epoch));
            tracker.state = Some(semantic::ProviderState::Unavailable);
            semantic::Identity {
                id: adapter_id.to_string(),
                generation: tracker.session_generation,
            }
        };
        let context = self.hardware_provider_context(adapter_id, &identity);
        let principal = self.hardware_provider_principal();
        self.authority
            .register_resource_facts_provider(
                &context,
                &principal,
                semantic::Provider {
                    identity: identity.clone(),
                    state: semantic::ProviderState::Unavailable,
                    capabilities: Vec::new(),
                },
            )
            .map_err(Self::hardware_provider_error)?;
        self.authority
            .reconcile_provider(&context, &principal, &identity)
            .map_err(Self::hardware_provider_error)?;
        Ok(())
    }

    fn hardware_provider_snapshot(
        &self,
        provider: semantic::Identity,
        observation: &HardwareAdapterObservation,
    ) -> semantic::ProviderSnapshot {
        let max_ttl_ms = self
            .adapter_poll_interval
            .as_millis()
            .saturating_mul(2)
            .clamp(1, u64::MAX as u128) as u64;
        let mut resources = observation.snapshot.resources.clone();
        for resource in &mut resources {
            resource.provider = provider.clone();
        }
        semantic::ProviderSnapshot {
            provider,
            snapshot_generation: observation.snapshot.generation,
            resources,
            workers: Vec::new(),
            endpoints: Vec::new(),
            sampled_at_unix_ms: observation.sampled_at_unix_ms,
            expires_at_unix_ms: observation
                .expires_at_unix_ms
                .min(observation.sampled_at_unix_ms.saturating_add(max_ttl_ms)),
        }
    }

    fn hardware_provider_context(
        &self,
        adapter_id: &str,
        provider: &semantic::Identity,
    ) -> AuthorityCallContext {
        let request_id = format!("hardware-provider-{adapter_id}-{}", provider.generation);
        AuthorityCallContext {
            contract: semantic::ContractRevision::current(),
            namespace: NamespaceId::default(),
            request_id: request_id.clone(),
            idempotency_key: request_id,
        }
    }

    fn hardware_provider_principal(&self) -> semantic::Principal {
        semantic::Principal {
            identity: semantic::Identity {
                id: "kernel-hardware".to_string(),
                generation: self.daemon.node_epoch.max(1),
            },
        }
    }

    fn hardware_provider_error(error: semantic::Rejection) -> ProviderError {
        ProviderError::new(
            "kernel-hardware-provider",
            &error.reason_code,
            &error.message,
        )
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
        self.release_lease_with_cleanup(&core_v1::ResourceLeaseRef {
            lease_name: lease.name.clone(),
            fence_token: lease.fence_token,
        })
    }

    /// Releases a lease only after physical cleanup of the instance still
    /// fenced by it has been confirmed.
    ///
    /// `ACTIVE -> RELEASED` is never a single mutation. The ledger moves to
    /// `RELEASING` first, the sandbox is reaped, and only a complete
    /// `CleanupReport` authorizes `complete_release`. Any failure after
    /// `RELEASING` ends in `FAILED` with the allocation still held, so a
    /// half-cleaned resource can never be handed to a new lease.
    pub(crate) fn release_lease_with_cleanup(
        &self,
        lease_ref: &core_v1::ResourceLeaseRef,
    ) -> Result<(), ProviderError> {
        self.record_runtime(
            RuntimeJournalEvent::LeaseReleaseStarted,
            None,
            Some(lease_ref),
            "LEASE_RELEASE_STARTED",
        )?;
        let retrying_failed_cleanup =
            self.daemon.lease(&lease_ref.lease_name)?.state == cy_kernel_api::LeaseState::Failed;
        let releasing = self
            .daemon
            .begin_release(&lease_ref.lease_name, lease_ref.fence_token)?;
        let outcome = self.confirm_lease_cleanup(lease_ref).and_then(|cleaned| {
            if retrying_failed_cleanup && cleaned.is_none() {
                return Err(ProviderError::new(
                    "kernel-daemon",
                    "CLEANUP_INCOMPLETE",
                    "a failed lease has no managed actor to prove physical cleanup",
                ));
            }
            self.record_runtime(
                RuntimeJournalEvent::LeaseReleased,
                cleaned.as_deref(),
                Some(lease_ref),
                "LEASE_RELEASED",
            )?;
            self.daemon
                .complete_release(&lease_ref.lease_name, lease_ref.fence_token)?;
            Ok(cleaned)
        });
        match outcome {
            Ok(cleaned) => {
                if let Some(instance_name) = cleaned {
                    // The Worker that held the released Lease no longer has
                    // authority: remove its Endpoint/Grant metadata before the
                    // instance record disappears.
                    let worker_identity = self
                        .instances
                        .lock()
                        .expect("instance lock poisoned")
                        .get(&instance_name)
                        .and_then(|process| {
                            process
                                .semantic_worker
                                .as_ref()
                                .map(|worker| worker.identity.clone())
                        });
                    if let Some(identity) = worker_identity {
                        self.authority.purge_endpoint_authority(&identity);
                    }
                    self.instances
                        .lock()
                        .expect("instance lock poisoned")
                        .remove(&instance_name);
                }
                Ok(())
            }
            Err(error) => {
                let _ = self
                    .daemon
                    .fail_release(&releasing.name, lease_ref.fence_token);
                Err(error)
            }
        }
    }

    /// Reaps the sandboxed instance still fenced by `lease_ref` and returns its
    /// runtime name when one was cleaned. An incomplete report is an error so
    /// the caller keeps the physical allocation held.
    fn confirm_lease_cleanup(
        &self,
        lease_ref: &core_v1::ResourceLeaseRef,
    ) -> Result<Option<String>, ProviderError> {
        let cleaned = {
            let mut instances = self.instances.lock().expect("instance lock poisoned");
            let bound = instances.iter_mut().find(|(_, process)| {
                process.lease.as_ref().is_some_and(|lease| {
                    lease.lease_name == lease_ref.lease_name
                        && lease.fence_token == lease_ref.fence_token
                })
            });
            match bound {
                Some((instance_name, process)) => {
                    let instance_name = instance_name.clone();
                    let report = process
                        .actor
                        .stop(&StopRequest {
                            grace_period: self.heartbeat.graceful_stop,
                            immediate: false,
                        })?
                        .clone();
                    Some((instance_name, report))
                }
                None => None,
            }
        };
        let Some((instance_name, report)) = cleaned else {
            return Ok(None);
        };
        self.publish_cleanup_events(&instance_name, &report);
        if !report.complete {
            if let Err(error) = self.record_runtime(
                RuntimeJournalEvent::InstanceCleanupFailed,
                Some(&instance_name),
                Some(lease_ref),
                &report.reason_code,
            ) {
                // Class C: the cleanup outcome is already fail-closed
                // (CLEANUP_INCOMPLETE, Lease FAILED, allocation held).
                eprintln!("runtime journal InstanceCleanupFailed write failed: {error}");
            }
            return Err(ProviderError::new(
                "kernel-daemon",
                "CLEANUP_INCOMPLETE",
                &report.reason_code,
            ));
        }
        self.record_runtime(
            RuntimeJournalEvent::InstanceTerminated,
            Some(&instance_name),
            Some(lease_ref),
            "CLEANUP_COMPLETE",
        )?;
        Ok(Some(instance_name))
    }
}

impl Deref for KernelServiceAdapter {
    type Target = AuthorityRuntime;

    fn deref(&self) -> &Self::Target {
        &self.authority.runtime
    }
}
