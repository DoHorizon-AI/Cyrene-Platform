//! Execution-control service tests.
//!
//! These tests cover authenticated session epochs, runtime enrollment,
//! command/assignment correlation, and stale-session rejection.

use super::*;

fn node(epoch: u64) -> NodeKey {
    NodeKey {
        node_id: "node-test".to_string(),
        node_epoch: epoch,
    }
}

fn runtime() -> semantic::Identity {
    semantic::Identity {
        id: "runtime-test".to_string(),
        generation: 1,
    }
}

#[tokio::test]
async fn higher_node_epoch_evicts_lower_sessions_and_bindings() {
    let old = node(1);
    let new = node(2);
    let (sender, _receiver) = mpsc::channel(1);
    let handle = Arc::new(SessionHandle::new("old-session".to_string(), sender));
    let mut registry = Registry::default();
    registry.admit_node_epoch(&old).unwrap();
    registry.hosts.insert(
        old.clone(),
        HostSession {
            handle: handle.clone(),
        },
    );
    registry.accepted_assignments.insert(
        "assignment-test".to_string(),
        AssignmentBinding {
            assignment_id: "assignment-test".to_string(),
            runtime: runtime(),
            node: old.clone(),
            lease: LeaseKey {
                identity: semantic::Identity {
                    id: "lease-test".to_string(),
                    generation: 1,
                },
                fence_token: 1,
            },
            digest: [0; 32],
        },
    );

    let evicted = registry.admit_node_epoch(&new).unwrap();

    assert_eq!(evicted.len(), 1);
    assert!(registry.hosts.is_empty());
    assert!(registry.accepted_assignments.is_empty());
    assert_eq!(registry.highest_node_epochs.get("node-test"), Some(&2));
    evicted[0].fence().await;
    assert!(handle.is_fenced());
}

#[test]
fn lower_node_epoch_is_rejected_without_mutation() {
    let mut registry = Registry::default();
    registry.admit_node_epoch(&node(3)).unwrap();

    let error = match registry.admit_node_epoch(&node(2)) {
        Ok(_) => panic!("a lower Node epoch must be rejected"),
        Err(error) => error,
    };

    assert_eq!(error.reason_code, "STALE_NODE_GENERATION");
    assert_eq!(registry.highest_node_epochs.get("node-test"), Some(&3));
}

#[tokio::test]
async fn runtime_resume_token_stays_stable_across_reconnect() {
    let service = ExecutionControlService::new(
        Arc::new(
            crate::authentication::CertificateFingerprintAuthenticator::new([(
                b"test-certificate".to_vec(),
                AuthenticatedAgent::Runtime {
                    runtime: runtime(),
                    node: node(1).to_proto(),
                },
            )])
            .unwrap(),
        ),
        Arc::new(cy_execution_fabric::DevelopmentEnrollmentProvider::new(
            ["unused-proof".to_string()],
            60_000,
        )),
        Duration::from_secs(1),
        Duration::from_secs(1),
    )
    .unwrap();
    let runtime = runtime();
    let node = node(1);
    let runtime_key = RuntimeKey {
        runtime: runtime.clone(),
        node: node.clone(),
    };
    let (host_sender, _host_receiver) = mpsc::channel(1);
    {
        let mut registry = service.inner.registry.lock().unwrap();
        registry.highest_node_epochs.insert(node.node_id.clone(), 1);
        let host_handle = Arc::new(SessionHandle::new("host-session".to_string(), host_sender));
        host_handle.mark_ready_for_test();
        registry.hosts.insert(
            node.clone(),
            HostSession {
                handle: host_handle,
            },
        );
        registry
            .runtime_node_bindings
            .insert(runtime.clone(), node.clone());
        registry
            .runtime_resume_tokens
            .insert(runtime_key.clone(), "resume-old".to_string());
        registry.runtime_grants.insert(
            runtime_key.clone(),
            EnrollmentGrant {
                workload_identity: semantic::Identity {
                    id: "workload-test".to_string(),
                    generation: 1,
                },
                scope: RuntimeScope {
                    organization_id: "organization-test".to_string(),
                    workspace_id: "workspace-test".to_string(),
                    runtime: runtime.clone(),
                },
                expires_at_unix_ms: now_unix_ms() + 60_000,
            },
        );
    }
    let hello = runtime_resume_hello(&runtime, &node, "resume-old");
    let (first_sender, _first_receiver) = mpsc::channel(4);
    let (first_handle, _) = service
        .establish_runtime(runtime.clone(), node.to_proto(), &hello, first_sender)
        .await
        .unwrap();

    let (replacement_sender, _replacement_receiver) = mpsc::channel(4);
    service
        .establish_runtime(runtime, node.to_proto(), &hello, replacement_sender)
        .await
        .unwrap();

    assert!(first_handle.is_fenced());
    let registry = service.inner.registry.lock().unwrap();
    assert_eq!(
        registry
            .runtime_resume_tokens
            .get(&runtime_key)
            .map(String::as_str),
        Some("resume-old")
    );
    assert_eq!(registry.runtimes.len(), 1);
}

#[tokio::test]
async fn pending_bootstrap_replays_same_token_until_first_agent_frame() {
    let service = ExecutionControlService::new(
        Arc::new(
            crate::authentication::CertificateFingerprintAuthenticator::new([(
                b"test-certificate".to_vec(),
                AuthenticatedAgent::Runtime {
                    runtime: runtime(),
                    node: node(1).to_proto(),
                },
            )])
            .unwrap(),
        ),
        Arc::new(cy_execution_fabric::DevelopmentEnrollmentProvider::new(
            ["one-shot-proof".to_string()],
            60_000,
        )),
        Duration::from_secs(1),
        Duration::from_secs(1),
    )
    .unwrap();
    let runtime = runtime();
    let node = node(1);
    let runtime_key = RuntimeKey {
        runtime: runtime.clone(),
        node: node.clone(),
    };
    let (host_sender, _host_receiver) = mpsc::channel(1);
    {
        let mut registry = service.inner.registry.lock().unwrap();
        registry.highest_node_epochs.insert(node.node_id.clone(), 1);
        let host_handle = Arc::new(SessionHandle::new("host-session".to_string(), host_sender));
        host_handle.mark_ready_for_test();
        registry.hosts.insert(
            node.clone(),
            HostSession {
                handle: host_handle,
            },
        );
    }
    let mut hello = runtime_resume_hello(&runtime, &node, "");
    hello.enrollment_proof = "one-shot-proof".to_string();

    let (first_sender, mut first_receiver) = mpsc::channel(4);
    service
        .establish_runtime(runtime.clone(), node.to_proto(), &hello, first_sender)
        .await
        .unwrap();
    let first_token = runtime_welcome_token(first_receiver.recv().await.unwrap().unwrap());

    let (retry_sender, mut retry_receiver) = mpsc::channel(4);
    let (retry_handle, retry_identity) = service
        .establish_runtime(runtime.clone(), node.to_proto(), &hello, retry_sender)
        .await
        .unwrap();
    let retry_token = runtime_welcome_token(retry_receiver.recv().await.unwrap().unwrap());
    assert_eq!(first_token, retry_token);
    assert!(service
        .inner
        .registry
        .lock()
        .unwrap()
        .pending_runtime_enrollments
        .contains_key(&runtime_key));

    service.confirm_runtime_enrollment(&retry_identity, &retry_handle.session_id);
    assert!(!service
        .inner
        .registry
        .lock()
        .unwrap()
        .pending_runtime_enrollments
        .contains_key(&runtime_key));

    let (late_sender, _late_receiver) = mpsc::channel(4);
    let error = match service
        .establish_runtime(runtime, node.to_proto(), &hello, late_sender)
        .await
    {
        Ok(_) => panic!("confirmed bootstrap proof must not be replayed"),
        Err(error) => error,
    };
    assert_eq!(error.reason_code, "RESUME_TOKEN_REQUIRED");
}

#[test]
fn unavailable_kernel_result_requires_reconciliation() {
    let result = KernelCommandResult {
        command_id: "command-1".to_string(),
        outcome: Some(kernel_command_result::Outcome::Error(
            cy_proto::google::rpc::Status {
                code: Code::Unavailable as i32,
                message: "UDS response was lost".to_string(),
                details: Vec::new(),
            },
        )),
    };

    let error = authority_lease_result(result).unwrap_err();
    assert_eq!(error.reason_code, "UNKNOWN_REQUIRES_RECONCILIATION");
    assert!(error.reconciliation_required);
}

#[test]
fn structured_kernel_rejection_remains_deterministic() {
    let result = KernelCommandResult {
        command_id: "command-1".to_string(),
        outcome: Some(kernel_command_result::Outcome::Error(
            cy_proto::google::rpc::Status {
                code: Code::FailedPrecondition as i32,
                message: "lease rejected".to_string(),
                details: vec![prost_types::Any {
                    type_url: "type.googleapis.com/cyrene.semantic.v1.Rejection".to_string(),
                    value: semantic_v1::Rejection {
                        reason_code: "LEASE_FENCED".to_string(),
                        message: "lease fence is stale".to_string(),
                    }
                    .encode_to_vec(),
                }],
            },
        )),
    };

    let error = authority_lease_result(result).unwrap_err();
    assert_eq!(error.reason_code, "LEASE_FENCED");
    assert!(!error.reconciliation_required);
}

#[test]
fn workload_user_and_actions_are_bounded_and_unique() {
    assert_eq!(
        validate_user_and_actions("", &["operation.run".to_string()])
            .unwrap_err()
            .reason_code,
        "WORKLOAD_USER_REQUIRED"
    );
    assert_eq!(
        validate_user_and_actions(
            &"界".repeat(MAX_WORKLOAD_USER_BYTES / "界".len() + 1),
            &["operation.run".to_string()]
        )
        .unwrap_err()
        .reason_code,
        "WORKLOAD_USER_LIMIT"
    );
    assert_eq!(
        validate_user_and_actions(
            "user",
            &["界".repeat(MAX_WORKLOAD_ACTION_BYTES / "界".len() + 1)]
        )
        .unwrap_err()
        .reason_code,
        "WORKLOAD_ACTION_LIMIT"
    );
    assert_eq!(
        validate_user_and_actions(
            "user",
            &["operation.run".to_string(), "operation.run".to_string()]
        )
        .unwrap_err()
        .reason_code,
        "WORKLOAD_ACTION_DUPLICATE"
    );
    assert_eq!(
        validate_user_and_actions("user", &vec!["a".to_string(); MAX_WORKLOAD_ACTIONS + 1])
            .unwrap_err()
            .reason_code,
        "WORKLOAD_ACTIONS_LIMIT"
    );
}

#[test]
fn pending_binding_includes_session_and_node_identity() {
    let first = SessionBinding::for_identity(&SessionIdentity::Host(node(1)), "session-one");
    let second = SessionBinding::for_identity(&SessionIdentity::Host(node(1)), "session-two");
    let different_node =
        SessionBinding::for_identity(&SessionIdentity::Host(node(2)), "session-one");

    assert_ne!(first, second);
    assert_ne!(first, different_node);
}

fn runtime_resume_hello(
    runtime: &semantic::Identity,
    node: &NodeKey,
    resume_token: &str,
) -> ExecutionAgentHello {
    ExecutionAgentHello {
        runtime: Some(core_v1::RuntimeRef {
            identity: Some(identity_to_proto(runtime)),
        }),
        scope: Some(core_v1::AccountScope {
            user_id: String::new(),
            organization_id: "organization-test".to_string(),
            workspace_id: "workspace-test".to_string(),
        }),
        attachment_type: core_v1::ExecutionAttachmentType::ContainerAgent as i32,
        persistence_class: core_v1::PersistenceClass::Ephemeral as i32,
        agent_version: "test".to_string(),
        min_protocol_version: 2,
        max_protocol_version: 2,
        resume_token: resume_token.to_string(),
        capabilities: vec![cy_execution_fabric::execution_capability(
            core_v1::ExecutionAttachmentType::ContainerAgent,
            false,
            core_v1::RestartCapability::None,
        )],
        enrollment_proof: String::new(),
        node: Some(core_v1::ExecutionNodeDescriptor {
            node: Some(node.to_proto()),
            node_type: "container".to_string(),
            persistent: Some(false),
        }),
        restart_capability: core_v1::RestartCapability::None as i32,
    }
}

fn runtime_welcome_token(frame: cy_proto::core_v1::ControlPlaneToNode) -> String {
    match frame.body {
        Some(control_plane_to_node::Body::ExecutionAgentWelcome(welcome)) => welcome.resume_token,
        _ => panic!("expected Runtime Welcome"),
    }
}

#[test]
fn mismatched_command_result_does_not_consume_waiter() {
    let service = ExecutionControlService::new(
        Arc::new(
            crate::authentication::CertificateFingerprintAuthenticator::new([(
                b"test-certificate".to_vec(),
                AuthenticatedAgent::Host {
                    node: node(1).to_proto(),
                },
            )])
            .unwrap(),
        ),
        Arc::new(cy_execution_fabric::DevelopmentEnrollmentProvider::new(
            ["unused-proof".to_string()],
            60_000,
        )),
        Duration::from_secs(1),
        Duration::from_secs(1),
    )
    .unwrap();
    let (sender, mut receiver) = oneshot::channel();
    service.inner.pending_commands.lock().unwrap().insert(
        "command-test".to_string(),
        PendingCommand {
            binding: SessionBinding::Host {
                node: node(1),
                session_id: "session-one".to_string(),
            },
            sender,
        },
    );

    let error = service
        .complete_command_result(
            &SessionIdentity::Host(node(2)),
            "session-two",
            &KernelCommandResult {
                command_id: "command-test".to_string(),
                ..Default::default()
            },
        )
        .unwrap_err();

    assert_eq!(error.reason_code, "PENDING_COMMAND_SESSION_MISMATCH");
    assert!(service
        .inner
        .pending_commands
        .lock()
        .unwrap()
        .contains_key("command-test"));
    assert!(receiver.try_recv().is_err());
}
