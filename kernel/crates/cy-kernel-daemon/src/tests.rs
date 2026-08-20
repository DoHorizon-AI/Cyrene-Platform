//! cy-kernel-daemon 单元测试与集成测试套件。

#![allow(deprecated)]

use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{atomic::Ordering, Arc},
    thread,
    time::Duration,
};

use cy_kernel_api::{
    semantic, CapabilityFact, CgroupLimits, CleanupReport, DeviceBinding, EnforcementMode,
    FailingRuntimeJournal, HostInventoryProvider, InstalledPluginResolver, InventorySnapshot,
    LaunchPlan, NodeCapabilities, ProcessCondition, ProcessHandle, ProcessRuntime, ProviderError,
    ResolvedLaunchPlan, ResourceProvider, RuntimeJournalEvent, RuntimeJournalRecord,
    RuntimeJournalSink, SandboxBackend, StopRequest, VerifiedInstallation,
};
use cy_proto::{core_v1, semantic_v1};
use cy_resource_manager::InMemoryResourceManager;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tonic::Request;

use crate::{
    adapter::{KernelServiceAdapter, OPERATION_EVENT_HISTORY_CAPACITY},
    convert::{
        merge_bindings, resource_request, to_plugin_instance, to_semantic_proto_contract_revision,
        to_semantic_proto_identity, unix_ms_from_timestamp,
    },
    daemon::KernelDaemon,
    peer_cred::{principal_from_peer_cred, PeerCred},
    session::{ManagedProcess, WorkerHeartbeatConfig},
    watchdog::InstanceActor,
};

const AUTHORITY_TEST_PEER: PeerCred = PeerCred {
    pid: 4242,
    uid: 1000,
    gid: 1000,
};

fn authority_request<T>(message: T) -> Request<T> {
    authority_request_for(AUTHORITY_TEST_PEER, message)
}

fn authority_request_for<T>(peer: PeerCred, message: T) -> Request<T> {
    let mut request = Request::new(message);
    request
        .extensions_mut()
        .insert(principal_from_peer_cred(&peer));
    request
}

#[derive(Debug)]
struct EmptyHardware;

impl HostInventoryProvider for EmptyHardware {
    fn probe_inventory(&self) -> Result<InventorySnapshot, ProviderError> {
        Ok(InventorySnapshot {
            generation: 1,
            resources: Vec::new(),
            capabilities: NodeCapabilities {
                ready: true,
                facts: Vec::new(),
                enforcement: Vec::new(),
            },
        })
    }
}

impl ResourceProvider for EmptyHardware {
    fn adapter_id(&self) -> &str {
        "test-adapter"
    }

    fn probe_resources(&self) -> Result<Vec<semantic::Resource>, ProviderError> {
        Ok(Vec::new())
    }

    fn create_binding(
        &self,
        _resource: &semantic::Resource,
    ) -> Result<DeviceBinding, ProviderError> {
        Err(ProviderError::new("test-adapter", "UNUSED", "no resources"))
    }

    fn read_health(&self, _device_id: &str) -> Result<cy_kernel_api::HealthReport, ProviderError> {
        Err(ProviderError::new("test-adapter", "UNUSED", "no resources"))
    }
}

#[derive(Debug, Clone)]
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
        "test-provider"
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
            required_gids: Vec::new(),
            enforcement: EnforcementMode::Hard,
            adapter_id: self.adapter_id().to_string(),
            reason_code: "test-binding".to_string(),
        })
    }

    fn read_health(
        &self,
        _resource_id: &str,
    ) -> Result<cy_kernel_api::HealthReport, ProviderError> {
        Ok(cy_kernel_api::HealthReport {
            healthy: Some(true),
            reason_code: "test-ready".to_string(),
            summary: "ready".to_string(),
        })
    }
}

fn test_resource() -> semantic::Resource {
    semantic::Resource {
        identity: semantic::Identity {
            id: "resource-1".to_string(),
            generation: 1,
        },
        provider: semantic::Identity {
            id: "test-provider".to_string(),
            generation: 1,
        },
        resource_class: "accelerator".to_string(),
        capabilities: vec![semantic::Capability {
            id: "accelerator.compute".to_string(),
            revision: 1,
            properties: BTreeMap::new(),
        }],
        capacity: BTreeMap::from([(
            "memory.allocatable".to_string(),
            semantic::Quantity {
                value: 1024,
                unit: "byte".to_string(),
            },
        )]),
        attributes: BTreeMap::new(),
        state: semantic::ResourceState::Ready,
        reason_code: "test-ready".to_string(),
        summary: "ready".to_string(),
        links: Vec::new(),
    }
}

fn semantic_lease_adapter() -> KernelServiceAdapter {
    let resource = test_resource();
    let hardware = Arc::new(TestHardware {
        resource: resource.clone(),
    });
    let daemon = Arc::new(KernelDaemon::new(
        hardware.clone(),
        hardware,
        Arc::new(InMemoryResourceManager::new("node", vec![resource])),
        Arc::new(FakeSandbox),
        "node",
        7,
    ));
    KernelServiceAdapter::new(daemon, Arc::new(UnusedResolver))
}

fn authority_context(request_id: &str) -> core_v1::AuthorityCallContext {
    core_v1::AuthorityCallContext {
        contract: Some(to_semantic_proto_contract_revision(
            &semantic::ContractRevision::current(),
        )),
        request_id: request_id.to_string(),
        idempotency_key: request_id.to_string(),
    }
}

#[derive(Debug)]
struct FakeSandbox;

impl ProcessRuntime for FakeSandbox {
    fn preflight(&self) -> NodeCapabilities {
        NodeCapabilities {
            ready: true,
            facts: vec![CapabilityFact {
                name: "test".to_string(),
                available: true,
                required: true,
                detail: "test".to_string(),
            }],
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
            start_time_ticks: None,
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
            conditions: Vec::<ProcessCondition>::new(),
            reason_code: "TEST_STOP".to_string(),
        })
    }
}

impl SandboxBackend for FakeSandbox {
    fn backend_id(&self) -> &str {
        "test"
    }
}

fn managed_test_process(
    instance_name: &str,
    generation: u64,
    lease: Option<core_v1::ResourceLeaseRef>,
) -> ManagedProcess {
    ManagedProcess {
        actor: InstanceActor::new_for_test(),
        lease,
        semantic_worker: None,
        plugin: core_v1::InstalledPluginRef {
            installation_name: instance_name.to_string(),
            plugin_id: "test".to_string(),
            version: "1".to_string(),
            component_id: "test".to_string(),
            manifest_digest: "sha256:test".to_string(),
            artifact_digest: "sha256:test".to_string(),
            verified_signature_identity: "test".to_string(),
        },
        generation,
        accepted_sequence: 0,
        last_heartbeat_at: None,
        runtime_state: core_v1::PluginRuntimeState::Starting as i32,
        health: None,
        restart_count: 0,
        watchdog_triggered: false,
        control: None,
        semantic_control: None,
        pending_shutdown: None,
    }
}

#[derive(Default)]
struct RecordingRuntimeJournal {
    records: std::sync::Mutex<Vec<RuntimeJournalRecord>>,
}

impl RuntimeJournalSink for RecordingRuntimeJournal {
    fn append(&self, record: RuntimeJournalRecord) -> Result<(), ProviderError> {
        self.records.lock().unwrap().push(record);
        Ok(())
    }
}

struct UnusedResolver;

impl InstalledPluginResolver for UnusedResolver {
    fn resolve_launch_plan(
        &self,
        _installation: &VerifiedInstallation,
        _instance_name: &str,
    ) -> Result<cy_kernel_api::ResolvedLaunchPlan, ProviderError> {
        Err(ProviderError::new(
            "test",
            "UNUSED",
            "not launched in this test",
        ))
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
            "test",
            "UNUSED",
            "legacy launch is not used",
        ))
    }

    fn resolve_worker_launch_plan(
        &self,
        worker: &semantic::Worker,
    ) -> Result<ResolvedLaunchPlan, ProviderError> {
        Ok(ResolvedLaunchPlan {
            installation: VerifiedInstallation {
                installation_name: "test-installation".to_string(),
                manifest_digest: "sha256:test".to_string(),
                artifact_digest: "sha256:test".to_string(),
                verified_signature_identity: "test".to_string(),
            },
            plan: LaunchPlan {
                instance_name: worker.identity.id.clone(),
                executable: PathBuf::from("worker"),
                args: Vec::new(),
                environment: BTreeMap::new(),
                cgroup_name: format!("instance-{}", worker.identity.id),
                limits: CgroupLimits::default(),
                transport_socket: None,
            },
        })
    }
}

fn semantic_worker_adapter() -> KernelServiceAdapter {
    let resource = test_resource();
    let hardware = Arc::new(TestHardware {
        resource: resource.clone(),
    });
    let daemon = Arc::new(KernelDaemon::new(
        hardware.clone(),
        hardware,
        Arc::new(InMemoryResourceManager::new("node", vec![resource])),
        Arc::new(FakeSandbox),
        "node",
        7,
    ));
    KernelServiceAdapter::new(daemon, Arc::new(TestWorkerResolver))
}

fn heartbeat_adapter() -> KernelServiceAdapter {
    let hardware = Arc::new(EmptyHardware);
    let daemon = Arc::new(KernelDaemon::new(
        hardware.clone(),
        hardware,
        Arc::new(InMemoryResourceManager::new("node", Vec::new())),
        Arc::new(FakeSandbox),
        "node",
        7,
    ));
    let adapter = KernelServiceAdapter::new(daemon, Arc::new(UnusedResolver))
        .with_worker_heartbeat(WorkerHeartbeatConfig {
            socket_path: PathBuf::from("/run/cyrene/test.sock"),
            interval: Duration::from_secs(1),
            timeout: Duration::from_secs(2),
            graceful_stop: Duration::from_secs(1),
            shutdown_ack_timeout: Duration::from_millis(50),
        });
    adapter.instances.lock().unwrap().insert(
        "worker_1".to_string(),
        managed_test_process("worker_1", 99, None),
    );
    adapter
}

#[test]
fn resource_limits_are_mapped_without_relaxing_request_validation() {
    let request = resource_request(
        "lease-1",
        4,
        semantic::Identity {
            id: "worker/test".to_string(),
            generation: 1,
        },
        None,
        &core_v1::ResourceRequirements {
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
    )
    .unwrap();
    assert_eq!(request.limits.cpu_max_millicores, Some(750));
    assert_eq!(request.limits.memory_max_bytes, Some(2048));

    let error = resource_request(
        "lease-2",
        4,
        semantic::Identity {
            id: "worker/test".to_string(),
            generation: 1,
        },
        None,
        &core_v1::ResourceRequirements {
            cpu: Some(core_v1::CpuRequirements {
                request_millicores: 751,
                limit_millicores: 750,
            }),
            accelerators: vec![core_v1::AcceleratorRequirements {
                count: 1,
                ..Default::default()
            }],
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(error.code(), tonic::Code::InvalidArgument);
}

#[test]
fn semantic_lease_rpc_is_vendor_neutral_fenced_and_ttl_bounded() {
    use core_v1::kernel_service_server::KernelService;

    let adapter = semantic_lease_adapter();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let lease = runtime
        .block_on(
            adapter.acquire_lease(Request::new(core_v1::AcquireLeaseRequest {
                mutation: Some(core_v1::MutationContext {
                    request: Some(core_v1::RequestContext {
                        request_id: "request-1".to_string(),
                        ..Default::default()
                    }),
                    idempotency_key: "semantic-1".to_string(),
                    expected_generation: Some(1),
                }),
                node: Some(core_v1::NodeRef {
                    node_id: "node".to_string(),
                    node_epoch: 7,
                }),
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
                cpu: None,
                memory: None,
            })),
        )
        .unwrap()
        .into_inner();

    assert_eq!(lease.state, semantic_v1::LeaseState::Active as i32);
    assert_eq!(lease.resources[0].id, "resource-1");
    assert!(lease.expires_at.is_some());
    let lease_identity = lease.identity.clone();

    let released = runtime
        .block_on(
            adapter.release_lease(Request::new(core_v1::ReleaseLeaseRequest {
                mutation: None,
                lease: lease_identity,
                fence_token: lease.fence_token,
            })),
        )
        .unwrap()
        .into_inner();
    assert_eq!(released.state, semantic_v1::LeaseState::Released as i32);

    let capabilities = adapter.daemon.get_kernel_capabilities().unwrap();
    assert!(capabilities.accelerators.is_empty());
    assert_eq!(capabilities.resources.len(), 1);
    assert_eq!(capabilities.resources[0].resource_class, "accelerator");
}

#[test]
fn authority_rejects_requests_without_peer_credentials() {
    use core_v1::kernel_authority_service_server::KernelAuthorityService;

    let adapter = semantic_lease_adapter();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let error = runtime
        .block_on(adapter.negotiate(Request::new(core_v1::NegotiateRequest::default())))
        .unwrap_err();

    assert_eq!(error.code(), tonic::Code::Unauthenticated);
}

#[test]
fn endpoint_authority_requires_the_worker_owner_principal() {
    use core_v1::{
        kernel_authority_service_server::KernelAuthorityService, AcquireSemanticLeaseRequest,
        AuthorizeEndpointRequest, PublishEndpointRequest, RenewLeaseRequest, RevokeEndpointRequest,
    };

    let adapter = semantic_lease_adapter();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let lease = runtime
        .block_on(
            adapter.acquire_lease(authority_request(AcquireSemanticLeaseRequest {
                context: Some(authority_context("endpoint-lease")),
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
                    seconds: 10,
                    nanos: 0,
                }),
            })),
        )
        .unwrap()
        .into_inner();
    let renewed = runtime
        .block_on(adapter.renew_lease(authority_request(RenewLeaseRequest {
            context: Some(authority_context("renew-endpoint-lease")),
            lease: lease.identity.clone(),
            fence_token: lease.fence_token,
            ttl: Some(prost_types::Duration {
                seconds: 20,
                nanos: 0,
            }),
        })))
        .unwrap()
        .into_inner();
    assert_eq!(renewed.fence_token, lease.fence_token);
    assert!(
        unix_ms_from_timestamp(renewed.expires_at.clone().unwrap(), "renewed").unwrap()
            > unix_ms_from_timestamp(lease.expires_at.clone().unwrap(), "lease").unwrap()
    );
    let mut process = managed_test_process(
        "worker-1",
        1,
        Some(core_v1::ResourceLeaseRef {
            lease_name: renewed.identity.as_ref().unwrap().id.clone(),
            fence_token: renewed.fence_token,
        }),
    );
    process.semantic_worker = Some(semantic::Worker {
        identity: semantic::Identity {
            id: "worker-1".to_string(),
            generation: 1,
        },
        principal: principal_from_peer_cred(&AUTHORITY_TEST_PEER).identity,
        provider: semantic::Identity {
            id: "provider-1".to_string(),
            generation: 1,
        },
        lease: semantic::Identity {
            id: renewed.identity.as_ref().unwrap().id.clone(),
            generation: renewed.identity.as_ref().unwrap().generation,
        },
        state: semantic::WorkerState::Starting,
        execution_ref: "test-ref".to_string(),
        limits: BTreeMap::new(),
    });
    adapter
        .instances
        .lock()
        .unwrap()
        .insert("worker-1".to_string(), process);

    let endpoint = runtime
        .block_on(
            adapter.publish_endpoint(authority_request(PublishEndpointRequest {
                context: Some(authority_context("publish-endpoint")),
                endpoint: Some(semantic_v1::Endpoint {
                    identity: Some(semantic_v1::Identity {
                        id: "endpoint-1".to_string(),
                        generation: 1,
                    }),
                    provider: Some(semantic_v1::Identity {
                        id: "provider-1".to_string(),
                        generation: 1,
                    }),
                    owner: Some(semantic_v1::Identity {
                        id: "worker-1".to_string(),
                        generation: 1,
                    }),
                    transport: "transport.uds".to_string(),
                    schema_id: "schema.v1".to_string(),
                    capabilities: Vec::new(),
                    public_attributes: Default::default(),
                }),
            })),
        )
        .unwrap()
        .into_inner();
    let non_owner = PeerCred {
        pid: 7171,
        uid: 2000,
        gid: 2000,
    };
    let denied_publish = runtime.block_on(adapter.publish_endpoint(authority_request_for(
        non_owner,
        PublishEndpointRequest {
            context: Some(authority_context("publish-endpoint-as-non-owner")),
            endpoint: Some(endpoint.clone()),
        },
    )));
    assert_eq!(
        denied_publish.unwrap_err().code(),
        tonic::Code::PermissionDenied
    );
    let grant_request = semantic_v1::EndpointGrant {
        identity: Some(semantic_v1::Identity {
            id: "grant-1".to_string(),
            generation: 1,
        }),
        endpoint: endpoint.identity.clone(),
        grantee: renewed.holder.clone(),
        lease: renewed.identity.clone(),
        fence_token: renewed.fence_token,
        expires_at: renewed.expires_at.clone(),
    };
    let denied_authorize = runtime.block_on(adapter.authorize_endpoint(authority_request_for(
        non_owner,
        AuthorizeEndpointRequest {
            context: Some(authority_context("authorize-endpoint-as-non-owner")),
            grant: Some(grant_request.clone()),
        },
    )));
    assert_eq!(
        denied_authorize.unwrap_err().code(),
        tonic::Code::PermissionDenied
    );
    let grant = runtime
        .block_on(
            adapter.authorize_endpoint(authority_request(AuthorizeEndpointRequest {
                context: Some(authority_context("authorize-endpoint")),
                grant: Some(grant_request),
            })),
        )
        .unwrap()
        .into_inner();
    assert_eq!(grant.fence_token, renewed.fence_token);

    let denied_revoke = runtime.block_on(adapter.revoke_endpoint(authority_request_for(
        non_owner,
        RevokeEndpointRequest {
            context: Some(authority_context("revoke-endpoint-as-non-owner")),
            grant: grant.identity.clone(),
        },
    )));
    assert_eq!(
        denied_revoke.unwrap_err().code(),
        tonic::Code::PermissionDenied
    );

    runtime
        .block_on(
            adapter.revoke_endpoint(authority_request(RevokeEndpointRequest {
                context: Some(authority_context("revoke-endpoint")),
                grant: grant.identity.clone(),
            })),
        )
        .unwrap();
    assert!(adapter.endpoint_grants.lock().unwrap().is_empty());
}

#[test]
fn authority_worker_operation_and_event_paths_do_not_use_plugin_or_lro_types() {
    use crate::watchdog::InstanceActorState;
    use core_v1::{
        kernel_authority_service_server::KernelAuthorityService, AcquireSemanticLeaseRequest,
        CancelSemanticOperationRequest, CreateOperationRequest, HeartbeatWorkerRequest,
        ReportOperationRequest, StartWorkerRequest, StopWorkerRequest, SubscribeEventsRequest,
    };

    let adapter = semantic_worker_adapter();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let lease = runtime
        .block_on(
            adapter.acquire_lease(authority_request(AcquireSemanticLeaseRequest {
                context: Some(authority_context("worker-lease")),
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
    let worker = semantic_v1::Worker {
        identity: Some(semantic_v1::Identity {
            id: "worker-1".to_string(),
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
    let started = runtime
        .block_on(adapter.start_worker(authority_request(StartWorkerRequest {
            context: Some(authority_context("start-worker")),
            worker: Some(worker.clone()),
        })))
        .unwrap()
        .into_inner();
    assert_eq!(started.kind, "worker.start");
    assert_eq!(started.state, semantic_v1::OperationState::Running as i32);
    // Direction 2: the launched worker must be driven by the wired-up
    // InstanceActor, not a bare SandboxedProcess.
    assert_eq!(
        adapter
            .instances
            .lock()
            .unwrap()
            .get("worker-1")
            .expect("started worker must be registered")
            .actor
            .state(),
        InstanceActorState::Healthy
    );
    let stored_principal = adapter
        .instances
        .lock()
        .unwrap()
        .get("worker-1")
        .and_then(|process| process.semantic_worker.as_ref())
        .expect("started worker must retain semantic identity")
        .principal
        .clone();
    assert_eq!(
        stored_principal.id, "unix-principal/uid-1000/gid-1000",
        "the authority must not trust Worker.principal from the request body",
    );
    assert_eq!(stored_principal.generation, 1);

    let running = runtime
        .block_on(
            adapter.heartbeat_worker(authority_request(HeartbeatWorkerRequest {
                context: Some(authority_context("heartbeat-worker")),
                worker: worker.identity.clone(),
                lease: lease.identity.clone(),
                fence_token: lease.fence_token,
            })),
        )
        .unwrap()
        .into_inner();
    assert_eq!(running.state, semantic_v1::WorkerState::Running as i32);

    let operation = semantic_v1::Operation {
        identity: Some(semantic_v1::Identity {
            id: "operation-1".to_string(),
            generation: 1,
        }),
        owner: worker.principal.clone(),
        executor: worker.provider.clone(),
        kind: "ai.train".to_string(),
        state: semantic_v1::OperationState::Created as i32,
        deadline: None,
        parent: None,
        metadata: Default::default(),
    };
    runtime
        .block_on(
            adapter.create_operation(authority_request(CreateOperationRequest {
                context: Some(authority_context("create-operation")),
                operation: Some(operation.clone()),
            })),
        )
        .unwrap();
    let mut reported = operation;
    reported.state = semantic_v1::OperationState::Running as i32;
    runtime
        .block_on(
            adapter.report_operation(authority_request(ReportOperationRequest {
                context: Some(authority_context("report-operation")),
                operation: Some(reported),
            })),
        )
        .unwrap();
    let cancelling = runtime
        .block_on(
            adapter.cancel_operation(authority_request(CancelSemanticOperationRequest {
                context: Some(authority_context("cancel-operation")),
                operation: Some(semantic_v1::Identity {
                    id: "operation-1".to_string(),
                    generation: 1,
                }),
            })),
        )
        .unwrap()
        .into_inner();
    assert_eq!(
        cancelling.state,
        semantic_v1::OperationState::Cancelling as i32
    );

    let cursor = semantic_v1::EventCursor {
        source: Some(to_semantic_proto_identity(&adapter.semantic_event_source())),
        sequence: 0,
    };
    let events = runtime
        .block_on(
            adapter.subscribe_events(authority_request(SubscribeEventsRequest {
                context: Some(authority_context("subscribe-events")),
                cursor: Some(cursor),
                page_size: 256,
            })),
        )
        .unwrap()
        .into_inner();
    assert_eq!(events.status, semantic_v1::ReplayStatus::Current as i32);
    assert!(events
        .events
        .iter()
        .any(|event| event.kind == "worker.starting"));
    assert!(events
        .events
        .iter()
        .any(|event| event.kind == "operation.running"));

    let source_changed = runtime
        .block_on(
            adapter.subscribe_events(authority_request(SubscribeEventsRequest {
                context: Some(authority_context("source-changed")),
                cursor: Some(semantic_v1::EventCursor {
                    source: Some(semantic_v1::Identity {
                        id: "another-kernel".to_string(),
                        generation: 1,
                    }),
                    sequence: 0,
                }),
                page_size: 1,
            })),
        )
        .unwrap()
        .into_inner();
    assert_eq!(
        source_changed.status,
        semantic_v1::ReplayStatus::SourceChanged as i32
    );
    for sequence in 0..=OPERATION_EVENT_HISTORY_CAPACITY {
        adapter.publish_semantic_event(
            semantic::Identity {
                id: "worker-1".to_string(),
                generation: 1,
            },
            "worker.observed",
            "cyrene.worker.v1",
            sequence.to_string().into_bytes(),
        );
    }
    let gap = runtime
        .block_on(
            adapter.subscribe_events(authority_request(SubscribeEventsRequest {
                context: Some(authority_context("replay-gap")),
                cursor: Some(semantic_v1::EventCursor {
                    source: Some(to_semantic_proto_identity(&adapter.semantic_event_source())),
                    sequence: 1,
                }),
                page_size: 1,
            })),
        )
        .unwrap()
        .into_inner();
    assert_eq!(gap.status, semantic_v1::ReplayStatus::Gap as i32);
    assert!(gap.events.is_empty());

    let stopped = runtime
        .block_on(adapter.stop_worker(authority_request(StopWorkerRequest {
            context: Some(authority_context("stop-worker")),
            worker: worker.identity,
            lease: lease.identity,
            fence_token: lease.fence_token,
            grace_period: Some(prost_types::Duration {
                seconds: 1,
                nanos: 0,
            }),
        })))
        .unwrap()
        .into_inner();
    assert_eq!(stopped.state, semantic_v1::OperationState::Succeeded as i32);
}

#[test]
fn heartbeat_requires_generation_and_monotonic_sequence() {
    use core_v1::plugin_lifecycle_service_server::PluginLifecycleService;
    let adapter = heartbeat_adapter();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let accepted = runtime
        .block_on(
            adapter.report_heartbeat(Request::new(core_v1::ReportHeartbeatRequest {
                context: None,
                plugin_instance_name: "worker_1".to_string(),
                generation: 99,
                sequence_number: 1,
                observed_at: None,
                runtime_state: core_v1::PluginRuntimeState::Healthy as i32,
                health: None,
                restart_count: 0,
            })),
        )
        .unwrap()
        .into_inner();
    assert_eq!(
        accepted.disposition,
        core_v1::HeartbeatDisposition::Accepted as i32
    );
    let duplicate = runtime
        .block_on(
            adapter.report_heartbeat(Request::new(core_v1::ReportHeartbeatRequest {
                context: None,
                plugin_instance_name: "worker_1".to_string(),
                generation: 99,
                sequence_number: 1,
                observed_at: None,
                runtime_state: core_v1::PluginRuntimeState::Healthy as i32,
                health: None,
                restart_count: 0,
            })),
        )
        .unwrap()
        .into_inner();
    assert_eq!(
        duplicate.disposition,
        core_v1::HeartbeatDisposition::Duplicate as i32
    );
    let stale = runtime
        .block_on(
            adapter.report_heartbeat(Request::new(core_v1::ReportHeartbeatRequest {
                context: None,
                plugin_instance_name: "worker_1".to_string(),
                generation: 98,
                sequence_number: 2,
                observed_at: None,
                runtime_state: core_v1::PluginRuntimeState::Healthy as i32,
                health: None,
                restart_count: 0,
            })),
        )
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
        kernel_authority_service_server::KernelAuthorityService, AcquireSemanticLeaseRequest,
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
            adapter.acquire_lease(authority_request(AcquireSemanticLeaseRequest {
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
    let (connection_id, welcome) = adapter
        .register_semantic_worker_control(
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
                .accept_semantic_shutdown_ack(&core_v1::WorkerControlShutdownAck {
                    worker: worker.identity.clone(),
                    lease: lease.identity.clone(),
                    fence_token: lease.fence_token,
                    shutdown_id: shutdown.shutdown_id,
                    drained: true,
                })
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
            adapter.cancel_operation(Request::new(core_v1::CancelOperationRequest {
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
            required_gids: vec![44],
            enforcement: EnforcementMode::Hard,
            adapter_id: "nvidia".to_string(),
            reason_code: "TEST".to_string(),
        },
        DeviceBinding {
            resource_id: "amd-0".to_string(),
            nodes: Vec::new(),
            environment: BTreeMap::new(),
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
            required_gids: Vec::new(),
            enforcement: EnforcementMode::Hard,
            adapter_id: "nvidia".to_string(),
            reason_code: "TEST".to_string(),
        },
        DeviceBinding {
            resource_id: "virtual-0".to_string(),
            nodes: Vec::new(),
            environment: BTreeMap::new(),
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
/// in-memory reservation is rolled back, leaving no active lease behind.
#[test]
fn acquire_lease_is_rolled_back_when_journal_write_fails() {
    use crate::convert::authority_lease_name;
    use core_v1::{
        kernel_authority_service_server::KernelAuthorityService, AcquireSemanticLeaseRequest,
    };

    let adapter = semantic_lease_adapter().with_runtime_journal(Arc::new(FailingRuntimeJournal));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let result = runtime.block_on(adapter.acquire_lease(authority_request(
        AcquireSemanticLeaseRequest {
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
        },
    )));
    assert!(
        result.is_err(),
        "acquire_lease must fail when the durable journal write fails"
    );

    // Rollback must have released the in-memory lease, so it is not externally
    // visible as an Active lease (its resource is freed) even though the
    // durable fence record was never persisted.
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
/// the reservation). This journal double allows `LeaseReserved` but fails
/// `LeaseReleased`, so a held lease survives a failed release attempt.
#[test]
fn release_lease_fails_closed_when_journal_write_fails() {
    use core_v1::{
        kernel_authority_service_server::KernelAuthorityService, AcquireSemanticLeaseRequest,
        ReleaseSemanticLeaseRequest,
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
            adapter.acquire_lease(authority_request(AcquireSemanticLeaseRequest {
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
        ReleaseSemanticLeaseRequest {
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
    let still_held = adapter
        .daemon
        .lease(&lease.identity.as_ref().unwrap().id)
        .unwrap();
    assert_eq!(still_held.state, cy_kernel_api::LeaseState::Releasing);
}
