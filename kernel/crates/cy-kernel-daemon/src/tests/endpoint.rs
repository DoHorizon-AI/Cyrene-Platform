//! Endpoint authority and grant lifecycle tests.
//!
//! These tests cover namespace, principal, lease, endpoint, and grant
//! authority relationships.
//! 中文：Endpoint authority 与 grant 生命周期测试。这些测试覆盖 namespace、principal、lease、endpoint 和 grant 的 authority 关系。

use super::*;

// Phase 5: Endpoint Authority Hardening (control plane)
//
// A Worker that owns an Endpoint derives its authority from its active Lease,
// its owner Principal and its Namespace. These tests prove that Endpoint
// publish/authorize/revoke require the owner Principal, that Namespace
// isolation holds, and that Lease release, Worker loss/replacement and Grant
// revocation remove the related Endpoint/Grant authority state so stale
// metadata cannot outlive its authority.
// ---------------------------------------------------------------------------
// 中文：阶段 5：Endpoint authority 加固（control plane）。拥有 Endpoint 的 Worker 通过其活动 Lease、owner Principal 和 Namespace 派生 authority。这些测试验证 Endpoint 的 publish/authorize/revoke 都要求 owner Principal；Namespace 隔离有效；Lease 释放、Worker 丢失或替换，以及 Grant 撤销都会清除相关 Endpoint/Grant authority 状态，避免过期 metadata 脱离其 authority 而继续存在。

/// Builds a namespace where one Worker owns an active Lease, a published
/// Endpoint, and an authorized EndpointGrant.
/// 中文：构造一个 namespace，其中有一个 Worker 拥有活动 Lease、已发布的 Endpoint 和已授权的 EndpointGrant。
fn endpoint_authority_scenario(
    adapter: &KernelServiceAdapter,
    namespace: &str,
    peer: PeerCred,
    worker_id: &str,
) -> (
    AuthorityCallContext,
    semantic::Principal,
    semantic::Identity,
    semantic::Lease,
    semantic::Endpoint,
    semantic::EndpointGrant,
) {
    let authority = adapter.authority();
    let context = scoped_authority_context(namespace, "endpoint-authority-scenario");
    let principal = principal_from_peer_cred(&peer);
    let worker_identity = semantic::Identity {
        id: worker_id.to_string(),
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
    authority
        .start_worker(
            &context,
            &principal,
            semantic::Worker {
                identity: worker_identity.clone(),
                principal: principal.identity.clone(),
                provider: semantic::Identity {
                    id: format!("provider-{worker_id}"),
                    generation: 1,
                },
                lease: lease.identity.clone(),
                state: semantic::WorkerState::Registered,
                execution_ref: "opaque-execution-reference".to_string(),
                limits: BTreeMap::new(),
            },
        )
        .unwrap();
    let endpoint = authority
        .publish_endpoint(
            &context,
            &principal,
            semantic::Endpoint {
                identity: semantic::Identity {
                    id: format!("endpoint-{worker_id}"),
                    generation: 1,
                },
                provider: semantic::Identity {
                    id: format!("provider-{worker_id}"),
                    generation: 1,
                },
                owner: worker_identity.clone(),
                transport: "transport.uds".to_string(),
                schema_id: "schema.v1".to_string(),
                capabilities: Vec::new(),
                public_attributes: BTreeMap::new(),
                connection_ref: "uds://runtime/direct-worker".to_string(),
                credential_ref: None,
            },
        )
        .unwrap();
    let grant = authority
        .authorize_endpoint(
            &context,
            &principal,
            semantic::EndpointGrant {
                identity: semantic::Identity {
                    id: format!("grant-{worker_id}"),
                    generation: 1,
                },
                endpoint: endpoint.identity.clone(),
                grantee: worker_identity.clone(),
                lease: lease.identity.clone(),
                fence_token: lease.fence_token,
                expires_at_unix_ms: u64::MAX,
            },
        )
        .unwrap();
    (context, principal, worker_identity, lease, endpoint, grant)
}

fn endpoint_accelerator_query() -> semantic::ResourceQuery {
    semantic::ResourceQuery {
        resource_class: "accelerator".to_string(),
        count: 1,
        required_capabilities: vec![semantic::CapabilityRequirement {
            id: "accelerator.compute".to_string(),
            minimum_revision: 1,
            required_properties: BTreeMap::new(),
        }],
        minimum_capacity: BTreeMap::new(),
    }
}

// Test A — cross-Principal Publish denied: an authenticated Principal A cannot
// publish an Endpoint for a Worker owned by Principal B.
// 中文：测试 A——拒绝跨 Principal Publish：经过认证的 Principal A 不能替 Principal B 所有的 Worker 发布 Endpoint。
#[test]
fn endpoint_cross_principal_publish_denied() {
    let adapter = semantic_worker_adapter();
    let authority = adapter.authority();
    let context = scoped_authority_context("ns-a", "cross-publish");
    let principal_a = principal_from_peer_cred(&AUTHORITY_TEST_PEER);
    let principal_b = principal_from_peer_cred(&PeerCred {
        pid: 7171,
        uid: 2000,
        gid: 2000,
    });
    let worker_b = semantic::Identity {
        id: "worker-b".to_string(),
        generation: 1,
    };
    // A owns the namespace; the Lease is held by worker-b.
    // 中文：Namespace 由 A 所有，但 Lease 由 worker-b 持有。
    let lease = authority
        .acquire_lease(
            &context,
            &principal_a,
            worker_b.clone(),
            endpoint_accelerator_query(),
            u64::MAX,
        )
        .unwrap();
    // Register a live Worker incarnation that is owned by Principal B.
    // 中文：注册一个属于 Principal B 的活动 Worker incarnation。
    let mut process = managed_test_process(
        "worker-b",
        1,
        Some(core_v1::ResourceLeaseRef {
            lease_name: lease.identity.id.clone(),
            fence_token: lease.fence_token,
        }),
    );
    process.semantic_worker = Some(semantic::Worker {
        identity: worker_b.clone(),
        principal: principal_b.identity.clone(),
        provider: semantic::Identity {
            id: "provider-b".to_string(),
            generation: 1,
        },
        lease: lease.identity.clone(),
        state: semantic::WorkerState::Running,
        execution_ref: "opaque-execution-reference".to_string(),
        limits: BTreeMap::new(),
    });
    adapter
        .instances
        .lock()
        .unwrap()
        .insert("worker-b".to_string(), process);
    authority
        .runtime
        .workers
        .lock()
        .unwrap()
        .insert(context.object_ref(worker_b.clone()), "worker-b".to_string());

    let denied = authority
        .publish_endpoint(
            &context,
            &principal_a,
            semantic::Endpoint {
                identity: semantic::Identity {
                    id: "endpoint-b".to_string(),
                    generation: 1,
                },
                provider: semantic::Identity {
                    id: "provider-b".to_string(),
                    generation: 1,
                },
                owner: worker_b.clone(),
                transport: "transport.uds".to_string(),
                schema_id: "schema.v1".to_string(),
                capabilities: Vec::new(),
                public_attributes: BTreeMap::new(),
                connection_ref: "uds://runtime/direct-worker".to_string(),
                credential_ref: None,
            },
        )
        .unwrap_err();
    assert_eq!(
        denied.reason_code, "AUTHORITY_DENIED",
        "authenticated Principal A must not publish an Endpoint for B's Worker"
    );
}

// Test B — cross-Principal Authorize denied.
// 中文：测试 B——拒绝跨 Principal Authorize。
#[test]
fn endpoint_cross_principal_authorize_denied() {
    let adapter = semantic_worker_adapter();
    let authority = adapter.authority();
    let context = scoped_authority_context("ns-a", "cross-authorize");
    let principal_b = principal_from_peer_cred(&PeerCred {
        pid: 7171,
        uid: 2000,
        gid: 2000,
    });
    let (_, _, worker_identity, lease, endpoint, _) =
        endpoint_authority_scenario(&adapter, "ns-a", AUTHORITY_TEST_PEER, "worker-a");
    let denied = authority
        .authorize_endpoint(
            &context,
            &principal_b,
            semantic::EndpointGrant {
                identity: semantic::Identity {
                    id: "grant-cross".to_string(),
                    generation: 1,
                },
                endpoint: endpoint.identity.clone(),
                grantee: worker_identity.clone(),
                lease: lease.identity.clone(),
                fence_token: lease.fence_token,
                expires_at_unix_ms: u64::MAX,
            },
        )
        .unwrap_err();
    assert_eq!(
        denied.reason_code, "NAMESPACE_AUTHORITY_DENIED",
        "authenticated Principal B must not authorize an Endpoint in A's Namespace"
    );
}

// Test C — cross-Principal Revoke denied.
// 中文：测试 C——拒绝跨 Principal Revoke。
#[test]
fn endpoint_cross_principal_revoke_denied() {
    let adapter = semantic_worker_adapter();
    let authority = adapter.authority();
    let context = scoped_authority_context("ns-a", "cross-revoke");
    let principal_b = principal_from_peer_cred(&PeerCred {
        pid: 7171,
        uid: 2000,
        gid: 2000,
    });
    let (_, _, _, _, _, grant) =
        endpoint_authority_scenario(&adapter, "ns-a", AUTHORITY_TEST_PEER, "worker-a");
    let denied = authority
        .revoke_endpoint(&context, &principal_b, &grant.identity)
        .unwrap_err();
    assert_eq!(
        denied.reason_code, "NAMESPACE_AUTHORITY_DENIED",
        "authenticated Principal B must not revoke A's Endpoint Grant"
    );
    assert!(
        authority
            .runtime
            .endpoint_grants
            .lock()
            .unwrap()
            .contains_key(&context.object_ref(grant.identity.clone())),
        "only the owner may revoke; the Grant must remain intact"
    );
}

// Test D — the Worker owner Principal is allowed to publish/authorize/revoke.
// 中文：测试 D——允许 Worker 所有者 Principal 执行 publish/authorize/revoke。
#[test]
fn endpoint_owner_principal_allowed() {
    let adapter = semantic_worker_adapter();
    let authority = adapter.authority();
    let (context, principal, _, _, endpoint, grant) =
        endpoint_authority_scenario(&adapter, "ns-a", AUTHORITY_TEST_PEER, "worker-a");
    // publish + authorize already succeeded inside the scenario.
    // 中文：publish 和 authorize 已在此场景中成功。
    assert!(authority
        .runtime
        .endpoints
        .lock()
        .unwrap()
        .contains_key(&context.object_ref(endpoint.identity.clone())));
    assert!(authority
        .runtime
        .endpoint_grants
        .lock()
        .unwrap()
        .contains_key(&context.object_ref(grant.identity.clone())));
    authority
        .revoke_endpoint(&context, &principal, &grant.identity)
        .unwrap();
    assert!(
        authority.runtime.endpoint_grants.lock().unwrap().is_empty(),
        "revoking the Grant by its owner must succeed"
    );
    assert!(
        authority
            .runtime
            .endpoints
            .lock()
            .unwrap()
            .contains_key(&context.object_ref(endpoint.identity.clone())),
        "revoking the Grant must not remove the Endpoint itself"
    );
}

// Test E — Namespace isolation: identical bare Worker/Endpoint/Lease IDs in two
// Namespaces cannot be operated across Namespace boundaries.
// 中文：测试 E——Namespace 隔离：两个 Namespace 中相同的裸 Worker/Endpoint/Lease ID 不能跨 Namespace 操作。
#[test]
fn endpoint_namespace_isolation() {
    let adapter = semantic_worker_adapter_with_resources(vec![
        test_resource_with_id("resource-a"),
        test_resource_with_id("resource-b"),
    ]);
    let authority = adapter.authority();
    let (context_a, principal_a, worker_a, _, endpoint_a, grant_a) =
        endpoint_authority_scenario(&adapter, "ns-a", AUTHORITY_TEST_PEER, "worker-x");
    let (context_b, _principal_b, worker_b, lease_b, endpoint_b, grant_b) =
        endpoint_authority_scenario(
            &adapter,
            "ns-b",
            PeerCred {
                pid: 4243,
                uid: 2000,
                gid: 2000,
            },
            "worker-x",
        );
    // The bare IDs are identical across Namespaces, but the records are scoped.
    // 中文：裸 ID 在各 Namespace 中相同，但记录按作用域分别归属。
    assert_eq!(endpoint_a.identity, endpoint_b.identity);
    assert_eq!(worker_a, worker_b);
    assert_eq!(grant_a.identity, grant_b.identity);
    assert_eq!(authority.runtime.endpoints.lock().unwrap().len(), 2);
    assert_eq!(authority.runtime.endpoint_grants.lock().unwrap().len(), 2);

    // A cannot operate B's record through A's Namespace: revoking the same bare
    // grant id in ns-a must not remove the ns-b copy.
    // 中文：A 不能通过自己的 Namespace 操作 B 的记录：在 ns-a 中撤销相同裸 grant ID，不得删除 ns-b 中的副本。
    authority
        .revoke_endpoint(&context_a, &principal_a, &grant_a.identity)
        .unwrap();
    assert!(
        !authority
            .runtime
            .endpoint_grants
            .lock()
            .unwrap()
            .contains_key(&context_a.object_ref(grant_a.identity.clone())),
        "A's revoke removes A's Namespace copy"
    );
    assert!(
        authority
            .runtime
            .endpoint_grants
            .lock()
            .unwrap()
            .contains_key(&context_b.object_ref(grant_b.identity.clone())),
        "the same bare grant id in B's Namespace must remain untouched"
    );

    // A cannot wield B's authority: authorizing a grant for the same endpoint
    // id with B's Lease/Fence is denied inside A's Namespace.
    // 中文：A 不能使用 B 的 authority：在 A 的 Namespace 中，使用 B 的 Lease/Fence 为相同 endpoint ID 授权 grant 会被拒绝。
    let denied = authority
        .authorize_endpoint(
            &context_a,
            &principal_a,
            semantic::EndpointGrant {
                identity: semantic::Identity {
                    id: "grant-x".to_string(),
                    generation: 1,
                },
                endpoint: endpoint_b.identity.clone(),
                grantee: worker_b.clone(),
                lease: lease_b.identity.clone(),
                fence_token: lease_b.fence_token,
                expires_at_unix_ms: u64::MAX,
            },
        )
        .unwrap_err();
    assert!(
        matches!(
            denied.reason_code.as_str(),
            "STALE_FENCE_TOKEN" | "ENDPOINT_NOT_FOUND"
        ),
        "cross-Namespace authorization using B's authority must be denied, got {}",
        denied.reason_code
    );
}

// Test F — stale Worker generation: loss/replacement of the Worker removes its
// Endpoint/Grant authority state and the old generation cannot republish.
// 中文：测试 F——Worker generation 过期：Worker 丢失或被替换后，会清除其 Endpoint/Grant authority 状态，旧 generation 不能再次发布。
#[test]
fn endpoint_stale_generation_invalidated() {
    let adapter = semantic_worker_adapter();
    let authority = adapter.authority();
    let (context, principal, worker_identity, _, endpoint, grant) =
        endpoint_authority_scenario(&adapter, "ns-a", AUTHORITY_TEST_PEER, "worker-a");
    assert!(authority
        .runtime
        .endpoints
        .lock()
        .unwrap()
        .contains_key(&context.object_ref(endpoint.identity.clone())));
    assert!(authority
        .runtime
        .endpoint_grants
        .lock()
        .unwrap()
        .contains_key(&context.object_ref(grant.identity.clone())));

    // Worker loss / replacement revokes the Lease and purges its authority.
    // 中文：Worker 丢失或替换会撤销 Lease 并清除其 authority。
    authority
        .mark_worker_lost(&context, &worker_identity, "TEST_WORKER_REPLACED")
        .unwrap();
    assert!(
        !authority
            .runtime
            .endpoints
            .lock()
            .unwrap()
            .contains_key(&context.object_ref(endpoint.identity.clone())),
        "Endpoint must be removed when its owning Worker is lost/replaced"
    );
    assert!(
        !authority
            .runtime
            .endpoint_grants
            .lock()
            .unwrap()
            .contains_key(&context.object_ref(grant.identity.clone())),
        "Grant must be removed when its owning Worker is lost/replaced"
    );

    // The stale generation can no longer publish: its Lease is revoked.
    // 中文：旧 generation 已无法发布：其 Lease 已撤销。
    let denied = authority
        .publish_endpoint(
            &context,
            &principal,
            semantic::Endpoint {
                identity: semantic::Identity {
                    id: "endpoint-a".to_string(),
                    generation: 1,
                },
                provider: semantic::Identity {
                    id: "provider-a".to_string(),
                    generation: 1,
                },
                owner: worker_identity.clone(),
                transport: "transport.uds".to_string(),
                schema_id: "schema.v1".to_string(),
                capabilities: Vec::new(),
                public_attributes: BTreeMap::new(),
                connection_ref: "uds://runtime/direct-worker".to_string(),
                credential_ref: None,
            },
        )
        .unwrap_err();
    assert_eq!(
        denied.reason_code, "LEASE_NOT_ACTIVE",
        "the old generation's Lease is revoked, so it has no Endpoint authority"
    );
}

// Test G — Lease release (without Worker loss) must invalidate the related
// Endpoint/Grant authority state.
// 中文：测试 G——释放 Lease（未发生 Worker 丢失）也必须使相关 Endpoint/Grant authority 状态失效。
#[test]
fn lease_release_invalidates_endpoint_grant_authority() {
    let adapter = semantic_worker_adapter();
    let authority = adapter.authority();
    let (context, principal, worker_identity, lease, endpoint, grant) =
        endpoint_authority_scenario(&adapter, "ns-a", AUTHORITY_TEST_PEER, "worker-a");
    assert!(authority
        .runtime
        .endpoint_grants
        .lock()
        .unwrap()
        .contains_key(&context.object_ref(grant.identity.clone())));

    // Release the Lease through the canonical release path (not worker loss).
    // 中文：通过规范 release 路径释放 Lease（不是 Worker 丢失路径）。
    authority
        .release_lease(&context, &principal, &lease.identity, lease.fence_token)
        .unwrap();
    assert!(
        !authority
            .runtime
            .endpoints
            .lock()
            .unwrap()
            .contains_key(&context.object_ref(endpoint.identity.clone())),
        "released Lease must remove the owner Worker's Endpoint"
    );
    assert!(
        !authority
            .runtime
            .endpoint_grants
            .lock()
            .unwrap()
            .contains_key(&context.object_ref(grant.identity.clone())),
        "released Lease must remove the dependent Grant"
    );

    // The grant is gone, so a fresh authorize against the released lease fails.
    // 中文：Grant 已删除，因此使用已释放 Lease 重新 authorize 会失败。
    let denied = authority
        .authorize_endpoint(
            &context,
            &principal,
            semantic::EndpointGrant {
                identity: semantic::Identity {
                    id: "grant-a".to_string(),
                    generation: 1,
                },
                endpoint: endpoint.identity.clone(),
                grantee: worker_identity.clone(),
                lease: lease.identity.clone(),
                fence_token: lease.fence_token,
                expires_at_unix_ms: u64::MAX,
            },
        )
        .unwrap_err();
    assert_eq!(
        denied.reason_code, "ENDPOINT_NOT_FOUND",
        "the released Worker's Endpoint must no longer resolve"
    );
}

// Test H — a revoked Grant is removed from Kernel authority state and can no
// longer be relied on by any consumer.
// 中文：测试 H——撤销的 Grant 会从 Kernel authority 状态中删除，任何消费者都不能继续依赖它。
#[test]
fn revoked_grant_removed_from_authority_state() {
    let adapter = semantic_worker_adapter();
    let authority = adapter.authority();
    let (context, principal, _, _, endpoint, grant) =
        endpoint_authority_scenario(&adapter, "ns-a", AUTHORITY_TEST_PEER, "worker-a");
    let grant_key = context.object_ref(grant.identity.clone());
    // Before revocation the Grant is present (the consumer's authority ticket).
    // 中文：撤销前 Grant 存在（它是消费者的 authority ticket）。
    assert!(authority
        .runtime
        .endpoint_grants
        .lock()
        .unwrap()
        .contains_key(&grant_key));

    authority
        .revoke_endpoint(&context, &principal, &grant.identity)
        .unwrap();
    assert!(
        !authority
            .runtime
            .endpoint_grants
            .lock()
            .unwrap()
            .contains_key(&grant_key),
        "revoked Grant must be removed from Kernel authority state"
    );
    assert!(
        authority
            .runtime
            .endpoints
            .lock()
            .unwrap()
            .contains_key(&context.object_ref(endpoint.identity.clone())),
        "revoking a Grant must not remove the Endpoint it referenced"
    );
}

// ---------------------------------------------------------------------------
