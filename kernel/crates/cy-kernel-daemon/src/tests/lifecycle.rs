//! Lifecycle, provider, operation, and lease transition tests.
//!
//! These tests cover the public Kernel lifecycle actions and their provider
//! and resource-manager boundaries.
//! 中文：生命周期、Provider、Operation 和 Lease 状态转换测试。覆盖公开 Kernel 生命周期操作，以及它们与 Provider 和 resource manager 的边界。

use super::*;

#[test]
fn heartbeat_requires_generation_and_monotonic_sequence() {
    use core_v1::plugin_lifecycle_service_server::PluginLifecycleService;
    let adapter = heartbeat_adapter();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let accepted = runtime
        .block_on(adapter.report_plugin_heartbeat(Request::new(
            core_v1::ReportPluginHeartbeatRequest {
                context: None,
                plugin_instance_name: "worker_1".to_string(),
                generation: 99,
                sequence_number: 1,
                observed_at: None,
                runtime_state: core_v1::PluginRuntimeState::Healthy as i32,
                health: None,
                restart_count: 0,
            },
        )))
        .unwrap()
        .into_inner();
    assert_eq!(
        accepted.disposition,
        core_v1::HeartbeatDisposition::Accepted as i32
    );
    let duplicate = runtime
        .block_on(adapter.report_plugin_heartbeat(Request::new(
            core_v1::ReportPluginHeartbeatRequest {
                context: None,
                plugin_instance_name: "worker_1".to_string(),
                generation: 99,
                sequence_number: 1,
                observed_at: None,
                runtime_state: core_v1::PluginRuntimeState::Healthy as i32,
                health: None,
                restart_count: 0,
            },
        )))
        .unwrap()
        .into_inner();
    assert_eq!(
        duplicate.disposition,
        core_v1::HeartbeatDisposition::Duplicate as i32
    );
    let stale = runtime
        .block_on(adapter.report_plugin_heartbeat(Request::new(
            core_v1::ReportPluginHeartbeatRequest {
                context: None,
                plugin_instance_name: "worker_1".to_string(),
                generation: 98,
                sequence_number: 2,
                observed_at: None,
                runtime_state: core_v1::PluginRuntimeState::Healthy as i32,
                health: None,
                restart_count: 0,
            },
        )))
        .unwrap()
        .into_inner();
    assert_eq!(
        stale.disposition,
        core_v1::HeartbeatDisposition::StaleGeneration as i32
    );
}

#[test]
fn worker_control_shutdown_waits_for_matching_ack() {
    let adapter = heartbeat_adapter();
    let (outbound, mut inbound) = mpsc::channel(1);
    let (connection_id, welcome) = adapter
        .register_worker_control(
            &core_v1::WorkerHello {
                plugin_instance_name: "worker_1".to_string(),
                generation: 99,
                protocol_version: 1,
            },
            outbound,
        )
        .unwrap();
    assert_eq!(
        welcome.desired_state,
        core_v1::DesiredPluginState::Running as i32
    );

    let acknowledger = {
        let adapter = adapter.clone();
        thread::spawn(move || {
            let frame = inbound
                .blocking_recv()
                .expect("Kernel must send Shutdown")
                .expect("Kernel control stream must remain healthy");
            let Some(core_v1::kernel_to_worker::Body::Shutdown(shutdown)) = frame.body else {
                panic!("expected WorkerShutdown frame");
            };
            adapter
                .accept_shutdown_ack(&core_v1::WorkerShutdownAck {
                    plugin_instance_name: "worker_1".to_string(),
                    generation: 99,
                    shutdown_id: shutdown.shutdown_id,
                    drained: true,
                    detail: "drained".to_string(),
                })
                .unwrap();
        })
    };

    assert!(adapter.request_worker_shutdown("worker_1", "TEST_STOP", false));
    acknowledger.join().unwrap();
    let instances = adapter.instances.lock().unwrap();
    let pending = instances["worker_1"].pending_shutdown.as_ref().unwrap();
    assert!(pending.acknowledged);
    assert!(pending.drained);
    drop(instances);
    adapter.unregister_worker_control("worker_1", 99, connection_id);
}

#[test]
fn semantic_worker_control_shutdown_is_fenced_and_acknowledged() {
    use core_v1::{
        kernel_authority_service_server::KernelAuthorityService, AcquireLeaseRequest,
        StartWorkerRequest,
    };

    let adapter = semantic_worker_adapter().with_worker_heartbeat(WorkerHeartbeatConfig {
        socket_path: PathBuf::from("/run/cyrene/worker.sock"),
        interval: Duration::from_secs(1),
        timeout: Duration::from_secs(2),
        graceful_stop: Duration::from_secs(1),
        shutdown_ack_timeout: Duration::from_millis(50),
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let lease = runtime
        .block_on(
            adapter.acquire_lease(authority_request(AcquireLeaseRequest {
                context: Some(authority_context("control-lease")),
                holder: Some(semantic_v1::Identity {
                    id: "worker-control-1".to_string(),
                    generation: 1,
                }),
                query: Some(semantic_v1::ResourceQuery {
                    resource_class: "accelerator".to_string(),
                    count: 1,
                    required_capabilities: vec![semantic_v1::CapabilityRequirement {
                        id: "accelerator.compute".to_string(),
                        minimum_revision: 1,
                        required_properties: Default::default(),
                    }],
                    minimum_capacity: Default::default(),
                }),
                ttl: Some(prost_types::Duration {
                    seconds: 30,
                    nanos: 0,
                }),
            })),
        )
        .unwrap()
        .into_inner();
    let worker = semantic_v1::Worker {
        identity: Some(semantic_v1::Identity {
            id: "worker-control-1".to_string(),
            generation: 1,
        }),
        principal: Some(semantic_v1::Identity {
            id: "principal-1".to_string(),
            generation: 1,
        }),
        provider: Some(semantic_v1::Identity {
            id: "provider-1".to_string(),
            generation: 1,
        }),
        lease: lease.identity.clone(),
        state: semantic_v1::WorkerState::Registered as i32,
        execution_ref: "opaque-execution-reference".to_string(),
        limits: Default::default(),
    };
    runtime
        .block_on(adapter.start_worker(authority_request(StartWorkerRequest {
            context: Some(authority_context("control-start")),
            worker: Some(worker.clone()),
        })))
        .unwrap();

    let (outbound, mut inbound) = mpsc::channel(1);
    let control_context = crate::convert::authority_call_context_from_proto(Some(
        &authority_context("control-connect"),
    ))
    .unwrap();
    let (connection_id, welcome) = adapter
        .register_semantic_worker_control(
            &control_context,
            &core_v1::WorkerControlHello {
                worker: worker.identity.clone(),
                lease: lease.identity.clone(),
                fence_token: lease.fence_token,
            },
            outbound,
        )
        .unwrap();
    assert_eq!(welcome.identity.id, "worker-control-1");

    let acknowledger = {
        let adapter = adapter.clone();
        let worker = worker.clone();
        let lease = lease.clone();
        let context = crate::convert::authority_call_context_from_proto(Some(&authority_context(
            "control-ack",
        )))
        .unwrap();
        thread::spawn(move || {
            let frame = inbound
                .blocking_recv()
                .expect("Kernel must send Shutdown")
                .expect("semantic Worker control stream must remain healthy");
            let Some(core_v1::kernel_to_worker_control::Body::Shutdown(shutdown)) = frame.body
            else {
                panic!("expected WorkerControlShutdown frame");
            };
            adapter
                .accept_semantic_shutdown_ack(
                    &context,
                    &core_v1::WorkerControlShutdownAck {
                        worker: worker.identity.clone(),
                        lease: lease.identity.clone(),
                        fence_token: lease.fence_token,
                        shutdown_id: shutdown.shutdown_id,
                        drained: true,
                    },
                )
                .unwrap();
        })
    };

    assert!(adapter.request_semantic_worker_shutdown("worker-control-1", "TEST_STOP"));
    acknowledger.join().unwrap();
    let instances = adapter.instances.lock().unwrap();
    let pending = instances["worker-control-1"]
        .pending_shutdown
        .as_ref()
        .unwrap();
    assert!(pending.acknowledged);
    assert!(pending.drained);
    drop(instances);
    adapter.unregister_semantic_worker_control("worker-control-1", 1, connection_id);
}

#[test]
fn watch_operations_replays_bounded_operation_and_runtime_events() {
    use core_v1::kernel_service_server::KernelService;

    let adapter = heartbeat_adapter();
    let operation = adapter.operation_running(
        "operations/launch-worker_1".to_string(),
        "worker_1".to_string(),
    );
    adapter.publish_runtime_event(
        core_v1::RuntimeEventType::WatchdogTriggered,
        "worker_1",
        "TEST_WATCHDOG",
        "test runtime event",
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let response = adapter
            .watch_operations(Request::new(core_v1::WatchOperationsRequest {
                context: None,
                operation_names: Vec::new(),
                resume_token: String::new(),
            }))
            .await
            .unwrap();
        let mut stream = response.into_inner();
        let first = stream.next().await.unwrap().unwrap();
        assert_eq!(first.operation.unwrap().name, operation.name);
        assert_eq!(first.sequence_number, 1);
        let second = stream.next().await.unwrap().unwrap();
        assert_eq!(
            second.runtime_event.unwrap().r#type,
            core_v1::RuntimeEventType::WatchdogTriggered as i32
        );
        assert_eq!(second.sequence_number, 2);
    });
}

#[test]
fn cancel_operation_reaps_worker_and_emits_terminal_operation() {
    use core_v1::kernel_service_server::KernelService;

    let journal = Arc::new(RecordingRuntimeJournal::default());
    let adapter = heartbeat_adapter().with_runtime_journal(journal.clone());
    let running = adapter.operation_running(
        "operations/launch-worker_1".to_string(),
        "worker_1".to_string(),
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let cancelled = runtime
        .block_on(
            adapter.cancel_operation(Request::new(core_v1::LegacyCancelOperationRequest {
                mutation: None,
                name: running.name.clone(),
            })),
        )
        .unwrap()
        .into_inner();
    assert_eq!(cancelled.state, core_v1::OperationState::Cancelled as i32);
    assert!(!adapter.instances.lock().unwrap().contains_key("worker_1"));
    let events = adapter.operation_events.lock().unwrap();
    assert!(events.iter().any(|event| {
        event.operation.as_ref().is_some_and(|operation| {
            operation.name == running.name
                && operation.state == core_v1::OperationState::Cancelled as i32
        })
    }));
    assert!(events.iter().any(|event| {
        event.runtime_event.as_ref().is_some_and(|runtime_event| {
            runtime_event.r#type == core_v1::RuntimeEventType::CleanupCompleted as i32
        })
    }));
    assert!(journal.records.lock().unwrap().iter().any(|record| {
        record.event == RuntimeJournalEvent::InstanceTerminated
            && record.instance_name.as_deref() == Some("worker_1")
            && record.reason_code == "CANCEL_COMPLETE"
    }));
}

#[test]
fn adapter_degraded_state_is_visible_on_managed_instances() {
    let adapter = heartbeat_adapter();
    adapter.adapter_available.store(false, Ordering::Relaxed);
    let instances = adapter.instances.lock().unwrap();
    let instance = to_plugin_instance(
        &adapter.daemon,
        "worker_1",
        &instances["worker_1"],
        adapter.adapter_available.load(Ordering::Relaxed),
    );
    let health = instance.health.unwrap();
    assert_eq!(health.status, core_v1::HealthStatus::Degraded as i32);
    assert_eq!(health.reason_code, "ADAPTER_DEGRADED");
}

#[test]
fn multi_adapter_bindings_preserve_provenance_without_relaxing_enforcement() {
    let merged = merge_bindings(vec![
        DeviceBinding {
            resource_id: "nvidia-0".to_string(),
            nodes: Vec::new(),
            environment: BTreeMap::new(),
            joinable_environment_keys: Default::default(),
            required_gids: vec![44],
            enforcement: EnforcementMode::Hard,
            adapter_id: "nvidia".to_string(),
            reason_code: "TEST".to_string(),
        },
        DeviceBinding {
            resource_id: "amd-0".to_string(),
            nodes: Vec::new(),
            environment: BTreeMap::new(),
            joinable_environment_keys: Default::default(),
            required_gids: vec![45],
            enforcement: EnforcementMode::Hard,
            adapter_id: "amd".to_string(),
            reason_code: "TEST".to_string(),
        },
    ])
    .unwrap();

    assert_eq!(merged.resource_id, "nvidia-0,amd-0");
    assert_eq!(merged.adapter_id, "amd,nvidia");
    assert_eq!(merged.required_gids, vec![44, 45]);
    assert_eq!(merged.enforcement, EnforcementMode::Hard);
}

#[test]
fn multi_adapter_bindings_reject_mixed_enforcement() {
    let error = merge_bindings(vec![
        DeviceBinding {
            resource_id: "nvidia-0".to_string(),
            nodes: Vec::new(),
            environment: BTreeMap::new(),
            joinable_environment_keys: Default::default(),
            required_gids: Vec::new(),
            enforcement: EnforcementMode::Hard,
            adapter_id: "nvidia".to_string(),
            reason_code: "TEST".to_string(),
        },
        DeviceBinding {
            resource_id: "virtual-0".to_string(),
            nodes: Vec::new(),
            environment: BTreeMap::new(),
            joinable_environment_keys: Default::default(),
            required_gids: Vec::new(),
            enforcement: EnforcementMode::VisibilityOnly,
            adapter_id: "virtual".to_string(),
            reason_code: "TEST".to_string(),
        },
    ])
    .unwrap_err();

    assert_eq!(error.reason_code, "MIXED_RESOURCE_ENFORCEMENT");
}

/// A lease must never become externally visible when the durable fence record
/// cannot be persisted. The journal write is authoritative: on failure the
/// in-memory acquisition is rolled back, leaving no active lease behind.
/// 中文：如果 durable Fence 记录无法持久化，Lease 绝不能对外可见。Journal 写入具有权威性：失败时必须回滚内存中的获取操作，不留下活动 Lease。
#[test]
fn acquire_lease_is_rolled_back_when_journal_write_fails() {
    use crate::convert::authority_lease_name;
    use core_v1::{kernel_authority_service_server::KernelAuthorityService, AcquireLeaseRequest};

    let adapter = semantic_lease_adapter().with_runtime_journal(Arc::new(FailingRuntimeJournal));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let result = runtime.block_on(
        adapter.acquire_lease(authority_request(AcquireLeaseRequest {
            context: Some(authority_context("rollback-journal-fail")),
            holder: Some(semantic_v1::Identity {
                id: "worker-1".to_string(),
                generation: 1,
            }),
            query: Some(semantic_v1::ResourceQuery {
                resource_class: "accelerator".to_string(),
                count: 1,
                required_capabilities: vec![semantic_v1::CapabilityRequirement {
                    id: "accelerator.compute".to_string(),
                    minimum_revision: 1,
                    required_properties: Default::default(),
                }],
                minimum_capacity: Default::default(),
            }),
            ttl: Some(prost_types::Duration {
                seconds: 30,
                nanos: 0,
            }),
        })),
    );
    assert!(
        result.is_err(),
        "acquire_lease must fail when the durable journal write fails"
    );

    // Rollback must have released the in-memory lease, so it is not externally
    // visible as an Active lease (its resource is freed) even though the
    // durable fence record was never persisted.
    // 中文：回滚必须释放内存中的 Lease；即使 durable Fence 记录没有持久化，它也不能作为 Active Lease 对外可见，且关联 Resource 已释放。
    let lease_name = authority_lease_name(&authority_context("rollback-journal-fail"));
    let rolled_back = adapter
        .daemon
        .lease(&lease_name)
        .expect("lease record should still exist after rollback");
    assert_eq!(
        rolled_back.state,
        cy_kernel_api::LeaseState::Released,
        "rolled-back lease must not remain Active"
    );
}

/// A release must fail closed: if the durable release record cannot be
/// persisted the in-memory lease is retained (we do not lose the evidence of
/// the acquisition). This journal double allows `LeaseAcquired` but fails
/// `LeaseReleased`, so a held lease survives a failed release attempt.
/// 中文：释放必须 fail-closed：durable release 记录无法持久化时，保留内存中的 Lease，不能丢失获取证据。此 journal double 允许 LeaseAcquired 记录，但会让 LeaseReleased 写入失败，因此释放失败后 Lease 仍被占用。
#[test]
fn release_lease_fails_closed_when_journal_write_fails() {
    use core_v1::{
        kernel_authority_service_server::KernelAuthorityService, AcquireLeaseRequest,
        ReleaseLeaseRequest,
    };

    #[derive(Default)]
    struct ReserveOkReleaseFailing {
        records: std::sync::Mutex<Vec<RuntimeJournalRecord>>,
    }
    impl RuntimeJournalSink for ReserveOkReleaseFailing {
        fn append(&self, record: RuntimeJournalRecord) -> Result<(), ProviderError> {
            if record.event == RuntimeJournalEvent::LeaseReleased {
                return Err(ProviderError::new(
                    "failing-journal",
                    "JOURNAL_WRITE_FAILED",
                    "injected durable release write failure",
                ));
            }
            self.records.lock().unwrap().push(record);
            Ok(())
        }
    }

    let adapter =
        semantic_lease_adapter().with_runtime_journal(Arc::new(ReserveOkReleaseFailing::default()));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let lease = runtime
        .block_on(
            adapter.acquire_lease(authority_request(AcquireLeaseRequest {
                context: Some(authority_context("release-journal-fail")),
                holder: Some(semantic_v1::Identity {
                    id: "worker-1".to_string(),
                    generation: 1,
                }),
                query: Some(semantic_v1::ResourceQuery {
                    resource_class: "accelerator".to_string(),
                    count: 1,
                    required_capabilities: vec![semantic_v1::CapabilityRequirement {
                        id: "accelerator.compute".to_string(),
                        minimum_revision: 1,
                        required_properties: Default::default(),
                    }],
                    minimum_capacity: Default::default(),
                }),
                ttl: Some(prost_types::Duration {
                    seconds: 30,
                    nanos: 0,
                }),
            })),
        )
        .unwrap()
        .into_inner();

    let release_result = runtime.block_on(adapter.release_lease(authority_request(
        ReleaseLeaseRequest {
            context: Some(authority_context("release-journal-fail")),
            lease: lease.identity.clone(),
            fence_token: lease.fence_token,
        },
    )));
    assert!(
        release_result.is_err(),
        "release_lease must fail when the durable journal write fails"
    );

    // Release authority is durable and cleanup is not yet confirmed, so the
    // lease must remain RELEASING and its resource unavailable. It must never
    // be reported as RELEASED when the terminal record cannot be persisted.
    // 中文：释放 authority 已持久化，但尚未确认清理完成，因此 Lease 必须保持 RELEASING，且其 Resource 不可使用。如果无法持久化终态记录，绝不能将其报告为 RELEASED。
    let still_held = adapter
        .daemon
        .lease(&lease.identity.as_ref().unwrap().id)
        .unwrap();
    assert_eq!(still_held.state, cy_kernel_api::LeaseState::Releasing);
}

#[test]
fn local_authority_owns_semantic_lease_transitions_without_tonic() {
    let adapter = semantic_lease_adapter();
    let authority = adapter.authority();
    let context = AuthorityCallContext {
        contract: semantic::ContractRevision::current(),
        namespace: NamespaceId::default(),
        request_id: "direct-authority".to_string(),
        idempotency_key: "direct-authority".to_string(),
    };
    let principal = principal_from_peer_cred(&AUTHORITY_TEST_PEER);
    let lease = authority
        .acquire_lease(
            &context,
            &principal,
            semantic::Identity {
                id: "direct-worker".to_string(),
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
        .expect("direct authority acquire should succeed");

    assert_eq!(lease.state, semantic::LeaseState::Active);
    assert_eq!(lease.holder.id, "direct-worker");
    assert_eq!(
        adapter.daemon.lease(&lease.identity.id).unwrap().state,
        cy_kernel_api::LeaseState::Active
    );
}

#[test]
fn provider_lifecycle_separates_session_snapshot_and_resource_generations() {
    let adapter = semantic_lease_adapter();
    let authority = adapter.authority();
    let principal = principal_from_peer_cred(&AUTHORITY_TEST_PEER);
    let context = scoped_authority_context("default", "provider-lifecycle");
    let provider_v1 = semantic_provider("test-provider", 1, semantic::ProviderState::Ready);
    authority
        .register_provider(&context, &principal, provider_v1.clone())
        .unwrap();

    let mut resource = test_resource();
    resource.identity.generation = 37;
    resource.provider = provider_v1.identity.clone();
    let snapshot_v4 = provider_snapshot(&provider_v1, 4, vec![resource.clone()], Vec::new());
    authority
        .publish_inventory(&context, &principal, snapshot_v4.clone())
        .unwrap();
    let first = authority
        .reconcile_provider(&context, &principal, &provider_v1.identity)
        .unwrap();
    assert_eq!(first.snapshot_generation, 4);
    assert_eq!(
        first.actions,
        vec![ProviderReconcileAction::RefreshResource(
            resource.identity.clone()
        )]
    );
    assert_eq!(
        authority
            .reconcile_provider(&context, &principal, &provider_v1.identity)
            .unwrap()
            .actions,
        vec![ProviderReconcileAction::Noop]
    );

    let provider_v2 = semantic_provider("test-provider", 2, semantic::ProviderState::Ready);
    authority
        .register_provider(&context, &principal, provider_v2.clone())
        .unwrap();
    assert_eq!(
        authority
            .publish_inventory(&context, &principal, snapshot_v4)
            .unwrap_err()
            .reason_code,
        "STALE_GENERATION"
    );
    let snapshot_v1_after_reconnect = provider_snapshot(
        &provider_v2,
        1,
        vec![semantic::Resource {
            provider: provider_v2.identity.clone(),
            ..resource.clone()
        }],
        Vec::new(),
    );
    authority
        .publish_inventory(&context, &principal, snapshot_v1_after_reconnect)
        .unwrap();

    let records = authority.runtime.providers.lock().unwrap();
    let record = records
        .get(&(NamespaceId::default(), "test-provider".to_string()))
        .unwrap();
    assert_eq!(record.provider.identity.generation, 2);
    assert_eq!(record.inventory.as_ref().unwrap().snapshot_generation, 1);
    assert_eq!(
        record.inventory.as_ref().unwrap().resources[0]
            .identity
            .generation,
        37
    );
}

#[test]
fn hardware_adapters_publish_separate_resource_only_provider_snapshots() {
    let mut resource_a = test_resource_with_id("resource-a");
    resource_a.identity.generation = 17;
    let mut resource_b = test_resource_with_id("resource-b");
    resource_b.identity.generation = 29;
    let adapter = hardware_provider_adapter(vec![
        (
            "adapter-a".to_string(),
            Arc::new(SnapshotHardware {
                generation: 41,
                resources: vec![resource_a.clone()],
            }) as Arc<dyn HardwareAdapter>,
        ),
        (
            "adapter-b".to_string(),
            Arc::new(SnapshotHardware {
                generation: 58,
                resources: vec![resource_b.clone()],
            }) as Arc<dyn HardwareAdapter>,
        ),
    ])
    .with_adapter_poll_interval(Duration::from_secs(3));

    adapter.sync_hardware_provider_facts().unwrap();
    let authority = adapter.authority();
    let records = authority.runtime.providers.lock().unwrap();
    for (adapter_id, resource, snapshot_generation) in
        [("adapter-a", resource_a, 41), ("adapter-b", resource_b, 58)]
    {
        let record = records
            .get(&(NamespaceId::default(), adapter_id.to_string()))
            .unwrap();
        assert_eq!(record.provider.identity.generation, 9);
        assert_eq!(record.provider.state, semantic::ProviderState::Ready);
        let snapshot = record.inventory.as_ref().unwrap();
        assert_eq!(snapshot.snapshot_generation, snapshot_generation);
        assert_eq!(snapshot.resources.len(), 1);
        assert_eq!(
            snapshot.resources[0].identity.generation,
            resource.identity.generation
        );
        assert_eq!(snapshot.resources[0].provider, record.provider.identity);
        assert!(snapshot.workers.is_empty());
        assert!(snapshot.endpoints.is_empty());
        assert!(snapshot.expires_at_unix_ms > snapshot.sampled_at_unix_ms);
        assert!(snapshot.expires_at_unix_ms - snapshot.sampled_at_unix_ms <= 6_000);
    }
    drop(records);
    assert_eq!(
        adapter.daemon.resources.inventory().generation,
        1,
        "the allocation ledger retains its independent aggregate generation"
    );

    // Fresh unchanged observations must advance publication and retain resource identity.
    // 硬件未变化的新采样仍需刷新发布代次及有效期,但不能更换资源身份。
    std::thread::sleep(Duration::from_millis(2));
    adapter.sync_hardware_provider_facts().unwrap();
    let records = authority.runtime.providers.lock().unwrap();
    for (id, generation, resource_generation) in [("adapter-a", 41, 17), ("adapter-b", 58, 29)] {
        let record = records
            .get(&(NamespaceId::default(), id.to_string()))
            .unwrap();
        let snapshot = record.inventory.as_ref().unwrap();
        assert!(snapshot.snapshot_generation > generation);
        assert_eq!(
            snapshot.resources[0].identity.generation,
            resource_generation
        );
        assert_eq!(record.provider.state, semantic::ProviderState::Ready);
    }
}

#[test]
fn unavailable_hardware_adapter_does_not_hide_other_provider_facts() {
    let adapter = hardware_provider_adapter(vec![
        (
            "adapter-good".to_string(),
            Arc::new(TestHardware {
                resources: vec![test_resource_with_id("resource-good")],
            }) as Arc<dyn HardwareAdapter>,
        ),
        (
            "adapter-failed".to_string(),
            Arc::new(FailingHardware) as Arc<dyn HardwareAdapter>,
        ),
    ]);

    adapter.sync_hardware_provider_facts().unwrap();
    let authority = adapter.authority();
    let records = authority.runtime.providers.lock().unwrap();
    let available = records
        .get(&(NamespaceId::default(), "adapter-good".to_string()))
        .unwrap();
    assert_eq!(available.provider.state, semantic::ProviderState::Ready);
    assert!(available.inventory.is_some());
    let unavailable = records
        .get(&(NamespaceId::default(), "adapter-failed".to_string()))
        .unwrap();
    assert_eq!(
        unavailable.provider.state,
        semantic::ProviderState::Unavailable
    );
    assert!(unavailable.inventory.is_none());
}

#[test]
fn resource_facts_reconciliation_never_owns_workers_or_leases() {
    let adapter = semantic_worker_adapter();
    let authority = adapter.authority();
    let principal = principal_from_peer_cred(&AUTHORITY_TEST_PEER);
    let context = scoped_authority_context("default", "resource-facts-only");
    let provider = semantic_provider("test-provider", 7, semantic::ProviderState::Ready);
    authority
        .register_resource_facts_provider(&context, &principal, provider.clone())
        .unwrap();
    let worker_identity = semantic::Identity {
        id: "hardware-worker".to_string(),
        generation: 1,
    };
    let lease = authority
        .acquire_lease(
            &context,
            &principal,
            worker_identity.clone(),
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
            future_expiry(),
        )
        .unwrap();
    let worker = semantic_worker_for(
        &worker_identity.id,
        provider.identity.clone(),
        lease.identity.clone(),
        semantic::WorkerState::Registered,
    );
    authority
        .start_worker(&context, &principal, worker.clone())
        .unwrap();
    let mut resource = test_resource();
    resource.provider = provider.identity.clone();
    authority
        .publish_inventory(
            &context,
            &principal,
            provider_snapshot(&provider, 1, vec![resource.clone()], Vec::new()),
        )
        .unwrap();

    assert_eq!(
        authority
            .reconcile_provider(&context, &principal, &provider.identity)
            .unwrap()
            .actions,
        vec![ProviderReconcileAction::RefreshResource(resource.identity)]
    );
    assert_eq!(
        adapter.daemon.lease(&lease.identity.id).unwrap().state,
        cy_kernel_api::LeaseState::Active
    );
    assert_ne!(
        adapter.instances.lock().unwrap()[&worker.identity.id]
            .semantic_worker
            .as_ref()
            .unwrap()
            .state,
        semantic::WorkerState::Lost
    );
}

#[test]
fn provider_unavailability_is_scoped_to_its_logical_identity() {
    let adapter = semantic_lease_adapter();
    let authority = adapter.authority();
    let principal = principal_from_peer_cred(&AUTHORITY_TEST_PEER);
    let context = scoped_authority_context("default", "provider-isolation");
    let provider_a = semantic_provider("provider-a", 1, semantic::ProviderState::Ready);
    let provider_b = semantic_provider("provider-b", 1, semantic::ProviderState::Ready);
    authority
        .register_provider(&context, &principal, provider_a.clone())
        .unwrap();
    authority
        .register_provider(&context, &principal, provider_b.clone())
        .unwrap();

    let provider_a_lost = semantic_provider("provider-a", 1, semantic::ProviderState::Unavailable);
    authority
        .register_provider(&context, &principal, provider_a_lost.clone())
        .unwrap();
    assert_eq!(
        authority
            .register_provider(&context, &principal, provider_a)
            .unwrap_err()
            .reason_code,
        "STALE_GENERATION"
    );
    let records = authority.runtime.providers.lock().unwrap();
    assert_eq!(
        records
            .get(&(NamespaceId::default(), "provider-a".to_string()))
            .unwrap()
            .provider
            .state,
        semantic::ProviderState::Unavailable
    );
    assert_eq!(
        records
            .get(&(NamespaceId::default(), "provider-b".to_string()))
            .unwrap()
            .provider
            .state,
        semantic::ProviderState::Ready
    );
}

#[test]
fn missing_worker_reconcile_revokes_authority_and_fences_old_worker() {
    let adapter = semantic_worker_adapter()
        .with_runtime_journal(Arc::new(RecordingRuntimeJournal::default()));
    let authority = adapter.authority();
    let principal = principal_from_peer_cred(&AUTHORITY_TEST_PEER);
    let context = scoped_authority_context("default", "worker-a-lease");
    let worker_a = semantic::Identity {
        id: "worker-a".to_string(),
        generation: 1,
    };
    let lease_a = authority
        .acquire_lease(
            &context,
            &principal,
            worker_a.clone(),
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
            future_expiry(),
        )
        .unwrap();
    let provider = semantic_provider("test-provider", 1, semantic::ProviderState::Ready);
    authority
        .register_provider(&context, &principal, provider.clone())
        .unwrap();
    let worker = semantic_worker_for(
        "worker-a",
        provider.identity.clone(),
        lease_a.identity.clone(),
        semantic::WorkerState::Registered,
    );
    authority
        .start_worker(&context, &principal, worker.clone())
        .unwrap();
    authority
        .report_heartbeat(
            &context,
            &principal,
            &worker.identity,
            &lease_a.identity,
            lease_a.fence_token,
        )
        .unwrap();
    let (cancel_sender, mut cancel_receiver) = mpsc::channel(1);
    adapter
        .instances
        .lock()
        .unwrap()
        .get_mut("worker-a")
        .unwrap()
        .actor
        .attach_transport_channel(cancel_sender);
    let cancellable_operation = semantic::Operation {
        identity: semantic::Identity {
            id: "operation-cancel-worker-a".to_string(),
            generation: 1,
        },
        owner: principal.identity.clone(),
        executor: worker.identity.clone(),
        kind: "worker.invoke".to_string(),
        state: semantic::OperationState::Created,
        deadline_unix_ms: None,
        parent: None,
        metadata: BTreeMap::new(),
    };
    authority
        .create_operation(&context, &principal, cancellable_operation.clone())
        .unwrap();
    authority
        .report_operation(
            &context,
            &principal,
            semantic::Operation {
                state: semantic::OperationState::Running,
                ..cancellable_operation.clone()
            },
        )
        .unwrap();
    assert_eq!(
        authority
            .cancel_operation(&context, &principal, &cancellable_operation.identity)
            .unwrap()
            .state,
        semantic::OperationState::Cancelling
    );
    match cancel_receiver.try_recv().unwrap() {
        WorkerTransportCommand::Cancel {
            cancel_request_id,
            target_request_id,
            generation,
            fence_token,
        } => {
            assert!(cancel_request_id.starts_with("cancel-"));
            assert_eq!(target_request_id, cancellable_operation.identity.id);
            assert_eq!(generation, 1);
            assert_eq!(fence_token, lease_a.fence_token);
        }
        WorkerTransportCommand::Request(_) => panic!("cancellation must reach the worker executor"),
    }
    assert_eq!(
        authority
            .report_operation(
                &context,
                &principal,
                semantic::Operation {
                    state: semantic::OperationState::Cancelled,
                    ..cancellable_operation.clone()
                },
            )
            .unwrap()
            .state,
        semantic::OperationState::Cancelled
    );
    let operation = semantic::Operation {
        identity: semantic::Identity {
            id: "operation-worker-a".to_string(),
            generation: 1,
        },
        owner: principal.identity.clone(),
        executor: worker.identity.clone(),
        kind: "worker.invoke".to_string(),
        state: semantic::OperationState::Created,
        deadline_unix_ms: None,
        parent: None,
        metadata: BTreeMap::new(),
    };
    authority
        .create_operation(&context, &principal, operation.clone())
        .unwrap();
    authority
        .report_operation(
            &context,
            &principal,
            semantic::Operation {
                state: semantic::OperationState::Running,
                ..operation.clone()
            },
        )
        .unwrap();
    let endpoint = semantic_endpoint_for(&worker);
    authority
        .publish_endpoint(&context, &principal, endpoint.clone())
        .unwrap();

    let mut resource = test_resource();
    resource.provider = provider.identity.clone();
    let stale_worker = semantic_worker_for(
        "worker-stale",
        provider.identity.clone(),
        semantic::Identity {
            id: "lease-stale".to_string(),
            generation: 1,
        },
        semantic::WorkerState::Running,
    );
    authority
        .publish_inventory(
            &context,
            &principal,
            provider_snapshot(
                &provider,
                1,
                vec![resource.clone()],
                vec![
                    semantic::Worker {
                        state: semantic::WorkerState::Running,
                        ..worker.clone()
                    },
                    stale_worker.clone(),
                ],
            ),
        )
        .unwrap();
    let initial_reconciliation = authority
        .reconcile_provider(&context, &principal, &provider.identity)
        .unwrap();
    assert!(initial_reconciliation.actions.contains(
        &ProviderReconcileAction::TerminateStaleWorker(stale_worker.identity.clone())
    ));

    authority
        .publish_inventory(
            &context,
            &principal,
            provider_snapshot(&provider, 2, vec![resource], Vec::new()),
        )
        .unwrap();
    authority
        .confirm_stale_worker_termination(
            &context,
            &principal,
            &provider.identity,
            1,
            &stale_worker.identity,
            true,
        )
        .unwrap();
    let reconciliation = authority
        .reconcile_provider(&context, &principal, &provider.identity)
        .unwrap();
    assert!(reconciliation
        .actions
        .contains(&ProviderReconcileAction::MarkWorkerLost(
            worker.identity.clone()
        )));
    assert!(reconciliation
        .actions
        .contains(&ProviderReconcileAction::RevokeLease(
            lease_a.identity.clone()
        )));
    assert!(reconciliation
        .actions
        .contains(&ProviderReconcileAction::MarkOperationLost(
            operation.identity.clone()
        )));
    assert!(reconciliation
        .actions
        .contains(&ProviderReconcileAction::RevokeEndpoint(
            endpoint.identity.clone()
        )));
    assert_eq!(
        adapter.instances.lock().unwrap()["worker-a"]
            .semantic_worker
            .as_ref()
            .unwrap()
            .state,
        semantic::WorkerState::Lost
    );
    let revoked = adapter.daemon.lease(&lease_a.identity.id).unwrap();
    assert_eq!(revoked.state, cy_kernel_api::LeaseState::Revoked);
    assert!(revoked.fence_token > lease_a.fence_token);
    assert_eq!(
        authority.runtime.semantic_operations.lock().unwrap()
            [&context.object_ref(operation.identity.clone())]
            .state,
        semantic::OperationState::Lost
    );
    assert!(!authority
        .runtime
        .endpoints
        .lock()
        .unwrap()
        .contains_key(&context.object_ref(endpoint.identity.clone())));

    assert_eq!(
        authority
            .report_heartbeat(
                &context,
                &principal,
                &worker.identity,
                &lease_a.identity,
                lease_a.fence_token,
            )
            .unwrap_err()
            .reason_code,
        "FENCE_MISMATCH"
    );
    assert_eq!(
        authority
            .renew_lease(
                &context,
                &principal,
                &lease_a.identity,
                lease_a.fence_token,
                future_expiry() + 60_000,
            )
            .unwrap_err()
            .reason_code,
        "STALE_FENCE_TOKEN"
    );
    assert!(authority
        .publish_endpoint(&context, &principal, endpoint)
        .is_err());
    assert!(authority
        .verify_worker_control(
            &context,
            &worker.identity,
            &lease_a.identity,
            lease_a.fence_token,
        )
        .is_err());

    let lease_b = authority
        .acquire_lease(
            &scoped_authority_context("default", "worker-b-lease"),
            &principal,
            semantic::Identity {
                id: "worker-b".to_string(),
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
            future_expiry(),
        )
        .unwrap();
    assert!(lease_b.fence_token > lease_a.fence_token);
    assert_eq!(
        authority
            .reconcile_provider(&context, &principal, &provider.identity)
            .unwrap()
            .actions,
        vec![ProviderReconcileAction::Noop]
    );
    let events = authority
        .read_events(
            &context,
            &principal,
            &semantic::EventCursor {
                source: authority.semantic_event_source(),
                sequence: 0,
            },
            256,
        )
        .unwrap();
    assert!(events
        .events
        .iter()
        .any(|event| event.kind == "worker.lost"));
    assert!(events
        .events
        .iter()
        .any(|event| event.kind == "lease.revoked"));
    assert!(events
        .events
        .iter()
        .any(|event| event.kind == "operation.lost"));
    assert!(events
        .events
        .iter()
        .any(|event| event.kind == "endpoint.revoked"));
    let snapshot = authority.snapshot(&context, &principal).unwrap();
    assert_eq!(snapshot.source, authority.semantic_event_source());
    assert_eq!(snapshot.cursor.sequence, events.latest_available_sequence);
    assert_eq!(snapshot.workers[0].state, semantic::WorkerState::Lost);
    assert_eq!(snapshot.leases[0].state, semantic::LeaseState::Revoked);
    assert_eq!(
        snapshot
            .operations
            .iter()
            .find(|current| current.identity == operation.identity)
            .unwrap()
            .state,
        semantic::OperationState::Lost
    );
    assert!(snapshot.endpoints.is_empty());
}

#[test]
fn owned_startup_failure_keeps_resource_unavailable_when_release_cannot_complete() {
    use core_v1::{kernel_authority_service_server::KernelAuthorityService, AcquireLeaseRequest};

    #[derive(Default)]
    struct TerminalReleaseJournal;

    impl RuntimeJournalSink for TerminalReleaseJournal {
        fn append(&self, record: RuntimeJournalRecord) -> Result<(), ProviderError> {
            if record.event == RuntimeJournalEvent::LeaseReleased {
                return Err(ProviderError::new(
                    "failing-journal",
                    "JOURNAL_WRITE_FAILED",
                    "startup cleanup terminal record cannot be persisted",
                ));
            }
            Ok(())
        }
    }

    let adapter = semantic_lease_adapter().with_runtime_journal(Arc::new(TerminalReleaseJournal));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let request = |request_id: &str| AcquireLeaseRequest {
        context: Some(authority_context(request_id)),
        holder: Some(semantic_v1::Identity {
            id: format!("worker-{request_id}"),
            generation: 1,
        }),
        query: Some(semantic_v1::ResourceQuery {
            resource_class: "accelerator".to_string(),
            count: 1,
            required_capabilities: vec![semantic_v1::CapabilityRequirement {
                id: "accelerator.compute".to_string(),
                minimum_revision: 1,
                required_properties: Default::default(),
            }],
            minimum_capacity: Default::default(),
        }),
        ttl: Some(prost_types::Duration {
            seconds: 30,
            nanos: 0,
        }),
    };
    let lease = runtime
        .block_on(adapter.acquire_lease(authority_request(request("startup-failure"))))
        .unwrap()
        .into_inner();
    let internal = adapter
        .daemon
        .lease(&lease.identity.as_ref().unwrap().id)
        .unwrap();

    let failure = adapter.release_owned_lease(true, &internal).unwrap_err();
    assert_eq!(failure.reason_code, "JOURNAL_WRITE_FAILED");
    assert_eq!(
        adapter.daemon.lease(&internal.name).unwrap().state,
        cy_kernel_api::LeaseState::Failed,
        "failed startup cleanup must retain the allocation"
    );
    assert!(runtime
        .block_on(adapter.acquire_lease(authority_request(request("replacement"))))
        .is_err());
}

/// End-to-end proof of the two-phase release invariant: a resource whose
/// instance cannot be physically reaped must never reach `RELEASED`. The
/// legacy `ReleaseLease` RPC must fail closed with `CLEANUP_INCOMPLETE`, the
/// lease must be left `FAILED` (allocation still held), and the half-cleaned
/// resource must NOT be handed to a replacement lease.
///
/// This exercises the real `release_lease` gRPC path (not just the port),
/// driving it through the shared `release_lease_with_cleanup` helper that now
/// gates every legacy release on confirmed physical cleanup.
/// 中文：端到端证明两阶段 release 不变量：无法物理 reap 其 instance 的 Resource 绝不能进入 RELEASED。旧版 ReleaseLease RPC 必须以 CLEANUP_INCOMPLETE fail-closed，Lease 保持 FAILED（allocation 仍被占用），半清理的 Resource 不能交给替代 Lease。该测试调用真实 release_lease gRPC 路径，而不只是端口；通过共享的 release_lease_with_cleanup helper，确认所有旧版 release 都受物理清理结果约束。
#[test]
fn uncleaned_resource_cannot_be_reacquired_after_failed_release() {
    use crate::watchdog::{InstanceActor, InstanceActorState};
    use core_v1::kernel_service_server::KernelService;
    use cy_kernel_api::{
        CgroupLimits, DeviceBinding, EnforcementMode, LaunchPlan, ProcessCondition, ProcessHandle,
        ProcessRuntime, SandboxBackend, StopRequest,
    };
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    /// Sandbox whose `stop` reports an incomplete cleanup: the instance is
    /// stuck (e.g. an uninterruptible process) and must be quarantined, never
    /// silently released back to the pool.
    /// 中文：Sandbox 的 stop 报告清理未完成：instance 卡住（例如不可中断进程）时必须进入 quarantine，绝不能静默释放回资源池。
    struct StuckSandbox;

    impl ProcessRuntime for StuckSandbox {
        fn preflight(&self) -> cy_kernel_api::NodeCapabilities {
            cy_kernel_api::NodeCapabilities {
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
        ) -> Result<cy_kernel_api::CleanupReport, ProviderError> {
            Ok(cy_kernel_api::CleanupReport {
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

    impl SandboxBackend for StuckSandbox {
        fn backend_id(&self) -> &str {
            "stuck-test"
        }
    }

    let adapter = semantic_lease_adapter();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    // Acquire the sole resource through the legacy KernelService RPC.
    // 中文：通过旧版 KernelService RPC 获取唯一的 Resource。
    let lease = runtime
        .block_on(
            adapter.acquire_lease(Request::new(core_v1::LegacyAcquireLeaseRequest {
                mutation: Some(core_v1::MutationContext {
                    request: Some(core_v1::RequestContext {
                        request_id: "e2e-release".to_string(),
                        ..Default::default()
                    }),
                    idempotency_key: "e2e-release".to_string(),
                    expected_generation: Some(1),
                }),
                node: Some(core_v1::NodeRef {
                    node_id: "node".to_string(),
                    node_epoch: 7,
                }),
                holder: Some(semantic_v1::Identity {
                    id: "worker-e2e".to_string(),
                    generation: 1,
                }),
                query: Some(semantic_v1::ResourceQuery {
                    resource_class: "accelerator".to_string(),
                    count: 1,
                    required_capabilities: vec![semantic_v1::CapabilityRequirement {
                        id: "accelerator.compute".to_string(),
                        minimum_revision: 1,
                        required_properties: Default::default(),
                    }],
                    minimum_capacity: Default::default(),
                }),
                ttl: Some(prost_types::Duration {
                    seconds: 30,
                    nanos: 0,
                }),
                cpu: None,
                memory: None,
            })),
        )
        .unwrap()
        .into_inner();

    let lease_identity = lease
        .identity
        .clone()
        .expect("acquired lease has an identity");
    let fence_token = lease.fence_token;

    // Register a running instance bound to the acquired lease, backed by a
    // sandbox that reports an incomplete cleanup when stopped.
    // 中文：注册一个绑定到已获取 Lease 的运行中 instance；其 sandbox 在停止时会报告清理未完成。
    let mut actor = InstanceActor::new(
        "stuck-instance",
        lease_identity.id.clone(),
        fence_token,
        Arc::new(StuckSandbox),
        LaunchPlan {
            instance_name: "stuck-instance".to_string(),
            executable: PathBuf::from("/bin/true"),
            args: Vec::new(),
            environment: BTreeMap::new(),
            cgroup_name: "stuck-instance".to_string(),
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
    );
    actor.start().expect("stuck instance must start");
    assert_eq!(actor.state(), InstanceActorState::Healthy);

    let mut process = managed_test_process(
        "stuck-instance",
        fence_token,
        Some(core_v1::ResourceLeaseRef {
            lease_name: lease_identity.id.clone(),
            fence_token,
        }),
    );
    process.actor = actor;
    adapter
        .instances
        .lock()
        .unwrap()
        .insert("stuck-instance".to_string(), process);

    // Release through the legacy RPC: because the instance cannot be reaped,
    // `release_lease_with_cleanup` must fail closed with CLEANUP_INCOMPLETE.
    // 中文：通过旧版 RPC 释放：由于 instance 无法 reap，release_lease_with_cleanup 必须以 CLEANUP_INCOMPLETE fail-closed。
    let release = runtime.block_on(adapter.release_lease(Request::new(
        core_v1::LegacyReleaseLeaseRequest {
            mutation: None,
            lease: Some(lease_identity.clone()),
            fence_token,
        },
    )));
    assert!(
        release.is_err(),
        "release must fail when physical cleanup is incomplete"
    );
    assert_eq!(
        release
            .unwrap_err()
            .metadata()
            .get("x-cyrene-reason-code")
            .and_then(|value| value.to_str().ok()),
        Some("CLEANUP_INCOMPLETE"),
        "a failed physical cleanup must surface CLEANUP_INCOMPLETE"
    );

    // The lease must be FAILED, never RELEASED: the allocation is still held
    // so it cannot be handed to a replacement while physically dirty.
    // 中文：Lease 必须进入 FAILED，绝不能进入 RELEASED：allocation 仍被占用，不能在物理状态尚未清理时交给替代 Lease。
    let held = adapter.daemon.lease(&lease_identity.id).unwrap();
    assert_eq!(
        held.state,
        cy_kernel_api::LeaseState::Failed,
        "a lease whose cleanup could not be confirmed must remain FAILED, not RELEASED"
    );

    // A FAILED legacy lease cannot be released after its actor disappears:
    // without a managed actor, a same-fence retry has no physical cleanup
    // proof and must keep the allocation held.
    // 中文：actor 消失后，FAILED 的旧版 Lease 不能被释放：没有受管理的 actor，同一 Fence 的重试无法提供物理清理证明，因此必须继续占用 allocation。
    adapter.instances.lock().unwrap().remove("stuck-instance");
    let missing_actor_retry = runtime.block_on(adapter.release_lease(Request::new(
        core_v1::LegacyReleaseLeaseRequest {
            mutation: None,
            lease: Some(lease_identity.clone()),
            fence_token,
        },
    )));
    assert_eq!(
        missing_actor_retry
            .unwrap_err()
            .metadata()
            .get("x-cyrene-reason-code")
            .and_then(|value| value.to_str().ok()),
        Some("CLEANUP_INCOMPLETE"),
        "a FAILED lease without an actor must fail closed"
    );
    assert_eq!(
        adapter.daemon.lease(&lease_identity.id).unwrap().state,
        cy_kernel_api::LeaseState::Failed,
        "a missing actor must not make a FAILED lease reusable"
    );

    // The half-cleaned resource must NOT be reacquired by another lease.
    // 中文：半清理的 Resource 不得由其他 Lease 重新获取。
    let reacquire = runtime.block_on(adapter.acquire_lease(Request::new(
        core_v1::LegacyAcquireLeaseRequest {
            mutation: Some(core_v1::MutationContext {
                request: Some(core_v1::RequestContext {
                    request_id: "e2e-reacquire".to_string(),
                    ..Default::default()
                }),
                idempotency_key: "e2e-reacquire".to_string(),
                expected_generation: Some(1),
            }),
            node: Some(core_v1::NodeRef {
                node_id: "node".to_string(),
                node_epoch: 7,
            }),
            holder: Some(semantic_v1::Identity {
                id: "worker-replacement".to_string(),
                generation: 1,
            }),
            query: Some(semantic_v1::ResourceQuery {
                resource_class: "accelerator".to_string(),
                count: 1,
                required_capabilities: vec![semantic_v1::CapabilityRequirement {
                    id: "accelerator.compute".to_string(),
                    minimum_revision: 1,
                    required_properties: Default::default(),
                }],
                minimum_capacity: Default::default(),
            }),
            ttl: Some(prost_types::Duration {
                seconds: 30,
                nanos: 0,
            }),
            cpu: None,
            memory: None,
        },
    )));
    assert!(
        reacquire.is_err(),
        "an incompletely cleaned resource must not be reacquired"
    );
    assert_eq!(
        reacquire
            .unwrap_err()
            .metadata()
            .get("x-cyrene-reason-code")
            .and_then(|value| value.to_str().ok()),
        Some("INSUFFICIENT_RESOURCES"),
        "the still-held FAILED lease must block reallocation with INSUFFICIENT_RESOURCES"
    );
}

/// A failed physical stop keeps the actor and its process handle bound to the
/// same Lease. A later release with the same fence may retry that actor; only
/// its complete cleanup report can transition the allocation to RELEASED.
/// 中文：物理 stop 失败时，actor 及其进程句柄继续绑定到同一个 Lease。之后使用相同 Fence 发起的 release 可以重试该 actor；只有拿到完整清理报告后，allocation 才能转为 RELEASED。
#[test]
fn failed_canonical_release_retries_same_actor_and_fence() {
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    struct RetrySandbox {
        stop_calls: Arc<AtomicUsize>,
    }

    impl ProcessRuntime for RetrySandbox {
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
            if self.stop_calls.fetch_add(1, AtomicOrdering::SeqCst) == 0 {
                return Err(ProviderError::new(
                    "retry-sandbox",
                    "STOP_TRANSIENT",
                    "first cleanup attempt failed",
                ));
            }
            Ok(CleanupReport {
                complete: true,
                exit_code: Some(0),
                oom_killed: false,
                conditions: Vec::new(),
                reason_code: "RETRY_CLEANUP_COMPLETE".to_string(),
            })
        }
    }

    impl SandboxBackend for RetrySandbox {
        fn backend_id(&self) -> &str {
            "retry-test"
        }
    }

    let stop_calls = Arc::new(AtomicUsize::new(0));
    let sandbox = Arc::new(RetrySandbox {
        stop_calls: Arc::clone(&stop_calls),
    });
    let resource = test_resource();
    let hardware = Arc::new(TestHardware {
        resources: vec![resource.clone()],
    });
    let daemon = Arc::new(KernelDaemon::new(
        hardware.clone(),
        hardware,
        Arc::new(InMemoryResourceManager::new("node", vec![resource])),
        sandbox.clone(),
        "node",
        7,
    ));
    let adapter = KernelServiceAdapter::new(daemon, Arc::new(UnusedResolver));
    let authority = adapter.authority();
    let context = scoped_authority_context("ns-failed-retry", "failed-retry");
    let principal = principal_from_peer_cred(&AUTHORITY_TEST_PEER);
    let worker = semantic::Identity {
        id: "worker-failed-retry".to_string(),
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
        .acquire_lease(&context, &principal, worker, query, u64::MAX)
        .expect("canonical lease should be acquired");
    let daemon_lease_name = authority
        .runtime
        .leases
        .lock()
        .unwrap()
        .get(&context.object_ref(lease.identity.clone()))
        .cloned()
        .expect("canonical lease must be registered");
    let daemon_lease = adapter
        .daemon
        .lease(&daemon_lease_name)
        .expect("daemon lease must exist");
    let allocation_resource_id = daemon_lease
        .allocations
        .first()
        .expect("lease has an allocation")
        .resource
        .id
        .clone();

    let mut actor = InstanceActor::new(
        "failed-retry-instance",
        daemon_lease.name.clone(),
        daemon_lease.fence_token,
        sandbox,
        LaunchPlan {
            instance_name: "failed-retry-instance".to_string(),
            executable: PathBuf::from("/bin/true"),
            args: Vec::new(),
            environment: BTreeMap::new(),
            cgroup_name: "failed-retry-instance".to_string(),
            limits: CgroupLimits::default(),
            working_dir: None,
            transport_socket: None,
        },
        DeviceBinding {
            resource_id: allocation_resource_id.clone(),
            nodes: Vec::new(),
            environment: BTreeMap::new(),
            joinable_environment_keys: Default::default(),
            required_gids: Vec::new(),
            enforcement: EnforcementMode::Soft,
            adapter_id: "retry-test".to_string(),
            reason_code: "test".to_string(),
        },
        Duration::from_secs(30),
    );
    actor.start().expect("retry instance must start");
    let mut process = managed_test_process(
        "failed-retry-instance",
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
        .insert("failed-retry-instance".to_string(), process);

    let first_error = authority
        .release_lease(&context, &principal, &lease.identity, lease.fence_token)
        .unwrap_err();
    assert_eq!(first_error.reason_code, "STOP_TRANSIENT");
    assert_eq!(stop_calls.load(AtomicOrdering::SeqCst), 1);
    assert_eq!(
        adapter.daemon.lease(&daemon_lease_name).unwrap().state,
        cy_kernel_api::LeaseState::Failed
    );
    assert!(adapter.daemon.is_allocated(&daemon_lease_name));

    let stale_error = authority
        .release_lease(&context, &principal, &lease.identity, lease.fence_token + 1)
        .unwrap_err();
    assert_eq!(stale_error.reason_code, "STALE_FENCE_TOKEN");
    assert_eq!(stop_calls.load(AtomicOrdering::SeqCst), 1);
    assert_eq!(
        adapter.daemon.lease(&daemon_lease_name).unwrap().state,
        cy_kernel_api::LeaseState::Failed
    );

    let released = authority
        .release_lease(&context, &principal, &lease.identity, lease.fence_token)
        .expect("same-fence retry should complete physical cleanup");
    assert_eq!(released.state, semantic::LeaseState::Released);
    assert_eq!(stop_calls.load(AtomicOrdering::SeqCst), 2);
    assert_eq!(
        adapter.daemon.lease(&daemon_lease_name).unwrap().state,
        cy_kernel_api::LeaseState::Released
    );
    assert!(!adapter.daemon.is_allocated(&daemon_lease_name));
}

// ---------------------------------------------------------------------------
