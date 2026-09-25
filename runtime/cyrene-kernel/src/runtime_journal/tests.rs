//! Durable journal and restart-recovery tests.
//!
//! These tests prove epoch/fence monotonicity, exact stale-process evidence,
//! and durable semantic event replay.
//! 中文：持久化 journal 与重启恢复测试。这些测试验证 epoch/Fence 单调性、精确的过期进程证据，以及语义事件的持久化重放。

use super::*;

use std::sync::Mutex;

struct RecoverySandbox {
    observed: Vec<RuntimeProcessEvidence>,
    recovered: Mutex<Vec<RuntimeProcessEvidence>>,
}

impl cy_kernel_api::ProcessRuntime for RecoverySandbox {
    fn preflight(&self) -> cy_kernel_api::NodeCapabilities {
        cy_kernel_api::NodeCapabilities {
            ready: true,
            facts: Vec::new(),
            enforcement: Vec::new(),
        }
    }

    fn launch(
        &self,
        _plan: &cy_kernel_api::LaunchPlan,
        _binding: &cy_kernel_api::DeviceBinding,
    ) -> Result<cy_kernel_api::ProcessHandle, ProviderError> {
        Err(ProviderError::new("test", "UNUSED", "recovery test"))
    }

    fn stop(
        &self,
        _handle: &cy_kernel_api::ProcessHandle,
        _request: &cy_kernel_api::StopRequest,
    ) -> Result<cy_kernel_api::CleanupReport, ProviderError> {
        Err(ProviderError::new("test", "UNUSED", "recovery test"))
    }
}

impl SandboxBackend for RecoverySandbox {
    fn backend_id(&self) -> &str {
        "recovery-test"
    }

    fn discover_recovery_processes(&self) -> Result<Vec<RuntimeProcessEvidence>, ProviderError> {
        Ok(self.observed.clone())
    }

    fn recover_stale_process(
        &self,
        evidence: &RuntimeProcessEvidence,
    ) -> Result<cy_kernel_api::CleanupReport, ProviderError> {
        self.recovered.lock().unwrap().push(evidence.clone());
        Ok(cy_kernel_api::CleanupReport {
            complete: true,
            exit_code: None,
            oom_killed: false,
            conditions: Vec::new(),
            reason_code: "RECOVERY_CLEANUP_COMPLETE".to_string(),
        })
    }
}

fn process_record(
    instance_name: &str,
    epoch: u64,
    evidence: Option<RuntimeProcessEvidence>,
) -> RuntimeProcessRecord {
    RuntimeProcessRecord {
        instance_name: instance_name.to_string(),
        lease_name: Some(format!("lease-{instance_name}")),
        fence_token: Some(epoch),
        node_epoch: epoch,
        runtime_evidence: evidence,
    }
}

#[test]
fn restart_advances_epoch_and_fence_without_recovering_instances() {
    let directory = tempfile::tempdir().unwrap();
    let journal = FileRuntimeJournal::open(directory.path().join("runtime.jsonl")).unwrap();
    let first = journal.begin_epoch("node-1").unwrap();
    journal
        .append(RuntimeJournalRecord {
            event: RuntimeJournalEvent::InstanceLaunched,
            node_id: "node-1".to_string(),
            node_epoch: first.node_epoch,
            instance_name: Some("instance-1".to_string()),
            lease_name: Some("lease-1".to_string()),
            fence_token: Some(41),
            reason_code: "WORKER_LAUNCHED".to_string(),
            runtime_evidence: Some(RuntimeProcessEvidence {
                cgroup_name: "instance-1".to_string(),
                pid: 42,
                start_time_ticks: 1,
            }),
        })
        .unwrap();
    let next = journal.begin_epoch("node-1").unwrap();
    assert!(next.node_epoch > first.node_epoch);
    assert_eq!(next.next_fence_token, 42);
    assert_eq!(next.runtime_processes.len(), 1);
    assert_eq!(next.runtime_processes[0].instance_name, "instance-1");
}

#[test]
fn recovery_classifies_without_adopting_valid_unknown_or_foreign_processes() {
    let valid = RuntimeProcessEvidence {
        cgroup_name: "instance-valid".to_string(),
        pid: 11,
        start_time_ticks: 101,
    };
    let stale = RuntimeProcessEvidence {
        cgroup_name: "instance-stale".to_string(),
        pid: 12,
        start_time_ticks: 102,
    };
    let foreign = RuntimeProcessEvidence {
        cgroup_name: "instance-foreign".to_string(),
        pid: 13,
        start_time_ticks: 103,
    };
    let recovery = RecoveryState {
        node_epoch: 9,
        next_fence_token: 1,
        runtime_processes: vec![
            process_record("valid", 9, Some(valid.clone())),
            process_record("stale", 8, Some(stale.clone())),
            process_record("unknown", 8, None),
        ],
    };
    let candidates = FileRuntimeJournal::classify_recovery(&recovery, &[valid, stale, foreign]);
    assert_eq!(
        candidates
            .iter()
            .map(|candidate| candidate.classification)
            .collect::<Vec<_>>(),
        vec![
            RecoveryClassification::Valid,
            RecoveryClassification::Stale,
            RecoveryClassification::Foreign,
            RecoveryClassification::Unknown,
        ]
    );
}

#[test]
fn recovery_reaps_only_exact_stale_evidence_and_journals_terminal_cleanup() {
    let directory = tempfile::tempdir().unwrap();
    let journal = FileRuntimeJournal::open(directory.path().join("runtime.jsonl")).unwrap();
    let first = journal.begin_epoch("node-1").unwrap();
    let evidence = RuntimeProcessEvidence {
        cgroup_name: "instance-stale".to_string(),
        pid: 42,
        start_time_ticks: 7,
    };
    journal
        .append(RuntimeJournalRecord {
            event: RuntimeJournalEvent::InstanceLaunched,
            node_id: "node-1".to_string(),
            node_epoch: first.node_epoch,
            instance_name: Some("worker-stale".to_string()),
            lease_name: Some("lease-stale".to_string()),
            fence_token: Some(5),
            reason_code: "WORKER_LAUNCHED".to_string(),
            runtime_evidence: Some(evidence.clone()),
        })
        .unwrap();
    let recovery = journal.begin_epoch("node-1").unwrap();
    let sandbox = RecoverySandbox {
        observed: vec![evidence.clone()],
        recovered: Mutex::new(Vec::new()),
    };

    journal
        .recover_before_listeners("node-1", &recovery, &sandbox)
        .unwrap();

    assert_eq!(*sandbox.recovered.lock().unwrap(), vec![evidence]);
    assert!(journal
        .recover("node-1")
        .unwrap()
        .runtime_processes
        .is_empty());
}

#[test]
fn recovery_never_adopts_a_valid_process() {
    let directory = tempfile::tempdir().unwrap();
    let journal = FileRuntimeJournal::open(directory.path().join("runtime.jsonl")).unwrap();
    let evidence = RuntimeProcessEvidence {
        cgroup_name: "instance-current".to_string(),
        pid: 42,
        start_time_ticks: 7,
    };
    let recovery = RecoveryState {
        node_epoch: 4,
        next_fence_token: 1,
        runtime_processes: vec![process_record("worker-current", 4, Some(evidence.clone()))],
    };
    let sandbox = RecoverySandbox {
        observed: vec![evidence],
        recovered: Mutex::new(Vec::new()),
    };

    let error = journal
        .recover_before_listeners("node-1", &recovery, &sandbox)
        .unwrap_err();
    assert_eq!(error.reason_code, "RECOVERY_VALID_PROCESS_UNADOPTED");
    assert!(sandbox.recovered.lock().unwrap().is_empty());
}

#[test]
fn recovery_ignores_only_an_incomplete_final_record() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("runtime.jsonl");
    let journal = FileRuntimeJournal::open(&path).unwrap();
    let first = journal.begin_epoch("node-1").unwrap();
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"{\"partial\"")
        .unwrap();

    let next = journal.begin_epoch("node-1").unwrap();
    assert!(next.node_epoch > first.node_epoch);
}

#[test]
fn recovery_rejects_nonfinal_corruption() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("runtime.jsonl");
    std::fs::write(&path, "{\"partial\"\n{\"also_partial\"").unwrap();
    let journal = FileRuntimeJournal::open(&path).unwrap();

    assert_eq!(
        journal.begin_epoch("node-1").unwrap_err().kind(),
        std::io::ErrorKind::InvalidData
    );
}

/// Crash/restart must not reuse fence tokens. The fence floor is taken from
/// the durably persisted journal (`recover()` returns max historical fence
/// + 1); a fresh manager seeded with that floor must allocate a strictly
///   greater token than the lease that existed before the restart.
/// 中文：崩溃/重启后不得复用 Fence token。Fence 下限取自已持久化的 journal（recover() 返回历史 Fence 最大值 + 1）；用该下限初始化的新 manager 必须分配出严格大于重启前 Lease 的 token。
#[test]
fn crash_restart_does_not_reuse_fence_tokens() {
    use cy_kernel_api::{
        semantic::{
            Capability, CapabilityRequirement, Identity, Resource, ResourceQuery, ResourceState,
        },
        ResourceLeaseManager, ResourceRequest, RuntimeJournalEvent,
    };
    use cy_resource_manager::InMemoryResourceManager;

    let directory = tempfile::tempdir().unwrap();
    let journal = FileRuntimeJournal::open(directory.path().join("runtime.jsonl")).unwrap();

    let resource = Resource {
        identity: Identity {
            id: "resource-1".to_string(),
            generation: 1,
        },
        provider: Identity {
            id: "test-provider".to_string(),
            generation: 1,
        },
        resource_class: "accelerator".to_string(),
        capabilities: vec![Capability {
            id: "accelerator.compute".to_string(),
            revision: 1,
            properties: Default::default(),
        }],
        capacity: Default::default(),
        attributes: Default::default(),
        state: ResourceState::Ready,
        reason_code: "test-ready".to_string(),
        summary: "healthy".to_string(),
        links: Vec::new(),
    };

    let generation = 1_u64;
    let request = ResourceRequest {
        lease_name: "lease-before-restart".to_string(),
        expected_inventory_generation: generation,
        holder: Identity {
            id: "worker/test".to_string(),
            generation: 1,
        },
        query: ResourceQuery {
            resource_class: "accelerator".to_string(),
            count: 1,
            required_capabilities: vec![CapabilityRequirement {
                id: "accelerator.compute".to_string(),
                minimum_revision: 1,
                required_properties: Default::default(),
            }],
            minimum_capacity: Default::default(),
        },
        expires_at_unix_ms: None,
        limits: Default::default(),
    };

    // Pre-restart: acquire a lease with fence token N and durably record it.
    // 中文：重启前：获取 Fence token 为 N 的 Lease，并将其持久化记录。
    let manager_before = InMemoryResourceManager::new("node-1", vec![resource.clone()]);
    let lease_before = manager_before.acquire_lease(request.clone()).unwrap();
    let fence_before = lease_before.fence_token;
    journal
        .append(RuntimeJournalRecord {
            event: RuntimeJournalEvent::LeaseAcquired,
            node_id: "node-1".to_string(),
            node_epoch: 0,
            instance_name: None,
            lease_name: Some(lease_before.name.clone()),
            fence_token: Some(fence_before),
            reason_code: "LEASE_ACQUIRED".to_string(),
            runtime_evidence: None,
        })
        .unwrap();
    drop(manager_before);

    // Restart: recover the durable fence floor and seed a fresh manager.
    // 中文：重启后：恢复持久化的 Fence 下限，并用它初始化新的 manager。
    let recovery = journal.recover("node-1").unwrap();
    assert_eq!(recovery.next_fence_token, fence_before + 1);
    let manager_after = InMemoryResourceManager::with_next_fence_token(
        "node-1",
        vec![resource],
        recovery.next_fence_token,
    );
    let lease_after = manager_after.acquire_lease(request).unwrap();
    assert!(
        lease_after.fence_token > fence_before,
        "fence token must not be reused across a restart"
    );
}

#[test]
fn two_epoch_restart_requires_client_resnapshot_and_never_adopts_old_authority() {
    use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Duration};

    use cy_kernel_api::{
        semantic, AuthorityCallContext, CleanupReport, DeviceBinding, EnforcementMode,
        HealthReport, HostInventoryProvider, InstalledPluginResolver, InventorySnapshot,
        KernelAuthority, LaunchPlan, NodeCapabilities, ProcessHandle, ProcessRuntime,
        ResolvedLaunchPlan, ResourceProvider, SandboxBackend, StopRequest, VerifiedInstallation,
    };
    use cy_kernel_daemon::{KernelDaemon, KernelServiceAdapter};
    use cy_resource_manager::InMemoryResourceManager;

    #[derive(Clone)]
    struct TestHardware {
        resource: semantic::Resource,
    }

    impl HostInventoryProvider for TestHardware {
        fn probe_inventory(&self) -> Result<InventorySnapshot, ProviderError> {
            Ok(InventorySnapshot {
                generation: 1,
                resources: vec![self.resource.clone()],
                capabilities: NodeCapabilities {
                    ready: true,
                    facts: Vec::new(),
                    enforcement: Vec::new(),
                },
            })
        }
    }

    impl ResourceProvider for TestHardware {
        fn adapter_id(&self) -> &str {
            "restart-test-hardware"
        }

        fn probe_resources(&self) -> Result<Vec<semantic::Resource>, ProviderError> {
            Ok(vec![self.resource.clone()])
        }

        fn create_binding(
            &self,
            resource: &semantic::Resource,
        ) -> Result<DeviceBinding, ProviderError> {
            Ok(DeviceBinding {
                resource_id: resource.identity.id.clone(),
                nodes: Vec::new(),
                environment: BTreeMap::new(),
                joinable_environment_keys: Default::default(),
                required_gids: Vec::new(),
                enforcement: EnforcementMode::ObserveOnly,
                adapter_id: self.adapter_id().to_string(),
                reason_code: "RESTART_TEST_BINDING".to_string(),
            })
        }

        fn read_health(&self, _resource_id: &str) -> Result<HealthReport, ProviderError> {
            Ok(HealthReport {
                healthy: Some(true),
                reason_code: "READY".to_string(),
                summary: "restart test resource is ready".to_string(),
            })
        }
    }

    struct TestWorkerResolver;

    impl InstalledPluginResolver for TestWorkerResolver {
        fn resolve_launch_plan(
            &self,
            _installation: &VerifiedInstallation,
            _instance_name: &str,
        ) -> Result<ResolvedLaunchPlan, ProviderError> {
            Err(ProviderError::new(
                "restart-test",
                "UNUSED",
                "legacy launch is unused",
            ))
        }

        fn resolve_worker_launch_plan(
            &self,
            worker: &semantic::Worker,
        ) -> Result<ResolvedLaunchPlan, ProviderError> {
            Ok(ResolvedLaunchPlan {
                installation: VerifiedInstallation {
                    installation_name: "restart-test".to_string(),
                    manifest_digest: "sha256:restart-test".to_string(),
                    artifact_digest: "sha256:restart-test".to_string(),
                    verified_signature_identity: "restart-test".to_string(),
                },
                plan: LaunchPlan {
                    instance_name: worker.identity.id.clone(),
                    executable: PathBuf::from("restart-test-worker"),
                    args: Vec::new(),
                    environment: BTreeMap::new(),
                    cgroup_name: format!("worker-{}", worker.identity.id),
                    limits: Default::default(),
                    working_dir: None,
                    transport_socket: None,
                },
            })
        }
    }

    struct NoAdoptionRuntime;

    impl ProcessRuntime for NoAdoptionRuntime {
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
            Ok(ProcessHandle {
                pid: 7,
                cgroup_path: PathBuf::from(format!("/restart-test/{}", plan.cgroup_name)),
                start_time_ticks: Some(11),
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
                reason_code: "STOPPED".to_string(),
            })
        }
    }

    impl SandboxBackend for NoAdoptionRuntime {
        fn backend_id(&self) -> &str {
            "restart-test"
        }
        // Recovery uses SandboxBackend's fail-closed defaults: this runtime
        // has no stale-process discovery or adoption path.
        // 中文：恢复使用 SandboxBackend 的 fail-closed 默认行为：此 runtime 没有发现或接管过期进程的路径。
    }

    struct StaleCleanupRuntime {
        observed: Vec<RuntimeProcessEvidence>,
        reaped: Mutex<Vec<RuntimeProcessEvidence>>,
    }

    impl ProcessRuntime for StaleCleanupRuntime {
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
            Err(ProviderError::new("restart-test", "UNUSED", "cleanup only"))
        }

        fn stop(
            &self,
            _handle: &ProcessHandle,
            _request: &StopRequest,
        ) -> Result<CleanupReport, ProviderError> {
            Err(ProviderError::new("restart-test", "UNUSED", "cleanup only"))
        }
    }

    impl SandboxBackend for StaleCleanupRuntime {
        fn backend_id(&self) -> &str {
            "restart-test-cleanup"
        }

        fn discover_recovery_processes(
            &self,
        ) -> Result<Vec<RuntimeProcessEvidence>, ProviderError> {
            Ok(self.observed.clone())
        }

        fn recover_stale_process(
            &self,
            evidence: &RuntimeProcessEvidence,
        ) -> Result<CleanupReport, ProviderError> {
            if !self.observed.contains(evidence) {
                return Err(ProviderError::new(
                    "restart-test",
                    "RECOVERY_EVIDENCE_UNKNOWN",
                    "cleanup requires exact observed evidence",
                ));
            }
            self.reaped.lock().unwrap().push(evidence.clone());
            Ok(CleanupReport {
                complete: true,
                exit_code: Some(0),
                oom_killed: false,
                conditions: Vec::new(),
                reason_code: "STALE_PROCESS_REAPED".to_string(),
            })
        }
    }

    let resource = semantic::Resource {
        identity: semantic::Identity {
            id: "resource-restart".to_string(),
            generation: 1,
        },
        provider: semantic::Identity {
            id: "restart-test-hardware".to_string(),
            generation: 1,
        },
        resource_class: "accelerator".to_string(),
        capabilities: vec![semantic::Capability {
            id: "accelerator.compute".to_string(),
            revision: 1,
            properties: BTreeMap::new(),
        }],
        capacity: BTreeMap::new(),
        attributes: BTreeMap::new(),
        state: semantic::ResourceState::Ready,
        reason_code: "READY".to_string(),
        summary: "restart test resource".to_string(),
        links: Vec::new(),
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
    let principal = semantic::Principal {
        identity: semantic::Identity {
            id: "client-restart-golden".to_string(),
            generation: 1,
        },
    };
    let context = |request_id: &str| AuthorityCallContext {
        contract: semantic::ContractRevision::current(),
        namespace: NamespaceId::default(),
        request_id: request_id.to_string(),
        idempotency_key: request_id.to_string(),
    };
    let expiry = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
            + Duration::from_secs(60).as_millis() as u64
    };

    let directory = tempfile::tempdir().unwrap();
    let journal =
        Arc::new(FileRuntimeJournal::open(directory.path().join("runtime.jsonl")).unwrap());

    // Epoch N owns a lease, Worker, Operation, and Endpoint and persists
    // source-scoped events through the same FileRuntimeJournal.
    // 中文：Epoch N 拥有 Lease、Worker、Operation 和 Endpoint，并通过同一个 FileRuntimeJournal 持久化按 source 区分的事件。
    let epoch_n = journal.begin_epoch("node-restart").unwrap();
    let hardware_n = Arc::new(TestHardware {
        resource: resource.clone(),
    });
    let resources_n = Arc::new(InMemoryResourceManager::with_next_fence_token(
        "node-restart",
        vec![resource.clone()],
        epoch_n.next_fence_token,
    ));
    let adapter_n = KernelServiceAdapter::new(
        Arc::new(KernelDaemon::new(
            hardware_n.clone(),
            hardware_n,
            resources_n,
            Arc::new(NoAdoptionRuntime),
            "node-restart",
            epoch_n.node_epoch,
        )),
        Arc::new(TestWorkerResolver),
    )
    .with_runtime_journal(journal.clone())
    .with_event_store(journal.clone());
    let authority_n = adapter_n.authority();
    let worker = semantic::Identity {
        id: "worker-before-restart".to_string(),
        generation: 1,
    };
    let lease_n = authority_n
        .acquire_lease(
            &context("lease-before-restart"),
            &principal,
            worker.clone(),
            query.clone(),
            expiry(),
        )
        .unwrap();
    authority_n
        .start_worker(
            &context("worker-before-restart"),
            &principal,
            semantic::Worker {
                identity: worker.clone(),
                principal: principal.identity.clone(),
                provider: resource.provider.clone(),
                lease: lease_n.identity.clone(),
                state: semantic::WorkerState::Registered,
                execution_ref: "opaque-restart-test".to_string(),
                limits: BTreeMap::new(),
            },
        )
        .unwrap();
    let operation_n = semantic::Operation {
        identity: semantic::Identity {
            id: "operation-before-restart".to_string(),
            generation: 1,
        },
        owner: principal.identity.clone(),
        executor: worker.clone(),
        kind: "worker.invoke".to_string(),
        state: semantic::OperationState::Created,
        deadline_unix_ms: None,
        parent: None,
        metadata: BTreeMap::new(),
    };
    authority_n
        .create_operation(
            &context("operation-before-restart"),
            &principal,
            operation_n.clone(),
        )
        .unwrap();
    let endpoint_n = semantic::Endpoint {
        identity: semantic::Identity {
            id: "endpoint-before-restart".to_string(),
            generation: 1,
        },
        provider: resource.provider.clone(),
        owner: worker.clone(),
        transport: "transport.uds".to_string(),
        schema_id: "cyrene.endpoint.v1".to_string(),
        capabilities: Vec::new(),
        public_attributes: BTreeMap::new(),
        connection_ref: "uds://runtime/direct-worker".to_string(),
        credential_ref: None,
    };
    authority_n
        .publish_endpoint(
            &context("endpoint-before-restart"),
            &principal,
            endpoint_n.clone(),
        )
        .unwrap();
    let snapshot_n = authority_n
        .snapshot(&context("snapshot-before-restart"), &principal)
        .unwrap();
    assert!(
        snapshot_n.cursor.sequence > 0,
        "epoch N must expose a durable cursor"
    );
    assert!(snapshot_n.leases.contains(&lease_n));
    assert!(snapshot_n.operations.contains(&operation_n));
    assert!(snapshot_n
        .workers
        .iter()
        .any(|current| current.identity == worker));
    assert!(snapshot_n.endpoints.contains(&endpoint_n));
    let cursor_n = snapshot_n.cursor.clone();
    let fence_n = lease_n.fence_token;
    drop(authority_n);
    drop(adapter_n);

    // Epoch N+1 closes the exact stale process, then starts from a fresh
    // resource ledger and authority. It never restores any old authority.
    // 中文：Epoch N+1 关闭精确匹配的过期进程，然后从全新的 resource ledger 和 authority 启动；它绝不会恢复任何旧 authority。
    let epoch_n_plus_one = journal.begin_epoch("node-restart").unwrap();
    assert!(epoch_n_plus_one.node_epoch > epoch_n.node_epoch);
    assert_eq!(epoch_n_plus_one.next_fence_token, fence_n + 1);
    let stale_evidence = epoch_n_plus_one.runtime_processes[0]
        .runtime_evidence
        .clone()
        .unwrap();
    let cleanup_runtime = StaleCleanupRuntime {
        observed: vec![stale_evidence.clone()],
        reaped: Mutex::new(Vec::new()),
    };
    journal
        .recover_before_listeners("node-restart", &epoch_n_plus_one, &cleanup_runtime)
        .unwrap();
    assert_eq!(
        *cleanup_runtime.reaped.lock().unwrap(),
        vec![stale_evidence]
    );

    let hardware_n_plus_one = Arc::new(TestHardware {
        resource: resource.clone(),
    });
    let resources_n_plus_one = Arc::new(InMemoryResourceManager::with_next_fence_token(
        "node-restart",
        vec![resource.clone()],
        epoch_n_plus_one.next_fence_token,
    ));
    let authority_n_plus_one = KernelServiceAdapter::new(
        Arc::new(KernelDaemon::new(
            hardware_n_plus_one.clone(),
            hardware_n_plus_one,
            resources_n_plus_one,
            Arc::new(NoAdoptionRuntime),
            "node-restart",
            epoch_n_plus_one.node_epoch,
        )),
        Arc::new(TestWorkerResolver),
    )
    .with_runtime_journal(journal.clone())
    .with_event_store(journal)
    .authority();

    // The same client binds anew in epoch N+1 before asking to replay its
    // old cursor; binding does not resurrect its prior authority objects.
    // 中文：同一个客户端会在 Epoch N+1 重新绑定，然后请求重放旧 cursor；重新绑定不会复活该客户端之前的 authority 对象。
    let operation_n_plus_one = semantic::Operation {
        identity: semantic::Identity {
            id: "operation-after-restart".to_string(),
            generation: 1,
        },
        owner: principal.identity.clone(),
        executor: semantic::Identity {
            id: "worker-after-restart".to_string(),
            generation: 1,
        },
        kind: "worker.invoke".to_string(),
        state: semantic::OperationState::Created,
        deadline_unix_ms: None,
        parent: None,
        metadata: BTreeMap::new(),
    };
    authority_n_plus_one
        .create_operation(
            &context("bind-after-restart"),
            &principal,
            operation_n_plus_one.clone(),
        )
        .unwrap();
    let source_changed = authority_n_plus_one
        .read_events(&context("replay-after-restart"), &principal, &cursor_n, 256)
        .unwrap();
    assert_eq!(source_changed.status, semantic::ReplayStatus::SourceChanged);
    assert!(source_changed.events.is_empty());

    let lease_n_plus_one = authority_n_plus_one
        .acquire_lease(
            &context("lease-after-restart"),
            &principal,
            operation_n_plus_one.executor.clone(),
            query,
            expiry(),
        )
        .unwrap();
    assert!(lease_n_plus_one.fence_token > fence_n);
    let snapshot_n_plus_one = authority_n_plus_one
        .snapshot(&context("snapshot-after-restart"), &principal)
        .unwrap();
    assert_ne!(snapshot_n_plus_one.source, snapshot_n.source);
    assert_eq!(snapshot_n_plus_one.source, source_changed.source);
    assert_eq!(
        snapshot_n_plus_one.cursor.source,
        snapshot_n_plus_one.source
    );
    assert!(snapshot_n_plus_one.cursor.sequence > 0);
    assert_eq!(snapshot_n_plus_one.leases, vec![lease_n_plus_one]);
    assert_eq!(snapshot_n_plus_one.operations, vec![operation_n_plus_one]);
    assert!(snapshot_n_plus_one.workers.is_empty());
    assert!(snapshot_n_plus_one.endpoints.is_empty());
    assert!(authority_n_plus_one
        .read_events(
            &context("resume-after-resnapshot"),
            &principal,
            &snapshot_n_plus_one.cursor,
            256,
        )
        .unwrap()
        .events
        .is_empty());
}

#[test]
fn semantic_events_are_durable_and_source_scoped() {
    let directory = tempfile::tempdir().unwrap();
    let journal = FileRuntimeJournal::open(directory.path().join("runtime.jsonl")).unwrap();
    let source = semantic::Identity {
        id: "kernel/node-1".to_string(),
        generation: 7,
    };
    journal
        .append_event(DurableEventRecord {
            namespace: "default".to_string(),
            event: semantic::Event {
                sequence: 3,
                source: source.clone(),
                subject: semantic::Identity {
                    id: "worker-1".to_string(),
                    generation: 1,
                },
                kind: "worker.lost".to_string(),
                observed_at_unix_ms: 1,
                schema_id: "cyrene.worker.v1".to_string(),
                body: Vec::new(),
            },
        })
        .unwrap();
    let records = journal
        .events_for_source(&source, "default")
        .unwrap()
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].event.sequence, 3);
    assert_eq!(records[0].event.kind, "worker.lost");
    assert!(journal
        .events_for_source(
            &semantic::Identity {
                id: source.id.clone(),
                generation: 8,
            },
            "default",
        )
        .unwrap()
        .unwrap()
        .is_empty());

    let mut out_of_order = records[0].clone();
    out_of_order.event.sequence = 2;
    journal.append_event(out_of_order).unwrap();
    assert_eq!(
        journal
            .events_for_source(&source, "default")
            .unwrap_err()
            .reason_code,
        "JOURNAL_CORRUPT"
    );
}

#[test]
fn semantic_event_replay_ignores_an_incomplete_final_record() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("runtime.jsonl");
    let journal = FileRuntimeJournal::open(&path).unwrap();
    let source = semantic::Identity {
        id: "kernel/node-1".to_string(),
        generation: 7,
    };
    journal
        .append_event(DurableEventRecord {
            namespace: "default".to_string(),
            event: semantic::Event {
                sequence: 1,
                source: source.clone(),
                subject: semantic::Identity {
                    id: "worker-1".to_string(),
                    generation: 1,
                },
                kind: "worker.lost".to_string(),
                observed_at_unix_ms: 1,
                schema_id: "cyrene.worker.v1".to_string(),
                body: Vec::new(),
            },
        })
        .unwrap();
    std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(b"{\"partial\"")
        .unwrap();

    let records = journal
        .events_for_source(&source, "default")
        .unwrap()
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].event.sequence, 1);
}

/// Golden Test C — Restart With Running Worker
///
/// Scenario:
/// Start real Worker + Lease + Endpoint in Epoch N.
/// Kill Kernel unexpectedly (crash simulation).
/// Restart in Epoch N+1.
///
/// Verifies:
/// - old authority is not silently adopted;
/// - recovery classifies reality correctly;
/// - stale process is reaped only with exact evidence;
/// - foreign/unknown is not killed;
/// - old cursor gets SOURCE_CHANGED;
/// - fresh snapshot contains no stale authority;
/// - replacement Fence is strictly newer.
/// 中文：黄金测试 C——Kernel 重启时仍有 Worker 运行。场景：在 Epoch N 启动真实 Worker、Lease 和 Endpoint；意外杀死 Kernel（模拟崩溃）；在 Epoch N+1 重启。验证旧 authority 不会被静默接管、恢复能正确分类现实状态、只有具备精确证据时才 reap 过期进程、不会杀死 Foreign/Unknown 进程、旧 cursor 返回 SOURCE_CHANGED、新 snapshot 不含过期 authority，且替代 Fence 严格更新。
#[test]
fn golden_test_c_restart_with_running_worker_no_adoption_and_fencing() {
    use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

    use cy_kernel_api::{
        semantic, AuthorityCallContext, CleanupReport, DeviceBinding, EnforcementMode,
        HealthReport, HostInventoryProvider, InstalledPluginResolver, InventorySnapshot,
        KernelAuthority, LaunchPlan, NodeCapabilities, ProcessHandle, ProcessRuntime,
        ResolvedLaunchPlan, ResourceProvider, SandboxBackend, StopRequest, VerifiedInstallation,
    };
    use cy_kernel_daemon::{KernelDaemon, KernelServiceAdapter};
    use cy_resource_manager::InMemoryResourceManager;

    #[derive(Clone)]
    struct TestHardware {
        resource: semantic::Resource,
    }

    impl HostInventoryProvider for TestHardware {
        fn probe_inventory(&self) -> Result<InventorySnapshot, ProviderError> {
            Ok(InventorySnapshot {
                generation: 1,
                resources: vec![self.resource.clone()],
                capabilities: NodeCapabilities {
                    ready: true,
                    facts: Vec::new(),
                    enforcement: Vec::new(),
                },
            })
        }
    }

    impl ResourceProvider for TestHardware {
        fn adapter_id(&self) -> &str {
            "restart-hardware-c"
        }

        fn probe_resources(&self) -> Result<Vec<semantic::Resource>, ProviderError> {
            Ok(vec![self.resource.clone()])
        }

        fn create_binding(
            &self,
            resource: &semantic::Resource,
        ) -> Result<DeviceBinding, ProviderError> {
            Ok(DeviceBinding {
                resource_id: resource.identity.id.clone(),
                nodes: Vec::new(),
                environment: BTreeMap::new(),
                joinable_environment_keys: Default::default(),
                required_gids: Vec::new(),
                enforcement: EnforcementMode::ObserveOnly,
                adapter_id: self.adapter_id().to_string(),
                reason_code: "GOLDEN_C_BINDING".to_string(),
            })
        }

        fn read_health(&self, _resource_id: &str) -> Result<HealthReport, ProviderError> {
            Ok(HealthReport {
                healthy: Some(true),
                reason_code: "READY".to_string(),
                summary: "ready".to_string(),
            })
        }
    }

    struct TestWorkerResolver;

    impl InstalledPluginResolver for TestWorkerResolver {
        fn resolve_launch_plan(
            &self,
            _installation: &VerifiedInstallation,
            _instance_name: &str,
        ) -> Result<ResolvedLaunchPlan, ProviderError> {
            Err(ProviderError::new("golden-c", "UNUSED", "unused"))
        }

        fn resolve_worker_launch_plan(
            &self,
            worker: &semantic::Worker,
        ) -> Result<ResolvedLaunchPlan, ProviderError> {
            Ok(ResolvedLaunchPlan {
                installation: VerifiedInstallation {
                    installation_name: "golden-c".to_string(),
                    manifest_digest: "sha256:golden-c".to_string(),
                    artifact_digest: "sha256:golden-c".to_string(),
                    verified_signature_identity: "golden-c".to_string(),
                },
                plan: LaunchPlan {
                    instance_name: worker.identity.id.clone(),
                    executable: PathBuf::from("worker"),
                    args: Vec::new(),
                    environment: BTreeMap::new(),
                    cgroup_name: format!("instance-{}", worker.identity.id),
                    limits: Default::default(),
                    working_dir: None,
                    transport_socket: None,
                },
            })
        }
    }

    #[derive(Clone)]
    struct ControlledRecoverySandbox {
        observed: Arc<Mutex<Vec<RuntimeProcessEvidence>>>,
        reaped: Arc<Mutex<Vec<RuntimeProcessEvidence>>>,
    }

    impl ProcessRuntime for ControlledRecoverySandbox {
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
            let evidence = RuntimeProcessEvidence {
                cgroup_name: plan.cgroup_name.clone(),
                pid: 3030,
                start_time_ticks: 5000,
            };
            self.observed.lock().unwrap().push(evidence);
            Ok(ProcessHandle {
                pid: 3030,
                cgroup_path: PathBuf::from(format!("/test/{}", plan.cgroup_name)),
                start_time_ticks: Some(5000),
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
                reason_code: "CONTROLLED_STOP".to_string(),
            })
        }
    }

    impl SandboxBackend for ControlledRecoverySandbox {
        fn backend_id(&self) -> &str {
            "controlled-recovery-backend"
        }

        fn discover_recovery_processes(
            &self,
        ) -> Result<Vec<RuntimeProcessEvidence>, ProviderError> {
            Ok(self.observed.lock().unwrap().clone())
        }

        fn recover_stale_process(
            &self,
            evidence: &RuntimeProcessEvidence,
        ) -> Result<CleanupReport, ProviderError> {
            self.reaped.lock().unwrap().push(evidence.clone());
            Ok(CleanupReport {
                complete: true,
                exit_code: Some(0),
                oom_killed: false,
                conditions: Vec::new(),
                reason_code: "STALE_PROCESS_REAPED".to_string(),
            })
        }
    }

    let resource = semantic::Resource {
        identity: semantic::Identity {
            id: "res-golden-c".to_string(),
            generation: 1,
        },
        provider: semantic::Identity {
            id: "restart-hardware-c".to_string(),
            generation: 1,
        },
        resource_class: "accelerator".to_string(),
        capabilities: vec![semantic::Capability {
            id: "accelerator.compute".to_string(),
            revision: 1,
            properties: BTreeMap::new(),
        }],
        capacity: BTreeMap::new(),
        attributes: BTreeMap::new(),
        state: semantic::ResourceState::Ready,
        reason_code: "READY".to_string(),
        summary: "ready".to_string(),
        links: Vec::new(),
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

    let principal = semantic::Principal {
        identity: semantic::Identity {
            id: "principal-golden-c".to_string(),
            generation: 1,
        },
    };

    let context = |req: &str| AuthorityCallContext {
        contract: semantic::ContractRevision::current(),
        namespace: NamespaceId::default(),
        request_id: req.to_string(),
        idempotency_key: req.to_string(),
    };

    let directory = tempfile::tempdir().unwrap();
    let journal_path = directory.path().join("golden_c_runtime.jsonl");
    let journal = Arc::new(FileRuntimeJournal::open(&journal_path).unwrap());

    let sandbox = ControlledRecoverySandbox {
        observed: Arc::new(Mutex::new(Vec::new())),
        reaped: Arc::new(Mutex::new(Vec::new())),
    };

    // ==========================================
    // 1. Epoch N: Launch Worker, Lease, Endpoint
    // ==========================================
    // 中文：1. Epoch N：启动 Worker、Lease 和 Endpoint。
    let epoch_n = journal.begin_epoch("node-golden-c").unwrap();
    let hardware_n = Arc::new(TestHardware {
        resource: resource.clone(),
    });
    let resources_n = Arc::new(InMemoryResourceManager::with_next_fence_token(
        "node-golden-c",
        vec![resource.clone()],
        epoch_n.next_fence_token,
    ));
    let adapter_n = KernelServiceAdapter::new(
        Arc::new(KernelDaemon::new(
            hardware_n.clone(),
            hardware_n,
            resources_n,
            Arc::new(sandbox.clone()),
            "node-golden-c",
            epoch_n.node_epoch,
        )),
        Arc::new(TestWorkerResolver),
    )
    .with_runtime_journal(journal.clone())
    .with_event_store(journal.clone());

    let authority_n = adapter_n.authority();
    let worker_id = semantic::Identity {
        id: "worker-golden-c".to_string(),
        generation: 1,
    };

    let lease_n = authority_n
        .acquire_lease(
            &context("lease-c"),
            &principal,
            worker_id.clone(),
            query.clone(),
            now_millis() + 60_000,
        )
        .unwrap();
    let fence_n = lease_n.fence_token;

    let worker_n = semantic::Worker {
        identity: worker_id.clone(),
        principal: principal.identity.clone(),
        provider: resource.provider.clone(),
        lease: lease_n.identity.clone(),
        state: semantic::WorkerState::Registered,
        execution_ref: "exec-c".to_string(),
        limits: BTreeMap::new(),
    };
    authority_n
        .start_worker(&context("worker-c"), &principal, worker_n.clone())
        .unwrap();

    let endpoint_n = semantic::Endpoint {
        identity: semantic::Identity {
            id: "endpoint-golden-c".to_string(),
            generation: 1,
        },
        provider: resource.provider.clone(),
        owner: worker_id.clone(),
        transport: "transport.uds".to_string(),
        schema_id: "schema.v1".to_string(),
        capabilities: Vec::new(),
        public_attributes: BTreeMap::new(),
        connection_ref: "uds://runtime/direct-worker".to_string(),
        credential_ref: None,
    };
    authority_n
        .publish_endpoint(&context("ep-c"), &principal, endpoint_n.clone())
        .unwrap();

    let snapshot_n = authority_n
        .snapshot(&context("snap-c"), &principal)
        .unwrap();
    assert_eq!(snapshot_n.workers.len(), 1);
    assert_eq!(snapshot_n.leases.len(), 1);
    assert_eq!(snapshot_n.endpoints.len(), 1);
    let cursor_n = snapshot_n.cursor.clone();

    // ==========================================
    // 2. Kill Kernel Unexpectedly (Crash Simulation)
    // ==========================================
    // 中文：2. 意外杀死 Kernel（模拟崩溃）。
    drop(authority_n);
    drop(adapter_n);

    // ==========================================
    // 3. Restart in Epoch N+1 and Recover
    // ==========================================
    // 中文：3. 在 Epoch N+1 重启并执行恢复。
    let epoch_n_plus_one = journal.begin_epoch("node-golden-c").unwrap();
    assert!(
        epoch_n_plus_one.node_epoch > epoch_n.node_epoch,
        "Node epoch advanced"
    );
    assert!(
        epoch_n_plus_one.next_fence_token > fence_n,
        "Next fence token strictly advanced"
    );

    // Verify recovery classification:
    // Add a foreign process evidence to sandbox observations to verify foreign is NOT killed
    // 中文：验证恢复分类：向 sandbox observations 添加 foreign 进程证据，以确认不会杀死该进程。
    let foreign_evidence = RuntimeProcessEvidence {
        cgroup_name: "instance-foreign".to_string(),
        pid: 9999,
        start_time_ticks: 88888,
    };
    sandbox
        .observed
        .lock()
        .unwrap()
        .push(foreign_evidence.clone());

    let candidates =
        FileRuntimeJournal::classify_recovery(&epoch_n_plus_one, &sandbox.observed.lock().unwrap());
    let stale_cand = candidates
        .iter()
        .find(|c| c.classification == RecoveryClassification::Stale);
    let foreign_cand = candidates
        .iter()
        .find(|c| c.classification == RecoveryClassification::Foreign);
    assert!(
        stale_cand.is_some(),
        "Exact leftover evidence classified as Stale"
    );
    assert!(
        foreign_cand.is_some(),
        "Foreign process classified as Foreign"
    );

    // Stale process is reaped with exact evidence
    // 中文：使用精确证据 reap 过期进程。
    let exact_stale_evidence = stale_cand.unwrap().observed.clone().unwrap();
    sandbox
        .observed
        .lock()
        .unwrap()
        .retain(|e| e != &foreign_evidence); // remove foreign before startup gate | 中文：在启动门禁前移除外部证据
    journal
        .recover_before_listeners("node-golden-c", &epoch_n_plus_one, &sandbox)
        .unwrap();

    assert_eq!(
        *sandbox.reaped.lock().unwrap(),
        vec![exact_stale_evidence],
        "Only exact stale evidence was reaped"
    );

    // ==========================================
    // 4. Start Fresh Kernel in Epoch N+1
    // ==========================================
    // 中文：4. 在 Epoch N+1 启动全新的 Kernel。
    let hardware_n_plus_one = Arc::new(TestHardware {
        resource: resource.clone(),
    });
    let resources_n_plus_one = Arc::new(InMemoryResourceManager::with_next_fence_token(
        "node-golden-c",
        vec![resource.clone()],
        epoch_n_plus_one.next_fence_token,
    ));
    let adapter_n_plus_one = KernelServiceAdapter::new(
        Arc::new(KernelDaemon::new(
            hardware_n_plus_one.clone(),
            hardware_n_plus_one,
            resources_n_plus_one,
            Arc::new(sandbox),
            "node-golden-c",
            epoch_n_plus_one.node_epoch,
        )),
        Arc::new(TestWorkerResolver),
    )
    .with_runtime_journal(journal.clone())
    .with_event_store(journal);

    let authority_n_plus_one = adapter_n_plus_one.authority();

    // Verify old authority is NOT silently adopted
    // 中文：验证旧 authority 不会被静默接管。
    let fresh_snapshot = authority_n_plus_one
        .snapshot(&context("fresh-snap"), &principal)
        .unwrap();
    assert!(
        fresh_snapshot.workers.is_empty(),
        "Old worker must NOT be silently adopted"
    );
    assert!(
        fresh_snapshot.endpoints.is_empty(),
        "Old endpoint must NOT be silently adopted"
    );
    assert!(
        fresh_snapshot.leases.is_empty(),
        "Old lease must NOT be silently adopted"
    );

    // Verify old cursor gets SOURCE_CHANGED
    // 中文：验证旧 cursor 返回 SOURCE_CHANGED。
    let source_changed = authority_n_plus_one
        .read_events(&context("replay-old-cursor"), &principal, &cursor_n, 256)
        .unwrap();
    assert_eq!(
        source_changed.status,
        semantic::ReplayStatus::SourceChanged,
        "Replay from old epoch cursor must return SourceChanged"
    );
    assert!(source_changed.events.is_empty());

    // Verify replacement Fence is strictly newer
    // 中文：验证替代 Fence 严格更新。
    let replacement_lease = authority_n_plus_one
        .acquire_lease(
            &context("repl-lease"),
            &principal,
            semantic::Identity {
                id: "worker-golden-c-new".to_string(),
                generation: 1,
            },
            query,
            now_millis() + 60_000,
        )
        .unwrap();
    assert_eq!(replacement_lease.state, semantic::LeaseState::Active);
    assert!(
        replacement_lease.fence_token > fence_n,
        "Replacement fence must be strictly newer than pre-crash fence"
    );
    assert!(replacement_lease.fence_token >= epoch_n_plus_one.next_fence_token);
}

/// Golden Test D — PID reuse / stale evidence
///
/// Scenario:
/// Exercise or simulate matching PID with mismatched process-start identity
/// (e.g. matching PID but mismatched start_time_ticks or cgroup).
///
/// Verifies:
/// - Recovery classifies reality correctly (Foreign for live process, Unknown for old record);
/// - Recovery must not claim it as the old Worker;
/// - Foreign process is never killed / never reaped;
/// - Recovery fails closed rather than adopting or destroying foreign state.
/// 中文：黄金测试 D——PID 复用与过期证据。场景：模拟 PID 相同但进程启动身份不匹配的情况（例如 PID 相同，但 start_time_ticks 或 cgroup 不匹配）。验证恢复能正确分类现实状态（存活进程为 Foreign，旧记录为 Unknown）；恢复不得将其认作旧 Worker；绝不杀死或 reap Foreign 进程；并且必须 fail-closed，不能接管或销毁 Foreign 状态。
#[test]
fn golden_test_d_pid_reuse_stale_evidence_protects_foreign_process() {
    use cy_kernel_api::{
        CleanupReport, DeviceBinding, LaunchPlan, NodeCapabilities, ProcessHandle, ProcessRuntime,
        StopRequest,
    };

    let directory = tempfile::tempdir().unwrap();
    let journal_path = directory.path().join("golden_d_runtime.jsonl");
    let journal = FileRuntimeJournal::open(&journal_path).unwrap();

    // 1. Durably record a worker launch in epoch 1 with PID 4040, ticks 10_000
    // 中文：1. 在 epoch 1 中持久记录一个 Worker 启动，PID 为 4040、ticks 为 10_000。
    let epoch_1 = journal.begin_epoch("node-golden-d").unwrap();
    journal
        .append(RuntimeJournalRecord {
            event: RuntimeJournalEvent::InstanceLaunched,
            node_id: "node-golden-d".to_string(),
            node_epoch: epoch_1.node_epoch,
            instance_name: Some("worker-pid-reuse".to_string()),
            lease_name: Some("lease-pid-reuse".to_string()),
            fence_token: Some(42),
            reason_code: "LAUNCHED".to_string(),
            runtime_evidence: Some(RuntimeProcessEvidence {
                cgroup_name: "instance-worker-pid-reuse".to_string(),
                pid: 4040,
                start_time_ticks: 10_000,
            }),
        })
        .unwrap();

    // 2. Kernel restarts in epoch 2
    // 中文：2. Kernel 在 epoch 2 重启。
    let epoch_2 = journal.begin_epoch("node-golden-d").unwrap();
    assert_eq!(epoch_2.runtime_processes.len(), 1);
    assert_eq!(
        epoch_2.runtime_processes[0].instance_name,
        "worker-pid-reuse"
    );

    // 3. Observed in sandbox: an OS process with matching PID 4040, BUT ticks = 99_999 (PID reused!)
    // 中文：3. sandbox 观察到 OS 进程 PID 为 4040，但 ticks = 99_999（PID 已复用）。
    let reused_pid_evidence = RuntimeProcessEvidence {
        cgroup_name: "instance-worker-pid-reuse".to_string(),
        pid: 4040,
        start_time_ticks: 99_999, // Mismatched process start time! | 中文：进程启动时间不匹配！
    };

    // 4. Verify classification:
    // - Live reused process is classified as Foreign (NOT Stale!)
    // - Stale journal record is classified as Unknown
    // 中文：4. 验证分类：复用后的存活进程归类为 Foreign（不是 Stale）；旧 journal 记录归类为 Unknown。
    let candidates =
        FileRuntimeJournal::classify_recovery(&epoch_2, std::slice::from_ref(&reused_pid_evidence));
    assert_eq!(candidates.len(), 2);
    let foreign_cand = candidates
        .iter()
        .find(|c| c.classification == RecoveryClassification::Foreign);
    let unknown_cand = candidates
        .iter()
        .find(|c| c.classification == RecoveryClassification::Unknown);
    assert!(
        foreign_cand.is_some(),
        "Reused PID with mismatched start ticks must be classified as Foreign"
    );
    assert!(
        unknown_cand.is_some(),
        "Old record without exact live match must be classified as Unknown"
    );

    // 5. Test recovery execution:
    // 中文：5. 测试恢复执行。
    #[derive(Default)]
    struct MockPidReuseSandbox {
        reaped: Mutex<Vec<RuntimeProcessEvidence>>,
        observed: Vec<RuntimeProcessEvidence>,
    }

    impl ProcessRuntime for MockPidReuseSandbox {
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
            Err(ProviderError::new("mock", "UNUSED", "unused"))
        }
        fn stop(
            &self,
            _handle: &ProcessHandle,
            _request: &StopRequest,
        ) -> Result<CleanupReport, ProviderError> {
            Err(ProviderError::new("mock", "UNUSED", "unused"))
        }
    }

    impl SandboxBackend for MockPidReuseSandbox {
        fn backend_id(&self) -> &str {
            "mock-pid-reuse"
        }
        fn discover_recovery_processes(
            &self,
        ) -> Result<Vec<RuntimeProcessEvidence>, ProviderError> {
            Ok(self.observed.clone())
        }
        fn recover_stale_process(
            &self,
            evidence: &RuntimeProcessEvidence,
        ) -> Result<CleanupReport, ProviderError> {
            self.reaped.lock().unwrap().push(evidence.clone());
            Ok(CleanupReport {
                complete: true,
                exit_code: Some(0),
                oom_killed: false,
                conditions: Vec::new(),
                reason_code: "REAPED".to_string(),
            })
        }
    }

    let sandbox = MockPidReuseSandbox {
        reaped: Mutex::new(Vec::new()),
        observed: vec![reused_pid_evidence.clone()],
    };

    // Recovery MUST fail closed and MUST NOT reap the foreign reused PID process!
    // 中文：恢复必须 fail-closed，绝不能 reap 复用该 PID 的 Foreign 进程！
    let recovery_result = journal.recover_before_listeners("node-golden-d", &epoch_2, &sandbox);
    assert!(
        recovery_result.is_err(),
        "Recovery must fail closed on foreign/unmatched process"
    );
    let error = recovery_result.unwrap_err();
    assert!(
        matches!(
            error.reason_code.as_str(),
            "RECOVERY_FOREIGN_PROCESS" | "RECOVERY_UNKNOWN_PROCESS"
        ),
        "Error code must indicate unverified runtime state: {}",
        error.reason_code
    );

    // Verification: The foreign process was NEVER reaped!
    // 中文：验证：Foreign 进程从未被 reap！
    assert!(
        sandbox.reaped.lock().unwrap().is_empty(),
        "Recovery must NOT reap or terminate the foreign reused-PID process"
    );
}
