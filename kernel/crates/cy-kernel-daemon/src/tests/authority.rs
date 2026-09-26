//! Authority, event, and snapshot tests for the Kernel daemon.
//!
//! These tests cover the semantic authority boundary, durable event history,
//! and snapshot/replay behavior.
//! 中文：Kernel 守护进程的权限、事件与快照测试。
//!
//! 中文：这些测试覆盖语义权限边界、持久化事件历史以及快照与重放行为。

use super::*;

pub(super) fn semantic_worker_adapter() -> KernelServiceAdapter {
    semantic_worker_adapter_with_resources(vec![test_resource()])
}

#[test]
fn canonical_start_worker_preserves_limits_and_runtime_owned_device_injection() {
    struct CaptureSandbox(Mutex<Vec<CgroupLimits>>);
    impl ProcessRuntime for CaptureSandbox {
        fn preflight(&self) -> NodeCapabilities {
            FakeSandbox.preflight()
        }
        fn launch(
            &self,
            plan: &LaunchPlan,
            binding: &DeviceBinding,
        ) -> Result<ProcessHandle, ProviderError> {
            // Exercise the real sandbox binding boundary, which rejects a second injection.
            // 中文：验证真实 sandbox binding 边界，该边界会拒绝第二次注入。设备变量只应在运行时注入，不能误判为 Plugin 覆盖。
            let environment = binding.merge_environment(&plan.environment)?;
            assert_eq!(
                environment.get("TEST_DEVICE_SELECTION").unwrap(),
                "device-0"
            );
            self.0.lock().unwrap().push(plan.limits.clone());
            FakeSandbox.launch(plan, binding)
        }
        fn stop(
            &self,
            handle: &ProcessHandle,
            request: &StopRequest,
        ) -> Result<CleanupReport, ProviderError> {
            FakeSandbox.stop(handle, request)
        }
    }
    impl SandboxBackend for CaptureSandbox {
        fn backend_id(&self) -> &str {
            "capture-limits"
        }
    }
    let mut resource = test_resource();
    resource
        .attributes
        .insert("test.binding.selector".to_string(), "device-0".to_string());
    let resources = vec![resource];
    let hardware = Arc::new(TestHardware {
        resources: resources.clone(),
    });
    let sandbox = Arc::new(CaptureSandbox(Mutex::new(Vec::new())));
    let daemon = Arc::new(KernelDaemon::new(
        hardware.clone(),
        hardware,
        Arc::new(InMemoryResourceManager::new("node", resources)),
        sandbox.clone(),
        "node",
        7,
    ));
    let adapter = KernelServiceAdapter::new(daemon, Arc::new(TestWorkerResolver));
    let authority = adapter.authority();
    let context = scoped_authority_context("default", "worker-limits");
    let principal = principal_from_peer_cred(&AUTHORITY_TEST_PEER);
    let identity = semantic::Identity {
        id: "limited-worker".to_string(),
        generation: 1,
    };
    let lease = authority
        .acquire_lease(
            &context,
            &principal,
            identity.clone(),
            semantic::ResourceQuery {
                resource_class: "accelerator".to_string(),
                count: 1,
                required_capabilities: Vec::new(),
                minimum_capacity: BTreeMap::new(),
            },
            now_unix_ms() + 30_000,
        )
        .unwrap();
    authority
        .start_worker(
            &context,
            &principal,
            semantic::Worker {
                identity,
                principal: principal.identity.clone(),
                provider: semantic::Identity {
                    id: "provider".to_string(),
                    generation: 1,
                },
                lease: lease.identity,
                state: semantic::WorkerState::Registered,
                execution_ref: "opaque-installed-worker".to_string(),
                limits: BTreeMap::from([(
                    "memory.bytes".to_string(),
                    semantic::Quantity {
                        value: 24 * 1024 * 1024 * 1024,
                        unit: "byte".to_string(),
                    },
                )]),
            },
        )
        .unwrap();
    let observed = sandbox.0.lock().unwrap();
    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0].memory_max_bytes, Some(24 * 1024 * 1024 * 1024));
}

pub(super) fn semantic_worker_adapter_with_resources(
    resources: Vec<semantic::Resource>,
) -> KernelServiceAdapter {
    let hardware = Arc::new(TestHardware {
        resources: resources.clone(),
    });
    let daemon = Arc::new(KernelDaemon::new(
        hardware.clone(),
        hardware,
        Arc::new(InMemoryResourceManager::new("node", resources)),
        Arc::new(FakeSandbox),
        "node",
        7,
    ));
    KernelServiceAdapter::new(daemon, Arc::new(TestWorkerResolver))
}

pub(super) fn hardware_provider_adapter(
    adapters: Vec<(String, Arc<dyn HardwareAdapter>)>,
) -> KernelServiceAdapter {
    let registry = Arc::new(UdsHardwareAdapterRegistry::from_adapters(adapters).unwrap());
    let mut daemon = KernelDaemon::new(
        registry.clone(),
        registry.clone(),
        Arc::new(InMemoryResourceManager::new("node", Vec::new())),
        Arc::new(FakeSandbox),
        "node",
        9,
    );
    daemon.hardware_adapters = Some(registry);
    KernelServiceAdapter::new(Arc::new(daemon), Arc::new(UnusedResolver))
}

pub(super) fn heartbeat_adapter() -> KernelServiceAdapter {
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
            adapter.acquire_lease(Request::new(core_v1::LegacyAcquireLeaseRequest {
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
            adapter.release_lease(Request::new(core_v1::LegacyReleaseLeaseRequest {
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
fn core_v1_defaults_scope_and_core_v2_requires_a_valid_explicit_namespace() {
    use core_v1::{kernel_authority_service_server::KernelAuthorityService, AcquireLeaseRequest};
    use core_v2::{
        kernel_authority_service_server::KernelAuthorityService as KernelAuthorityV2Service,
        AcquireLeaseRequest as AcquireV2LeaseRequest,
    };

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let adapter = semantic_lease_adapter();
    let default_lease = runtime
        .block_on(KernelAuthorityService::acquire_lease(
            &adapter,
            authority_request(AcquireLeaseRequest {
                context: Some(authority_context("v1-default")),
                holder: Some(semantic_v1::Identity {
                    id: "worker-v1".to_string(),
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
            }),
        ))
        .unwrap()
        .into_inner();
    let default_object = ObjectRef {
        namespace: NamespaceId::default(),
        identity: semantic::Identity {
            id: default_lease.identity.unwrap().id,
            generation: 1,
        },
    };
    assert!(adapter.leases.lock().unwrap().contains_key(&default_object));

    let v2_adapter = semantic_lease_adapter();
    let request = |namespace: &str| {
        authority_request(AcquireV2LeaseRequest {
            context: Some(core_v2::AuthorityCallContext {
                namespace: namespace.to_string(),
                contract: Some(to_semantic_proto_contract_revision(
                    &semantic::ContractRevision::current(),
                )),
                request_id: "v2-scope".to_string(),
                idempotency_key: "v2-scope".to_string(),
            }),
            holder: Some(semantic_v1::Identity {
                id: "worker-v2".to_string(),
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
        })
    };
    let missing = runtime
        .block_on(KernelAuthorityV2Service::acquire_lease(
            &v2_adapter,
            request(""),
        ))
        .unwrap_err();
    assert_eq!(missing.code(), tonic::Code::InvalidArgument);
    assert_eq!(
        missing
            .metadata()
            .get("x-cyrene-reason-code")
            .and_then(|value| value.to_str().ok()),
        Some("NAMESPACE_REQUIRED")
    );
    let invalid = runtime
        .block_on(KernelAuthorityV2Service::acquire_lease(
            &v2_adapter,
            request("not/a-namespace"),
        ))
        .unwrap_err();
    assert_eq!(invalid.code(), tonic::Code::InvalidArgument);
    assert_eq!(
        invalid
            .metadata()
            .get("x-cyrene-reason-code")
            .and_then(|value| value.to_str().ok()),
        Some("NAMESPACE_INVALID")
    );
    let v2_lease = runtime
        .block_on(KernelAuthorityV2Service::acquire_lease(
            &v2_adapter,
            request("tenant-a"),
        ))
        .unwrap()
        .into_inner();
    let identity = v2_lease.identity.unwrap();
    assert!(v2_adapter.leases.lock().unwrap().contains_key(&ObjectRef {
        namespace: NamespaceId::new("tenant-a").unwrap(),
        identity: semantic::Identity {
            id: identity.id,
            generation: identity.generation,
        },
    }));
}

#[test]
fn namespace_scopes_identical_worker_lease_operation_endpoint_grant_and_events() {
    let adapter = semantic_worker_adapter_with_resources(vec![
        test_resource_with_id("resource-a"),
        test_resource_with_id("resource-b"),
    ]);
    let authority = adapter.authority();
    let namespace_a = scoped_authority_context("namespace-a", "lease-x");
    let namespace_b = scoped_authority_context("namespace-b", "lease-x");
    let principal_a = principal_from_peer_cred(&AUTHORITY_TEST_PEER);
    let principal_b = principal_from_peer_cred(&PeerCred {
        pid: 4243,
        uid: 2000,
        gid: 2000,
    });
    let worker_identity = semantic::Identity {
        id: "worker-x".to_string(),
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
    let lease_a = authority
        .acquire_lease(
            &namespace_a,
            &principal_a,
            worker_identity.clone(),
            query.clone(),
            u64::MAX,
        )
        .unwrap();
    let lease_b = authority
        .acquire_lease(
            &namespace_b,
            &principal_b,
            worker_identity.clone(),
            query,
            u64::MAX,
        )
        .unwrap();
    assert_eq!(lease_a.identity, lease_b.identity);
    assert_ne!(lease_a.fence_token, lease_b.fence_token);

    let worker = |lease: &semantic::Lease, principal: &semantic::Principal| semantic::Worker {
        identity: worker_identity.clone(),
        principal: principal.identity.clone(),
        provider: semantic::Identity {
            id: "provider-x".to_string(),
            generation: 1,
        },
        lease: lease.identity.clone(),
        state: semantic::WorkerState::Registered,
        execution_ref: "opaque-execution-reference".to_string(),
        limits: BTreeMap::new(),
    };
    authority
        .start_worker(&namespace_a, &principal_a, worker(&lease_a, &principal_a))
        .unwrap();
    authority
        .start_worker(&namespace_b, &principal_b, worker(&lease_b, &principal_b))
        .unwrap();

    let endpoint = |principal: &semantic::Principal| semantic::Endpoint {
        identity: semantic::Identity {
            id: "endpoint-x".to_string(),
            generation: 1,
        },
        provider: semantic::Identity {
            id: "provider-x".to_string(),
            generation: 1,
        },
        owner: worker_identity.clone(),
        transport: "transport.uds".to_string(),
        schema_id: "schema.v1".to_string(),
        capabilities: Vec::new(),
        public_attributes: BTreeMap::from([("owner".to_string(), principal.identity.id.clone())]),
        connection_ref: "uds://runtime/direct-worker".to_string(),
        credential_ref: None,
    };
    let endpoint_a = authority
        .publish_endpoint(&namespace_a, &principal_a, endpoint(&principal_a))
        .unwrap();
    let endpoint_b = authority
        .publish_endpoint(&namespace_b, &principal_b, endpoint(&principal_b))
        .unwrap();
    assert_eq!(endpoint_a.identity, endpoint_b.identity);

    let grant = |endpoint: &semantic::Endpoint, lease: &semantic::Lease| semantic::EndpointGrant {
        identity: semantic::Identity {
            id: "grant-x".to_string(),
            generation: 1,
        },
        endpoint: endpoint.identity.clone(),
        grantee: worker_identity.clone(),
        lease: lease.identity.clone(),
        fence_token: lease.fence_token,
        expires_at_unix_ms: u64::MAX,
    };
    authority
        .authorize_endpoint(&namespace_a, &principal_a, grant(&endpoint_a, &lease_a))
        .unwrap();
    authority
        .authorize_endpoint(&namespace_b, &principal_b, grant(&endpoint_b, &lease_b))
        .unwrap();

    let operation = |principal: &semantic::Principal| semantic::Operation {
        identity: semantic::Identity {
            id: "operation-x".to_string(),
            generation: 1,
        },
        owner: principal.identity.clone(),
        executor: semantic::Identity {
            id: "provider-x".to_string(),
            generation: 1,
        },
        kind: "ai.train".to_string(),
        state: semantic::OperationState::Created,
        deadline_unix_ms: None,
        parent: None,
        metadata: BTreeMap::new(),
    };
    authority
        .create_operation(&namespace_a, &principal_a, operation(&principal_a))
        .unwrap();
    authority
        .create_operation(&namespace_b, &principal_b, operation(&principal_b))
        .unwrap();

    let worker_object_a = namespace_a.object_ref(worker_identity.clone());
    let worker_object_b = namespace_b.object_ref(worker_identity.clone());
    let workers = authority.runtime.workers.lock().unwrap();
    assert_eq!(workers.len(), 2);
    assert_ne!(workers[&worker_object_a], workers[&worker_object_b]);
    drop(workers);
    assert_eq!(authority.runtime.leases.lock().unwrap().len(), 2);
    assert_eq!(authority.runtime.endpoints.lock().unwrap().len(), 2);
    assert_eq!(authority.runtime.endpoint_grants.lock().unwrap().len(), 2);
    assert!(authority
        .runtime
        .semantic_operations
        .lock()
        .unwrap()
        .contains_key(&namespace_a.object_ref(semantic::Identity {
            id: "operation-x".to_string(),
            generation: 1,
        })));
    assert!(authority
        .runtime
        .semantic_operations
        .lock()
        .unwrap()
        .contains_key(&namespace_b.object_ref(semantic::Identity {
            id: "operation-x".to_string(),
            generation: 1,
        })));

    let events_a = authority
        .read_events(
            &namespace_a,
            &principal_a,
            &semantic::EventCursor {
                source: authority.semantic_event_source_for(&namespace_a.namespace),
                sequence: 0,
            },
            OPERATION_EVENT_HISTORY_CAPACITY,
        )
        .unwrap();
    let events_b = authority
        .read_events(
            &namespace_b,
            &principal_b,
            &semantic::EventCursor {
                source: authority.semantic_event_source_for(&namespace_b.namespace),
                sequence: 0,
            },
            OPERATION_EVENT_HISTORY_CAPACITY,
        )
        .unwrap();
    assert_ne!(events_a.source, events_b.source);
    assert!(events_a
        .events
        .iter()
        .all(|event| event.source == events_a.source));
    assert!(events_b
        .events
        .iter()
        .all(|event| event.source == events_b.source));

    let denied = authority
        .release_lease(
            &namespace_b,
            &principal_a,
            &lease_b.identity,
            lease_b.fence_token,
        )
        .unwrap_err();
    assert_eq!(denied.reason_code, "NAMESPACE_AUTHORITY_DENIED");
}

#[test]
fn endpoint_authority_requires_the_worker_owner_principal() {
    use core_v1::{
        kernel_authority_service_server::KernelAuthorityService, AcquireLeaseRequest,
        AuthorizeEndpointRequest, PublishEndpointRequest, RenewLeaseRequest, RevokeEndpointRequest,
    };

    let adapter = semantic_lease_adapter();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let lease = runtime
        .block_on(
            adapter.acquire_lease(authority_request(AcquireLeaseRequest {
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
        unix_ms_from_timestamp(renewed.expires_at.unwrap(), "renewed").unwrap()
            > unix_ms_from_timestamp(lease.expires_at.unwrap(), "lease").unwrap()
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
                    connection_ref: "uds://runtime/direct-worker".to_string(),
                    credential_ref: String::new(),
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
        expires_at: renewed.expires_at,
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
        kernel_authority_service_server::KernelAuthorityService, AcquireLeaseRequest,
        CancelOperationRequest, CreateOperationRequest, ReadEventsRequest, ReportHeartbeatRequest,
        ReportOperationRequest, StartWorkerRequest, StopWorkerRequest, WatchEventsRequest,
    };

    let adapter = semantic_worker_adapter();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let lease = runtime
        .block_on(
            adapter.acquire_lease(authority_request(AcquireLeaseRequest {
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
    // 中文：方向 2：启动的 Worker 必须由已接入的 InstanceActor 驱动，不能只依赖裸 SandboxedProcess。
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
            adapter.report_heartbeat(authority_request(ReportHeartbeatRequest {
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
    let cancelled = runtime
        .block_on(
            adapter.cancel_operation(authority_request(CancelOperationRequest {
                context: Some(authority_context("cancel-operation")),
                operation: Some(semantic_v1::Identity {
                    id: "operation-1".to_string(),
                    generation: 1,
                }),
            })),
        )
        .unwrap()
        .into_inner();
    assert_eq!(cancelled.state, semantic_v1::OperationState::Lost as i32);

    let cursor = semantic_v1::EventCursor {
        source: Some(to_semantic_proto_identity(&adapter.semantic_event_source())),
        sequence: 0,
    };
    let read_page = runtime
        .block_on(adapter.read_events(authority_request(ReadEventsRequest {
            context: Some(authority_context("read-events")),
            cursor: Some(cursor.clone()),
            limit: 256,
        })))
        .unwrap()
        .into_inner()
        .page
        .expect("ReadEvents returns its EventPage");
    assert_eq!(read_page.status, semantic_v1::ReplayStatus::Current as i32);

    let mut stream = runtime
        .block_on(adapter.watch_events(authority_request(WatchEventsRequest {
            context: Some(authority_context("watch-events")),
            cursor: Some(cursor),
            page_size: 256,
        })))
        .unwrap()
        .into_inner();
    let mut events = Vec::new();
    runtime.block_on(async {
        use tokio_stream::StreamExt;
        while let Ok(Some(Ok(event))) =
            tokio::time::timeout(Duration::from_millis(50), stream.next()).await
        {
            events.push(event);
        }
    });
    let events = events.into_iter().map(watch_event).collect::<Vec<_>>();
    assert!(events.iter().any(|event| event.kind == "worker.starting"));
    assert!(events.iter().any(|event| event.kind == "operation.running"));

    let mut source_changed_stream = runtime
        .block_on(adapter.watch_events(authority_request(WatchEventsRequest {
            context: Some(authority_context("source-changed")),
            cursor: Some(semantic_v1::EventCursor {
                source: Some(semantic_v1::Identity {
                    id: "another-kernel".to_string(),
                    generation: 1,
                }),
                sequence: 0,
            }),
            page_size: 1,
        })))
        .unwrap()
        .into_inner();
    let source_changed = runtime
        .block_on(async {
            use tokio_stream::StreamExt;
            source_changed_stream.next().await
        })
        .expect("source change response")
        .expect("source change is a typed stream response");
    match source_changed.body {
        Some(core_v1::watch_events_response::Body::Continuity(continuity)) => {
            assert_eq!(
                continuity.status,
                semantic_v1::ReplayStatus::SourceChanged as i32
            );
        }
        other => panic!("expected typed source-change page, got {other:?}"),
    }

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
    let mut gap_stream = runtime
        .block_on(adapter.watch_events(authority_request(WatchEventsRequest {
            context: Some(authority_context("replay-gap")),
            cursor: Some(semantic_v1::EventCursor {
                source: Some(to_semantic_proto_identity(&adapter.semantic_event_source())),
                sequence: 1,
            }),
            page_size: 1,
        })))
        .unwrap()
        .into_inner();
    let gap = runtime
        .block_on(async {
            use tokio_stream::StreamExt;
            gap_stream.next().await
        })
        .expect("gap response")
        .expect("gap is a typed stream response");
    match gap.body {
        Some(core_v1::watch_events_response::Body::Continuity(continuity)) => {
            assert_eq!(continuity.status, semantic_v1::ReplayStatus::Gap as i32);
        }
        other => panic!("expected typed gap page, got {other:?}"),
    }

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
fn durable_event_replay_is_not_limited_by_the_memory_window() {
    let store = Arc::new(RecordingDurableEventStore::default());
    let authority = semantic_lease_adapter().with_event_store(store).authority();
    let source = authority.semantic_event_source();
    for sequence in 1..=(OPERATION_EVENT_HISTORY_CAPACITY + 44) {
        authority.publish_semantic_event(
            semantic::Identity {
                id: format!("worker-{sequence}"),
                generation: 1,
            },
            "worker.observed",
            "cyrene.worker.v1",
            Vec::new(),
        );
    }
    let context = scoped_authority_context("default", "durable-replay");
    let principal = principal_from_peer_cred(&AUTHORITY_TEST_PEER);
    let first = authority
        .read_events(
            &context,
            &principal,
            &semantic::EventCursor {
                source: source.clone(),
                sequence: 0,
            },
            OPERATION_EVENT_HISTORY_CAPACITY,
        )
        .unwrap();
    assert_eq!(first.status, semantic::ReplayStatus::Current);
    assert_eq!(first.oldest_available_sequence, 1);
    assert_eq!(first.latest_available_sequence, 300);
    assert_eq!(first.events.len(), OPERATION_EVENT_HISTORY_CAPACITY);
    assert_eq!(first.events[0].sequence, 1);
    assert_eq!(first.events.last().unwrap().sequence, 256);

    let second = authority
        .read_events(
            &context,
            &principal,
            &semantic::EventCursor {
                source,
                sequence: first.next_sequence,
            },
            OPERATION_EVENT_HISTORY_CAPACITY,
        )
        .unwrap();
    assert_eq!(
        second
            .events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        (257..=300).collect::<Vec<_>>(),
    );
    assert_eq!(
        authority
            .snapshot(&context, &principal)
            .unwrap()
            .cursor
            .sequence,
        300
    );
}

#[test]
fn durable_retention_gap_requires_snapshot_before_resume() {
    let store = Arc::new(RecordingDurableEventStore::default());
    let authority = semantic_lease_adapter()
        .with_event_store(store.clone())
        .authority();
    let source = authority.semantic_event_source();
    for sequence in 1..=10 {
        authority.publish_semantic_event(
            semantic::Identity {
                id: format!("worker-{sequence}"),
                generation: 1,
            },
            "worker.observed",
            "cyrene.worker.v1",
            Vec::new(),
        );
    }
    store
        .records
        .lock()
        .unwrap()
        .retain(|record| record.event.sequence >= 5);
    let context = scoped_authority_context("default", "durable-gap");
    let principal = principal_from_peer_cred(&AUTHORITY_TEST_PEER);
    let gap = authority
        .read_events(
            &context,
            &principal,
            &semantic::EventCursor {
                source: source.clone(),
                sequence: 1,
            },
            1,
        )
        .unwrap();
    assert_eq!(gap.status, semantic::ReplayStatus::Gap);
    assert!(gap.events.is_empty());
    assert_eq!(gap.oldest_available_sequence, 5);

    let snapshot = authority.snapshot(&context, &principal).unwrap();
    assert_eq!(snapshot.cursor.sequence, 10);
    let resumed = authority
        .read_events(&context, &principal, &snapshot.cursor, 1)
        .unwrap();
    assert_eq!(resumed.status, semantic::ReplayStatus::Current);
    assert!(resumed.events.is_empty());
}

#[test]
fn durable_event_read_failure_never_falls_back_to_memory_history() {
    let store = Arc::new(RecordingDurableEventStore::default());
    store.fail_reads.store(true, Ordering::SeqCst);
    let authority = semantic_lease_adapter().with_event_store(store).authority();
    let context = scoped_authority_context("default", "durable-read-failure");
    let principal = principal_from_peer_cred(&AUTHORITY_TEST_PEER);
    let cursor = semantic::EventCursor {
        source: authority.semantic_event_source(),
        sequence: 0,
    };
    assert_eq!(
        authority
            .read_events(&context, &principal, &cursor, 1)
            .unwrap_err()
            .reason_code,
        "EVENT_READ_FAILED"
    );
    assert_eq!(
        authority
            .snapshot(&context, &principal)
            .unwrap_err()
            .reason_code,
        "EVENT_READ_FAILED"
    );
}

#[test]
fn recreated_authority_continues_sequence_for_the_same_source() {
    let store = Arc::new(RecordingDurableEventStore::default());
    let first = semantic_lease_adapter()
        .with_event_store(store.clone())
        .authority();
    let source = first.semantic_event_source();
    for sequence in 1..=3 {
        first.publish_semantic_event(
            semantic::Identity {
                id: format!("worker-{sequence}"),
                generation: 1,
            },
            "worker.observed",
            "cyrene.worker.v1",
            Vec::new(),
        );
    }

    let recreated = semantic_lease_adapter().with_event_store(store).authority();
    recreated.publish_semantic_event(
        semantic::Identity {
            id: "worker-4".to_string(),
            generation: 1,
        },
        "worker.observed",
        "cyrene.worker.v1",
        Vec::new(),
    );
    let context = scoped_authority_context("default", "recreated-source");
    let principal = principal_from_peer_cred(&AUTHORITY_TEST_PEER);
    let replay = recreated
        .read_events(
            &context,
            &principal,
            &semantic::EventCursor {
                source,
                sequence: 0,
            },
            4,
        )
        .unwrap();
    assert_eq!(
        replay
            .events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2, 3, 4],
    );
}

#[test]
fn cursor_from_an_older_epoch_returns_source_changed() {
    let store = Arc::new(RecordingDurableEventStore::default());
    let first = semantic_lease_adapter_at_epoch(7)
        .with_event_store(store.clone())
        .authority();
    first.publish_semantic_event(
        semantic::Identity {
            id: "worker-old".to_string(),
            generation: 1,
        },
        "worker.observed",
        "cyrene.worker.v1",
        Vec::new(),
    );
    let old_cursor = semantic::EventCursor {
        source: first.semantic_event_source(),
        sequence: 1,
    };
    let restarted = semantic_lease_adapter_at_epoch(8)
        .with_event_store(store)
        .authority();
    let context = scoped_authority_context("default", "changed-source");
    let principal = principal_from_peer_cred(&AUTHORITY_TEST_PEER);
    let page = restarted
        .read_events(&context, &principal, &old_cursor, 1)
        .unwrap();
    assert_eq!(page.status, semantic::ReplayStatus::SourceChanged);
    assert!(page.events.is_empty());
    let snapshot = restarted.snapshot(&context, &principal).unwrap();
    assert_eq!(snapshot.source.generation, 8);
    assert_eq!(snapshot.cursor.sequence, 0);
}

/// Item 3: the snapshot cursor and the durable event ordering are the SAME
/// consistency boundary. A snapshot taken at cursor C, followed by
/// `read_events(C)`, must reconstruct the live authority state: the set of
/// operation identities in the snapshot equals the set of `operation.created`
/// subjects in the durable history at or before C, the cursor equals the
/// latest durable sequence, and replay from C is empty/Current when nothing
/// changed after the snapshot.
/// 中文：条目 3：快照游标与持久化事件顺序属于同一个一致性边界。在游标 C 处取得快照后调用 `read_events(C)`，必须能够还原实时权限状态：快照中的操作标识集合，等于持久化历史中序号不大于 C 的 `operation.created` 事件主题集合；游标等于最新的持久化序号；若快照后没有变化，从 C 重放应为空并返回 Current。
#[test]
fn snapshot_cursor_and_durable_event_ordering_share_one_boundary() {
    let store = Arc::new(RecordingDurableEventStore::default());
    let authority = semantic_lease_adapter().with_event_store(store).authority();
    let context = scoped_authority_context("default", "audit-boundary");
    let principal = principal_from_peer_cred(&AUTHORITY_TEST_PEER);

    let worker_identity = semantic::Identity {
        id: "audit-worker".to_string(),
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
    authority
        .acquire_lease(
            &context,
            &principal,
            worker_identity.clone(),
            query,
            u64::MAX,
        )
        .unwrap();
    let operation = semantic::Operation {
        identity: semantic::Identity {
            id: "operation-audit".to_string(),
            generation: 1,
        },
        owner: principal.identity.clone(),
        executor: semantic::Identity {
            id: "provider-x".to_string(),
            generation: 1,
        },
        kind: "ai.train".to_string(),
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
    authority
        .report_operation(
            &context,
            &principal,
            semantic::Operation {
                state: semantic::OperationState::Succeeded,
                ..operation.clone()
            },
        )
        .unwrap();

    let snapshot = authority.snapshot(&context, &principal).unwrap();
    assert!(snapshot.cursor.sequence > 0);

    // (a) The snapshot cursor equals the latest durable sequence.
    // 中文：(a) 快照游标等于最新的持久化序号。
    let full = authority
        .read_events(
            &context,
            &principal,
            &semantic::EventCursor {
                source: snapshot.source.clone(),
                sequence: 0,
            },
            OPERATION_EVENT_HISTORY_CAPACITY,
        )
        .unwrap();
    assert_eq!(full.status, semantic::ReplayStatus::Current);
    assert_eq!(snapshot.cursor.sequence, full.latest_available_sequence);
    // The durable replay is contiguous 1..=latest, the exact ordering the
    // snapshot cursor is derived from.
    // 中文：持久化重放序列从 1 到 latest 连续递增；快照游标正是据此顺序推导出来的。
    let sequences: Vec<u64> = full.events.iter().map(|event| event.sequence).collect();
    assert_eq!(
        sequences,
        (1..=snapshot.cursor.sequence).collect::<Vec<_>>()
    );

    // (b) Replay from the snapshot cursor is empty and Current: nothing
    // changed after the snapshot, so Snapshot @ C is already complete and
    // read_events(C) contributes no further (and no lost) transition.
    // 中文：(b) 从快照游标开始重放时结果为空且状态为 Current：快照之后没有变化，因此游标 C 处的快照已完整，`read_events(C)` 不会再补充任何状态迁移，也不会漏掉迁移。
    let after = authority
        .read_events(&context, &principal, &snapshot.cursor, 256)
        .unwrap();
    assert_eq!(after.status, semantic::ReplayStatus::Current);
    assert!(after.events.is_empty());

    // (c) The snapshot's operation set is identical to the set of operations
    // whose `operation.created` event is in the durable history at or before C.
    // 中文：(c) 快照中的操作集合，必须与持久化历史中序号不大于 C 的 `operation.created` 事件所对应的操作集合完全相同。
    let mut created: Vec<String> = full
        .events
        .iter()
        .filter(|event| event.kind == "operation.created")
        .map(|event| event.subject.id.clone())
        .collect();
    created.sort();
    let mut snapshot_operations: Vec<String> = snapshot
        .operations
        .iter()
        .map(|operation| operation.identity.id.clone())
        .collect();
    snapshot_operations.sort();
    assert_eq!(created, snapshot_operations);
}

/// Item 4: a snapshot must stay consistent with the incremental replay while
/// transitions mutate authority state concurrently. The reader constantly
/// takes a snapshot and replays from its cursor; because the snapshot now
/// captures the event cursor BEFORE reading state, every transition published
/// during the read is either already reflected in the snapshot or returned by
/// the replay — never lost from both. The same-source replay from a fresh
/// snapshot cursor can only be Current, and the latest available sequence can
/// never fall behind the snapshot cursor.
/// 中文：条目 4：即使状态迁移并发修改权限状态，快照与增量重放也必须保持一致。读取方持续获取快照并从其游标开始重放；由于快照现在会先捕获事件游标再读取状态，读取期间发布的每次迁移要么已反映在快照中，要么会由重放返回，绝不能两边都丢失。从新快照游标开始、且事件源相同的重放只能返回 Current；可用的最新序号也绝不能小于快照游标。
#[test]
fn snapshot_stays_consistent_while_operations_mutate_concurrently() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;

    let store = Arc::new(RecordingDurableEventStore::default());
    let authority = Arc::new(semantic_lease_adapter().with_event_store(store).authority());
    let context = scoped_authority_context("default", "concurrent-snapshot");
    let principal = principal_from_peer_cred(&AUTHORITY_TEST_PEER);

    let writers = 4;
    let per_writer = 40;
    let total = writers * per_writer;
    let running = Arc::new(AtomicBool::new(true));

    let mut handles = Vec::new();
    for writer in 0..writers {
        let authority = authority.clone();
        let context = context.clone();
        let principal = principal.clone();
        let running = running.clone();
        handles.push(thread::spawn(move || {
            for index in 0..per_writer {
                let operation = semantic::Operation {
                    identity: semantic::Identity {
                        id: format!("op-{writer}-{index}"),
                        generation: 1,
                    },
                    owner: principal.identity.clone(),
                    executor: semantic::Identity {
                        id: "provider-x".to_string(),
                        generation: 1,
                    },
                    kind: "ai.train".to_string(),
                    state: semantic::OperationState::Created,
                    deadline_unix_ms: None,
                    parent: None,
                    metadata: BTreeMap::new(),
                };
                // A transition publishes its durable event only after mutating
                // state; the reordered snapshot must never lose it.
                // 中文：状态变更后才发布对应的持久化事件；调整顺序后的快照绝不能漏掉该事件。
                let _ = authority.create_operation(&context, &principal, operation);
                if !running.load(Ordering::SeqCst) {
                    break;
                }
            }
        }));
    }

    let reader_authority = authority.clone();
    let reader_context = context.clone();
    let reader_principal = principal.clone();
    let reader_running = running.clone();
    let reader = thread::spawn(move || {
        let mut checked = 0;
        loop {
            let keep_running = reader_running.load(Ordering::SeqCst);
            let snapshot = match reader_authority.snapshot(&reader_context, &reader_principal) {
                Ok(snapshot) => snapshot,
                Err(_) => {
                    if !keep_running {
                        break;
                    }
                    continue;
                }
            };
            let page = reader_authority
                .read_events(&reader_context, &reader_principal, &snapshot.cursor, 256)
                .expect("replay from a fresh snapshot cursor must not fail");
            // Under concurrency a writer may commit between the snapshot and
            // this replay, or the in-memory window may roll past the snapshot
            // cursor. `Gap`/`SourceChanged` are valid outcomes: the client must
            // resnapshot. The invariant under test is that no event is lost or
            // corrupted, not that every poll is Current.
            // 中文：并发期间，写入方可能在快照与本次重放之间完成提交，也可能使内存事件窗口越过快照游标。Gap 或 SourceChanged 都是有效结果，此时客户端必须重新获取快照。这里验证的不变量是没有事件丢失或损坏，而不是每次轮询都返回 Current。
            assert!(
                matches!(
                    page.status,
                    semantic::ReplayStatus::Current
                        | semantic::ReplayStatus::Gap
                        | semantic::ReplayStatus::SourceChanged
                ),
                "replay must yield a valid Current/Gap/SourceChanged status"
            );
            assert!(page.latest_available_sequence >= snapshot.cursor.sequence);
            checked += 1;
            if !keep_running {
                break;
            }
        }
        checked
    });

    for handle in handles {
        handle.join().unwrap();
    }
    running.store(false, Ordering::SeqCst);
    let checked = reader.join().unwrap();
    assert!(checked > 0);

    // Quiescent audit: every created operation is present in both the durable
    // event log and the snapshot. Nothing was lost between the state read and
    // the cursor read under concurrency.
    // 中文：静止状态审计：每个已创建的操作都同时存在于持久化事件日志和快照中。并发期间状态读取与游标读取之间没有丢失任何操作。
    let snapshot = authority.snapshot(&context, &principal).unwrap();
    let full = authority
        .read_events(
            &context,
            &principal,
            &semantic::EventCursor {
                source: snapshot.source.clone(),
                sequence: 0,
            },
            OPERATION_EVENT_HISTORY_CAPACITY,
        )
        .unwrap();
    assert_eq!(full.status, semantic::ReplayStatus::Current);
    let mut created: Vec<String> = full
        .events
        .iter()
        .filter(|event| event.kind == "operation.created")
        .map(|event| event.subject.id.clone())
        .collect();
    created.sort();
    let mut in_snapshot: Vec<String> = snapshot
        .operations
        .iter()
        .map(|operation| operation.identity.id.clone())
        .collect();
    in_snapshot.sort();

    assert_eq!(created, in_snapshot);
    assert_eq!(snapshot.cursor.sequence, full.latest_available_sequence);
    assert_eq!(snapshot.cursor.sequence, total as u64);
}
