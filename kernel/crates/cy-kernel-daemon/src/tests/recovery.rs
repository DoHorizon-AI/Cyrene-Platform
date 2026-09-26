//! Watchdog, recovery, lease-expiry, and cleanup lifecycle tests.
//!
//! These tests cover failure detection, incomplete cleanup, and recovery
//! transitions before a lease becomes reusable.
//! 中文：Watchdog、恢复、Lease 到期和清理生命周期测试。这些测试覆盖故障检测、未完成的清理，以及 Lease 可重新使用前的恢复状态转换。

use super::*;

// Phase 6: Legacy Lease Lifecycle Closure (watchdog e2e)
//
// A Lease bound to a running execution domain must reach RELEASED only after
// ACTIVE -> RELEASING -> physical cleanup confirmed. The heartbeat-timeout
// watchdog is a production Lease lifecycle path: these tests drive the real
// `enforce_heartbeat_deadlines` scan and prove that an incomplete cleanup never
// exposes RELEASED and never reallocates the resource, while a complete
// cleanup releases the Lease and lets a replacement Lease advance the Fence.
// ---------------------------------------------------------------------------
// 中文：阶段 6：旧版 Lease 生命周期闭环（watchdog E2E）。绑定到运行中执行域的 Lease 只有在 ACTIVE -> RELEASING -> 确认物理清理后才能进入 RELEASED。heartbeat 超时 watchdog 是生产 Lease 生命周期路径：测试运行真实的 enforce_heartbeat_deadlines 扫描，并证明清理未完成时不会暴露 RELEASED，也不会重新分配资源；清理完成后会释放 Lease，并允许替代 Lease 使用更高的 Fence。

/// Sandbox whose `stop()` reports an incomplete physical cleanup, simulating a
/// worker that cannot be reaped after a heartbeat timeout.
/// 中文：Sandbox 的 stop() 报告物理清理未完成，用于模拟 Worker 在 heartbeat 超时后无法被 reap。
struct UninterruptibleSandbox;

impl ProcessRuntime for UninterruptibleSandbox {
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
            pid: 1,
            cgroup_path: PathBuf::from("/test"),
            start_time_ticks: Some(1),
            transport_socket: None,
        })
    }

    fn stop(
        &self,
        _handle: &ProcessHandle,
        _request: &StopRequest,
    ) -> Result<CleanupReport, ProviderError> {
        Ok(CleanupReport {
            complete: false,
            exit_code: None,
            oom_killed: false,
            conditions: vec![ProcessCondition {
                reason_code: "REAP_TIMEOUT".to_string(),
                summary: "instance could not be reaped".to_string(),
            }],
            reason_code: "PROCESS_UNINTERRUPTIBLE".to_string(),
        })
    }
}

impl SandboxBackend for UninterruptibleSandbox {
    fn backend_id(&self) -> &str {
        "uninterruptible-test"
    }
}

/// Builds an adapter holding one unique resource, an ACTIVE Lease bound to a
/// started execution domain, and an overdue heartbeat so the next watchdog
/// scan treats `instance_name` as timed out.
/// 中文：构建一个 adapter，其中包含唯一 Resource、绑定到已启动执行域的 ACTIVE Lease，以及已超时的 heartbeat，使下一次 watchdog 扫描将 instance_name 判定为超时。
fn watchdog_instance_scenario(
    runtime: Arc<dyn SandboxBackend>,
    instance_name: &str,
) -> (KernelServiceAdapter, cy_kernel_api::ResourceLease) {
    use crate::watchdog::InstanceActorState;

    let adapter = semantic_worker_adapter_with_resources(vec![test_resource()])
        .with_worker_heartbeat(WorkerHeartbeatConfig {
            socket_path: PathBuf::from("/run/cyrene/watchdog.sock"),
            interval: Duration::from_secs(1),
            timeout: Duration::from_millis(50),
            graceful_stop: Duration::from_millis(50),
            shutdown_ack_timeout: Duration::from_millis(20),
        });
    let holder = semantic::Identity {
        id: "watchdog-holder".to_string(),
        generation: 1,
    };
    let requirements = core_v1::ResourceRequirements {
        cpu: Some(core_v1::CpuRequirements {
            request_millicores: 500,
            limit_millicores: 750,
        }),
        memory: Some(core_v1::MemoryRequirements {
            request_bytes: 1024,
            limit_bytes: 2048,
        }),
        ephemeral_storage_limit_bytes: 0,
        accelerators: vec![core_v1::AcceleratorRequirements {
            count: 1,
            ..Default::default()
        }],
    };
    let request =
        resource_request("watchdog-lease", 1, holder, None, &requirements).expect("request");
    let lease = adapter
        .daemon
        .acquire_lease(request)
        .expect("unique resource is allocatable");

    let mut actor = InstanceActor::new(
        instance_name,
        lease.name.clone(),
        lease.fence_token,
        runtime,
        LaunchPlan {
            instance_name: instance_name.to_string(),
            executable: PathBuf::from("/bin/true"),
            args: Vec::new(),
            environment: BTreeMap::new(),
            cgroup_name: instance_name.to_string(),
            limits: CgroupLimits::default(),
            working_dir: None,
            transport_socket: None,
        },
        DeviceBinding {
            resource_id: lease.name.clone(),
            nodes: Vec::new(),
            environment: BTreeMap::new(),
            joinable_environment_keys: Default::default(),
            required_gids: Vec::new(),
            enforcement: EnforcementMode::Soft,
            adapter_id: "test".to_string(),
            reason_code: "test".to_string(),
        },
        Duration::from_millis(10),
    );
    actor.start().expect("watchdog instance must start");
    assert_eq!(actor.state(), InstanceActorState::Healthy);

    let mut process = managed_test_process(
        instance_name,
        lease.fence_token,
        Some(core_v1::ResourceLeaseRef {
            lease_name: lease.name.clone(),
            fence_token: lease.fence_token,
        }),
    );
    process.actor = actor;
    adapter
        .instances
        .lock()
        .unwrap()
        .insert(instance_name.to_string(), process);
    // Make the instance overdue: last heartbeat is far older than the 10ms
    // deadline configured on the actor.
    // 中文：将该 instance 设为超时：相对于 actor 配置的 10ms deadline，最后一次 heartbeat 早已过期。
    adapter
        .instances
        .lock()
        .unwrap()
        .get_mut(instance_name)
        .unwrap()
        .actor
        .on_heartbeat_received(std::time::Instant::now() - Duration::from_secs(1));
    (adapter, lease)
}

#[test]
fn watchdog_incomplete_cleanup_never_releases_and_blocks_reacquire() {
    let (adapter, lease) =
        watchdog_instance_scenario(Arc::new(UninterruptibleSandbox), "watchdog-w1");
    let old_fence = lease.fence_token;

    adapter.enforce_heartbeat_deadlines();

    // The Lease must never reach RELEASED: it fails closed instead.
    // 中文：Lease 绝不能进入 RELEASED；遇到失败时必须 fail-closed。
    let after = adapter.daemon.lease(&lease.name).unwrap();
    assert_eq!(
        after.state,
        cy_kernel_api::LeaseState::Failed,
        "incomplete cleanup must fail the Lease, never release it"
    );
    assert_ne!(
        after.state,
        cy_kernel_api::LeaseState::Released,
        "RELEASED must never be visible for an incompletely cleaned domain"
    );

    // The still-held allocation must block reacquisition.
    // 中文：仍被占用的 allocation 必须阻止重新获取资源。
    let holder = semantic::Identity {
        id: "watchdog-holder-retry".to_string(),
        generation: 1,
    };
    let requirements = core_v1::ResourceRequirements {
        cpu: Some(core_v1::CpuRequirements {
            request_millicores: 500,
            limit_millicores: 750,
        }),
        memory: Some(core_v1::MemoryRequirements {
            request_bytes: 1024,
            limit_bytes: 2048,
        }),
        ephemeral_storage_limit_bytes: 0,
        accelerators: vec![core_v1::AcceleratorRequirements {
            count: 1,
            ..Default::default()
        }],
    };
    let request =
        resource_request("watchdog-lease-retry", 1, holder, None, &requirements).expect("request");
    let reacquire = adapter.daemon.acquire_lease(request);
    assert!(
        reacquire.is_err(),
        "the still-held FAILED lease must block reallocation with INSUFFICIENT_RESOURCES"
    );
    assert!(
        old_fence > 0,
        "the acquired lease must carry a non-zero Fence"
    );
}

#[test]
fn watchdog_complete_cleanup_releases_and_replacement_fence_advances() {
    let (adapter, lease) = watchdog_instance_scenario(Arc::new(FakeSandbox), "watchdog-w2");
    let old_fence = lease.fence_token;

    adapter.enforce_heartbeat_deadlines();

    // Physical cleanup completed: the Lease is RELEASED.
    // 中文：物理清理已完成：Lease 进入 RELEASED。
    let after = adapter.daemon.lease(&lease.name).unwrap();
    assert_eq!(
        after.state,
        cy_kernel_api::LeaseState::Released,
        "complete cleanup must release the Lease"
    );

    // The resource is allocatable again and a replacement Lease succeeds with
    // a strictly higher Fence token (monotonic fencing).
    // 中文：资源重新变为可分配状态，替代 Lease 获取成功，且其 Fence token 严格增大（单调 fencing）。
    let holder = semantic::Identity {
        id: "watchdog-holder-replacement".to_string(),
        generation: 1,
    };
    let requirements = core_v1::ResourceRequirements {
        cpu: Some(core_v1::CpuRequirements {
            request_millicores: 500,
            limit_millicores: 750,
        }),
        memory: Some(core_v1::MemoryRequirements {
            request_bytes: 1024,
            limit_bytes: 2048,
        }),
        ephemeral_storage_limit_bytes: 0,
        accelerators: vec![core_v1::AcceleratorRequirements {
            count: 1,
            ..Default::default()
        }],
    };
    let request = resource_request("watchdog-lease-replacement", 1, holder, None, &requirements)
        .expect("request");
    let replacement = adapter
        .daemon
        .acquire_lease(request)
        .expect("replacement Lease must succeed after complete cleanup");
    assert!(
        replacement.fence_token > old_fence,
        "replacement Fence ({}) must exceed the old Fence ({})",
        replacement.fence_token,
        old_fence
    );
}

// ---------------------------------------------------------------------------
// Phase 7: Durability and Journal Failure Policy
//
// Durable writes are Class A (durable-before-visible), Class B (durable intent
// / physical action / durable outcome), or Class C (best-effort telemetry).
// These tests inject persistence failures and prove the policy: no unsafe
// visible authority, no fence reuse, no silently reusable resource, and no
// corruption of authority state when observability fails.
// ---------------------------------------------------------------------------
// 中文：阶段 7：持久化与 Journal 故障策略。持久写入分为 A 类（先持久化再可见）、B 类（先持久化意图，再执行物理操作，最后持久化结果）和 C 类（尽力而为的 telemetry）。这些测试注入持久化失败并验证策略：authority 不会以不安全状态可见、Fence 不会复用、资源不会被静默地重新使用，而且 observability 故障不会破坏 authority 状态。

// Class B intent: the watchdog must never begin a physical release without its
// durable LEASE_RELEASE_STARTED record. A failing journal defers the release to
// the next scan (fail-closed) instead of leaking the Lease as ACTIVE behind a
// stopped Worker.
// 中文：B 类意图：watchdog 在持久化 LEASE_RELEASE_STARTED 记录前绝不能开始物理释放。若 journal 写入失败，则本次释放延至下一次扫描（fail-closed），避免 Worker 已停止但 Lease 仍以 ACTIVE 状态泄漏。
#[test]
fn watchdog_release_intent_journal_failure_defers_fail_closed() {
    let adapter = semantic_worker_adapter_with_resources(vec![test_resource()])
        .with_worker_heartbeat(WorkerHeartbeatConfig {
            socket_path: PathBuf::from("/run/cyrene/watchdog-journal.sock"),
            interval: Duration::from_secs(1),
            timeout: Duration::from_millis(50),
            graceful_stop: Duration::from_millis(50),
            shutdown_ack_timeout: Duration::from_millis(20),
        })
        .with_runtime_journal(Arc::new(FailingRuntimeJournal));
    let holder = semantic::Identity {
        id: "watchdog-journal-holder".to_string(),
        generation: 1,
    };
    let requirements = core_v1::ResourceRequirements {
        cpu: Some(core_v1::CpuRequirements {
            request_millicores: 500,
            limit_millicores: 750,
        }),
        memory: Some(core_v1::MemoryRequirements {
            request_bytes: 1024,
            limit_bytes: 2048,
        }),
        ephemeral_storage_limit_bytes: 0,
        accelerators: vec![core_v1::AcceleratorRequirements {
            count: 1,
            ..Default::default()
        }],
    };
    let request = resource_request("watchdog-journal-lease", 1, holder, None, &requirements)
        .expect("request");
    let lease = adapter
        .daemon
        .acquire_lease(request)
        .expect("unique resource is allocatable");
    let mut actor = InstanceActor::new(
        "watchdog-journal-w1",
        lease.name.clone(),
        lease.fence_token,
        Arc::new(FakeSandbox),
        LaunchPlan {
            instance_name: "watchdog-journal-w1".to_string(),
            executable: PathBuf::from("/bin/true"),
            args: Vec::new(),
            environment: BTreeMap::new(),
            cgroup_name: "watchdog-journal-w1".to_string(),
            limits: CgroupLimits::default(),
            working_dir: None,
            transport_socket: None,
        },
        DeviceBinding {
            resource_id: lease.name.clone(),
            nodes: Vec::new(),
            environment: BTreeMap::new(),
            joinable_environment_keys: Default::default(),
            required_gids: Vec::new(),
            enforcement: EnforcementMode::Soft,
            adapter_id: "test".to_string(),
            reason_code: "test".to_string(),
        },
        Duration::from_millis(10),
    );
    actor.start().expect("watchdog instance must start");
    let mut process = managed_test_process(
        "watchdog-journal-w1",
        lease.fence_token,
        Some(core_v1::ResourceLeaseRef {
            lease_name: lease.name.clone(),
            fence_token: lease.fence_token,
        }),
    );
    process.actor = actor;
    adapter
        .instances
        .lock()
        .unwrap()
        .insert("watchdog-journal-w1".to_string(), process);
    adapter
        .instances
        .lock()
        .unwrap()
        .get_mut("watchdog-journal-w1")
        .unwrap()
        .actor
        .on_heartbeat_received(std::time::Instant::now() - Duration::from_secs(1));

    // The durable intent cannot be persisted: the watchdog must defer.
    // 中文：durable intent 无法持久化：watchdog 必须推迟释放。
    adapter.enforce_heartbeat_deadlines();

    // Fail-closed: the physical release never began; the Lease stays ACTIVE.
    // 中文：fail-closed：物理释放尚未开始，Lease 仍为 ACTIVE。
    let after = adapter.daemon.lease(&lease.name).unwrap();
    assert_eq!(
        after.state,
        cy_kernel_api::LeaseState::Active,
        "the release must not begin without its durable intent"
    );
    // The Worker is not stopped/removed and the watchdog is re-armed.
    // 中文：Worker 不会停止或删除，并且 watchdog 会重新安排检查。
    let instances = adapter.instances.lock().unwrap();
    let process = instances
        .get("watchdog-journal-w1")
        .expect("the instance must remain for the next scan");
    assert!(
        !process.watchdog_triggered,
        "the watchdog must be re-armed to retry the durable intent"
    );
    drop(instances);
    // The resource is not silently reusable.
    // 中文：资源不会被静默地重新使用。
    let retry = resource_request(
        "watchdog-journal-retry",
        1,
        semantic::Identity {
            id: "watchdog-journal-retry-holder".to_string(),
            generation: 1,
        },
        None,
        &requirements,
    )
    .expect("request");
    assert!(
        adapter.daemon.acquire_lease(retry).is_err(),
        "the still-ACTIVE Lease must keep the resource non-allocatable"
    );
}

// Class A/B launch evidence: a Worker must never become visible without its
// durable InstanceLaunched record. The launch fails closed and the Lease stays
// held, so the resource is not silently reusable.
// 中文：A/B 类启动证据：没有 InstanceLaunched 持久记录时，Worker 绝不能变为可见。启动必须 fail-closed 失败并继续占用 Lease，避免资源被静默地重新使用。
#[test]
fn worker_launch_journal_failure_fails_closed_and_keeps_lease_held() {
    #[derive(Default)]
    struct LaunchFailingJournal {
        records: std::sync::Mutex<Vec<RuntimeJournalRecord>>,
    }
    impl RuntimeJournalSink for LaunchFailingJournal {
        fn append(&self, record: RuntimeJournalRecord) -> Result<(), ProviderError> {
            if record.event == RuntimeJournalEvent::InstanceLaunched {
                return Err(ProviderError::new(
                    "failing-journal",
                    "JOURNAL_WRITE_FAILED",
                    "injected worker launch write failure",
                ));
            }
            self.records.lock().unwrap().push(record);
            Ok(())
        }
    }

    let adapter = semantic_worker_adapter_with_resources(vec![test_resource()])
        .with_runtime_journal(Arc::new(LaunchFailingJournal::default()));
    let authority = adapter.authority();
    let context = scoped_authority_context("ns-launch-fail", "launch-journal-fail");
    let principal = principal_from_peer_cred(&AUTHORITY_TEST_PEER);
    let worker_identity = semantic::Identity {
        id: "worker-launch-fail".to_string(),
        generation: 1,
    };
    let query = semantic::ResourceQuery {
        resource_class: "accelerator".to_string(),
        count: 1,
        required_capabilities: vec![semantic::CapabilityRequirement {
            id: "accelerator.compute".to_string(),
            minimum_revision: 1,
            required_properties: BTreeMap::new(),
        }],
        minimum_capacity: BTreeMap::new(),
    };
    let lease = authority
        .acquire_lease(
            &context,
            &principal,
            worker_identity.clone(),
            query,
            u64::MAX,
        )
        .unwrap();
    assert_eq!(lease.state, semantic::LeaseState::Active);

    // The InstanceLaunched durable evidence cannot be persisted.
    // 中文：InstanceLaunched 的持久证据无法写入。
    let launch = authority.start_worker(
        &context,
        &principal,
        semantic::Worker {
            identity: worker_identity.clone(),
            principal: principal.identity.clone(),
            provider: semantic::Identity {
                id: "provider-launch-fail".to_string(),
                generation: 1,
            },
            lease: lease.identity.clone(),
            state: semantic::WorkerState::Registered,
            execution_ref: "opaque-execution-reference".to_string(),
            limits: BTreeMap::new(),
        },
    );
    assert!(
        launch.is_err(),
        "launch must fail closed when its durable evidence cannot be persisted"
    );
    assert!(
        authority.runtime.instances.lock().unwrap().is_empty(),
        "no Worker may be externally visible without its durable launch evidence"
    );

    // The held Lease blocks reallocation: no silently reusable resource.
    // 中文：被占用的 Lease 会阻止资源重新分配，避免资源被静默地重新使用。
    let retry = authority.acquire_lease(
        &context,
        &principal,
        semantic::Identity {
            id: "worker-launch-fail-retry".to_string(),
            generation: 1,
        },
        semantic::ResourceQuery {
            resource_class: "accelerator".to_string(),
            count: 1,
            required_capabilities: vec![semantic::CapabilityRequirement {
                id: "accelerator.compute".to_string(),
                minimum_revision: 1,
                required_properties: BTreeMap::new(),
            }],
            minimum_capacity: BTreeMap::new(),
        },
        u64::MAX,
    );
    assert!(
        retry.is_err(),
        "the held ACTIVE Lease must block reallocation"
    );
}

// Class C: semantic event projections are observability, not correctness
// evidence. When the durable event store rejects an append the projection is
// dropped without corrupting authority state or panicking.
// 中文：C 类：语义事件投影属于 observability，不是正确性证据。durable event store 拒绝追加时，应丢弃该投影，不得破坏 authority 状态或触发 panic。
#[test]
fn semantic_event_append_failure_degrades_stream_without_silent_gap() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};

    struct FailOnceEventStore {
        fail_next: AtomicBool,
        append_count: AtomicUsize,
    }
    impl DurableEventStore for FailOnceEventStore {
        fn append_event(&self, _record: DurableEventRecord) -> Result<(), ProviderError> {
            self.append_count.fetch_add(1, AtomicOrdering::SeqCst);
            if self.fail_next.swap(false, AtomicOrdering::SeqCst) {
                return Err(ProviderError::new(
                    "test",
                    "EVENT_APPEND_FAILED",
                    "injected event append failure",
                ));
            }
            Ok(())
        }

        fn events_for_source(
            &self,
            _source: &semantic::Identity,
            _namespace: &str,
        ) -> Result<Option<Vec<DurableEventRecord>>, ProviderError> {
            Ok(None)
        }
    }

    let store = Arc::new(FailOnceEventStore {
        fail_next: AtomicBool::new(true),
        append_count: AtomicUsize::new(0),
    });
    let adapter = semantic_worker_adapter_with_resources(vec![test_resource()])
        .with_event_store(store.clone());
    let authority = adapter.authority();
    let context = scoped_authority_context("ns-event-fail", "event-append-fail");
    let principal = principal_from_peer_cred(&AUTHORITY_TEST_PEER);
    let worker_identity = semantic::Identity {
        id: "worker-event-fail".to_string(),
        generation: 1,
    };
    let query = semantic::ResourceQuery {
        resource_class: "accelerator".to_string(),
        count: 1,
        required_capabilities: vec![semantic::CapabilityRequirement {
            id: "accelerator.compute".to_string(),
            minimum_revision: 1,
            required_properties: BTreeMap::new(),
        }],
        minimum_capacity: BTreeMap::new(),
    };
    let lease = authority
        .acquire_lease(
            &context,
            &principal,
            worker_identity.clone(),
            query,
            u64::MAX,
        )
        .unwrap();
    assert_eq!(lease.state, semantic::LeaseState::Active);

    // The first normative event (worker.starting) append fails: the stream must
    // degrade rather than silently lose the fact.
    // 中文：首个规范事件（worker.starting）追加失败：事件流必须降级，不能静默丢失该事实。
    authority
        .start_worker(
            &context,
            &principal,
            semantic::Worker {
                identity: worker_identity.clone(),
                principal: principal.identity.clone(),
                provider: semantic::Identity {
                    id: "provider-event-fail".to_string(),
                    generation: 1,
                },
                lease: lease.identity.clone(),
                state: semantic::WorkerState::Registered,
                execution_ref: "opaque-execution-reference".to_string(),
                limits: BTreeMap::new(),
            },
        )
        .unwrap();
    assert!(
        !authority.runtime.instances.lock().unwrap().is_empty(),
        "authority state must be intact despite the failed event append"
    );

    // The store has since RECOVERED, but the stream must NOT continue as if
    // contiguous: an already-subscribed client must never silently miss the
    // failed fact while later events advance. The second publish is suppressed
    // by the degraded stream, so the store never sees another append attempt.
    // 中文：即使 store 随后恢复，事件流也不得假装序列仍连续：已订阅客户端绝不能在后续事件推进时静默漏掉失败的事实。降级后的事件流会抑制第二次 publish，因此 store 不会再收到追加请求。
    authority.publish_semantic_event_in(
        &context.namespace,
        worker_identity.clone(),
        "worker.running",
        "cyrene.worker.v1",
        Vec::new(),
    );
    assert_eq!(
        store.append_count.load(AtomicOrdering::SeqCst),
        1,
        "a degraded stream must not attempt further durable appends"
    );

    // The degraded stream is externally observable: replay surfaces Gap
    // (resnapshot required), never a silent Current-with-no-new-events stall.
    // 中文：降级后的事件流可从外部观察：replay 会显示 Gap（需要重新获取 snapshot），不能静默停滞并返回没有新事件的 Current。
    let page = authority
        .read_events(
            &context,
            &principal,
            &semantic::EventCursor {
                source: authority.semantic_event_source_for(&context.namespace),
                sequence: 0,
            },
            OPERATION_EVENT_HISTORY_CAPACITY,
        )
        .unwrap();
    assert_eq!(
        page.status,
        semantic::ReplayStatus::Gap,
        "a degraded stream must surface as Gap so clients resnapshot, not stall silently"
    );
    assert!(
        page.events.is_empty(),
        "no events may replay past a lost fact"
    );
}

#[test]
fn worker_launch_persistence_failure_reaps_physical_process() {
    use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

    #[derive(Default)]
    struct LaunchFailingJournal {
        records: std::sync::Mutex<Vec<RuntimeJournalRecord>>,
    }
    impl RuntimeJournalSink for LaunchFailingJournal {
        fn append(&self, record: RuntimeJournalRecord) -> Result<(), ProviderError> {
            if record.event == RuntimeJournalEvent::InstanceLaunched {
                return Err(ProviderError::new(
                    "failing-journal",
                    "JOURNAL_WRITE_FAILED",
                    "injected worker launch write failure",
                ));
            }
            self.records.lock().unwrap().push(record);
            Ok(())
        }
    }

    /// Records whether the physical sandbox process was launched and stopped,
    /// so the test can prove the spawned domain is synchronously reaped.
    /// 中文：记录物理 sandbox 进程是否已启动和停止，以便测试证明生成的执行域已同步 reap。
    #[derive(Clone, Default)]
    struct RecordingSandbox {
        launched: Arc<AtomicBool>,
        stopped: Arc<AtomicBool>,
    }
    impl ProcessRuntime for RecordingSandbox {
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
            self.launched.store(true, AtomicOrdering::SeqCst);
            Ok(ProcessHandle {
                pid: 1,
                cgroup_path: PathBuf::from("/test"),
                start_time_ticks: Some(1),
                transport_socket: None,
            })
        }

        fn stop(
            &self,
            _handle: &ProcessHandle,
            _request: &StopRequest,
        ) -> Result<CleanupReport, ProviderError> {
            self.stopped.store(true, AtomicOrdering::SeqCst);
            Ok(CleanupReport {
                complete: true,
                exit_code: Some(0),
                oom_killed: false,
                conditions: Vec::new(),
                reason_code: "TEST_STOP".to_string(),
            })
        }
    }
    impl SandboxBackend for RecordingSandbox {
        fn backend_id(&self) -> &str {
            "recording-test"
        }
    }

    let sandbox = Arc::new(RecordingSandbox::default());
    let hardware = Arc::new(TestHardware {
        resources: vec![test_resource()],
    });
    let daemon = Arc::new(KernelDaemon::new(
        hardware.clone(),
        hardware,
        Arc::new(InMemoryResourceManager::new("node", vec![test_resource()])),
        sandbox.clone(),
        "node",
        7,
    ));
    let adapter = KernelServiceAdapter::new(daemon, Arc::new(TestWorkerResolver))
        .with_runtime_journal(Arc::new(LaunchFailingJournal::default()));
    let authority = adapter.authority();
    let context = scoped_authority_context("ns-reap", "launch-reap");
    let principal = principal_from_peer_cred(&AUTHORITY_TEST_PEER);
    let worker_identity = semantic::Identity {
        id: "worker-reap".to_string(),
        generation: 1,
    };
    let query = semantic::ResourceQuery {
        resource_class: "accelerator".to_string(),
        count: 1,
        required_capabilities: vec![semantic::CapabilityRequirement {
            id: "accelerator.compute".to_string(),
            minimum_revision: 1,
            required_properties: BTreeMap::new(),
        }],
        minimum_capacity: BTreeMap::new(),
    };
    let lease = authority
        .acquire_lease(
            &context,
            &principal,
            worker_identity.clone(),
            query,
            u64::MAX,
        )
        .unwrap();

    // InstanceLaunched cannot be persisted after the physical spawn.
    // 中文：物理 spawn 之后，InstanceLaunched 无法持久化。
    let launch = authority.start_worker(
        &context,
        &principal,
        semantic::Worker {
            identity: worker_identity.clone(),
            principal: principal.identity.clone(),
            provider: semantic::Identity {
                id: "provider-reap".to_string(),
                generation: 1,
            },
            lease: lease.identity.clone(),
            state: semantic::WorkerState::Registered,
            execution_ref: "opaque-execution-reference".to_string(),
            limits: BTreeMap::new(),
        },
    );
    assert!(
        launch.is_err(),
        "launch must fail closed when InstanceLaunched cannot be persisted"
    );
    // The physical domain WAS spawned...
    // 中文：物理执行域确实已 spawn……
    assert!(
        sandbox.launched.load(AtomicOrdering::SeqCst),
        "the sandbox must have physically launched the process"
    );
    // ...and it was synchronously reaped before the failure was returned, so no
    // untracked physical execution domain remains alive.
    // 中文：……并且在返回失败前已同步 reap，因此没有未跟踪的物理执行域继续存活。
    assert!(
        sandbox.stopped.load(AtomicOrdering::SeqCst),
        "the launched physical process must be reaped before returning the failure"
    );
    assert!(
        authority.runtime.instances.lock().unwrap().is_empty(),
        "no semantic Worker may be visible without its durable launch evidence"
    );
    assert_eq!(lease.state, semantic::LeaseState::Active);
}

// Worst-case restart boundary: physical spawn succeeds -> InstanceLaunched
// persistence fails -> synchronous reap is incomplete -> InstanceCleanupFailed
// persistence ALSO fails. The durable journal must still retain the pre-launch
// intent (InstanceLaunching) so restart Discover/Classify can recognize "launch
// intended, outcome unknown" instead of treating the lease as a cleanly
// reserved, never-bound resource.
// 中文：最坏重启边界：物理 spawn 成功 -> InstanceLaunched 持久化失败 -> 同步 reap 未完成 -> InstanceCleanupFailed 持久化也失败。durable journal 仍必须保留启动前的意图（InstanceLaunching），使重启 Discover/Classify 能识别“已计划启动但结果未知”，而不是误判为已预留但从未绑定资源。
#[test]
fn worker_launch_double_persistence_failure_keeps_pre_launch_intent() {
    #[derive(Default)]
    struct LaunchEvidenceFailingJournal {
        records: std::sync::Mutex<Vec<RuntimeJournalRecord>>,
    }
    impl RuntimeJournalSink for LaunchEvidenceFailingJournal {
        fn append(&self, record: RuntimeJournalRecord) -> Result<(), ProviderError> {
            if matches!(
                record.event,
                RuntimeJournalEvent::InstanceLaunched | RuntimeJournalEvent::InstanceCleanupFailed
            ) {
                return Err(ProviderError::new(
                    "failing-journal",
                    "JOURNAL_WRITE_FAILED",
                    "injected post-spawn persistence failure",
                ));
            }
            self.records.lock().unwrap().push(record);
            Ok(())
        }
    }

    let journal = Arc::new(LaunchEvidenceFailingJournal::default());
    let hardware = Arc::new(TestHardware {
        resources: vec![test_resource()],
    });
    let daemon = Arc::new(KernelDaemon::new(
        hardware.clone(),
        hardware,
        Arc::new(InMemoryResourceManager::new("node", vec![test_resource()])),
        Arc::new(UninterruptibleSandbox),
        "node",
        7,
    ));
    let adapter = KernelServiceAdapter::new(daemon, Arc::new(TestWorkerResolver))
        .with_runtime_journal(journal.clone());
    let authority = adapter.authority();
    let context = scoped_authority_context("ns-double", "launch-double");
    let principal = principal_from_peer_cred(&AUTHORITY_TEST_PEER);
    let worker_identity = semantic::Identity {
        id: "worker-double".to_string(),
        generation: 1,
    };
    let query = semantic::ResourceQuery {
        resource_class: "accelerator".to_string(),
        count: 1,
        required_capabilities: vec![semantic::CapabilityRequirement {
            id: "accelerator.compute".to_string(),
            minimum_revision: 1,
            required_properties: BTreeMap::new(),
        }],
        minimum_capacity: BTreeMap::new(),
    };
    let lease = authority
        .acquire_lease(
            &context,
            &principal,
            worker_identity.clone(),
            query,
            u64::MAX,
        )
        .unwrap();

    // Spawn succeeds; InstanceLaunched fails; the synchronous reap is incomplete
    // (UninterruptibleSandbox) so InstanceCleanupFailed is attempted and ALSO
    // fails.
    // 中文：spawn 成功；InstanceLaunched 持久化失败；由于 UninterruptibleSandbox，同步 reap 未完成，因此尝试写入 InstanceCleanupFailed，但该写入也失败。
    let launch = authority.start_worker(
        &context,
        &principal,
        semantic::Worker {
            identity: worker_identity.clone(),
            principal: principal.identity.clone(),
            provider: semantic::Identity {
                id: "provider-double".to_string(),
                generation: 1,
            },
            lease: lease.identity.clone(),
            state: semantic::WorkerState::Registered,
            execution_ref: "opaque-execution-reference".to_string(),
            limits: BTreeMap::new(),
        },
    );
    assert!(
        launch.is_err(),
        "launch must fail closed on the post-spawn persistence failure"
    );

    // The durable journal still carries the pre-launch intent.
    // 中文：durable journal 仍保留启动前的意图。
    let records = journal.records.lock().unwrap();
    assert!(
        records.iter().any(|record| {
            record.event == RuntimeJournalEvent::InstanceLaunching
                && record.instance_name.as_deref() == Some("worker-double")
        }),
        "the pre-launch durable intent must survive the double persistence failure"
    );
    assert!(
        records
            .iter()
            .any(|record| record.event == RuntimeJournalEvent::LeaseAcquired),
        "the lease acquisition must be durably present"
    );
    assert!(
        !records
            .iter()
            .any(|record| record.event == RuntimeJournalEvent::InstanceLaunched),
        "no launch outcome may be falsely recorded"
    );
    drop(records);
}

// ---------------------------------------------------------------------------
// Phase 8: Canonical / Legacy Convergence parity
//
// Legacy RPCs must be compatibility projections over the shared semantic core
// (the resource-manager ledger and the shared release helper), never an
// independent Lease lifecycle. These tests prove legacy and canonical paths
// reach identical semantic results for the same scenario.
// ---------------------------------------------------------------------------
// 中文：阶段 8：规范路径与旧版路径收敛一致性。旧版 RPC 必须是共享语义核心（resource-manager ledger 和共享 release helper）的兼容投影，不能拥有独立 Lease 生命周期。这些测试证明相同场景下，旧版和规范路径会得到相同语义结果。

// Lease release parity: on incomplete physical cleanup, the legacy `release_lease`
// RPC and the canonical `authority.release_lease` fail closed identically —
// the Lease is FAILED (never RELEASED) and the resource cannot be reacquired.
// 中文：Lease 释放一致性：物理清理未完成时，旧版 release_lease RPC 与规范 authority.release_lease 都以相同方式 fail-closed——Lease 进入 FAILED（绝不进入 RELEASED），资源不能重新获取。
#[test]
fn legacy_and_canonical_release_fail_closed_identically_on_incomplete_cleanup() {
    let resources = vec![test_resource(), test_resource_with_id("resource-2")];
    let hardware = Arc::new(TestHardware {
        resources: resources.clone(),
    });
    let daemon = Arc::new(KernelDaemon::new(
        hardware.clone(),
        hardware,
        Arc::new(InMemoryResourceManager::new("node", resources)),
        Arc::new(UninterruptibleSandbox),
        "node",
        7,
    ));
    let adapter = KernelServiceAdapter::new(daemon, Arc::new(TestWorkerResolver));
    let authority = adapter.authority();
    // Distinct idempotency keys so the two acquires produce distinct Leases.
    // 中文：使用不同的 idempotency key，确保两次 acquire 创建不同的 Lease。
    let context_canonical = scoped_authority_context("ns-parity", "parity-canonical");
    let context_legacy = scoped_authority_context("ns-parity", "parity-legacy");
    let principal = principal_from_peer_cred(&AUTHORITY_TEST_PEER);
    let query = semantic::ResourceQuery {
        resource_class: "accelerator".to_string(),
        count: 1,
        required_capabilities: vec![semantic::CapabilityRequirement {
            id: "accelerator.compute".to_string(),
            minimum_revision: 1,
            required_properties: BTreeMap::new(),
        }],
        minimum_capacity: BTreeMap::new(),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let bind_stuck = |adapter: &KernelServiceAdapter,
                      worker_id: &str,
                      daemon_lease: &cy_kernel_api::ResourceLease| {
        let mut actor = InstanceActor::new(
            worker_id,
            daemon_lease.name.clone(),
            daemon_lease.fence_token,
            Arc::new(UninterruptibleSandbox),
            LaunchPlan {
                instance_name: worker_id.to_string(),
                executable: PathBuf::from("/bin/true"),
                args: Vec::new(),
                environment: BTreeMap::new(),
                cgroup_name: worker_id.to_string(),
                limits: CgroupLimits::default(),
                working_dir: None,
                transport_socket: None,
            },
            DeviceBinding {
                resource_id: daemon_lease.name.clone(),
                nodes: Vec::new(),
                environment: BTreeMap::new(),
                joinable_environment_keys: Default::default(),
                required_gids: Vec::new(),
                enforcement: EnforcementMode::Soft,
                adapter_id: "test".to_string(),
                reason_code: "test".to_string(),
            },
            Duration::from_secs(30),
        );
        actor.start().expect("stuck instance must start");
        let mut process = managed_test_process(
            worker_id,
            daemon_lease.fence_token,
            Some(core_v1::ResourceLeaseRef {
                lease_name: daemon_lease.name.clone(),
                fence_token: daemon_lease.fence_token,
            }),
        );
        process.actor = actor;
        adapter
            .instances
            .lock()
            .unwrap()
            .insert(worker_id.to_string(), process);
    };

    // --- Canonical path ---
    // 中文：规范路径。
    let canonical_worker = semantic::Identity {
        id: "worker-canonical".to_string(),
        generation: 1,
    };
    let canonical_lease = authority
        .acquire_lease(
            &context_canonical,
            &principal,
            canonical_worker.clone(),
            query,
            u64::MAX,
        )
        .unwrap();
    let canonical_daemon_name = authority
        .runtime
        .leases
        .lock()
        .unwrap()
        .get(&context_canonical.object_ref(canonical_lease.identity.clone()))
        .cloned()
        .expect("canonical lease is registered");
    bind_stuck(
        &adapter,
        "worker-canonical",
        &adapter.daemon.lease(&canonical_daemon_name).unwrap(),
    );
    let canonical_error = authority
        .release_lease(
            &context_canonical,
            &principal,
            &canonical_lease.identity,
            canonical_lease.fence_token,
        )
        .unwrap_err();
    assert_eq!(
        canonical_error.reason_code, "CLEANUP_INCOMPLETE",
        "canonical release must fail closed on incomplete cleanup"
    );
    let canonical_after = adapter.daemon.lease(&canonical_daemon_name).unwrap();
    assert_eq!(
        canonical_after.state,
        cy_kernel_api::LeaseState::Failed,
        "canonical release must leave the Lease FAILED, never RELEASED"
    );

    // A FAILED lease cannot be released just because its actor disappeared.
    // The same-fence retry requires a still-managed actor whose stop call can
    // return a fresh physical cleanup report.
    // 中文：actor 消失后，FAILED Lease 仍不能被释放。同一 Fence 的重试要求 actor 仍受管理，且其 stop 调用能返回新的物理清理报告。
    adapter.instances.lock().unwrap().remove("worker-canonical");
    let missing_actor_error = authority
        .release_lease(
            &context_canonical,
            &principal,
            &canonical_lease.identity,
            canonical_lease.fence_token,
        )
        .unwrap_err();
    assert_eq!(missing_actor_error.reason_code, "CLEANUP_INCOMPLETE");
    assert_eq!(
        adapter.daemon.lease(&canonical_daemon_name).unwrap().state,
        cy_kernel_api::LeaseState::Failed,
        "missing actor must leave the allocation failed and held"
    );

    // --- Legacy path ---
    // 中文：旧版路径。
    let legacy_worker = semantic::Identity {
        id: "worker-legacy".to_string(),
        generation: 1,
    };
    // Acquire through the legacy path (daemon.reserve) so the lease identity
    // is the daemon lease name the legacy release RPC resolves by.
    // 中文：通过旧版路径（daemon.reserve）获取资源，使 Lease identity 成为旧版 release RPC 用来查找的 daemon lease 名称。
    let legacy_requirements = core_v1::ResourceRequirements {
        cpu: Some(core_v1::CpuRequirements {
            request_millicores: 500,
            limit_millicores: 750,
        }),
        memory: Some(core_v1::MemoryRequirements {
            request_bytes: 1024,
            limit_bytes: 2048,
        }),
        ephemeral_storage_limit_bytes: 0,
        accelerators: vec![core_v1::AcceleratorRequirements {
            count: 1,
            ..Default::default()
        }],
    };
    let legacy_request = resource_request(
        "legacy-parity-lease",
        1,
        legacy_worker.clone(),
        None,
        &legacy_requirements,
    )
    .expect("legacy request");
    let legacy_daemon_lease = adapter
        .daemon
        .acquire_lease(legacy_request)
        .expect("the second resource is allocatable");
    bind_stuck(&adapter, "worker-legacy", &legacy_daemon_lease);
    let legacy_error = runtime
        .block_on(
            <KernelServiceAdapter as core_v1::kernel_service_server::KernelService>::release_lease(
                &adapter,
                authority_request(core_v1::LegacyReleaseLeaseRequest {
                    mutation: None,
                    lease: Some(semantic_v1::Identity {
                        id: legacy_daemon_lease.name.clone(),
                        generation: legacy_daemon_lease.generation,
                    }),
                    fence_token: legacy_daemon_lease.fence_token,
                }),
            ),
        )
        .unwrap_err();
    assert_eq!(
        legacy_error
            .metadata()
            .get("x-cyrene-reason-code")
            .and_then(|value| value.to_str().ok()),
        Some("CLEANUP_INCOMPLETE"),
        "legacy release must fail closed on incomplete cleanup"
    );
    let legacy_after = adapter.daemon.lease(&legacy_daemon_lease.name).unwrap();
    assert_eq!(
        legacy_after.state,
        cy_kernel_api::LeaseState::Failed,
        "legacy release must leave the Lease FAILED, never RELEASED"
    );

    // Both paths leave the resource non-reacquirable (no silently reusable
    // resource) and neither exposes RELEASED.
    // 中文：两条路径都会让资源保持不可重新获取状态（不会静默复用），并且都不会暴露 RELEASED。
    assert_ne!(canonical_after.state, cy_kernel_api::LeaseState::Released);
    assert_ne!(legacy_after.state, cy_kernel_api::LeaseState::Released);
    let retry = authority.acquire_lease(
        &context_legacy,
        &principal,
        semantic::Identity {
            id: "worker-parity-retry".to_string(),
            generation: 1,
        },
        semantic::ResourceQuery {
            resource_class: "accelerator".to_string(),
            count: 1,
            required_capabilities: vec![semantic::CapabilityRequirement {
                id: "accelerator.compute".to_string(),
                minimum_revision: 1,
                required_properties: BTreeMap::new(),
            }],
            minimum_capacity: BTreeMap::new(),
        },
        u64::MAX,
    );
    assert!(
        retry.is_err(),
        "both FAILED Leases must keep the resources non-reacquirable"
    );
}

// Process launch convergence: like canonical start_worker it now persists
// the Class B pre-launch intent before the physical spawn, so restart recovery
// can classify intent-without-outcome identically.
// 中文：进程启动收敛：与规范 start_worker 一样，现在会在物理 spawn 前持久化 B 类启动意图，因此重启恢复能以相同方式识别“有意图但无结果”的状态。
#[test]
fn launch_process_records_pre_launch_intent_like_canonical_start_worker() {
    use core_v1::kernel_service_server::KernelService;
    #[derive(Default)]
    struct IntentRecordingJournal {
        records: std::sync::Mutex<Vec<RuntimeJournalRecord>>,
    }
    impl RuntimeJournalSink for IntentRecordingJournal {
        fn append(&self, record: RuntimeJournalRecord) -> Result<(), ProviderError> {
            self.records.lock().unwrap().push(record);
            Ok(())
        }
    }

    struct ParityResolver;
    impl InstalledPluginResolver for ParityResolver {
        fn resolve_launch_plan(
            &self,
            installation: &VerifiedInstallation,
            instance_name: &str,
        ) -> Result<ResolvedLaunchPlan, ProviderError> {
            Ok(ResolvedLaunchPlan {
                installation: installation.clone(),
                plan: LaunchPlan {
                    instance_name: instance_name.to_string(),
                    executable: PathBuf::from("worker"),
                    args: Vec::new(),
                    environment: BTreeMap::new(),
                    cgroup_name: format!("instance-{instance_name}"),
                    limits: CgroupLimits::default(),
                    working_dir: None,
                    transport_socket: None,
                },
            })
        }

        fn resolve_worker_launch_plan(
            &self,
            _worker: &semantic::Worker,
        ) -> Result<ResolvedLaunchPlan, ProviderError> {
            Err(ProviderError::new("test", "UNUSED", "not used here"))
        }
    }

    let journal = Arc::new(IntentRecordingJournal::default());
    let hardware = Arc::new(TestHardware {
        resources: vec![test_resource()],
    });
    let daemon = Arc::new(KernelDaemon::new(
        hardware.clone(),
        hardware,
        Arc::new(InMemoryResourceManager::new("node", vec![test_resource()])),
        Arc::new(FakeSandbox),
        "node",
        7,
    ));
    let adapter = KernelServiceAdapter::new(daemon, Arc::new(ParityResolver))
        .with_runtime_journal(journal.clone());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let operation = runtime
        .block_on(
            adapter.launch_process(authority_request(core_v1::LaunchProcessRequest {
                node: Some(core_v1::NodeRef {
                    node_id: "node".to_string(),
                    node_epoch: 7,
                }),
                plugin: Some(core_v1::InstalledPluginRef {
                    installation_name: "plugin-parity".to_string(),
                    plugin_id: "test".to_string(),
                    version: "1".to_string(),
                    component_id: "test".to_string(),
                    manifest_digest: "sha256:test".to_string(),
                    artifact_digest: "sha256:test".to_string(),
                    verified_signature_identity: "test".to_string(),
                }),
                allocation: Some(core_v1::launch_process_request::Allocation::ResourceClaim(
                    core_v1::ResourceRequirements {
                        cpu: Some(core_v1::CpuRequirements {
                            request_millicores: 500,
                            limit_millicores: 750,
                        }),
                        memory: Some(core_v1::MemoryRequirements {
                            request_bytes: 1024,
                            limit_bytes: 2048,
                        }),
                        ephemeral_storage_limit_bytes: 0,
                        accelerators: vec![core_v1::AcceleratorRequirements {
                            count: 1,
                            ..Default::default()
                        }],
                    },
                )),
                mutation: Some(core_v1::MutationContext {
                    request: Some(core_v1::RequestContext {
                        request_id: "parity-launch".to_string(),
                        ..Default::default()
                    }),
                    idempotency_key: "parity-launch".to_string(),
                    expected_generation: Some(1),
                }),
                ..Default::default()
            })),
        )
        .unwrap()
        .into_inner();
    assert_eq!(operation.state, core_v1::OperationState::Running as i32);

    let records = journal.records.lock().unwrap();
    let intent = records
        .iter()
        .find(|record| record.event == RuntimeJournalEvent::InstanceLaunching);
    assert!(
        intent.is_some_and(|record| record.instance_name.as_deref() == Some("plugin-parity")),
        "launch_process must persist the pre-launch intent like canonical start_worker"
    );
    assert!(
        records
            .iter()
            .any(|record| record.event == RuntimeJournalEvent::InstanceLaunched),
        "the launch outcome must also be durably recorded"
    );
    drop(records);
}

#[test]
fn lease_expiry_actively_revokes_worker_advances_fence_and_cleans_up() {
    let adapter = semantic_worker_adapter();
    let authority = adapter.authority();
    let context = AuthorityCallContext {
        contract: semantic::ContractRevision::current(),
        namespace: NamespaceId::default(),
        request_id: "test-active-expiry".to_string(),
        idempotency_key: "test-active-expiry".to_string(),
    };
    let principal = principal_from_peer_cred(&AUTHORITY_TEST_PEER);
    let lease = authority
        .acquire_lease(
            &context,
            &principal,
            semantic::Identity {
                id: "worker-expiring".to_string(),
                generation: 1,
            },
            semantic::ResourceQuery {
                resource_class: "accelerator".to_string(),
                count: 1,
                required_capabilities: vec![semantic::CapabilityRequirement {
                    id: "accelerator.compute".to_string(),
                    minimum_revision: 1,
                    required_properties: BTreeMap::new(),
                }],
                minimum_capacity: BTreeMap::new(),
            },
            now_unix_ms().saturating_add(25),
        )
        .unwrap();

    let worker = semantic_worker_for(
        "worker-expiring",
        semantic_provider("test-provider", 1, semantic::ProviderState::Ready).identity,
        lease.identity.clone(),
        semantic::WorkerState::Registered,
    );
    authority
        .start_worker(&context, &principal, worker.clone())
        .unwrap();

    let endpoint = semantic_endpoint_for(&worker);
    authority
        .publish_endpoint(&context, &principal, endpoint.clone())
        .unwrap();

    thread::sleep(Duration::from_millis(40));

    // Active lease expiry scan triggers worker revocation, fence advance, and cleanup
    // 中文：活动 Lease 到期扫描会触发 Worker 撤销、Fence 前移和清理。
    let actions = authority.enforce_lease_expiry().unwrap();
    assert!(actions
        .iter()
        .any(|action| matches!(action, ProviderReconcileAction::MarkWorkerLost(id) if id == &worker.identity)));

    // Verify worker is Lost
    // 中文：验证 Worker 已进入 Lost。
    let instances = authority.runtime.instances.lock().unwrap();
    let process = instances.get("worker-expiring");
    assert!(process
        .and_then(|p| p.semantic_worker.as_ref())
        .is_some_and(|w| w.state == semantic::WorkerState::Lost));
    drop(instances);

    // Verify endpoint authority is purged
    // 中文：验证 Endpoint authority 已清除。
    assert!(!authority
        .runtime
        .endpoints
        .lock()
        .unwrap()
        .contains_key(&context.object_ref(endpoint.identity)));

    // Verify lease is revoked and resource can be re-allocated after cleanup
    // 中文：验证 Lease 已撤销，并且清理完成后资源可以重新分配。
    assert_eq!(
        adapter.daemon.lease(&lease.identity.id).unwrap().state,
        cy_kernel_api::LeaseState::Revoked
    );

    // Replacement lease can now be acquired because cleanup succeeded
    // 中文：清理成功，因此现在可以获取替代 Lease。
    let context_repl = AuthorityCallContext {
        contract: semantic::ContractRevision::current(),
        namespace: NamespaceId::default(),
        request_id: "test-active-expiry-replacement".to_string(),
        idempotency_key: "test-active-expiry-replacement".to_string(),
    };
    let new_lease = authority
        .acquire_lease(
            &context_repl,
            &principal,
            semantic::Identity {
                id: "worker-replacement".to_string(),
                generation: 1,
            },
            semantic::ResourceQuery {
                resource_class: "accelerator".to_string(),
                count: 1,
                required_capabilities: vec![semantic::CapabilityRequirement {
                    id: "accelerator.compute".to_string(),
                    minimum_revision: 1,
                    required_properties: BTreeMap::new(),
                }],
                minimum_capacity: BTreeMap::new(),
            },
            u64::MAX,
        )
        .unwrap();
    assert_eq!(new_lease.state, semantic::LeaseState::Active);
    assert!(new_lease.fence_token > lease.fence_token);
}

#[test]
fn renew_just_before_expiry_succeeds_and_extends_authority() {
    let adapter = semantic_worker_adapter();
    let authority = adapter.authority();
    let context = AuthorityCallContext {
        contract: semantic::ContractRevision::current(),
        namespace: NamespaceId::default(),
        request_id: "test-renew-before-expiry".to_string(),
        idempotency_key: "test-renew-before-expiry".to_string(),
    };
    let principal = principal_from_peer_cred(&AUTHORITY_TEST_PEER);
    let lease = authority
        .acquire_lease(
            &context,
            &principal,
            semantic::Identity {
                id: "worker-renew".to_string(),
                generation: 1,
            },
            semantic::ResourceQuery {
                resource_class: "accelerator".to_string(),
                count: 1,
                required_capabilities: vec![semantic::CapabilityRequirement {
                    id: "accelerator.compute".to_string(),
                    minimum_revision: 1,
                    required_properties: BTreeMap::new(),
                }],
                minimum_capacity: BTreeMap::new(),
            },
            now_unix_ms().saturating_add(200),
        )
        .unwrap();

    let worker = semantic_worker_for(
        "worker-renew",
        semantic_provider("test-provider", 1, semantic::ProviderState::Ready).identity,
        lease.identity.clone(),
        semantic::WorkerState::Registered,
    );
    authority
        .start_worker(&context, &principal, worker.clone())
        .unwrap();

    // Renew before expiry
    // 中文：在到期前续租。
    let extended_expiry = now_unix_ms().saturating_add(60_000);
    let renewed = authority
        .renew_lease(
            &context,
            &principal,
            &lease.identity,
            lease.fence_token,
            extended_expiry,
        )
        .unwrap();
    assert_eq!(renewed.state, semantic::LeaseState::Active);
    assert_eq!(renewed.fence_token, lease.fence_token);

    // Sleep past the original 200ms deadline
    // 中文：等待超过原始的 200ms deadline。
    thread::sleep(Duration::from_millis(220));

    // Active lease expiry scan must NOT expire the renewed lease
    // 中文：活动 Lease 到期扫描不得使已续租的 Lease 过期。
    authority.enforce_lease_expiry().unwrap();

    // Heartbeat still succeeds on the renewed lease
    // 中文：使用续租后的 Lease 发送 heartbeat 仍然成功。
    let hb = authority
        .report_heartbeat(
            &context,
            &principal,
            &worker.identity,
            &lease.identity,
            lease.fence_token,
        )
        .unwrap();
    assert_eq!(hb.state, semantic::WorkerState::Running);
}

#[test]
fn renew_racing_with_or_after_expiry_is_rejected_and_fails_closed() {
    let adapter = semantic_worker_adapter();
    let authority = adapter.authority();
    let context = AuthorityCallContext {
        contract: semantic::ContractRevision::current(),
        namespace: NamespaceId::default(),
        request_id: "test-stale-renew".to_string(),
        idempotency_key: "test-stale-renew".to_string(),
    };
    let principal = principal_from_peer_cred(&AUTHORITY_TEST_PEER);
    let lease = authority
        .acquire_lease(
            &context,
            &principal,
            semantic::Identity {
                id: "worker-stale-renew".to_string(),
                generation: 1,
            },
            semantic::ResourceQuery {
                resource_class: "accelerator".to_string(),
                count: 1,
                required_capabilities: vec![semantic::CapabilityRequirement {
                    id: "accelerator.compute".to_string(),
                    minimum_revision: 1,
                    required_properties: BTreeMap::new(),
                }],
                minimum_capacity: BTreeMap::new(),
            },
            now_unix_ms().saturating_add(25),
        )
        .unwrap();

    thread::sleep(Duration::from_millis(40));

    // Active expiry revokes the lease
    // 中文：活动到期扫描会撤销 Lease。
    authority.enforce_lease_expiry().unwrap();

    // Renew with old fence token after expiry is rejected
    // 中文：到期后使用旧 Fence token 续租会被拒绝。
    let err = authority
        .renew_lease(
            &context,
            &principal,
            &lease.identity,
            lease.fence_token,
            now_unix_ms().saturating_add(60_000),
        )
        .unwrap_err();
    assert!(matches!(
        err.reason_code.as_str(),
        "STALE_FENCE_TOKEN" | "LEASE_NOT_ACTIVE" | "LEASE_EXPIRY_REGRESSION"
    ));
}

#[test]
fn heartbeat_after_lease_expiry_is_rejected_and_fenced() {
    let adapter = semantic_worker_adapter();
    let authority = adapter.authority();
    let context = AuthorityCallContext {
        contract: semantic::ContractRevision::current(),
        namespace: NamespaceId::default(),
        request_id: "test-stale-hb".to_string(),
        idempotency_key: "test-stale-hb".to_string(),
    };
    let principal = principal_from_peer_cred(&AUTHORITY_TEST_PEER);
    let lease = authority
        .acquire_lease(
            &context,
            &principal,
            semantic::Identity {
                id: "worker-stale-hb".to_string(),
                generation: 1,
            },
            semantic::ResourceQuery {
                resource_class: "accelerator".to_string(),
                count: 1,
                required_capabilities: vec![semantic::CapabilityRequirement {
                    id: "accelerator.compute".to_string(),
                    minimum_revision: 1,
                    required_properties: BTreeMap::new(),
                }],
                minimum_capacity: BTreeMap::new(),
            },
            now_unix_ms().saturating_add(25),
        )
        .unwrap();

    let worker = semantic_worker_for(
        "worker-stale-hb",
        semantic_provider("test-provider", 1, semantic::ProviderState::Ready).identity,
        lease.identity.clone(),
        semantic::WorkerState::Registered,
    );
    authority
        .start_worker(&context, &principal, worker.clone())
        .unwrap();

    thread::sleep(Duration::from_millis(40));

    // Heartbeat after expiry is rejected
    // 中文：Lease 到期后发送 heartbeat 会被拒绝。
    let err = authority
        .report_heartbeat(
            &context,
            &principal,
            &worker.identity,
            &lease.identity,
            lease.fence_token,
        )
        .unwrap_err();
    assert_eq!(err.reason_code, "FENCE_MISMATCH");

    // Active expiry marks worker lost
    // 中文：活动到期扫描会将 Worker 标记为 lost。
    authority.enforce_lease_expiry().unwrap();
    let instances = authority.runtime.instances.lock().unwrap();
    assert_eq!(
        instances["worker-stale-hb"]
            .semantic_worker
            .as_ref()
            .unwrap()
            .state,
        semantic::WorkerState::Lost
    );
}

#[test]
fn expiry_with_incomplete_cleanup_blocks_resource_reallocation() {
    let (adapter, lease) = watchdog_instance_scenario(
        Arc::new(UninterruptibleSandbox),
        "worker.default.worker-incomplete-clean",
    );
    let authority = adapter.authority();
    let context = AuthorityCallContext {
        contract: semantic::ContractRevision::current(),
        namespace: NamespaceId::default(),
        request_id: "test-incomplete-clean".to_string(),
        idempotency_key: "test-incomplete-clean".to_string(),
    };
    let principal = principal_from_peer_cred(&AUTHORITY_TEST_PEER);

    // Expire the lease in the resource manager
    // 中文：在 resource manager 中使 Lease 到期。
    let _ = authority.runtime.daemon.renew_lease(
        &lease.name,
        lease.fence_token,
        now_unix_ms().saturating_add(25),
    );
    thread::sleep(Duration::from_millis(40));

    // Active expiry runs on the uncleaned instance
    // 中文：对尚未清理的 instance 执行活动到期扫描。
    authority.enforce_lease_expiry().unwrap();

    // Cleanup failed due to UninterruptibleSandbox -> complete_revocation must NOT have been called!
    // Attempting to re-acquire the resource must fail closed (INSUFFICIENT_RESOURCES)
    // 中文：由于 UninterruptibleSandbox，清理失败，因此绝不能调用 complete_revocation！尝试重新获取资源必须 fail-closed，返回 INSUFFICIENT_RESOURCES。
    let reacquire = authority.acquire_lease(
        &context,
        &principal,
        semantic::Identity {
            id: "worker-new".to_string(),
            generation: 1,
        },
        semantic::ResourceQuery {
            resource_class: "accelerator".to_string(),
            count: 1,
            required_capabilities: vec![semantic::CapabilityRequirement {
                id: "accelerator.compute".to_string(),
                minimum_revision: 1,
                required_properties: BTreeMap::new(),
            }],
            minimum_capacity: BTreeMap::new(),
        },
        u64::MAX,
    );
    assert!(reacquire.is_err());
    assert_eq!(reacquire.unwrap_err().reason_code, "INSUFFICIENT_RESOURCES");
}

#[test]
fn expired_lease_cannot_publish_or_authorize_endpoints() {
    let adapter = semantic_worker_adapter();
    let authority = adapter.authority();
    let context = AuthorityCallContext {
        contract: semantic::ContractRevision::current(),
        namespace: NamespaceId::default(),
        request_id: "test-endpoint-expiry".to_string(),
        idempotency_key: "test-endpoint-expiry".to_string(),
    };
    let principal = principal_from_peer_cred(&AUTHORITY_TEST_PEER);
    let lease = authority
        .acquire_lease(
            &context,
            &principal,
            semantic::Identity {
                id: "worker-endpoint-expiry".to_string(),
                generation: 1,
            },
            semantic::ResourceQuery {
                resource_class: "accelerator".to_string(),
                count: 1,
                required_capabilities: vec![semantic::CapabilityRequirement {
                    id: "accelerator.compute".to_string(),
                    minimum_revision: 1,
                    required_properties: BTreeMap::new(),
                }],
                minimum_capacity: BTreeMap::new(),
            },
            now_unix_ms().saturating_add(25),
        )
        .unwrap();

    let worker = semantic_worker_for(
        "worker-endpoint-expiry",
        semantic_provider("test-provider", 1, semantic::ProviderState::Ready).identity,
        lease.identity.clone(),
        semantic::WorkerState::Registered,
    );
    authority
        .start_worker(&context, &principal, worker.clone())
        .unwrap();

    thread::sleep(Duration::from_millis(40));

    // Publish endpoint after expiry is rejected
    // 中文：到期后发布 Endpoint 会被拒绝。
    let endpoint = semantic_endpoint_for(&worker);
    let pub_err = authority
        .publish_endpoint(&context, &principal, endpoint.clone())
        .unwrap_err();
    assert_eq!(pub_err.reason_code, "LEASE_NOT_ACTIVE");

    // Authorize endpoint targeting expired grantee lease is rejected
    // 中文：针对 grantee Lease 已过期的 Endpoint 授权会被拒绝。
    let grant = semantic::EndpointGrant {
        identity: semantic::Identity {
            id: "grant-1".to_string(),
            generation: 1,
        },
        endpoint: endpoint.identity.clone(),
        grantee: worker.identity.clone(),
        lease: lease.identity.clone(),
        fence_token: lease.fence_token,
        expires_at_unix_ms: now_unix_ms() + 10_000,
    };
    let auth_err = authority
        .authorize_endpoint(&context, &principal, grant)
        .unwrap_err();
    assert!(matches!(
        auth_err.reason_code.as_str(),
        "ENDPOINT_NOT_FOUND" | "LEASE_NOT_ACTIVE"
    ));
}

#[test]
fn expired_lease_advances_fence_and_replacement_lease_gets_newer_fence() {
    let adapter = semantic_worker_adapter();
    let authority = adapter.authority();
    let context_a = AuthorityCallContext {
        contract: semantic::ContractRevision::current(),
        namespace: NamespaceId::default(),
        request_id: "test-fence-advance-a".to_string(),
        idempotency_key: "test-fence-advance-a".to_string(),
    };
    let principal = principal_from_peer_cred(&AUTHORITY_TEST_PEER);
    let lease_a = authority
        .acquire_lease(
            &context_a,
            &principal,
            semantic::Identity {
                id: "worker-fence-a".to_string(),
                generation: 1,
            },
            semantic::ResourceQuery {
                resource_class: "accelerator".to_string(),
                count: 1,
                required_capabilities: vec![semantic::CapabilityRequirement {
                    id: "accelerator.compute".to_string(),
                    minimum_revision: 1,
                    required_properties: BTreeMap::new(),
                }],
                minimum_capacity: BTreeMap::new(),
            },
            now_unix_ms().saturating_add(25),
        )
        .unwrap();

    thread::sleep(Duration::from_millis(40));
    authority.enforce_lease_expiry().unwrap();

    // Check that the revoked lease fence advanced
    // 中文：检查已撤销 Lease 的 Fence 是否已前移。
    let revoked_a = adapter.daemon.lease(&lease_a.identity.id).unwrap();
    assert!(revoked_a.fence_token > lease_a.fence_token);

    // Replacement lease B gets an even newer fence token
    // 中文：替代 Lease B 会获得更新的 Fence token。
    let context_b = AuthorityCallContext {
        contract: semantic::ContractRevision::current(),
        namespace: NamespaceId::default(),
        request_id: "test-fence-advance-b".to_string(),
        idempotency_key: "test-fence-advance-b".to_string(),
    };
    let lease_b = authority
        .acquire_lease(
            &context_b,
            &principal,
            semantic::Identity {
                id: "worker-fence-b".to_string(),
                generation: 1,
            },
            semantic::ResourceQuery {
                resource_class: "accelerator".to_string(),
                count: 1,
                required_capabilities: vec![semantic::CapabilityRequirement {
                    id: "accelerator.compute".to_string(),
                    minimum_revision: 1,
                    required_properties: BTreeMap::new(),
                }],
                minimum_capacity: BTreeMap::new(),
            },
            u64::MAX,
        )
        .unwrap();

    assert!(lease_b.fence_token > revoked_a.fence_token);
    assert!(lease_b.fence_token > lease_a.fence_token);
}

#[test]
fn active_expiry_of_standalone_lease_revokes_and_reclaims_resource() {
    let adapter = semantic_worker_adapter();
    let authority = adapter.authority();
    let context_1 = AuthorityCallContext {
        contract: semantic::ContractRevision::current(),
        namespace: NamespaceId::default(),
        request_id: "test-standalone-expiry-1".to_string(),
        idempotency_key: "test-standalone-expiry-1".to_string(),
    };
    let principal = principal_from_peer_cred(&AUTHORITY_TEST_PEER);
    let lease = authority
        .acquire_lease(
            &context_1,
            &principal,
            semantic::Identity {
                id: "holder-standalone".to_string(),
                generation: 1,
            },
            semantic::ResourceQuery {
                resource_class: "accelerator".to_string(),
                count: 1,
                required_capabilities: vec![semantic::CapabilityRequirement {
                    id: "accelerator.compute".to_string(),
                    minimum_revision: 1,
                    required_properties: BTreeMap::new(),
                }],
                minimum_capacity: BTreeMap::new(),
            },
            now_unix_ms().saturating_add(25),
        )
        .unwrap();

    thread::sleep(Duration::from_millis(40));

    let actions = authority.enforce_lease_expiry().unwrap();
    assert!(actions
        .iter()
        .any(|a| matches!(a, ProviderReconcileAction::RevokeLease(id) if id == &lease.identity)));

    // Standalone lease had no process -> cleanup is immediate, resource reusable
    // 中文：独立 Lease 没有关联进程，因此清理会立即完成，资源可重新使用。
    let context_2 = AuthorityCallContext {
        contract: semantic::ContractRevision::current(),
        namespace: NamespaceId::default(),
        request_id: "test-standalone-expiry-2".to_string(),
        idempotency_key: "test-standalone-expiry-2".to_string(),
    };
    let new_lease = authority
        .acquire_lease(
            &context_2,
            &principal,
            semantic::Identity {
                id: "holder-standalone-2".to_string(),
                generation: 1,
            },
            semantic::ResourceQuery {
                resource_class: "accelerator".to_string(),
                count: 1,
                required_capabilities: vec![semantic::CapabilityRequirement {
                    id: "accelerator.compute".to_string(),
                    minimum_revision: 1,
                    required_properties: BTreeMap::new(),
                }],
                minimum_capacity: BTreeMap::new(),
            },
            u64::MAX,
        )
        .unwrap();
    assert_eq!(new_lease.state, semantic::LeaseState::Active);
}

#[test]
fn expiry_durability_failure_fails_closed_and_retries_until_cleanup() {
    #[derive(Default)]
    struct SwitchableExpiryJournal {
        should_fail: std::sync::atomic::AtomicBool,
        records: std::sync::Mutex<Vec<RuntimeJournalRecord>>,
    }
    impl RuntimeJournalSink for SwitchableExpiryJournal {
        fn append(&self, record: RuntimeJournalRecord) -> Result<(), ProviderError> {
            if self.should_fail.load(std::sync::atomic::Ordering::SeqCst)
                && matches!(
                    record.event,
                    RuntimeJournalEvent::WorkerLost | RuntimeJournalEvent::LeaseRevoked
                )
            {
                return Err(ProviderError::new(
                    "switchable-journal",
                    "JOURNAL_WRITE_FAILED",
                    "injected active expiry write failure",
                ));
            }
            self.records.lock().unwrap().push(record);
            Ok(())
        }
    }

    let journal = Arc::new(SwitchableExpiryJournal::default());
    let adapter = semantic_worker_adapter_with_resources(vec![test_resource()])
        .with_runtime_journal(journal.clone());
    let authority = adapter.authority();
    let context = AuthorityCallContext {
        contract: semantic::ContractRevision::current(),
        namespace: NamespaceId::default(),
        request_id: "test-expiry-durability-fail".to_string(),
        idempotency_key: "test-expiry-durability-fail".to_string(),
    };
    let principal = principal_from_peer_cred(&AUTHORITY_TEST_PEER);
    let worker_identity = semantic::Identity {
        id: "worker-expiring-fail".to_string(),
        generation: 1,
    };
    let query = semantic::ResourceQuery {
        resource_class: "accelerator".to_string(),
        count: 1,
        required_capabilities: vec![semantic::CapabilityRequirement {
            id: "accelerator.compute".to_string(),
            minimum_revision: 1,
            required_properties: BTreeMap::new(),
        }],
        minimum_capacity: BTreeMap::new(),
    };

    // 1. ACTIVE Lease + running Worker
    // 中文：1. ACTIVE Lease 与运行中的 Worker。
    let lease = authority
        .acquire_lease(
            &context,
            &principal,
            worker_identity.clone(),
            query.clone(),
            now_unix_ms().saturating_add(25),
        )
        .unwrap();

    let worker = semantic_worker_for(
        &worker_identity.id,
        semantic_provider("test-provider", 1, semantic::ProviderState::Ready).identity,
        lease.identity.clone(),
        semantic::WorkerState::Registered,
    );
    authority
        .start_worker(&context, &principal, worker.clone())
        .unwrap();

    let endpoint = semantic_endpoint_for(&worker);
    authority
        .publish_endpoint(&context, &principal, endpoint.clone())
        .unwrap();

    // 2. TTL expires
    // 中文：2. TTL 到期。
    thread::sleep(Duration::from_millis(40));

    // 3. Inject failure in the first durability/revocation step after expiry detection
    // 中文：3. 在到期检测后的第一个持久化/撤销步骤注入故障。
    journal
        .should_fail
        .store(true, std::sync::atomic::Ordering::SeqCst);

    // Watchdog / enforce_lease_expiry runs; first durability step fails
    // 中文：运行 watchdog / enforce_lease_expiry；第一个持久化步骤失败。
    let actions = authority.enforce_lease_expiry().unwrap();
    assert!(!actions
        .iter()
        .any(|a| matches!(a, ProviderReconcileAction::RevokeLease(_))));

    // 4. Verify old authority remains rejected (fail-closed)
    // 中文：4. 验证旧 authority 仍被拒绝（fail-closed）。
    let hb_err = authority
        .report_heartbeat(&context, &principal, &worker_identity, &lease.identity, 1)
        .unwrap_err();
    assert_eq!(hb_err.reason_code, "FENCE_MISMATCH");

    let renew_err = authority
        .renew_lease(
            &context,
            &principal,
            &lease.identity,
            1,
            now_unix_ms().saturating_add(10_000),
        )
        .unwrap_err();
    assert_eq!(renew_err.reason_code, "LEASE_NOT_ACTIVE");

    let pub_err = authority
        .publish_endpoint(&context, &principal, endpoint.clone())
        .unwrap_err();
    assert_eq!(pub_err.reason_code, "LEASE_NOT_ACTIVE");

    // 5. Verify Resource is NOT reusable (allocation held despite Expired state)
    // 中文：5. 验证 Resource 仍不可重新使用（即使状态为 Expired，allocation 仍被占用）。
    let context_realloc = AuthorityCallContext {
        contract: semantic::ContractRevision::current(),
        namespace: NamespaceId::default(),
        request_id: "test-realloc-blocked".to_string(),
        idempotency_key: "test-realloc-blocked".to_string(),
    };
    let realloc_err = authority
        .acquire_lease(
            &context_realloc,
            &principal,
            semantic::Identity {
                id: "other-worker".to_string(),
                generation: 1,
            },
            query.clone(),
            now_unix_ms().saturating_add(10_000),
        )
        .unwrap_err();
    assert_eq!(realloc_err.reason_code, "INSUFFICIENT_RESOURCES");
    assert_eq!(
        adapter.daemon.lease(&lease.identity.id).unwrap().state,
        cy_kernel_api::LeaseState::Expired
    );

    // 6. Verify watchdog/reconciliation retries rather than permanently ignoring the EXPIRED Lease
    // 中文：6. 验证 watchdog/reconciliation 会重试，而不是永久忽略 EXPIRED Lease。
    let retry_actions = authority.enforce_lease_expiry().unwrap();
    assert!(!retry_actions
        .iter()
        .any(|a| matches!(a, ProviderReconcileAction::RevokeLease(_))));

    // 7. Remove failure
    // 中文：7. 移除故障。
    journal
        .should_fail
        .store(false, std::sync::atomic::Ordering::SeqCst);

    // 8. Cleanup / revocation completes on next cycle
    // 中文：8. 下一轮完成清理/撤销。
    let final_actions = authority.enforce_lease_expiry().unwrap();
    assert!(final_actions.iter().any(
        |a| matches!(a, ProviderReconcileAction::MarkWorkerLost(id) if id == &worker.identity)
    ));

    // Verify lease is now Revoked in daemon
    // 中文：验证 daemon 中的 Lease 现在已是 Revoked。
    assert_eq!(
        adapter.daemon.lease(&lease.identity.id).unwrap().state,
        cy_kernel_api::LeaseState::Revoked
    );

    // 9. Resource becomes reusable
    // 中文：9. Resource 重新变为可使用。
    let repl_lease = authority
        .acquire_lease(
            &context_realloc,
            &principal,
            semantic::Identity {
                id: "other-worker".to_string(),
                generation: 1,
            },
            query,
            u64::MAX,
        )
        .unwrap();
    assert_eq!(repl_lease.state, semantic::LeaseState::Active);
}

#[test]
fn post_revoke_durability_and_cleanup_failure_retries_until_convergence() {
    #[derive(Default)]
    struct PostRevokeFailingJournal {
        fail_fence_advanced: std::sync::atomic::AtomicBool,
        fail_instance_terminated: std::sync::atomic::AtomicBool,
        records: std::sync::Mutex<Vec<RuntimeJournalRecord>>,
    }
    impl RuntimeJournalSink for PostRevokeFailingJournal {
        fn append(&self, record: RuntimeJournalRecord) -> Result<(), ProviderError> {
            if self
                .fail_fence_advanced
                .load(std::sync::atomic::Ordering::SeqCst)
                && record.event == RuntimeJournalEvent::FenceAdvanced
            {
                return Err(ProviderError::new(
                    "post-revoke-journal",
                    "JOURNAL_WRITE_FAILED",
                    "injected post-revoke FenceAdvanced write failure",
                ));
            }
            if self
                .fail_instance_terminated
                .load(std::sync::atomic::Ordering::SeqCst)
                && record.event == RuntimeJournalEvent::InstanceTerminated
            {
                return Err(ProviderError::new(
                    "post-revoke-journal",
                    "JOURNAL_WRITE_FAILED",
                    "injected post-revoke InstanceTerminated write failure",
                ));
            }
            self.records.lock().unwrap().push(record);
            Ok(())
        }
    }

    let journal = Arc::new(PostRevokeFailingJournal::default());
    let adapter = semantic_worker_adapter_with_resources(vec![test_resource()])
        .with_runtime_journal(journal.clone());
    let authority = adapter.authority();
    let context = AuthorityCallContext {
        contract: semantic::ContractRevision::current(),
        namespace: NamespaceId::default(),
        request_id: "test-post-revoke-fail".to_string(),
        idempotency_key: "test-post-revoke-fail".to_string(),
    };
    let principal = principal_from_peer_cred(&AUTHORITY_TEST_PEER);
    let worker_identity = semantic::Identity {
        id: "worker-post-revoke-fail".to_string(),
        generation: 1,
    };
    let query = semantic::ResourceQuery {
        resource_class: "accelerator".to_string(),
        count: 1,
        required_capabilities: vec![semantic::CapabilityRequirement {
            id: "accelerator.compute".to_string(),
            minimum_revision: 1,
            required_properties: BTreeMap::new(),
        }],
        minimum_capacity: BTreeMap::new(),
    };

    // 1. ACTIVE Lease + running Worker
    // 中文：1. ACTIVE Lease 与运行中的 Worker。
    let lease = authority
        .acquire_lease(
            &context,
            &principal,
            worker_identity.clone(),
            query.clone(),
            now_unix_ms().saturating_add(25),
        )
        .unwrap();

    let worker = semantic_worker_for(
        &worker_identity.id,
        semantic_provider("test-provider", 1, semantic::ProviderState::Ready).identity,
        lease.identity.clone(),
        semantic::WorkerState::Registered,
    );
    authority
        .start_worker(&context, &principal, worker.clone())
        .unwrap();

    let endpoint = semantic_endpoint_for(&worker);
    authority
        .publish_endpoint(&context, &principal, endpoint.clone())
        .unwrap();

    // 2. TTL expires
    // 中文：2. TTL 到期。
    thread::sleep(Duration::from_millis(40));

    // 3. Inject failure in FenceAdvanced (AFTER daemon.revoke has already succeeded)
    // 中文：3. 在 FenceAdvanced 处注入故障（此时 daemon.revoke 已成功）。
    journal
        .fail_fence_advanced
        .store(true, std::sync::atomic::Ordering::SeqCst);

    // Watchdog / enforce_lease_expiry runs:
    // WorkerLost succeeds -> daemon.revoke succeeds (Lease is now Revoked) -> FenceAdvanced fails!
    // 中文：运行 watchdog / enforce_lease_expiry：WorkerLost 成功 -> daemon.revoke 成功（Lease 已 Revoked）-> FenceAdvanced 失败！
    let _ = authority.enforce_lease_expiry();

    // 4. Verify lease in daemon is indeed REVOKED and fence advanced
    // 中文：4. 验证 daemon 中的 Lease 确实为 REVOKED，且 Fence 已前移。
    let revoked_lease = adapter.daemon.lease(&lease.identity.id).unwrap();
    assert_eq!(revoked_lease.state, cy_kernel_api::LeaseState::Revoked);
    assert!(revoked_lease.fence_token > lease.fence_token);

    // 5. Verify physical allocation remains held (fail-closed) because complete_revocation was not reached
    // 中文：5. 验证物理 allocation 仍被占用（fail-closed），因为尚未执行 complete_revocation。
    assert!(adapter.daemon.is_allocated(&lease.identity.id));
    let context_realloc = AuthorityCallContext {
        contract: semantic::ContractRevision::current(),
        namespace: NamespaceId::default(),
        request_id: "test-post-revoke-realloc".to_string(),
        idempotency_key: "test-post-revoke-realloc".to_string(),
    };
    let realloc_err = authority
        .acquire_lease(
            &context_realloc,
            &principal,
            semantic::Identity {
                id: "other-worker-post-revoke".to_string(),
                generation: 1,
            },
            query.clone(),
            now_unix_ms().saturating_add(10_000),
        )
        .unwrap_err();
    assert_eq!(realloc_err.reason_code, "INSUFFICIENT_RESOURCES");

    // 6. Verify authority operations remain rejected
    // 中文：6. 验证 authority 操作仍会被拒绝。
    let hb_err = authority
        .report_heartbeat(
            &context,
            &principal,
            &worker_identity,
            &lease.identity,
            revoked_lease.fence_token,
        )
        .unwrap_err();
    assert_eq!(hb_err.reason_code, "FENCE_MISMATCH");

    // 7. Verify watchdog continues to discover the REVOKED lease on subsequent cycles (retries)
    // Clear FenceAdvanced failure, but inject InstanceTerminated failure
    // 中文：7. 验证后续周期中的 watchdog 仍会发现该 REVOKED Lease 并重试。清除 FenceAdvanced 故障，但注入 InstanceTerminated 故障。
    journal
        .fail_fence_advanced
        .store(false, std::sync::atomic::Ordering::SeqCst);
    journal
        .fail_instance_terminated
        .store(true, std::sync::atomic::Ordering::SeqCst);

    let _ = authority.enforce_lease_expiry();
    // Allocation remains held because InstanceTerminated failed before complete_revocation
    // 中文：由于 InstanceTerminated 在 complete_revocation 前失败，allocation 仍被占用。
    assert!(adapter.daemon.is_allocated(&lease.identity.id));

    // 8. Clear all failures: next watchdog cycle finishes complete_revocation
    // 中文：8. 清除所有故障：下一轮 watchdog 将完成 complete_revocation。
    journal
        .fail_instance_terminated
        .store(false, std::sync::atomic::Ordering::SeqCst);

    let final_actions = authority.enforce_lease_expiry().unwrap();
    assert!(final_actions.iter().any(
        |a| matches!(a, ProviderReconcileAction::MarkWorkerLost(id) if id == &worker.identity)
    ));

    // Allocation is released!
    // 中文：allocation 已释放！
    assert!(!adapter.daemon.is_allocated(&lease.identity.id));

    // 9. Resource becomes reusable and replacement lease succeeds
    // 中文：9. Resource 现在可重新使用，并且替代 Lease 获取成功。
    let repl_lease = authority
        .acquire_lease(
            &context_realloc,
            &principal,
            semantic::Identity {
                id: "other-worker-post-revoke".to_string(),
                generation: 1,
            },
            query,
            u64::MAX,
        )
        .unwrap();
    assert_eq!(repl_lease.state, semantic::LeaseState::Active);
    assert!(repl_lease.fence_token > revoked_lease.fence_token);
}

// =========================================================================
// 中文：结束分隔线。
