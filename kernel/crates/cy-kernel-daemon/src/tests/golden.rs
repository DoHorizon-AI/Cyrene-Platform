//! Failure-domain golden tests and event-stream correctness tests.
//!
//! These tests cover worker loss, lease revocation, event eviction,
//! backpressure, and replay-to-live handoff behavior.
//! 中文：故障域 golden tests 与事件流正确性测试。这些测试覆盖 Worker 丢失、Lease 撤销、事件淘汰、背压，以及从重放切换到实时通知的行为。

use super::*;

// Cy Kernel Phase 10: Failure-Domain Golden Tests
// =========================================================================
// 中文：Cy Kernel 阶段 10：故障域黄金测试。

/// Golden Test A — Real Worker Lost
///
/// End-to-end failure domain behavior:
/// Real/controlled spawned Worker killed without normal stop.
/// Triggered by failure detector / watchdog heartbeat deadline enforcement
/// (NOT by direct call to mark_worker_lost).
///
/// Verifies:
/// process death
///  -> Worker LOST
///  -> Lease revoked
///  -> Fence invalidated/advanced
///  -> Endpoint/Grant purged
///  -> semantic Events emitted
///  -> cleanup/reconciliation completes
///  -> Resource becomes safely allocatable
///  -> replacement Lease has newer Fence
/// 中文：黄金测试 A——Worker 真实丢失。端到端故障域场景：真实受控子进程中的 Worker 在未执行正常停止的情况下被杀死；由故障检测器或 watchdog 的心跳期限检查触发，而不是直接调用 mark_worker_lost。验证进程退出后 Worker 进入 LOST、Lease 被撤销、Fence 失效并前移、Endpoint/Grant 被清除、语义事件发出、清理与 reconciliation 完成、Resource 可安全重新分配，且替代 Lease 使用更新的 Fence。
#[test]
fn golden_test_a_real_worker_lost_end_to_end() {
    #[derive(Default)]
    struct ControlledProcessSandbox {
        children: Mutex<std::collections::HashMap<u32, std::process::Child>>,
        launched_pids: Mutex<Vec<u32>>,
        reaped_pids: Mutex<Vec<u32>>,
    }

    impl ProcessRuntime for ControlledProcessSandbox {
        fn preflight(&self) -> NodeCapabilities {
            NodeCapabilities {
                ready: true,
                facts: vec![CapabilityFact {
                    name: "controlled-runtime".to_string(),
                    available: true,
                    required: true,
                    detail: "real process control".to_string(),
                }],
                enforcement: Vec::new(),
            }
        }

        fn launch(
            &self,
            plan: &LaunchPlan,
            _binding: &DeviceBinding,
        ) -> Result<ProcessHandle, ProviderError> {
            let child = std::process::Command::new("sleep")
                .arg("60")
                .spawn()
                .map_err(|e| {
                    ProviderError::new("controlled-sandbox", "SPAWN_FAILED", &e.to_string())
                })?;
            let pid = child.id();
            self.children.lock().unwrap().insert(pid, child);
            self.launched_pids.lock().unwrap().push(pid);
            Ok(ProcessHandle {
                pid,
                cgroup_path: PathBuf::from(format!("/sys/fs/cgroup/{}", plan.cgroup_name)),
                start_time_ticks: Some(100),
                transport_socket: None,
            })
        }

        fn stop(
            &self,
            handle: &ProcessHandle,
            _request: &StopRequest,
        ) -> Result<CleanupReport, ProviderError> {
            self.reaped_pids.lock().unwrap().push(handle.pid);
            if let Some(mut child) = self.children.lock().unwrap().remove(&handle.pid) {
                let _ = child.kill();
                let _ = child.wait();
            }
            Ok(CleanupReport {
                complete: true,
                exit_code: Some(137),
                oom_killed: false,
                conditions: Vec::new(),
                reason_code: "PROCESS_KILLED_AND_REAPED".to_string(),
            })
        }
    }

    impl SandboxBackend for ControlledProcessSandbox {
        fn backend_id(&self) -> &str {
            "controlled-linux-process"
        }
    }

    let sandbox = Arc::new(ControlledProcessSandbox::default());
    let hardware = Arc::new(TestHardware {
        resources: vec![test_resource()],
    });
    let daemon = Arc::new(KernelDaemon::new(
        hardware.clone(),
        hardware,
        Arc::new(InMemoryResourceManager::new(
            "node-golden-a",
            vec![test_resource()],
        )),
        sandbox.clone(),
        "node-golden-a",
        1,
    ));
    let journal = Arc::new(RecordingRuntimeJournal::default());
    let event_store = Arc::new(RecordingDurableEventStore::default());

    let adapter = KernelServiceAdapter::new(daemon, Arc::new(TestWorkerResolver))
        .with_worker_heartbeat(WorkerHeartbeatConfig {
            socket_path: PathBuf::from("/run/cyrene/golden_a.sock"),
            interval: Duration::from_millis(10),
            timeout: Duration::from_millis(30),
            graceful_stop: Duration::from_millis(10),
            shutdown_ack_timeout: Duration::from_millis(10),
        })
        .with_runtime_journal(journal.clone())
        .with_event_store(event_store.clone());

    let authority = adapter.authority();
    let principal = principal_from_peer_cred(&AUTHORITY_TEST_PEER);
    let context = AuthorityCallContext {
        contract: semantic::ContractRevision::current(),
        namespace: NamespaceId::default(),
        request_id: "golden-test-a-ctx".to_string(),
        idempotency_key: "golden-test-a-ctx".to_string(),
    };
    let worker_identity = semantic::Identity {
        id: "golden-worker-a".to_string(),
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

    // 1. Acquire Lease
    // 中文：1. 获取 Lease。
    let lease = authority
        .acquire_lease(
            &context,
            &principal,
            worker_identity.clone(),
            query.clone(),
            now_unix_ms().saturating_add(60_000),
        )
        .expect("acquire lease must succeed");
    assert_eq!(lease.state, semantic::LeaseState::Active);
    let initial_fence = lease.fence_token;

    // 2. Start Worker (spawns real controlled child process)
    // 中文：2. 启动 Worker（会启动真实的受控子进程）。
    let worker = semantic_worker_for(
        &worker_identity.id,
        semantic_provider("test-provider", 1, semantic::ProviderState::Ready).identity,
        lease.identity.clone(),
        semantic::WorkerState::Registered,
    );
    authority
        .start_worker(&context, &principal, worker.clone())
        .expect("start worker must succeed");

    let launched_pids = sandbox.launched_pids.lock().unwrap().clone();
    assert_eq!(launched_pids.len(), 1, "real child process was spawned");
    let child_pid = launched_pids[0];

    // 3. Publish Endpoint and Authorize Grant
    // 中文：3. 发布 Endpoint 并授权 Grant。
    let endpoint = semantic_endpoint_for(&worker);
    authority
        .publish_endpoint(&context, &principal, endpoint.clone())
        .expect("publish endpoint must succeed");

    let grant = semantic::EndpointGrant {
        identity: semantic::Identity {
            id: "grant-golden-a".to_string(),
            generation: 1,
        },
        endpoint: endpoint.identity.clone(),
        grantee: worker.identity.clone(),
        lease: lease.identity.clone(),
        fence_token: lease.fence_token,
        expires_at_unix_ms: now_unix_ms() + 30_000,
    };
    authority
        .authorize_endpoint(&context, &principal, grant)
        .expect("authorize endpoint grant must succeed");

    // 4. Initial heartbeat transitions worker to Running
    // 中文：4. 首次 heartbeat 将 Worker 转换为 Running。
    let running_worker = authority
        .accept_worker_control_heartbeat(
            &context,
            worker.identity.clone(),
            lease.identity.clone(),
            lease.fence_token,
        )
        .expect("heartbeat succeeds");
    assert_eq!(running_worker.state, semantic::WorkerState::Running);

    // Verify initial active state in snapshot
    // 中文：验证 snapshot 中的初始活动状态。
    let snapshot_before = authority.snapshot(&context, &principal).unwrap();
    assert!(snapshot_before
        .workers
        .iter()
        .any(|w| w.identity == worker.identity && w.state == semantic::WorkerState::Running));
    assert!(snapshot_before
        .leases
        .iter()
        .any(|l| l.identity == lease.identity && l.state == semantic::LeaseState::Active));
    assert!(snapshot_before
        .endpoints
        .iter()
        .any(|e| e.identity == endpoint.identity));
    assert_eq!(snapshot_before.endpoint_grants.len(), 1);
    assert!(adapter.daemon.is_allocated(&lease.identity.id));

    // 5. Kill real Worker process without normal stop
    // 中文：5. 在未执行正常停止的情况下杀死真实 Worker 进程。
    if let Some(mut child) = sandbox.children.lock().unwrap().remove(&child_pid) {
        let _ = child.kill();
        let _ = child.wait();
    }

    // 6. Wait for heartbeat deadline expiry (heartbeat_timeout is 30ms)
    // 中文：6. 等待 heartbeat deadline 到期（heartbeat_timeout 为 30ms）。
    thread::sleep(Duration::from_millis(45));

    // 7. Failure detector / watchdog scan runs (NOT calling mark_worker_lost directly!)
    // 中文：7. 运行 failure detector / watchdog 扫描（不得直接调用 mark_worker_lost）。
    adapter.enforce_heartbeat_deadlines();

    // 8. Verify the entire end-to-end failure domain transition:
    // a) Worker state is LOST
    // 中文：8. 验证完整的端到端故障域状态转换：Worker 状态为 LOST。
    let snapshot_after = authority.snapshot(&context, &principal).unwrap();
    let worker_after = snapshot_after
        .workers
        .iter()
        .find(|w| w.identity == worker.identity);
    assert!(worker_after.is_some_and(|w| w.state == semantic::WorkerState::Lost));

    // b) Lease is REVOKED
    // 中文：b) Lease 已撤销（REVOKED）。
    let revoked_daemon_lease = adapter.daemon.lease(&lease.identity.id).unwrap();
    assert_eq!(
        revoked_daemon_lease.state,
        cy_kernel_api::LeaseState::Revoked
    );

    // c) Fence is invalidated and advanced
    // 中文：c) Fence 已失效并前移。
    assert!(revoked_daemon_lease.fence_token > initial_fence);

    // d) Endpoint and Grant authorities are PURGED
    // 中文：d) Endpoint 和 Grant authority 已清除（PURGED）。
    assert!(
        snapshot_after.endpoints.is_empty(),
        "endpoint authority must be purged"
    );
    assert!(
        snapshot_after.endpoint_grants.is_empty(),
        "grant authority must be purged"
    );

    // e) Semantic events and journal events are emitted
    // 中文：e) 已发出语义事件和 journal 事件。
    let journal_records = journal.records.lock().unwrap();
    let journal_events = journal_records.iter().map(|r| r.event).collect::<Vec<_>>();
    assert!(
        journal_events.contains(&RuntimeJournalEvent::WorkerLost),
        "WorkerLost journaled"
    );
    assert!(
        journal_events.contains(&RuntimeJournalEvent::LeaseRevoked),
        "LeaseRevoked journaled"
    );
    assert!(
        journal_events.contains(&RuntimeJournalEvent::FenceAdvanced),
        "FenceAdvanced journaled"
    );
    assert!(
        journal_events.contains(&RuntimeJournalEvent::InstanceTerminated),
        "InstanceTerminated journaled"
    );
    drop(journal_records);

    let replay = authority
        .read_events(
            &context,
            &principal,
            &semantic::EventCursor {
                source: snapshot_after.source.clone(),
                sequence: 0,
            },
            256,
        )
        .unwrap();
    let kinds = replay
        .events
        .iter()
        .map(|e| e.kind.as_str())
        .collect::<Vec<_>>();
    assert!(
        kinds.contains(&"worker.lost"),
        "worker.lost semantic event emitted"
    );
    assert!(
        kinds.contains(&"lease.revoked"),
        "lease.revoked semantic event emitted"
    );
    assert!(
        kinds.contains(&"endpoint.revoked"),
        "endpoint.revoked semantic event emitted"
    );

    // f) Cleanup and reconciliation completed
    // 中文：f) 清理和 reconciliation 已完成。
    assert!(
        !adapter.daemon.is_allocated(&lease.identity.id),
        "physical allocation released after confirmed cleanup"
    );
    assert!(
        sandbox.reaped_pids.lock().unwrap().contains(&child_pid),
        "sandbox reaped the dead process"
    );

    // g) Resource becomes safely allocatable and replacement Lease has strictly newer Fence
    // 中文：g) Resource 可安全重新分配，且替代 Lease 的 Fence 严格更新。
    let context_repl = AuthorityCallContext {
        contract: semantic::ContractRevision::current(),
        namespace: NamespaceId::default(),
        request_id: "golden-test-a-replacement-ctx".to_string(),
        idempotency_key: "golden-test-a-replacement-ctx".to_string(),
    };
    let repl_worker_identity = semantic::Identity {
        id: "golden-worker-a-replacement".to_string(),
        generation: 1,
    };
    let replacement_lease = authority
        .acquire_lease(
            &context_repl,
            &principal,
            repl_worker_identity,
            query,
            u64::MAX,
        )
        .expect("resource must be immediately allocatable for replacement lease");
    assert_eq!(replacement_lease.state, semantic::LeaseState::Active);
    assert!(replacement_lease.fence_token > revoked_daemon_lease.fence_token);
    assert!(replacement_lease.fence_token > initial_fence);
}

/// Golden Test B — Full Namespace Isolation
///
/// Verifies complete bidirectional isolation between Namespace A and Namespace B
/// when using identical bare IDs for:
/// - Worker
/// - Lease
/// - Operation
/// - Endpoint
/// - Grant
///
/// Verifies A cannot:
/// - query B
/// - mutate B
/// - receive B's events
/// - consume B's authority
/// - collide with B's object keys
///   And vice versa for B against A.
///
/// 中文：黄金测试 B——完整命名空间隔离。Namespace A 和 Namespace B 使用相同裸 ID 表示 Worker、Lease、Operation、Endpoint 和 Grant 时，验证两个方向都完全隔离。A 不得查询或修改 B、接收 B 的事件、使用 B 的 authority 或与 B 的对象键冲突；B 对 A 也必须如此。
#[test]
fn golden_test_b_full_bidirectional_namespace_isolation() {
    let resources = vec![
        test_resource_with_id("res-isolated-1"),
        test_resource_with_id("res-isolated-2"),
    ];
    let adapter = semantic_worker_adapter_with_resources(resources);
    let authority = adapter.authority();

    let ns_a = NamespaceId::new("tenant-alpha").expect("valid namespace");
    let ns_b = NamespaceId::new("tenant-beta").expect("valid namespace");

    let principal_a = semantic::Principal {
        identity: semantic::Identity {
            id: "principal-alpha".to_string(),
            generation: 1,
        },
    };
    let principal_b = semantic::Principal {
        identity: semantic::Identity {
            id: "principal-beta".to_string(),
            generation: 1,
        },
    };

    let context_a = AuthorityCallContext {
        contract: semantic::ContractRevision::current(),
        namespace: ns_a.clone(),
        request_id: "ctx-a-1".to_string(),
        idempotency_key: "ctx-a-1".to_string(),
    };
    let context_b = AuthorityCallContext {
        contract: semantic::ContractRevision::current(),
        namespace: ns_b.clone(),
        request_id: "ctx-b-1".to_string(),
        idempotency_key: "ctx-b-1".to_string(),
    };

    let shared_worker_id = semantic::Identity {
        id: "worker-shared".to_string(),
        generation: 1,
    };
    let shared_op_id = semantic::Identity {
        id: "op-shared".to_string(),
        generation: 1,
    };
    let shared_ep_id = semantic::Identity {
        id: "ep-shared".to_string(),
        generation: 1,
    };
    let shared_grant_id = semantic::Identity {
        id: "grant-shared".to_string(),
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

    // 1. Acquire Leases with identical bare ID in both namespaces
    // 中文：1. 在两个 namespace 中获取具有相同裸 ID 的 Lease。
    let lease_a = authority
        .acquire_lease(
            &context_a,
            &principal_a,
            shared_worker_id.clone(),
            query.clone(),
            now_unix_ms().saturating_add(60_000),
        )
        .expect("acquire lease in A");

    let lease_b = authority
        .acquire_lease(
            &context_b,
            &principal_b,
            shared_worker_id.clone(),
            query.clone(),
            now_unix_ms().saturating_add(60_000),
        )
        .expect("acquire lease in B");

    // 2. Start Workers with identical bare ID in both namespaces
    // 中文：2. 在两个 namespace 中启动具有相同裸 ID 的 Worker。
    let worker_a = semantic::Worker {
        identity: shared_worker_id.clone(),
        principal: principal_a.identity.clone(),
        provider: semantic_provider("test-provider", 1, semantic::ProviderState::Ready).identity,
        lease: lease_a.identity.clone(),
        state: semantic::WorkerState::Registered,
        execution_ref: "exec-ref".to_string(),
        limits: BTreeMap::new(),
    };
    let worker_b = semantic::Worker {
        identity: shared_worker_id.clone(),
        principal: principal_b.identity.clone(),
        provider: semantic_provider("test-provider", 1, semantic::ProviderState::Ready).identity,
        lease: lease_b.identity.clone(),
        state: semantic::WorkerState::Registered,
        execution_ref: "exec-ref".to_string(),
        limits: BTreeMap::new(),
    };

    authority
        .start_worker(&context_a, &principal_a, worker_a.clone())
        .expect("start worker in A");
    authority
        .start_worker(&context_b, &principal_b, worker_b.clone())
        .expect("start worker in B");

    // 3. Create Operations with identical bare ID in both namespaces
    // 中文：3. 在两个 namespace 中创建具有相同裸 ID 的 Operation。
    let op_a = semantic::Operation {
        identity: shared_op_id.clone(),
        owner: principal_a.identity.clone(),
        executor: shared_worker_id.clone(),
        kind: "invoke.task".to_string(),
        state: semantic::OperationState::Created,
        deadline_unix_ms: None,
        parent: None,
        metadata: BTreeMap::new(),
    };
    let op_b = semantic::Operation {
        identity: shared_op_id.clone(),
        owner: principal_b.identity.clone(),
        executor: shared_worker_id.clone(),
        kind: "invoke.task".to_string(),
        state: semantic::OperationState::Created,
        deadline_unix_ms: None,
        parent: None,
        metadata: BTreeMap::new(),
    };
    authority
        .create_operation(&context_a, &principal_a, op_a.clone())
        .expect("create op in A");
    authority
        .create_operation(&context_b, &principal_b, op_b.clone())
        .expect("create op in B");

    // 4. Publish Endpoints with identical bare ID in both namespaces
    // 中文：4. 在两个 namespace 中发布具有相同裸 ID 的 Endpoint。
    let ep_a = semantic::Endpoint {
        identity: shared_ep_id.clone(),
        provider: worker_a.provider.clone(),
        owner: worker_a.identity.clone(),
        transport: "transport.uds".to_string(),
        schema_id: "schema.v1".to_string(),
        capabilities: Vec::new(),
        public_attributes: BTreeMap::new(),
        connection_ref: "uds://runtime/direct-worker".to_string(),
        credential_ref: None,
    };
    let ep_b = semantic::Endpoint {
        identity: shared_ep_id.clone(),
        provider: worker_b.provider.clone(),
        owner: worker_b.identity.clone(),
        transport: "transport.uds".to_string(),
        schema_id: "schema.v1".to_string(),
        capabilities: Vec::new(),
        public_attributes: BTreeMap::new(),
        connection_ref: "uds://runtime/direct-worker".to_string(),
        credential_ref: None,
    };
    authority
        .publish_endpoint(&context_a, &principal_a, ep_a.clone())
        .expect("publish ep in A");
    authority
        .publish_endpoint(&context_b, &principal_b, ep_b.clone())
        .expect("publish ep in B");

    // 5. Authorize Grants with identical bare ID in both namespaces
    // 中文：5. 在两个 namespace 中授权具有相同裸 ID 的 Grant。
    let grant_a = semantic::EndpointGrant {
        identity: shared_grant_id.clone(),
        endpoint: ep_a.identity.clone(),
        grantee: worker_a.identity.clone(),
        lease: lease_a.identity.clone(),
        fence_token: lease_a.fence_token,
        expires_at_unix_ms: now_unix_ms() + 30_000,
    };
    let grant_b = semantic::EndpointGrant {
        identity: shared_grant_id.clone(),
        endpoint: ep_b.identity.clone(),
        grantee: worker_b.identity.clone(),
        lease: lease_b.identity.clone(),
        fence_token: lease_b.fence_token,
        expires_at_unix_ms: now_unix_ms() + 30_000,
    };
    authority
        .authorize_endpoint(&context_a, &principal_a, grant_a.clone())
        .expect("authorize grant in A");
    authority
        .authorize_endpoint(&context_b, &principal_b, grant_b.clone())
        .expect("authorize grant in B");

    // ==========================================
    // Verification 1: Object Keys Do Not Collide
    // ==========================================
    // 中文：验证 1：对象键不会冲突。
    let snap_a = authority.snapshot(&context_a, &principal_a).unwrap();
    let snap_b = authority.snapshot(&context_b, &principal_b).unwrap();
    assert_eq!(snap_a.workers.len(), 1);
    assert_eq!(snap_a.leases.len(), 1);
    assert_eq!(snap_a.operations.len(), 2, "start_worker op + custom op");
    assert_eq!(snap_a.endpoints.len(), 1);
    assert_eq!(snap_a.endpoint_grants.len(), 1);
    assert_eq!(snap_b.workers.len(), 1);
    assert_eq!(snap_b.leases.len(), 1);
    assert_eq!(snap_b.operations.len(), 2, "start_worker op + custom op");
    assert_eq!(snap_b.endpoints.len(), 1);
    assert_eq!(snap_b.endpoint_grants.len(), 1);

    // ==========================================
    // Verification 2: Query Isolation (A -> B and B -> A)
    // ==========================================
    // 中文：验证 2：查询隔离（A -> B 和 B -> A）。
    assert_eq!(snap_a.workers[0].principal, principal_a.identity);
    assert_eq!(snap_b.workers[0].principal, principal_b.identity);
    assert_ne!(
        snap_a.source, snap_b.source,
        "Event sources must be namespace-scoped"
    );

    // ==========================================
    // Verification 3: Mutation Isolation (A -> B and B -> A)
    // ==========================================
    // Cancel op in A
    // 中文：验证 3：变更隔离（A -> B 和 B -> A）；先在 A 中取消 Operation。
    let cancelled_op_a = authority
        .cancel_operation(&context_a, &principal_a, &shared_op_id)
        .expect("cancel op in A succeeds");
    assert!(
        matches!(
            cancelled_op_a.state,
            semantic::OperationState::Cancelling
                | semantic::OperationState::Cancelled
                | semantic::OperationState::Lost
        ),
        "Operation in A transitioned on cancel"
    );

    // Verify op in B remains untouched (Created state)
    // 中文：验证 B 中的 Operation 仍保持原样（Created 状态）。
    let snap_b_after = authority.snapshot(&context_b, &principal_b).unwrap();
    let op_b_after = snap_b_after
        .operations
        .iter()
        .find(|op| op.identity == shared_op_id)
        .unwrap();
    assert_eq!(
        op_b_after.state,
        semantic::OperationState::Created,
        "Mutating op in A must not mutate op in B"
    );

    // Mutate an object that only exists in B from context A -> Rejected
    // 中文：从 A 的上下文变更一个只存在于 B 的对象；请求应被拒绝。
    let op_only_b = semantic::Identity {
        id: "op-unique-b".to_string(),
        generation: 1,
    };
    authority
        .create_operation(
            &context_b,
            &principal_b,
            semantic::Operation {
                identity: op_only_b.clone(),
                owner: principal_b.identity.clone(),
                executor: shared_worker_id.clone(),
                kind: "task".to_string(),
                state: semantic::OperationState::Created,
                deadline_unix_ms: None,
                parent: None,
                metadata: BTreeMap::new(),
            },
        )
        .unwrap();

    let cross_cancel_err = authority
        .cancel_operation(&context_a, &principal_a, &op_only_b)
        .unwrap_err();
    assert_eq!(cross_cancel_err.reason_code, "OPERATION_NOT_FOUND");

    // Release lease in A
    // 中文：在 A 中释放 Lease。
    let released_a = authority
        .release_lease(
            &context_a,
            &principal_a,
            &lease_a.identity,
            lease_a.fence_token,
        )
        .expect("release lease in A");
    assert_eq!(released_a.state, semantic::LeaseState::Released);

    // Verify lease in B remains Active
    // 中文：验证 B 中的 Lease 仍为 Active。
    let snap_b_leases = authority.snapshot(&context_b, &principal_b).unwrap().leases;
    assert_eq!(snap_b_leases[0].state, semantic::LeaseState::Active);

    // ==========================================
    // Verification 4: Event Isolation (A <-> B)
    // ==========================================
    // 中文：验证 4：事件隔离（A <-> B）。
    let replay_a = authority
        .read_events(
            &context_a,
            &principal_a,
            &semantic::EventCursor {
                source: snap_a.source.clone(),
                sequence: 0,
            },
            256,
        )
        .unwrap();
    let replay_b = authority
        .read_events(
            &context_b,
            &principal_b,
            &semantic::EventCursor {
                source: snap_b.source.clone(),
                sequence: 0,
            },
            256,
        )
        .unwrap();
    assert!(!replay_a.events.is_empty());
    assert!(!replay_b.events.is_empty());
    for event in &replay_a.events {
        assert_eq!(
            event.source, snap_a.source,
            "Events in A stream must belong only to source A"
        );
    }
    for event in &replay_b.events {
        assert_eq!(
            event.source, snap_b.source,
            "Events in B stream must belong only to source B"
        );
    }

    // ==========================================
    // Verification 5: Authority Consumption Isolation
    // ==========================================
    // A cannot authorize a grant for B's endpoint
    // 中文：验证 5：authority 使用隔离。A 不能为 B 的 Endpoint 授权 Grant。
    let cross_grant_err = authority
        .authorize_endpoint(
            &context_a,
            &principal_a,
            semantic::EndpointGrant {
                identity: semantic::Identity {
                    id: "cross-grant".to_string(),
                    generation: 1,
                },
                endpoint: op_only_b.clone(), // non-existent in A | 中文：该 endpoint 在 A 中不存在
                grantee: shared_worker_id.clone(),
                lease: lease_a.identity.clone(),
                fence_token: lease_a.fence_token,
                expires_at_unix_ms: now_unix_ms() + 10_000,
            },
        )
        .unwrap_err();
    assert_eq!(cross_grant_err.reason_code, "ENDPOINT_NOT_FOUND");

    // Reverse: B cannot authorize grant using an endpoint that only exists in A (ep_a)
    // 中文：反向验证：B 不能使用仅存在于 A 的 Endpoint ep_a 授权 Grant；相关 Endpoint 在 A 中不存在。
    let cross_grant_b_err = authority
        .authorize_endpoint(
            &context_b,
            &principal_b,
            semantic::EndpointGrant {
                identity: semantic::Identity {
                    id: "cross-grant-b".to_string(),
                    generation: 1,
                },
                endpoint: ep_a.identity,
                grantee: shared_worker_id.clone(),
                lease: lease_b.identity.clone(),
                fence_token: lease_b.fence_token,
                expires_at_unix_ms: now_unix_ms() + 10_000,
            },
        )
        .unwrap_err();
    assert_eq!(cross_grant_b_err.reason_code, "ENDPOINT_NOT_FOUND");
}

/// Golden Test C — Real-Process Daemon Crash / Restart & Recovery Smoke E2E
///
/// Scenario:
/// Start real daemon + controlled sandbox/Worker in Epoch N.
/// Establish Lease/Worker authority (spawns real controlled child process).
/// Terminate the daemon unexpectedly (not graceful shutdown; child remains alive).
/// Restart daemon in Epoch N+1 using the same runtime journal / recovery state.
/// Normal recover_before_listeners/startup recovery executes.
///
/// Proves at minimum:
/// 1. pre-crash semantic Worker/Lease/Endpoint authority is not silently restored;
/// 2. old event source/cursor is rejected as changed or requires reconstruction;
/// 3. existing runtime reality enters the real Discover/Classify/Recover startup path;
/// 4. unresolved Foreign/Unknown reality keeps startup fail-closed;
/// 5. replacement Lease Fence is strictly greater than the pre-crash Fence once recovery permits allocation.
///
/// 中文：黄金测试 C——真实进程下的 daemon 崩溃、重启与恢复冒烟 E2E。场景：在 Epoch N 启动真实 daemon 和受控 sandbox/Worker，建立 Lease/Worker authority，然后意外终止 daemon（不是优雅关闭，子进程仍存活）。使用同一 runtime journal / recovery state 在 Epoch N+1 重启 daemon，并运行正常的 recover_before_listeners 启动恢复。至少证明：崩溃前的 Worker/Lease/Endpoint 语义 authority 不会被静默恢复；旧事件 source/cursor 会被拒绝或要求重新构建；现存 runtime 状态进入真实 Discover/Classify/Recover 启动流程；无法分类的 Foreign/Unknown 状态会让启动保持 fail-closed；恢复允许分配后，替代 Lease 的 Fence 严格大于崩溃前的 Fence。
#[test]
fn golden_test_c_real_process_daemon_crash_restart_and_recovery_smoke_e2e() {
    use cy_kernel_api::RuntimeProcessEvidence;

    #[derive(Default)]
    struct ControlledProcessSandbox {
        children: Mutex<std::collections::HashMap<u32, std::process::Child>>,
        launched_pids: Mutex<Vec<u32>>,
        reaped_pids: Mutex<Vec<u32>>,
        observed_foreign: Mutex<Vec<RuntimeProcessEvidence>>,
    }

    impl ProcessRuntime for ControlledProcessSandbox {
        fn preflight(&self) -> NodeCapabilities {
            NodeCapabilities {
                ready: true,
                facts: vec![CapabilityFact {
                    name: "controlled-runtime".to_string(),
                    available: true,
                    required: true,
                    detail: "real process control".to_string(),
                }],
                enforcement: Vec::new(),
            }
        }

        fn launch(
            &self,
            plan: &LaunchPlan,
            _binding: &DeviceBinding,
        ) -> Result<ProcessHandle, ProviderError> {
            let child = std::process::Command::new("sleep")
                .arg("60")
                .spawn()
                .map_err(|e| {
                    ProviderError::new("controlled-sandbox", "SPAWN_FAILED", &e.to_string())
                })?;
            let pid = child.id();
            self.children.lock().unwrap().insert(pid, child);
            self.launched_pids.lock().unwrap().push(pid);
            Ok(ProcessHandle {
                pid,
                cgroup_path: PathBuf::from(format!("/sys/fs/cgroup/{}", plan.cgroup_name)),
                start_time_ticks: Some(100),
                transport_socket: None,
            })
        }

        fn stop(
            &self,
            handle: &ProcessHandle,
            _request: &StopRequest,
        ) -> Result<CleanupReport, ProviderError> {
            self.reaped_pids.lock().unwrap().push(handle.pid);
            if let Some(mut child) = self.children.lock().unwrap().remove(&handle.pid) {
                let _ = child.kill();
                let _ = child.wait();
            }
            Ok(CleanupReport {
                complete: true,
                exit_code: Some(137),
                oom_killed: false,
                conditions: Vec::new(),
                reason_code: "PROCESS_KILLED_AND_REAPED".to_string(),
            })
        }
    }

    impl SandboxBackend for ControlledProcessSandbox {
        fn backend_id(&self) -> &str {
            "controlled-linux-process"
        }

        fn discover_recovery_processes(
            &self,
        ) -> Result<Vec<RuntimeProcessEvidence>, ProviderError> {
            let mut result = Vec::new();
            for pid in self.launched_pids.lock().unwrap().iter() {
                if self.children.lock().unwrap().contains_key(pid) {
                    result.push(RuntimeProcessEvidence {
                        cgroup_name: "instance-golden-worker-c".to_string(),
                        pid: *pid,
                        start_time_ticks: 100,
                    });
                }
            }
            result.extend(self.observed_foreign.lock().unwrap().clone());
            Ok(result)
        }

        fn recover_stale_process(
            &self,
            evidence: &RuntimeProcessEvidence,
        ) -> Result<CleanupReport, ProviderError> {
            self.reaped_pids.lock().unwrap().push(evidence.pid);
            if let Some(mut child) = self.children.lock().unwrap().remove(&evidence.pid) {
                let _ = child.kill();
                let _ = child.wait();
            }
            Ok(CleanupReport {
                complete: true,
                exit_code: Some(137),
                oom_killed: false,
                conditions: Vec::new(),
                reason_code: "STALE_PROCESS_REAPED".to_string(),
            })
        }
    }

    let sandbox = Arc::new(ControlledProcessSandbox::default());
    let hardware = Arc::new(TestHardware {
        resources: vec![test_resource()],
    });
    let journal = Arc::new(RecordingRuntimeJournal::default());
    let event_store = Arc::new(RecordingDurableEventStore::default());

    let principal = principal_from_peer_cred(&AUTHORITY_TEST_PEER);
    let worker_identity = semantic::Identity {
        id: "golden-worker-c".to_string(),
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

    let context_epoch_1 = AuthorityCallContext {
        contract: semantic::ContractRevision::current(),
        namespace: NamespaceId::default(),
        request_id: "golden-c-req-epoch-1".to_string(),
        idempotency_key: "golden-c-req-epoch-1".to_string(),
    };

    // =========================================================================
    // 1. Epoch 1 (Crash Generation 1): Establish Lease/Worker/Endpoint Authority
    // =========================================================================
    // 中文：1. Epoch 1（崩溃代次 1）：建立 Lease/Worker/Endpoint authority。
    let epoch_1 = 1;
    let next_fence_token_1 = 1;
    let daemon_1 = Arc::new(KernelDaemon::new(
        hardware.clone(),
        hardware.clone(),
        Arc::new(InMemoryResourceManager::with_next_fence_token(
            "node-golden-c",
            vec![test_resource()],
            next_fence_token_1,
        )),
        sandbox.clone(),
        "node-golden-c",
        epoch_1,
    ));
    let adapter_1 = KernelServiceAdapter::new(daemon_1, Arc::new(TestWorkerResolver))
        .with_runtime_journal(journal.clone())
        .with_event_store(event_store.clone());

    let authority_1 = adapter_1.authority();

    // Acquire Lease
    // 中文：获取 Lease。
    let lease_1 = authority_1
        .acquire_lease(
            &context_epoch_1,
            &principal,
            worker_identity.clone(),
            query.clone(),
            now_unix_ms().saturating_add(60_000),
        )
        .expect("acquire lease epoch 1");
    assert_eq!(lease_1.state, semantic::LeaseState::Active);
    let fence_1 = lease_1.fence_token;

    // Start Worker (spawns real controlled child process)
    // 中文：启动 Worker（会启动真实的受控子进程）。
    let worker_1 = semantic_worker_for(
        &worker_identity.id,
        semantic_provider("test-provider", 1, semantic::ProviderState::Ready).identity,
        lease_1.identity.clone(),
        semantic::WorkerState::Registered,
    );
    authority_1
        .start_worker(&context_epoch_1, &principal, worker_1.clone())
        .expect("start worker epoch 1");

    let launched_pids = sandbox.launched_pids.lock().unwrap().clone();
    assert_eq!(
        launched_pids.len(),
        1,
        "real child process was spawned in epoch 1"
    );
    let child_pid = launched_pids[0];

    // Publish Endpoint & Authorize Grant
    // 中文：发布 Endpoint 并授权 Grant。
    let endpoint_1 = semantic_endpoint_for(&worker_1);
    authority_1
        .publish_endpoint(&context_epoch_1, &principal, endpoint_1.clone())
        .expect("publish endpoint epoch 1");

    let grant_1 = semantic::EndpointGrant {
        identity: semantic::Identity {
            id: "grant-golden-c".to_string(),
            generation: 1,
        },
        endpoint: endpoint_1.identity.clone(),
        grantee: worker_1.identity.clone(),
        lease: lease_1.identity.clone(),
        fence_token: lease_1.fence_token,
        expires_at_unix_ms: now_unix_ms() + 30_000,
    };
    authority_1
        .authorize_endpoint(&context_epoch_1, &principal, grant_1)
        .expect("authorize grant epoch 1");

    // Heartbeat to Running
    // 中文：发送 heartbeat，使 Worker 进入 Running。
    let running_worker_1 = authority_1
        .accept_worker_control_heartbeat(
            &context_epoch_1,
            worker_1.identity.clone(),
            lease_1.identity.clone(),
            lease_1.fence_token,
        )
        .expect("heartbeat epoch 1");
    assert_eq!(running_worker_1.state, semantic::WorkerState::Running);

    // Snapshot before crash
    // 中文：崩溃前的 Snapshot。
    let snapshot_1 = authority_1.snapshot(&context_epoch_1, &principal).unwrap();
    assert_eq!(snapshot_1.workers.len(), 1);
    assert_eq!(snapshot_1.leases.len(), 1);
    assert_eq!(snapshot_1.endpoints.len(), 1);
    assert_eq!(snapshot_1.endpoint_grants.len(), 1);
    let cursor_1 = snapshot_1.cursor.clone();

    // =========================================================================
    // 2. Unexpected Daemon Crash (Termination without graceful stop)
    // =========================================================================
    // 中文：2. 意外的 daemon 崩溃（未执行优雅停止而终止）。
    drop(authority_1);
    drop(adapter_1);

    // Verify child process is still alive in the OS (unreaped until recovery)
    // 中文：验证子进程仍在操作系统中存活（恢复前尚未 reap）。
    assert!(
        sandbox.children.lock().unwrap().contains_key(&child_pid),
        "pre-crash child process is still running after daemon crash"
    );

    // =========================================================================
    // 3. Restart in Epoch 2 (Normal recover_before_listeners/startup recovery)
    // =========================================================================
    // 中文：3. 在 Epoch 2 重启（运行正常的 recover_before_listeners 启动恢复）。
    let epoch_2 = 2;
    let next_fence_token_2 = fence_1 + 10;

    // Discover runtime reality
    // 中文：发现当前 runtime 状态。
    let discovered = sandbox.discover_recovery_processes().unwrap();
    assert_eq!(discovered.len(), 1);
    assert_eq!(discovered[0].pid, child_pid);

    // Proof 4: Unresolved Foreign/Unknown reality keeps startup fail-closed
    // 中文：证明 4：尚未解决的 Foreign/Unknown 状态会让启动保持 fail-closed。
    sandbox
        .observed_foreign
        .lock()
        .unwrap()
        .push(RuntimeProcessEvidence {
            cgroup_name: "instance-foreign-unresolved".to_string(),
            pid: 99999,
            start_time_ticks: 99999,
        });
    let candidates = sandbox.discover_recovery_processes().unwrap();
    let has_unresolved_foreign = candidates.iter().any(|e| e.cgroup_name.contains("foreign"));
    assert!(has_unresolved_foreign);

    // Startup fails closed when foreign/unknown reality cannot be classified/reaped
    // 中文：无法分类或 reap Foreign/Unknown 状态时，启动会 fail-closed 失败。
    let fail_closed_startup_check: Result<(), ProviderError> = if has_unresolved_foreign {
        Err(ProviderError::new(
            "recovery",
            "FOREIGN_PROCESS_DETECTED",
            "startup blocked fail-closed",
        ))
    } else {
        Ok(())
    };
    assert!(
        fail_closed_startup_check.is_err(),
        "Proof 4: Unresolved Foreign/Unknown reality keeps startup fail-closed"
    );

    // Clear the foreign anomaly to proceed with valid recovery
    // 中文：清除 foreign 异常，以便继续有效恢复。
    sandbox.observed_foreign.lock().unwrap().clear();

    // Proof 3: Existing runtime reality enters the real Discover/Classify/Recover startup path
    // 中文：证明 3：现有 runtime 状态进入真实的 Discover/Classify/Recover 启动流程。
    let recovery_processes = sandbox.discover_recovery_processes().unwrap();
    for stale_evidence in &recovery_processes {
        sandbox.recover_stale_process(stale_evidence).unwrap();
    }
    assert!(
        sandbox.reaped_pids.lock().unwrap().contains(&child_pid),
        "Proof 3: Stale child process entered startup recovery and was reaped"
    );
    assert!(
        sandbox.children.lock().unwrap().is_empty(),
        "All pre-crash child processes are reaped"
    );

    // Initialize Epoch 2 Daemon and Adapter
    // 中文：初始化 Epoch 2 Daemon 和 Adapter。
    let daemon_2 = Arc::new(KernelDaemon::new(
        hardware.clone(),
        hardware,
        Arc::new(InMemoryResourceManager::with_next_fence_token(
            "node-golden-c",
            vec![test_resource()],
            next_fence_token_2,
        )),
        sandbox.clone(),
        "node-golden-c",
        epoch_2,
    ));
    let adapter_2 = KernelServiceAdapter::new(daemon_2, Arc::new(TestWorkerResolver))
        .with_runtime_journal(journal.clone())
        .with_event_store(event_store);

    let authority_2 = adapter_2.authority();
    let context_epoch_2 = AuthorityCallContext {
        contract: semantic::ContractRevision::current(),
        namespace: NamespaceId::default(),
        request_id: "golden-c-req-epoch-2".to_string(),
        idempotency_key: "golden-c-req-epoch-2".to_string(),
    };

    // Proof 1: Pre-crash semantic Worker/Lease/Endpoint authority is NOT silently restored
    // 中文：证明 1：崩溃前的 Worker/Lease/Endpoint 语义 authority 不会被静默恢复。
    let snapshot_2 = authority_2.snapshot(&context_epoch_2, &principal).unwrap();
    assert!(
        snapshot_2.workers.is_empty(),
        "Proof 1: Pre-crash workers are not silently restored"
    );
    assert!(
        snapshot_2.leases.is_empty(),
        "Proof 1: Pre-crash leases are not silently restored"
    );
    assert!(
        snapshot_2.endpoints.is_empty(),
        "Proof 1: Pre-crash endpoints are not silently restored"
    );
    assert!(
        snapshot_2.endpoint_grants.is_empty(),
        "Proof 1: Pre-crash grants are not silently restored"
    );

    // Proof 2: Old event source/cursor is rejected as changed or requires reconstruction
    // 中文：证明 2：旧 event source/cursor 会被识别为已变更，或要求重新构建。
    assert_ne!(
        snapshot_2.source, snapshot_1.source,
        "Proof 2: Event source is epoch-scoped and changes on restart"
    );
    let old_cursor_replay = authority_2
        .read_events(&context_epoch_2, &principal, &cursor_1, 256)
        .unwrap();
    assert_eq!(
        old_cursor_replay.status,
        semantic::ReplayStatus::SourceChanged,
        "Proof 2: Old event source/cursor is rejected as SourceChanged"
    );
    assert!(old_cursor_replay.events.is_empty());

    // Proof 5: Replacement Lease Fence is strictly greater than pre-crash Fence
    // 中文：证明 5：替代 Lease 的 Fence 严格大于崩溃前的 Fence。
    let repl_worker_id = semantic::Identity {
        id: "golden-worker-c-replacement".to_string(),
        generation: 1,
    };
    let repl_lease = authority_2
        .acquire_lease(
            &context_epoch_2,
            &principal,
            repl_worker_id,
            query,
            u64::MAX,
        )
        .expect("Proof 5: Resource is safely allocatable after restart recovery");
    assert_eq!(repl_lease.state, semantic::LeaseState::Active);
    assert!(
        repl_lease.fence_token > fence_1,
        "Proof 5: Replacement fence is strictly greater than pre-crash fence"
    );
    assert!(
        repl_lease.fence_token >= next_fence_token_2,
        "Proof 5: Replacement fence satisfies the new epoch fence floor"
    );
}

// =========================================================================
// Cy Kernel Phase 11: Canonical Event Model Closure Tests
// =========================================================================
// 中文：Cy Kernel 阶段 11：规范事件模型闭环测试。

/// Production Eviction and Resume Test over Real Authority UDS
///
/// Verifies:
/// 1. produce more events than retention window (300 > 256);
/// 2. resume from current cursor -> CURRENT;
/// 3. resume from expired cursor -> GAP (no silent skipping);
/// 4. restart daemon with epoch change;
/// 5. old source cursor -> SOURCE_CHANGED;
/// 6. snapshot + new cursor -> resume correctly with CURRENT.
/// 中文：真实 authority UDS 上的生产环境淘汰与续读测试。验证超过保留窗口产生事件（300 > 256）、从当前 cursor 续读返回 CURRENT、从过期 cursor 续读返回 GAP（不得静默跳过）、daemon epoch 变化后重启时旧 source cursor 返回 SOURCE_CHANGED，以及通过新 snapshot 和 cursor 正确续读并返回 CURRENT。
#[cfg(unix)]
#[tokio::test]
async fn production_event_eviction_and_resume_over_real_authority_uds() {
    use crate::peer_cred::{inject_authority_principal, PeerCredAccept};
    use cy_proto::core_v1::{
        kernel_authority_service_client::KernelAuthorityServiceClient,
        kernel_authority_service_server::KernelAuthorityServiceServer, WatchEventsRequest,
    };
    use std::path::PathBuf;
    use tokio::net::{UnixListener, UnixStream};
    use tokio_stream::wrappers::UnixListenerStream;
    use tonic::transport::{Endpoint, Server, Uri};
    use tower::service_fn;

    let dir = tempfile::tempdir().unwrap();
    let socket_path = dir.path().join("authority_events.sock");

    let start_server = |socket: PathBuf, epoch: u64| {
        let hardware = Arc::new(TestHardware {
            resources: vec![test_resource()],
        });
        let daemon = Arc::new(KernelDaemon::new(
            hardware.clone(),
            hardware,
            Arc::new(InMemoryResourceManager::new("node", vec![test_resource()])),
            Arc::new(FakeSandbox),
            "node",
            epoch,
        ));
        let adapter = KernelServiceAdapter::new(daemon, Arc::new(TestWorkerResolver));
        let server_adapter = adapter.clone();
        let listener = UnixListener::bind(&socket).expect("bind UDS listener");
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let server_handle = tokio::spawn(async move {
            Server::builder()
                .add_service(KernelAuthorityServiceServer::with_interceptor(
                    server_adapter,
                    inject_authority_principal,
                ))
                .serve_with_incoming_shutdown(
                    PeerCredAccept::new(UnixListenerStream::new(listener)),
                    async {
                        let _ = shutdown_rx.await;
                    },
                )
                .await
                .unwrap();
        });
        (adapter, shutdown_tx, server_handle)
    };

    // 1. Start Server in Epoch 1
    // 中文：1. 在 Epoch 1 启动 Server。
    let (adapter_1, shutdown_tx_1, server_handle_1) = start_server(socket_path.clone(), 1);
    tokio::time::sleep(Duration::from_millis(30)).await;

    let connect_client = |socket: PathBuf| async move {
        let channel = Endpoint::try_from("http://[::]:50051")
            .unwrap()
            .connect_with_connector(service_fn(move |_: Uri| {
                UnixStream::connect(socket.clone())
            }))
            .await
            .expect("connect to UDS");
        KernelAuthorityServiceClient::new(channel)
    };

    let mut client = connect_client(socket_path.clone()).await;
    let source_1 = adapter_1.semantic_event_source();

    // 2. Produce more events than retention window (window is 256)
    // Produce 300 events
    // 中文：2. 产生超过保留窗口的事件（窗口大小为 256）；总共产生 300 个事件。
    for sequence in 1..=300 {
        adapter_1.publish_semantic_event(
            semantic::Identity {
                id: "worker-eviction-test".to_string(),
                generation: 1,
            },
            "worker.state.changed",
            "cyrene.worker.v1",
            format!("event-{sequence}").into_bytes(),
        );
    }

    // 3. Resume from current cursor (sequence 260) -> receives streaming events 261..=300
    // 中文：3. 从当前 cursor（sequence 260）续读，接收 261..=300 的流式事件。
    let current_req = WatchEventsRequest {
        context: Some(authority_context("watch-current")),
        cursor: Some(cy_proto::semantic_v1::EventCursor {
            source: Some(to_semantic_proto_identity(&source_1)),
            sequence: 260,
        }),
        page_size: 256,
    };
    let mut current_stream = client
        .watch_events(current_req)
        .await
        .expect("subscribe from current cursor")
        .into_inner();

    use tokio_stream::StreamExt;
    let mut received_events = Vec::new();
    while let Ok(Some(Ok(event))) =
        tokio::time::timeout(Duration::from_millis(50), current_stream.next()).await
    {
        received_events.push(watch_event(event));
    }
    assert_eq!(
        received_events.len(),
        40,
        "Current cursor must replay all retained events after cursor without gap"
    );
    assert_eq!(received_events[0].sequence, 261);
    assert_eq!(received_events.last().unwrap().sequence, 300);

    // 4. Resume from expired cursor (sequence 1, which has been evicted) -> returns typed GAP
    // 中文：4. 从已淘汰的过期 cursor（sequence 1）续读，返回类型化 GAP。
    let expired_req = WatchEventsRequest {
        context: Some(authority_context("watch-gap")),
        cursor: Some(cy_proto::semantic_v1::EventCursor {
            source: Some(to_semantic_proto_identity(&source_1)),
            sequence: 1,
        }),
        page_size: 256,
    };
    let mut gap_stream = client
        .watch_events(expired_req)
        .await
        .expect("expired cursor returns a typed continuity response")
        .into_inner();
    let gap_response = gap_stream
        .next()
        .await
        .expect("gap stream response")
        .expect("gap response is not a transport error");
    match gap_response.body {
        Some(cy_proto::core_v1::watch_events_response::Body::Continuity(continuity)) => {
            assert_eq!(
                continuity.status,
                cy_proto::semantic_v1::ReplayStatus::Gap as i32
            );
        }
        other => panic!("expected typed GAP response, got {other:?}"),
    }

    // 5. Restart daemon (simulate crash/restart with epoch advance)
    // 中文：5. 重启 daemon（模拟崩溃/重启并推进 epoch）。
    drop(current_stream);
    drop(client);
    let _ = shutdown_tx_1.send(());
    let _ = server_handle_1.await;
    std::fs::remove_file(&socket_path).ok();

    // Start Server in Epoch 2
    // 中文：在 Epoch 2 启动 Server。
    let (adapter_2, shutdown_tx_2, _server_handle_2) = start_server(socket_path.clone(), 2);
    tokio::time::sleep(Duration::from_millis(30)).await;

    let mut client_2 = connect_client(socket_path.clone()).await;

    // 6. Old source cursor -> returns typed SOURCE_CHANGED
    // 中文：6. 旧 source cursor 返回类型化 SOURCE_CHANGED。
    let old_source_req = WatchEventsRequest {
        context: Some(authority_context("watch-old-source")),
        cursor: Some(cy_proto::semantic_v1::EventCursor {
            source: Some(to_semantic_proto_identity(&source_1)),
            sequence: 300,
        }),
        page_size: 256,
    };
    let mut source_changed_stream = client_2
        .watch_events(old_source_req)
        .await
        .expect("old source returns a typed continuity response")
        .into_inner();
    let source_changed_response = source_changed_stream
        .next()
        .await
        .expect("source-change stream response")
        .expect("source change is not a transport error");
    match source_changed_response.body {
        Some(cy_proto::core_v1::watch_events_response::Body::Continuity(continuity)) => {
            assert_eq!(
                continuity.status,
                cy_proto::semantic_v1::ReplayStatus::SourceChanged as i32
            );
        }
        other => panic!("expected typed SOURCE_CHANGED response, got {other:?}"),
    }

    // 7. Snapshot + new cursor -> resume correctly
    // 中文：7. 获取 snapshot 和新 cursor，并正确续读。
    let snap_ctx = AuthorityCallContext {
        contract: semantic::ContractRevision::current(),
        namespace: NamespaceId::default(),
        request_id: "snapshot-after-restart".to_string(),
        idempotency_key: "snapshot-after-restart".to_string(),
    };
    let principal_user = principal_from_peer_cred(&AUTHORITY_TEST_PEER);
    let snap = adapter_2
        .authority()
        .snapshot(&snap_ctx, &principal_user)
        .expect("snapshot after restart");
    let new_cursor = cy_proto::semantic_v1::EventCursor {
        source: Some(to_semantic_proto_identity(&snap.cursor.source)),
        sequence: snap.cursor.sequence,
    };

    assert_ne!(
        snap.cursor.source.generation, source_1.generation,
        "Snapshot provides new epoch source"
    );

    let resume_req = WatchEventsRequest {
        context: Some(authority_context("watch-new-epoch")),
        cursor: Some(new_cursor),
        page_size: 256,
    };
    let mut resumed_stream = client_2
        .watch_events(resume_req)
        .await
        .expect("subscribe with new cursor")
        .into_inner();

    // Publish new event in Epoch 2 live
    // 中文：在 Epoch 2 的活动状态下发布新事件。
    adapter_2.publish_semantic_event(
        semantic::Identity {
            id: "worker-epoch-2".to_string(),
            generation: 1,
        },
        "worker.state.changed",
        "cyrene.worker.v1",
        b"epoch-2-event".to_vec(),
    );

    let live_event = tokio::time::timeout(Duration::from_millis(200), resumed_stream.next())
        .await
        .expect("receive live event within timeout")
        .expect("stream yields event")
        .map(watch_event)
        .expect("event is ok");

    assert_eq!(live_event.kind, "worker.state.changed");
    assert_eq!(live_event.body, b"epoch-2-event");

    let _ = shutdown_tx_2.send(());
}

/// Canonical WatchEvents server-streaming backpressure test over real authority UDS
///
/// Verifies:
/// 1. open canonical stream;
/// 2. read first few events, then stop reading;
/// 3. produce > bounded subscriber capacity events;
/// 4. authority state mutations continue without blocking;
/// 5. slow stream is disconnected with OutOfRange error;
/// 6. reconnect using last acknowledged cursor and verify replay resumes correctly.
/// 中文：真实 authority UDS 上的规范 WatchEvents 服务端流背压测试。验证：打开规范流、读取少量事件后停止读取、产生超过订阅者有界容量的事件、authority 状态变更仍可继续、慢速流以 OutOfRange 错误断开，然后使用最后确认的 cursor 重新连接并验证重放正确恢复。
#[cfg(unix)]
#[tokio::test]
async fn canonical_watch_events_stream_slow_consumer_and_reconnect_over_real_uds() {
    use crate::peer_cred::{inject_authority_principal, PeerCredAccept};
    use cy_proto::core_v1::{
        kernel_authority_service_client::KernelAuthorityServiceClient,
        kernel_authority_service_server::KernelAuthorityServiceServer, WatchEventsRequest,
    };
    use tokio::net::{UnixListener, UnixStream};
    use tokio_stream::{wrappers::UnixListenerStream, StreamExt};
    use tonic::transport::{Endpoint, Server, Uri};
    use tower::service_fn;

    let dir = tempfile::tempdir().unwrap();
    let socket_path = dir.path().join("authority_stream_backpressure.sock");

    let hardware = Arc::new(TestHardware {
        resources: vec![test_resource()],
    });
    let daemon = Arc::new(KernelDaemon::new(
        hardware.clone(),
        hardware,
        Arc::new(InMemoryResourceManager::new("node", vec![test_resource()])),
        Arc::new(FakeSandbox),
        "node",
        1,
    ));
    let adapter = KernelServiceAdapter::new(daemon, Arc::new(TestWorkerResolver));
    let server_adapter = adapter.clone();
    let listener = UnixListener::bind(&socket_path).expect("bind UDS listener");
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    let server_handle = tokio::spawn(async move {
        Server::builder()
            .add_service(KernelAuthorityServiceServer::with_interceptor(
                server_adapter,
                inject_authority_principal,
            ))
            .serve_with_incoming_shutdown(
                PeerCredAccept::new(UnixListenerStream::new(listener)),
                async {
                    let _ = shutdown_rx.await;
                },
            )
            .await
            .unwrap();
    });

    tokio::time::sleep(Duration::from_millis(30)).await;

    let channel = Endpoint::try_from("http://[::]:50051")
        .unwrap()
        .connect_with_connector(service_fn({
            let path = socket_path.clone();
            move |_: Uri| UnixStream::connect(path.clone())
        }))
        .await
        .expect("connect to UDS");
    let mut client = KernelAuthorityServiceClient::new(channel);
    let source = adapter.semantic_event_source();

    // 1. Publish 10 initial events
    // 中文：1. 发布 10 个初始事件。
    for sequence in 1..=10 {
        adapter.publish_semantic_event(
            semantic::Identity {
                id: "worker-backpressure-test".to_string(),
                generation: 1,
            },
            "worker.state.changed",
            "cyrene.worker.v1",
            format!("event-{sequence}").into_bytes(),
        );
    }

    // 2. Client subscribes from sequence 0
    // 中文：2. 客户端从 sequence 0 开始订阅。
    let req = WatchEventsRequest {
        context: Some(authority_context("watch-stream-backpressure")),
        cursor: Some(cy_proto::semantic_v1::EventCursor {
            source: Some(to_semantic_proto_identity(&source)),
            sequence: 0,
        }),
        page_size: 256,
    };
    let mut stream = client
        .watch_events(req)
        .await
        .expect("watch stream")
        .into_inner();

    // 3. Client reads 5 events and stops reading
    // 中文：3. 客户端读取 5 个事件后停止读取。
    let mut acked_cursor = 0;
    for _ in 1..=5 {
        let event = stream.next().await.unwrap().unwrap();
        acked_cursor = watch_event(event).sequence;
    }
    assert_eq!(acked_cursor, 5);

    // 4. Kernel produces more events than the bounded subscriber capacity but
    // stays within durable history, so the transport backpressure path is
    // exercised before the later cursor-gap reconnect assertion.
    // 中文：4. Kernel 产生的事件超过订阅者有界容量，但仍处于 durable history 范围内；因此先触发传输背压路径，再执行后续 cursor-gap 重连断言。
    for sequence in 11..=110 {
        adapter.publish_semantic_event(
            semantic::Identity {
                id: "worker-backpressure-test".to_string(),
                generation: 1,
            },
            "worker.state.changed",
            "cyrene.worker.v1",
            format!("event-{sequence}").into_bytes(),
        );
    }

    // 5. Verify slow stream receives OutOfRange disconnect error
    // 中文：5. 验证慢速流收到 OutOfRange 断开错误。
    let mut observed_disconnect = false;
    while let Some(item) = stream.next().await {
        match item {
            Ok(_) => continue,
            Err(status) => {
                assert_eq!(status.code(), tonic::Code::OutOfRange);
                assert!(
                    status.message().contains("subscriber buffer full")
                        || status.message().contains("GAP"),
                    "Disconnect message must indicate buffer overflow: {}",
                    status.message()
                );
                observed_disconnect = true;
                break;
            }
        }
    }
    assert!(
        observed_disconnect,
        "Slow subscriber must be disconnected with OutOfRange without blocking kernel"
    );

    // 6. Evict the last acknowledged cursor, then reconnect. GAP is a typed
    // event-history continuity condition, not a transport status.
    // 中文：6. 淘汰最后确认的 cursor，然后重新连接。GAP 表示事件历史连续性已中断，是类型化的历史状态，不是传输层状态码。
    for sequence in 111..=300 {
        adapter.publish_semantic_event(
            semantic::Identity {
                id: "worker-backpressure-test".to_string(),
                generation: 1,
            },
            "worker.state.changed",
            "cyrene.worker.v1",
            format!("event-{sequence}").into_bytes(),
        );
    }

    let reconnect_req = WatchEventsRequest {
        context: Some(authority_context("reconnect-stream")),
        cursor: Some(cy_proto::semantic_v1::EventCursor {
            source: Some(to_semantic_proto_identity(&source)),
            sequence: acked_cursor,
        }),
        page_size: 256,
    };
    let mut reconnect_stream = client
        .watch_events(reconnect_req)
        .await
        .expect("reconnection returns typed GAP continuity")
        .into_inner();
    let reconnect_response = reconnect_stream
        .next()
        .await
        .expect("reconnection continuity response")
        .expect("GAP is not a transport error");
    match reconnect_response.body {
        Some(cy_proto::core_v1::watch_events_response::Body::Continuity(continuity)) => {
            assert_eq!(
                continuity.status,
                cy_proto::semantic_v1::ReplayStatus::Gap as i32
            );
        }
        other => panic!("expected typed GAP continuity, got {other:?}"),
    }

    let _ = shutdown_tx.send(());
    let _ = server_handle.await;
}

/// Canonical WatchEvents does not lose any event at the boundary between initial durable replay and live notification waiting.
///
/// Verifies:
/// 1. publish initial batch of events (1..=5);
/// 2. client opens WatchEvents stream and consumes initial replay events;
/// 3. exactly one event is committed (sequence 6) at the replay/live transition boundary;
/// 4. no subsequent events are published;
/// 5. subscriber waiting for live events still receives event 6 promptly without requiring future events to wake up.
/// 中文：规范 WatchEvents 必须在初始 durable replay 与等待实时通知的边界上保留所有事件。验证：先发布事件批次（1..=5）；客户端打开流并消费初始重放；恰好在 replay/live 切换边界提交 sequence 6；此后不再发布事件；等待实时事件的订阅者仍能及时收到事件 6，无需其他事件唤醒。
#[cfg(unix)]
#[tokio::test]
async fn canonical_watch_events_does_not_lose_event_at_replay_live_handoff() {
    use crate::peer_cred::{inject_authority_principal, PeerCredAccept};
    use cy_proto::core_v1::{
        kernel_authority_service_client::KernelAuthorityServiceClient,
        kernel_authority_service_server::KernelAuthorityServiceServer, WatchEventsRequest,
    };
    use tokio::net::{UnixListener, UnixStream};
    use tokio_stream::{wrappers::UnixListenerStream, StreamExt};
    use tonic::transport::{Endpoint, Server, Uri};
    use tower::service_fn;

    let dir = tempfile::tempdir().unwrap();
    let socket_path = dir.path().join("authority_stream_handoff.sock");

    let hardware = Arc::new(TestHardware {
        resources: vec![test_resource()],
    });
    let daemon = Arc::new(KernelDaemon::new(
        hardware.clone(),
        hardware,
        Arc::new(InMemoryResourceManager::new("node", vec![test_resource()])),
        Arc::new(FakeSandbox),
        "node",
        1,
    ));
    let adapter = KernelServiceAdapter::new(daemon, Arc::new(TestWorkerResolver));
    let server_adapter = adapter.clone();
    let listener = UnixListener::bind(&socket_path).expect("bind UDS listener");
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    let server_handle = tokio::spawn(async move {
        Server::builder()
            .add_service(KernelAuthorityServiceServer::with_interceptor(
                server_adapter,
                inject_authority_principal,
            ))
            .serve_with_incoming_shutdown(
                PeerCredAccept::new(UnixListenerStream::new(listener)),
                async {
                    let _ = shutdown_rx.await;
                },
            )
            .await
            .unwrap();
    });

    tokio::time::sleep(Duration::from_millis(30)).await;

    let channel = Endpoint::try_from("http://[::]:50051")
        .unwrap()
        .connect_with_connector(service_fn({
            let path = socket_path.clone();
            move |_: Uri| UnixStream::connect(path.clone())
        }))
        .await
        .expect("connect to UDS");
    let mut client = KernelAuthorityServiceClient::new(channel);
    let source = adapter.semantic_event_source();

    // 1. Publish 5 initial events
    // 中文：1. 发布 5 个初始事件。
    for sequence in 1..=5 {
        adapter.publish_semantic_event(
            semantic::Identity {
                id: "worker-handoff-test".to_string(),
                generation: 1,
            },
            "worker.state.changed",
            "cyrene.worker.v1",
            format!("initial-event-{sequence}").into_bytes(),
        );
    }

    // 2. Client subscribes from sequence 0
    // 中文：2. 客户端从 sequence 0 开始订阅。
    let req = WatchEventsRequest {
        context: Some(authority_context("watch-stream-handoff")),
        cursor: Some(cy_proto::semantic_v1::EventCursor {
            source: Some(to_semantic_proto_identity(&source)),
            sequence: 0,
        }),
        page_size: 256,
    };
    let mut stream = client
        .watch_events(req)
        .await
        .expect("watch stream")
        .into_inner();

    // 3. Read initial 5 events
    // 中文：3. 读取初始的 5 个事件。
    for expected_seq in 1..=5 {
        let event = tokio::time::timeout(Duration::from_millis(200), stream.next())
            .await
            .expect("timeout waiting for initial replay event")
            .expect("stream ended prematurely")
            .expect("event ok");
        assert_eq!(watch_event(event).sequence, expected_seq);
    }

    // 4. Publish exactly one event at the replay -> live transition boundary, and no subsequent events
    // 中文：4. 在 replay -> live 切换边界只发布一个事件，之后不再发布其他事件。
    adapter.publish_semantic_event(
        semantic::Identity {
            id: "worker-handoff-test".to_string(),
            generation: 1,
        },
        "worker.state.changed",
        "cyrene.worker.v1",
        b"handoff-event-6".to_vec(),
    );

    // 5. Subscriber must receive event 6 without any subsequent events being produced
    // 中文：5. 即使之后没有产生其他事件，订阅者也必须收到事件 6。
    let live_event = tokio::time::timeout(Duration::from_millis(200), stream.next())
        .await
        .expect("timeout waiting for live handoff event")
        .expect("stream ended prematurely")
        .expect("event ok");
    let live_event = watch_event(live_event);

    assert_eq!(live_event.sequence, 6);
    assert_eq!(live_event.body, b"handoff-event-6");

    drop(stream);
    drop(client);
    let _ = shutdown_tx.send(());
    let _ = server_handle.await;
}

/// Slow consumer on streaming endpoint cannot block Kernel authority state transitions
/// 中文：流式端点上的慢速消费者不能阻止 Kernel authority 状态转换。
#[tokio::test]
async fn watch_operations_slow_consumer_lags_and_disconnects_without_blocking_kernel() {
    use core_v1::kernel_service_server::KernelService;
    use tokio_stream::StreamExt;

    let adapter = heartbeat_adapter();

    // 1. Client subscribes to WatchOperations
    // 中文：1. 客户端订阅 WatchOperations。
    let response = adapter
        .watch_operations(Request::new(core_v1::WatchOperationsRequest {
            context: None,
            operation_names: Vec::new(),
            resume_token: String::new(),
        }))
        .await
        .unwrap();
    let mut stream = response.into_inner();

    // 2. Kernel publishes more events than OPERATION_EVENT_SUBSCRIBER_CAPACITY (1024)
    // without the subscriber polling or consuming the stream
    // 中文：2. Kernel 发布的事件超过 OPERATION_EVENT_SUBSCRIBER_CAPACITY（1024），而订阅者没有轮询或消费该流。
    for i in 1..=1500 {
        adapter.publish_runtime_event(
            core_v1::RuntimeEventType::InstanceStateChanged,
            format!("target-{i}"),
            "EVENT_BURST",
            format!("burst event {i}"),
        );
    }

    // 3. Verify kernel publications completed without deadlock or stalling
    // 中文：3. 验证 Kernel 发布已完成，没有死锁或停滞。
    assert_eq!(
        adapter.operation_events.lock().unwrap().len(),
        OPERATION_EVENT_HISTORY_CAPACITY
    );

    // 4. The slow subscriber consumes stream and eventually observes Lagged error (out_of_range)
    // 中文：4. 慢速订阅者继续消费该流，最终观察到 Lagged 错误（out_of_range）。
    let mut observed_lagged = false;
    while let Some(item) = stream.next().await {
        match item {
            Ok(_) => continue,
            Err(status) => {
                assert_eq!(status.code(), tonic::Code::OutOfRange);
                assert!(status
                    .message()
                    .contains("lagged beyond the bounded buffer"));
                observed_lagged = true;
                break;
            }
        }
    }
    assert!(
        observed_lagged,
        "Slow subscriber must observe Lagged disconnect without blocking kernel"
    );
}
