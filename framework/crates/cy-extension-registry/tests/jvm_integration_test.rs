//! JVM plugin integration test via the InstanceActor + sandboxd transport.
//!
//! MIGRATION NOTE: The previous version of this test used `PluginSupervisor::new()` to spawn
//! a JVM worker via stdio. Since all plugin lifecycle is now managed by `InstanceActor` and the
//! sandboxd channel, this test needs to:
//!   1. Use the sandboxd UDS client to obtain the opaque worker socket endpoint.
//!   2. Let the framework transport attach `WorkerTransportCommand` to the actor.
//!   3. Then use `RemoteNotification` / `RemoteExecutionEngine` as before.
//!
//! The test remains `#[ignore]` because this repository does not ship a runnable
//! JVM worker installation record and supervised sandboxd fixture.
//! See: `kernel/crates/cy-kernel-daemon/src/watchdog/instance_actor.rs` and
//!      `adapters/execution/sandboxd/`.

use std::process::Command;

/// JVM plugin full wire-protocol lifecycle test.
/// Ignored until a real JVM worker installation and sandboxd fixture are available.
#[tokio::test]
#[ignore = "No runnable JVM worker installation/sandboxd fixture in this repository"]
async fn test_jvm_plugin_full_wire_protocol_lifecycle() {
    // Check java availability
    let java_check = Command::new("java").arg("-version").output();
    if java_check.is_err() || !java_check.unwrap().status.success() {
        println!("Java runtime absent on host; skipping JVM integration test gracefully.");
        return;
    }

    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let root_dir = manifest_dir.ancestors().nth(3).unwrap();
    let jar_path = root_dir.join("examples/plugins/jvm/poc/protobuf-java.jar");

    if !jar_path.exists() {
        println!(
            "protobuf-java.jar absent at {}; skipping JVM integration test.",
            jar_path.display()
        );
        return;
    }

    // TODO: Replace with a verified installation and a running sandboxd service
    // once this repository carries a runnable JVM worker artifact:
    //
    //   let sandbox_client = UdsSandboxAdapterClient::connect(sandboxd_socket).await?;
    //   let mut actor = InstanceActor::new("jvm-poc", "lease-jvm", 1,
    //       Arc::new(sandbox_client), plan, binding, Duration::from_secs(30));
    //   actor.start().unwrap();
    //   let actor_arc = Arc::new(AsyncMutex::new(actor));
    //
    //   let notification = RemoteNotification::new("jvm-poc", actor_arc.clone());
    //   notification.send_notification("deploy", "deployment succeeded", "info").await.unwrap();
    //
    println!("JVM integration test skeleton present; full sandboxd wire-up pending.");
}
