use cy_extension_registry::{ExtensionRegistry, RemoteProbe};
use cy_plugin_supervisor::PluginSupervisor;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex as AsyncMutex;

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
async fn test_remote_probe_proxy_and_registry() {
    let (py_bin, script_path) = get_python_and_script();
    let supervisor = PluginSupervisor::new("com.cy.probe.nvidia", py_bin, vec![script_path]);

    // 1. Unhealthy state test
    let sup_arc = Arc::new(AsyncMutex::new(supervisor));
    let remote_probe = Arc::new(RemoteProbe::new("com.cy.probe.nvidia", sup_arc.clone()));

    let mut registry = ExtensionRegistry::new();
    registry.register_probe(remote_probe.clone());

    // 1. Unstarted (Discovered) state test -> calling probe automatically starts supervisor and succeeds
    let probe_ref = registry.get_probe("com.cy.probe.nvidia").unwrap();
    let hw = probe_ref
        .detect_hardware()
        .await
        .expect("Auto-start detect hardware failed");
    assert_eq!(hw.os.name, "Linux");

    // After handshake, calling probe returns valid HardwareManifest
    let hw = probe_ref
        .detect_hardware()
        .await
        .expect("Detect hardware failed");
    assert_eq!(hw.os.name, "Linux");

    // 3. Stop supervisor -> Proxy returns Unavailable again
    {
        let mut sup = sup_arc.lock().await;
        sup.stop().await.unwrap();
    }

    let res_stopped = probe_ref.detect_hardware().await;
    assert!(res_stopped.is_err());
}
