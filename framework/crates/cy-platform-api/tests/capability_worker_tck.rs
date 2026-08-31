// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: framework/crates/cy-platform-api/tests/capability_worker_tck.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
use std::{collections::HashMap, path::PathBuf, process::Command, time::Duration};

use cy_manifest::{
    CapabilityDescriptor, CapabilityId, CapabilityInterfaceVersion, Edition, ExecutionMode,
    PluginCapabilitiesManifest, PluginDependencies, PluginManifest, PluginMetadata, RestartPolicy,
    Runtime,
};
use cy_platform_api::{
    ApplicationEventStreamEndReason, AtomicCancellationToken, CapabilityWorkerActivator,
    NeverCancelled, WorkerActivationOptions, WorkerTerminalError,
};
use serde_json::json;

fn root_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn python_executable() -> String {
    std::env::var("CYRENE_PYTHON").unwrap_or_else(|_| {
        if cfg!(windows) {
            "python".to_string()
        } else {
            "python3".to_string()
        }
    })
}

fn python_worker_available() -> bool {
    Command::new(python_executable())
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn skip_without_python() -> bool {
    if python_worker_available() {
        return false;
    }

    eprintln!(
        "Skipping Python worker TCK: no usable Python interpreter found; set CYRENE_PYTHON to one"
    );
    true
}

fn fixture_manifest() -> PluginManifest {
    PluginManifest {
        plugin: PluginMetadata {
            id: "com.cyrene.tck.generic-worker".to_string(),
            name: "Generic TCK Worker".to_string(),
            version: "1.0.0".to_string(),
            api_version: "1.0".to_string(),
            kind: "capability-worker".to_string(),
            edition: Edition::Community,
            runtime: Some(Runtime::SubprocessPython),
            license_gate: false,
            entrypoint: Some("fixtures.test_worker:GenericTckWorker".to_string()),
            description: Some("Platform TCK worker".to_string()),
            author: None,
            license: None,
            source_target: None,
            status: Some("active".to_string()),
            protocol_version: None,
            scope: None,
            restart_policy: RestartPolicy::Never,
        },
        capabilities: PluginCapabilitiesManifest::default(),
        dependencies: PluginDependencies::default(),
        components: Vec::new(),
        launch: None,
        permissions: None,
        resources: None,
        package: None,
        capability_descriptors: vec![
            CapabilityDescriptor::new(
                CapabilityId::new("test.capability.v1").unwrap(),
                CapabilityInterfaceVersion::new("1").unwrap(),
                vec![ExecutionMode::Worker],
            )
            .unwrap(),
            CapabilityDescriptor::new(
                CapabilityId::new("test.application-events.v1").unwrap(),
                CapabilityInterfaceVersion::new("1").unwrap(),
                vec![ExecutionMode::Worker],
            )
            .unwrap(),
        ],
        artifact: None,
    }
}

fn worker_options() -> WorkerActivationOptions {
    let root = root_dir();
    let python_sdk_dir = root.join("sdk/python");
    let shim_dir = root.join("sdk/python/cyrene_worker_shim");
    let tests_dir = root.join("framework/crates/cy-platform-api/tests");

    WorkerActivationOptions {
        working_dir: Some(tests_dir),
        python_path: vec![python_sdk_dir, shim_dir],
        python_executable: Some(python_executable()),
        environment: HashMap::new(),
        handshake_timeout: Duration::from_secs(5),
        default_invoke_timeout: Duration::from_secs(5),
        shutdown_grace_period: Duration::from_secs(2),
        max_message_bytes: 1024 * 1024,
    }
}

#[test]
fn test_capability_worker_activation_and_handshake() {
    if skip_without_python() {
        return;
    }

    let manifest = fixture_manifest();
    let options = worker_options();

    let mut client = CapabilityWorkerActivator::activate_from_manifest(&manifest, &options)
        .expect("worker activation and handshake should succeed");

    assert_eq!(client.plugin_id(), "com.cyrene.tck.generic-worker");
    assert_eq!(client.plugin_version(), "1.0.0");
    assert_eq!(client.api_version(), "1.0");
    assert!(
        client
            .declared_capabilities()
            .contains(&"test.capability.v1".to_string())
    );

    client
        .shutdown(Duration::from_secs(1))
        .expect("shutdown should succeed");
}

fn event_filter(mode: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({"mode": mode})).unwrap()
}

#[test]
fn test_capability_worker_application_events_single_and_ordered() {
    if skip_without_python() {
        return;
    }

    let manifest = fixture_manifest();
    let options = worker_options();
    let mut client =
        CapabilityWorkerActivator::activate_from_manifest(&manifest, &options).unwrap();

    let single = client
        .subscribe_application_events_with_buffer(
            "test.application-events.v1",
            &event_filter("single"),
            4,
            Duration::from_secs(2),
        )
        .unwrap();
    let event = single.next(Duration::from_secs(2)).unwrap();
    assert_eq!(event.event_sequence, 1);
    assert_eq!(event.event_type, "synthetic");
    assert_eq!(event.payload, b"one");
    let terminal = single.next(Duration::from_secs(2)).unwrap_err();
    assert_eq!(
        terminal.termination().unwrap().reason,
        ApplicationEventStreamEndReason::NormalCompletion
    );

    let ordered = client
        .subscribe_application_events_with_buffer(
            "test.application-events.v1",
            &event_filter("ordered"),
            4,
            Duration::from_secs(2),
        )
        .unwrap();
    let values = (0..3)
        .map(|_| String::from_utf8(ordered.next(Duration::from_secs(2)).unwrap().payload).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(values, vec!["1", "2", "3"]);
    assert_eq!(
        ordered
            .next(Duration::from_secs(2))
            .unwrap_err()
            .termination()
            .unwrap()
            .reason,
        ApplicationEventStreamEndReason::NormalCompletion
    );

    client.shutdown(Duration::from_secs(1)).unwrap();
}

#[test]
fn test_capability_worker_application_events_bounded_backpressure_and_slow_consumer() {
    if skip_without_python() {
        return;
    }

    let manifest = fixture_manifest();
    let options = worker_options();
    let mut client =
        CapabilityWorkerActivator::activate_from_manifest(&manifest, &options).unwrap();

    let burst = client
        .subscribe_application_events_with_buffer(
            "test.application-events.v1",
            &event_filter("burst"),
            2,
            Duration::from_secs(2),
        )
        .unwrap();
    assert_eq!(
        burst.next(Duration::from_secs(2)).unwrap().event_sequence,
        1
    );
    assert_eq!(
        burst.next(Duration::from_secs(2)).unwrap().event_sequence,
        2
    );
    let terminal = burst.next(Duration::from_secs(2)).unwrap_err();
    assert_eq!(
        terminal.termination().unwrap().reason,
        ApplicationEventStreamEndReason::Backpressure
    );

    let slow = client
        .subscribe_application_events_with_buffer(
            "test.application-events.v1",
            &event_filter("slow"),
            1,
            Duration::from_secs(2),
        )
        .unwrap();
    assert_eq!(slow.next(Duration::from_secs(2)).unwrap().payload, b"slow");
    assert_eq!(
        slow.next(Duration::from_secs(2))
            .unwrap_err()
            .termination()
            .unwrap()
            .reason,
        ApplicationEventStreamEndReason::NormalCompletion
    );

    client.shutdown(Duration::from_secs(1)).unwrap();
}

#[test]
fn test_capability_worker_application_events_pre_cancel_and_unsubscribe() {
    if skip_without_python() {
        return;
    }

    let manifest = fixture_manifest();
    let options = worker_options();
    let mut client =
        CapabilityWorkerActivator::activate_from_manifest(&manifest, &options).unwrap();

    let cancellation = AtomicCancellationToken::new();
    cancellation.cancel();
    let pre_cancelled = client.subscribe_application_events_with_cancellation(
        "test.application-events.v1",
        &event_filter("hold"),
        2,
        Duration::from_secs(2),
        &cancellation,
    );
    assert!(matches!(
        pre_cancelled,
        Err(WorkerTerminalError::Cancelled(_))
    ));

    let subscription = client
        .subscribe_application_events_with_buffer(
            "test.application-events.v1",
            &event_filter("hold"),
            2,
            Duration::from_secs(2),
        )
        .unwrap();
    subscription
        .unsubscribe(&mut client, Duration::from_secs(2))
        .unwrap();
    let terminal = subscription.next(Duration::from_secs(2)).unwrap_err();
    assert_eq!(
        terminal.termination().unwrap().reason,
        ApplicationEventStreamEndReason::Cancelled
    );

    // Cancel while the synthetic producer is still in flight; the delayed
    // event must not turn cancellation into a successful completion.
    let in_flight = client
        .subscribe_application_events_with_buffer(
            "test.application-events.v1",
            &event_filter("slow"),
            2,
            Duration::from_secs(2),
        )
        .unwrap();
    in_flight
        .unsubscribe(&mut client, Duration::from_secs(2))
        .unwrap();
    assert_eq!(
        in_flight
            .next(Duration::from_secs(2))
            .unwrap_err()
            .termination()
            .unwrap()
            .reason,
        ApplicationEventStreamEndReason::Cancelled
    );

    client.shutdown(Duration::from_secs(1)).unwrap();
}

#[test]
fn test_capability_worker_application_events_generation_and_worker_crash() {
    if skip_without_python() {
        return;
    }

    let manifest = fixture_manifest();
    let options = worker_options();
    let mut client =
        CapabilityWorkerActivator::activate_from_manifest(&manifest, &options).unwrap();

    let generation = client
        .subscribe_application_events_with_buffer(
            "test.application-events.v1",
            &event_filter("generation"),
            2,
            Duration::from_secs(2),
        )
        .unwrap();
    assert_eq!(
        generation
            .next(Duration::from_secs(2))
            .unwrap_err()
            .termination()
            .unwrap()
            .reason,
        ApplicationEventStreamEndReason::GenerationTerminated
    );

    let crash = client
        .subscribe_application_events_with_buffer(
            "test.application-events.v1",
            &event_filter("hold"),
            2,
            Duration::from_secs(2),
        )
        .unwrap();
    let invoke_error = client.invoke(
        "test.capability.v1",
        "crash",
        b"{}",
        Duration::from_secs(2),
        &NeverCancelled,
    );
    assert!(matches!(
        invoke_error,
        Err(WorkerTerminalError::WorkerCrashed(_))
    ));
    assert_eq!(
        crash
            .next(Duration::from_secs(2))
            .unwrap_err()
            .termination()
            .unwrap()
            .reason,
        ApplicationEventStreamEndReason::WorkerCrash
    );
}

#[test]
fn test_capability_worker_application_events_normal_shutdown() {
    if skip_without_python() {
        return;
    }

    let manifest = fixture_manifest();
    let options = worker_options();
    let mut client =
        CapabilityWorkerActivator::activate_from_manifest(&manifest, &options).unwrap();
    let subscription = client
        .subscribe_application_events_with_buffer(
            "test.application-events.v1",
            &event_filter("hold"),
            2,
            Duration::from_secs(2),
        )
        .unwrap();

    client.shutdown(Duration::from_secs(1)).unwrap();
    assert_eq!(
        subscription
            .next(Duration::from_secs(2))
            .unwrap_err()
            .termination()
            .unwrap()
            .reason,
        ApplicationEventStreamEndReason::NormalCompletion
    );
}

#[test]
fn test_capability_worker_typed_invocation() {
    if skip_without_python() {
        return;
    }

    let manifest = fixture_manifest();
    let options = worker_options();
    let mut client =
        CapabilityWorkerActivator::activate_from_manifest(&manifest, &options).unwrap();

    let req_payload = serde_json::to_vec(&json!({"message": "ping"})).unwrap();
    let resp_bytes = client
        .invoke(
            "test.capability.v1",
            "echo",
            &req_payload,
            Duration::from_secs(2),
            &NeverCancelled,
        )
        .expect("invoke echo should succeed");

    let resp_json: serde_json::Value = serde_json::from_slice(&resp_bytes).unwrap();
    assert_eq!(resp_json["echo"], "ping");

    // Test compute
    let compute_payload = serde_json::to_vec(&json!({"value": 21})).unwrap();
    let compute_resp_bytes = client
        .invoke(
            "test.capability.v1",
            "compute",
            &compute_payload,
            Duration::from_secs(2),
            &NeverCancelled,
        )
        .expect("invoke compute should succeed");

    let compute_json: serde_json::Value = serde_json::from_slice(&compute_resp_bytes).unwrap();
    assert_eq!(compute_json["result"], 42);

    client.shutdown(Duration::from_secs(1)).unwrap();
}

#[test]
fn test_capability_worker_invalid_request_error() {
    if skip_without_python() {
        return;
    }

    let manifest = fixture_manifest();
    let options = worker_options();
    let mut client =
        CapabilityWorkerActivator::activate_from_manifest(&manifest, &options).unwrap();

    let invalid_payload = serde_json::to_vec(&json!({"value": -5})).unwrap();
    let err = client
        .invoke(
            "test.capability.v1",
            "compute",
            &invalid_payload,
            Duration::from_secs(2),
            &NeverCancelled,
        )
        .expect_err("negative value must return invalid request error");

    assert!(matches!(err, WorkerTerminalError::InvalidRequest(_)));
    assert!(err.message().contains("INVALID_INPUT"));

    client.shutdown(Duration::from_secs(1)).unwrap();
}

#[test]
fn test_capability_worker_execution_failure_error() {
    if skip_without_python() {
        return;
    }

    let manifest = fixture_manifest();
    let options = worker_options();
    let mut client =
        CapabilityWorkerActivator::activate_from_manifest(&manifest, &options).unwrap();

    let err = client
        .invoke(
            "test.capability.v1",
            "fail",
            b"{}",
            Duration::from_secs(2),
            &NeverCancelled,
        )
        .expect_err("fail operation must return execution failure error");

    assert!(matches!(
        err,
        WorkerTerminalError::CapabilityExecutionFailure(_)
    ));
    assert!(err.message().contains("EXECUTION_FAILED"));

    client.shutdown(Duration::from_secs(1)).unwrap();
}

#[test]
fn test_capability_worker_pre_cancellation() {
    if skip_without_python() {
        return;
    }

    let manifest = fixture_manifest();
    let options = worker_options();
    let mut client =
        CapabilityWorkerActivator::activate_from_manifest(&manifest, &options).unwrap();

    let cancellation = AtomicCancellationToken::new();
    cancellation.cancel(); // Pre-cancel

    let err = client
        .invoke(
            "test.capability.v1",
            "echo",
            b"{}",
            Duration::from_secs(2),
            &cancellation,
        )
        .expect_err("pre-cancelled invocation must fail immediately");

    assert!(matches!(err, WorkerTerminalError::Cancelled(_)));

    client.shutdown(Duration::from_secs(1)).unwrap();
}

#[test]
fn test_capability_worker_inflight_cancellation() {
    if skip_without_python() {
        return;
    }

    let manifest = fixture_manifest();
    let options = worker_options();
    let mut client =
        CapabilityWorkerActivator::activate_from_manifest(&manifest, &options).unwrap();

    let cancellation = AtomicCancellationToken::new();
    let cancel_token_clone = cancellation.clone();

    // Spawn thread to cancel after 50ms
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        cancel_token_clone.cancel();
    });

    let slow_payload = serde_json::to_vec(&json!({"delay_ms": 1000})).unwrap();
    let err = client
        .invoke(
            "test.capability.v1",
            "slow_operation",
            &slow_payload,
            Duration::from_secs(3),
            &cancellation,
        )
        .expect_err("in-flight cancelled invocation must return Cancelled error");

    assert!(matches!(err, WorkerTerminalError::Cancelled(_)));

    client.shutdown(Duration::from_secs(1)).unwrap();
}

#[test]
fn test_capability_worker_timeout() {
    if skip_without_python() {
        return;
    }

    let manifest = fixture_manifest();
    let options = worker_options();
    let mut client =
        CapabilityWorkerActivator::activate_from_manifest(&manifest, &options).unwrap();

    let slow_payload = serde_json::to_vec(&json!({"delay_ms": 1000})).unwrap();
    let err = client
        .invoke(
            "test.capability.v1",
            "slow_operation",
            &slow_payload,
            Duration::from_millis(50), // very short timeout
            &NeverCancelled,
        )
        .expect_err("slow operation should timeout");

    assert!(matches!(err, WorkerTerminalError::Timeout(_)));

    client.shutdown(Duration::from_secs(1)).unwrap();
}

#[test]
fn test_capability_worker_crash_detection() {
    if skip_without_python() {
        return;
    }

    let manifest = fixture_manifest();
    let options = worker_options();
    let mut client =
        CapabilityWorkerActivator::activate_from_manifest(&manifest, &options).unwrap();

    let err = client
        .invoke(
            "test.capability.v1",
            "crash",
            b"{}",
            Duration::from_secs(2),
            &NeverCancelled,
        )
        .expect_err("crashing worker must be detected");

    assert!(matches!(err, WorkerTerminalError::WorkerCrashed(_)));
}

#[test]
fn test_capability_worker_drop_cleanup() {
    if skip_without_python() {
        return;
    }

    let manifest = fixture_manifest();
    let options = worker_options();
    let client = CapabilityWorkerActivator::activate_from_manifest(&manifest, &options).unwrap();

    // Drop client without explicit shutdown
    drop(client);
    // Cleanup ensures child is terminated
}
