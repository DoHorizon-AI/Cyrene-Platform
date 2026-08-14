//! JVM plugin integration test via the InstanceActor + sandboxd transport.
//!
//! MIGRATION NOTE: The previous version of this test used `PluginSupervisor::new()` to spawn
//! a JVM worker via stdio. Since all plugin lifecycle is now managed by `InstanceActor` and the
//! sandboxd channel, this test needs to:
//!   1. Use the sandboxd UDS client to establish a transport channel.
//!   2. Call `InstanceActor::attach_transport_channel` with the resulting `mpsc::Sender<Envelope>`.
//!   3. Then use `RemoteNotification` / `RemoteExecutionEngine` as before.
//!
//! The test body is marked `#[ignore]` until the sandboxd e2e wire-up is complete.
//! See: `kernel/crates/cy-kernel-daemon/src/watchdog/instance_actor.rs` and
//!      `adapters/execution/sandboxd/`.

use std::process::Command;

/// JVM plugin full wire-protocol lifecycle test.
/// Ignored until sandboxd UDS transport channel is wired end-to-end.
#[tokio::test]
#[ignore = "Pending sandboxd e2e wire-up; PluginSupervisor stdio path removed"]
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

    // TODO: Replace with InstanceActor construction + attach_transport_channel once
    // sandboxd UDS transport is wired end-to-end:
    //
    //   let sandbox_client = UdsSandboxAdapterClient::connect(sandboxd_socket).await?;
    //   let mut actor = InstanceActor::new("jvm-poc", "lease-jvm", 1,
    //       Arc::new(sandbox_client), plan, binding, Duration::from_secs(30));
    //   actor.start().unwrap();
    //   let (tx, rx) = mpsc::channel(32);
    //   actor.attach_transport_channel(tx);
    //   let actor_arc = Arc::new(AsyncMutex::new(actor));
    //
    //   let notification = RemoteNotification::new("jvm-poc", actor_arc.clone());
    //   notification.send_notification("deploy", "deployment succeeded", "info").await.unwrap();
    //
    println!("JVM integration test skeleton present; full sandboxd wire-up pending.");
}
