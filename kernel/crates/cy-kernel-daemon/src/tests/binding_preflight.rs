//! Binding-aware sandbox admission and lease-ownership regression tests.
//!
//! 绑定感知沙箱准入与租约所有权回归测试。

use super::*;
use cy_kernel_api::{LeaseState, ResourceLeaseManager};
use cy_proto::core_v1::kernel_service_server::KernelService;

#[derive(Default)]
struct RecordingJournal {
    records: Mutex<Vec<RuntimeJournalRecord>>,
}

impl RuntimeJournalSink for RecordingJournal {
    fn append(&self, record: RuntimeJournalRecord) -> Result<(), ProviderError> {
        self.records.lock().unwrap().push(record);
        Ok(())
    }
}

struct RejectingBindingSandbox {
    launch_count: Arc<std::sync::atomic::AtomicUsize>,
}

impl ProcessRuntime for RejectingBindingSandbox {
    fn preflight(&self) -> NodeCapabilities {
        NodeCapabilities {
            ready: true,
            facts: vec![CapabilityFact {
                name: "device-bpf-capable".to_string(),
                available: true,
                required: false,
                detail: "node-level probe only".to_string(),
            }],
            enforcement: Vec::new(),
        }
    }

    fn preflight_for_binding(&self, binding: &DeviceBinding) -> NodeCapabilities {
        assert_eq!(binding.enforcement, EnforcementMode::Hard);
        NodeCapabilities {
            ready: false,
            facts: vec![CapabilityFact {
                name: "device-bpf-capable".to_string(),
                available: false,
                required: true,
                detail: "DEVICE_BPF_ATTACH_FAILED: simulated binding rejection".to_string(),
            }],
            enforcement: Vec::new(),
        }
    }

    fn launch(
        &self,
        _plan: &LaunchPlan,
        _binding: &DeviceBinding,
    ) -> Result<ProcessHandle, ProviderError> {
        self.launch_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err(ProviderError::new(
            "test",
            "UNEXPECTED_LAUNCH",
            "binding rejection must precede physical launch",
        ))
    }

    fn stop(
        &self,
        _handle: &ProcessHandle,
        _request: &StopRequest,
    ) -> Result<CleanupReport, ProviderError> {
        Ok(CleanupReport {
            complete: true,
            exit_code: None,
            oom_killed: false,
            conditions: Vec::new(),
            reason_code: "CLEANUP_COMPLETE".to_string(),
        })
    }
}

impl SandboxBackend for RejectingBindingSandbox {
    fn backend_id(&self) -> &str {
        "binding-preflight-test"
    }
}

fn rejection_adapter(
    resources: Arc<InMemoryResourceManager>,
    journal: Arc<RecordingJournal>,
    launch_count: Arc<std::sync::atomic::AtomicUsize>,
) -> KernelServiceAdapter {
    let hardware = Arc::new(TestHardware {
        resources: vec![test_resource()],
    });
    let daemon = Arc::new(KernelDaemon::new(
        hardware.clone(),
        hardware,
        resources,
        Arc::new(RejectingBindingSandbox { launch_count }),
        "node",
        7,
    ));
    KernelServiceAdapter::new(daemon, Arc::new(TestWorkerResolver)).with_runtime_journal(journal)
}

/// Simulates an older in-process runtime that only exposes node-level
/// preflight. Its inherited binding method must not authorize a HARD launch.
struct LegacyNodeOnlySandbox {
    facts: Vec<CapabilityFact>,
}

impl ProcessRuntime for LegacyNodeOnlySandbox {
    fn preflight(&self) -> NodeCapabilities {
        NodeCapabilities {
            ready: true,
            facts: self.facts.clone(),
            enforcement: Vec::new(),
        }
    }

    fn launch(
        &self,
        _plan: &LaunchPlan,
        _binding: &DeviceBinding,
    ) -> Result<ProcessHandle, ProviderError> {
        Err(ProviderError::new(
            "legacy-node-only-test",
            "UNEXPECTED_LAUNCH",
            "preflight contract test must not launch",
        ))
    }

    fn stop(
        &self,
        _handle: &ProcessHandle,
        _request: &StopRequest,
    ) -> Result<CleanupReport, ProviderError> {
        Err(ProviderError::new(
            "legacy-node-only-test",
            "UNEXPECTED_STOP",
            "preflight contract test must not stop",
        ))
    }
}

impl SandboxBackend for LegacyNodeOnlySandbox {
    fn backend_id(&self) -> &str {
        "legacy-node-only-test"
    }
}

fn daemon_for_preflight(sandbox: Arc<dyn SandboxBackend>) -> KernelDaemon {
    let resources = vec![test_resource()];
    let hardware = Arc::new(TestHardware {
        resources: resources.clone(),
    });
    KernelDaemon::new(
        hardware.clone(),
        hardware,
        Arc::new(InMemoryResourceManager::new("node", resources)),
        sandbox,
        "node",
        7,
    )
}

fn binding_with_enforcement(enforcement: EnforcementMode) -> DeviceBinding {
    DeviceBinding {
        resource_id: "gpu-0".to_string(),
        nodes: Vec::new(),
        environment: BTreeMap::new(),
        joinable_environment_keys: Default::default(),
        required_gids: Vec::new(),
        enforcement,
        adapter_id: "test-adapter".to_string(),
        reason_code: "test-binding".to_string(),
    }
}

#[test]
fn hard_binding_rejects_ready_legacy_preflight_with_missing_or_optional_proof() {
    let scenarios = [
        ("missing", Vec::new()),
        (
            "optional",
            vec![CapabilityFact {
                name: "device-bpf-capable".to_string(),
                available: true,
                required: false,
                detail: "node-level probe only".to_string(),
            }],
        ),
    ];

    for (scenario, facts) in scenarios {
        let daemon = daemon_for_preflight(Arc::new(LegacyNodeOnlySandbox { facts }));
        let error = daemon
            .preflight_binding(&binding_with_enforcement(EnforcementMode::Hard))
            .expect_err("a ready node-level preflight cannot prove HARD binding isolation");

        assert_eq!(error.reason_code, "BINDING_PREFLIGHT_FAILED", "{scenario}");
        assert!(
            error
                .message
                .contains("HARD binding requires an available required device-bpf-capable fact"),
            "{scenario}: unexpected detail: {}",
            error.message
        );
    }
}

#[test]
fn soft_binding_preserves_ready_node_level_preflight_compatibility() {
    let daemon = daemon_for_preflight(Arc::new(LegacyNodeOnlySandbox { facts: Vec::new() }));

    daemon
        .preflight_binding(&binding_with_enforcement(EnforcementMode::Soft))
        .expect("SOFT binding admission continues to use node-level readiness");
}

#[test]
fn rejected_resource_claim_is_released_before_launch_intent() {
    let resources = Arc::new(InMemoryResourceManager::new("node", vec![test_resource()]));
    let journal = Arc::new(RecordingJournal::default());
    let launch_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let adapter = rejection_adapter(resources.clone(), journal.clone(), launch_count.clone());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let result = runtime.block_on(adapter.launch_process(authority_request(
        core_v1::LaunchProcessRequest {
            node: Some(core_v1::NodeRef {
                node_id: "node".to_string(),
                node_epoch: 7,
            }),
            plugin: Some(core_v1::InstalledPluginRef {
                installation_name: "preflight-rejected-plugin".to_string(),
                plugin_id: "test".to_string(),
                version: "1".to_string(),
                component_id: "test".to_string(),
                manifest_digest: "sha256:test".to_string(),
                artifact_digest: "sha256:test".to_string(),
                verified_signature_identity: "test".to_string(),
            }),
            allocation: Some(core_v1::launch_process_request::Allocation::ResourceClaim(
                core_v1::ResourceRequirements {
                    accelerators: vec![core_v1::AcceleratorRequirements {
                        count: 1,
                        ..Default::default()
                    }],
                    ..Default::default()
                },
            )),
            mutation: Some(core_v1::MutationContext {
                request: Some(core_v1::RequestContext {
                    request_id: "binding-preflight-reject".to_string(),
                    ..Default::default()
                }),
                idempotency_key: "binding-preflight-reject".to_string(),
                expected_generation: Some(1),
            }),
            ..Default::default()
        },
    )));

    let error = result.expect_err("binding admission must reject this launch");
    assert!(
        error.message().contains("DEVICE_BPF_ATTACH_FAILED"),
        "unexpected status: {error:?}"
    );
    assert_eq!(launch_count.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(adapter.instances.lock().unwrap().is_empty());
    let leases = resources.leases();
    assert_eq!(leases.len(), 1);
    assert_eq!(leases[0].state, LeaseState::Released);
    assert!(!ResourceLeaseManager::is_allocated(
        resources.as_ref(),
        &leases[0].name
    ));
    assert!(!journal.records.lock().unwrap().iter().any(|record| {
        matches!(
            record.event,
            RuntimeJournalEvent::InstanceLaunching | RuntimeJournalEvent::InstanceLaunched
        )
    }));
}

#[test]
fn rejected_start_worker_preserves_caller_owned_existing_lease() {
    let resources = Arc::new(InMemoryResourceManager::new("node", vec![test_resource()]));
    let journal = Arc::new(RecordingJournal::default());
    let launch_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let adapter = rejection_adapter(resources.clone(), journal.clone(), launch_count.clone());
    let authority = adapter.authority();
    let context = scoped_authority_context("binding-preflight", "existing-lease");
    let principal = principal_from_peer_cred(&AUTHORITY_TEST_PEER);
    let worker_identity = semantic::Identity {
        id: "existing-lease-worker".to_string(),
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
                required_capabilities: Vec::new(),
                minimum_capacity: BTreeMap::new(),
            },
            now_unix_ms() + 30_000,
        )
        .unwrap();
    let internal_lease = resources.leases().pop().expect("acquired lease");

    let error = authority
        .start_worker(
            &context,
            &principal,
            semantic::Worker {
                identity: worker_identity,
                principal: principal.identity.clone(),
                provider: semantic::Identity {
                    id: "provider".to_string(),
                    generation: 1,
                },
                lease: lease.identity,
                state: semantic::WorkerState::Registered,
                execution_ref: "opaque-installed-worker".to_string(),
                limits: BTreeMap::new(),
            },
        )
        .expect_err("binding admission must reject this Worker start");

    assert_eq!(error.reason_code, "BINDING_PREFLIGHT_FAILED");
    assert_eq!(launch_count.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(adapter.instances.lock().unwrap().is_empty());
    let current = resources.get_lease(&internal_lease.name).unwrap();
    assert_eq!(current.state, LeaseState::Active);
    assert!(ResourceLeaseManager::is_allocated(
        resources.as_ref(),
        &internal_lease.name
    ));
    assert!(!journal.records.lock().unwrap().iter().any(|record| {
        matches!(
            record.event,
            RuntimeJournalEvent::InstanceLaunching | RuntimeJournalEvent::InstanceLaunched
        )
    }));
}
