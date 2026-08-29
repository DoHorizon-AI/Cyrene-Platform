use cy_plugin_protocol::pb::{DetectHardwareRequest, Invoke};
use cy_plugin_supervisor::{PluginRuntimeState, PluginSupervisor, SupervisorError};
use std::path::PathBuf;
use std::time::Duration;

fn get_python_and_script() -> (String, String) {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let repo_root = manifest_dir.join("../../..");
    let echo_script = repo_root.join("tests/fixtures/echo_plugin.py");
    let windows_venv = repo_root.join(".venv/Scripts/python.exe");
    let unix_venv = repo_root.join(".venv/bin/python");

    let python_bin = std::env::var("CYRENE_TEST_PYTHON").unwrap_or_else(|_| {
        if windows_venv.exists() {
            windows_venv.to_string_lossy().to_string()
        } else if unix_venv.exists() {
            unix_venv.to_string_lossy().to_string()
        } else {
            "python".to_string()
        }
    });

    (python_bin, echo_script.to_string_lossy().to_string())
}

#[tokio::test]
async fn test_supervisor_normal_lifecycle_and_invoke() {
    let (py_bin, script_path) = get_python_and_script();
    let mut supervisor = PluginSupervisor::new("com.cy.probe.nvidia", py_bin, vec![script_path]);

    assert_eq!(supervisor.state(), PluginRuntimeState::Discovered);

    // 1. Start and handshake
    supervisor
        .start(Duration::from_secs(5))
        .await
        .expect("Supervisor start failed");
    assert_eq!(supervisor.state(), PluginRuntimeState::Healthy);
    assert_eq!(supervisor.declared_capabilities(), &["nvidia", "cuda"]);

    // 2. Invoke RPC
    let invoke_payload = Invoke {
        extension_point: "probe".to_string(),
        method: "detect_hardware".to_string(),
        request: Some(cy_plugin_protocol::pb::invoke::Request::DetectHardware(
            DetectHardwareRequest {},
        )),
    };

    let result = supervisor
        .invoke(invoke_payload, Duration::from_secs(5))
        .await
        .expect("Invoke failed");
    assert!(result.response.is_some());

    // 3. Stop
    supervisor.stop().await.expect("Stop failed");
    assert_eq!(supervisor.state(), PluginRuntimeState::Stopped);
}

#[tokio::test]
async fn test_supervisor_non_existent_executable() {
    let mut supervisor = PluginSupervisor::new("invalid.plugin", "non_existent_bin_xyz123", vec![]);
    let res = supervisor.start(Duration::from_secs(2)).await;

    assert!(res.is_err());
    assert_eq!(supervisor.state(), PluginRuntimeState::Unavailable);
}

#[tokio::test]
async fn test_supervisor_quarantine_on_crash_loop() {
    let (py_bin, script_path) = get_python_and_script();
    let mut supervisor = PluginSupervisor::new("crash.plugin", py_bin, vec![script_path]);

    // Loop simulating repeated crashes
    for i in 0..3 {
        if supervisor.state() != PluginRuntimeState::Healthy {
            let _ = supervisor.start(Duration::from_secs(5)).await;
        }

        let invoke_payload = Invoke {
            extension_point: "probe".to_string(),
            method: "crash".to_string(),
            request: None,
        };
        let res = supervisor
            .invoke(invoke_payload, Duration::from_secs(2))
            .await;
        assert!(res.is_err(), "Iteration {} expected crash failure", i);
    }

    assert_eq!(supervisor.state(), PluginRuntimeState::Quarantined);
    assert!(supervisor.state_reason().unwrap().contains("Quarantined"));

    // Attempt start when quarantined should fail immediately
    let err = supervisor.start(Duration::from_secs(1)).await;
    assert!(matches!(err, Err(SupervisorError::Quarantined)));

    // Manual unquarantine
    supervisor.unquarantine();
    assert_eq!(supervisor.state(), PluginRuntimeState::Stopped);
}
