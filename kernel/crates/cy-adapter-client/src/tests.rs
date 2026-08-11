#![allow(deprecated)]

use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use cy_kernel_api::{
    semantic::{self, Resource, ResourceState},
    DeviceBinding, EnforcementMode, HealthReport, HostInventoryProvider, InventorySnapshot,
    NodeCapabilities, ProviderError, ResourceProvider,
};
use cy_proto::{hardware_v1, semantic_v1};

use crate::{
    client::HardwareAdapter,
    convert::{ensure_inventory_fresh, resource_state_from_proto},
    credential::PeerCredentialExpectation,
    registry::UdsHardwareAdapterRegistry,
    transport::{read_frame, write_frame},
};

#[derive(Clone)]
struct FakeAdapter {
    id: String,
    device_id: String,
}

impl FakeAdapter {
    fn resource(&self) -> Resource {
        Resource {
            identity: semantic::Identity {
                id: self.device_id.clone(),
                generation: 1,
            },
            provider: semantic::Identity {
                id: self.id.clone(),
                generation: 1,
            },
            resource_class: "accelerator".to_string(),
            capabilities: Vec::new(),
            capacity: BTreeMap::new(),
            attributes: BTreeMap::new(),
            state: ResourceState::Ready,
            reason_code: "test".to_string(),
            summary: "healthy".to_string(),
            links: Vec::new(),
        }
    }
}

impl HostInventoryProvider for FakeAdapter {
    fn probe_inventory(&self) -> Result<InventorySnapshot, ProviderError> {
        Ok(InventorySnapshot {
            generation: 1,
            resources: vec![self.resource()],
            capabilities: NodeCapabilities {
                ready: true,
                facts: Vec::new(),
                enforcement: Vec::new(),
            },
        })
    }
}

impl ResourceProvider for FakeAdapter {
    fn adapter_id(&self) -> &str {
        &self.id
    }

    fn probe_resources(&self) -> Result<Vec<Resource>, ProviderError> {
        Ok(vec![self.resource()])
    }

    fn create_binding(&self, resource: &Resource) -> Result<DeviceBinding, ProviderError> {
        if resource.identity.id != self.device_id {
            return Err(ProviderError::new(
                &self.id,
                "RESOURCE_NOT_FOUND",
                &resource.identity.id,
            ));
        }
        Ok(DeviceBinding {
            resource_id: resource.identity.id.clone(),
            nodes: Vec::new(),
            environment: BTreeMap::new(),
            required_gids: Vec::new(),
            enforcement: EnforcementMode::Unenforced,
            adapter_id: self.id.clone(),
            reason_code: "TEST".to_string(),
        })
    }

    fn read_health(&self, resource_id: &str) -> Result<HealthReport, ProviderError> {
        if resource_id != self.device_id {
            return Err(ProviderError::new(
                &self.id,
                "RESOURCE_NOT_FOUND",
                resource_id,
            ));
        }
        Ok(HealthReport {
            healthy: Some(true),
            reason_code: "TEST".to_string(),
            summary: "healthy".to_string(),
        })
    }
}

#[test]
fn frames_round_trip() {
    let mut wire = Vec::new();
    write_frame(&mut wire, b"adapter").unwrap();
    assert_eq!(read_frame(wire.as_slice()).unwrap(), b"adapter");
}

#[test]
fn unknown_resource_state_is_rejected() {
    assert_eq!(
        resource_state_from_proto(semantic_v1::ResourceState::Unspecified as i32)
            .unwrap_err()
            .reason_code,
        "UNKNOWN_ENUM_VALUE"
    );
}

#[test]
fn peer_credential_policy_rejects_a_mismatched_adapter_peer() {
    let policy = PeerCredentialExpectation {
        uid: Some(1000),
        gid: Some(2000),
    };
    assert!(policy.verify("test", 1000, 2000).is_ok());
    assert_eq!(
        policy.verify("test", 1001, 2000).unwrap_err().reason_code,
        "ADAPTER_PEER_CREDENTIAL_MISMATCH"
    );
}

#[test]
fn peer_credential_policy_covers_partial_and_empty_expectations() {
    // (expected_uid, expected_gid, actual_uid, actual_gid, accepted)
    let cases = [
        (Some(1000), None, 1000, 9999, true),
        (Some(1000), None, 1001, 1000, false),
        (None, Some(2000), 9999, 2000, true),
        (None, Some(2000), 2000, 2001, false),
        (None, None, 0, 0, true),
        (None, None, u32::MAX, u32::MAX, true),
        (Some(1000), Some(2000), 1001, 2001, false),
        (Some(1000), Some(2000), 1000, 2001, false),
        (Some(1000), Some(2000), 1000, 2000, true),
    ];
    for (uid, gid, actual_uid, actual_gid, accepted) in cases {
        let policy = PeerCredentialExpectation { uid, gid };
        assert_eq!(policy.is_configured(), uid.is_some() || gid.is_some());
        let verdict = policy.verify("test", actual_uid, actual_gid);
        assert_eq!(
            verdict.is_ok(),
            accepted,
            "unexpected verdict for expectation ({uid:?}, {gid:?}) against ({actual_uid}, {actual_gid})"
        );
        if !accepted {
            assert_eq!(
                verdict.unwrap_err().reason_code,
                "ADAPTER_PEER_CREDENTIAL_MISMATCH"
            );
        }
    }
}

#[test]
fn expired_or_missing_inventory_facts_fail_closed() {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .min(i64::MAX as u64) as i64;
    let valid = hardware_v1::HardwareInventory {
        generation: 1,
        devices: Vec::new(),
        facts: Vec::new(),
        enforcement: Vec::new(),
        sampled_at: Some(prost_types::Timestamp {
            seconds: now.saturating_sub(1),
            nanos: 0,
        }),
        expires_at: Some(prost_types::Timestamp {
            seconds: now.saturating_add(30),
            nanos: 0,
        }),
        resources: Vec::new(),
    };
    assert!(ensure_inventory_fresh("test", &valid).is_ok());
    let expired = hardware_v1::HardwareInventory {
        expires_at: Some(prost_types::Timestamp {
            seconds: now.saturating_sub(1),
            nanos: 0,
        }),
        ..valid
    };
    assert_eq!(
        ensure_inventory_fresh("test", &expired)
            .unwrap_err()
            .reason_code,
        "ADAPTER_FACT_EXPIRED"
    );
}

#[test]
fn registry_aggregates_adapters_and_routes_binding_by_provenance() {
    let registry = UdsHardwareAdapterRegistry::from_adapters(vec![
        (
            "adapter_a".to_string(),
            Arc::new(FakeAdapter {
                id: "adapter_a".to_string(),
                device_id: "gpu-a".to_string(),
            }) as Arc<dyn HardwareAdapter>,
        ),
        (
            "adapter_b".to_string(),
            Arc::new(FakeAdapter {
                id: "adapter_b".to_string(),
                device_id: "gpu-b".to_string(),
            }) as Arc<dyn HardwareAdapter>,
        ),
    ])
    .unwrap();
    let inventory = HostInventoryProvider::probe_inventory(&registry).unwrap();
    assert_eq!(inventory.resources.len(), 2);
    let resource = inventory
        .resources
        .iter()
        .find(|resource| resource.identity.id == "gpu-b")
        .unwrap();
    assert_eq!(resource.provider.id, "adapter_b");
    let binding = registry
        .create_binding_for_generation(resource, inventory.generation)
        .unwrap();
    assert_eq!(binding.adapter_id, "adapter_b");
    assert_eq!(binding.resource_id, "gpu-b");
}

#[test]
fn registry_rejects_duplicate_device_ids_across_adapters() {
    let registry = UdsHardwareAdapterRegistry::from_adapters(vec![
        (
            "adapter_a".to_string(),
            Arc::new(FakeAdapter {
                id: "adapter_a".to_string(),
                device_id: "same-device".to_string(),
            }) as Arc<dyn HardwareAdapter>,
        ),
        (
            "adapter_b".to_string(),
            Arc::new(FakeAdapter {
                id: "adapter_b".to_string(),
                device_id: "same-device".to_string(),
            }) as Arc<dyn HardwareAdapter>,
        ),
    ])
    .unwrap();
    assert_eq!(
        HostInventoryProvider::probe_inventory(&registry)
            .unwrap_err()
            .reason_code,
        "ADAPTER_RESOURCE_ID_COLLISION"
    );
}

/// Real Unix-domain-socket coverage for the Kernel-side peer-credential
/// admission check. Gated to Linux because SO_PEERCRED is the supported
/// credential source; these cases are compiled out on other hosts.
#[cfg(target_os = "linux")]
mod linux_uds {
    use std::{
        fs,
        os::unix::net::{UnixListener, UnixStream},
        path::PathBuf,
        sync::mpsc,
        time::Duration,
    };

    use crate::{
        credential::PeerCredentialExpectation,
        transport::{exchange, read_frame, write_frame},
    };

    /// SO_PEERCRED on a loopback socket pair reports this process, which is
    /// the ground truth the Kernel-side check compares against.
    fn own_peer_credentials() -> PeerCredentialExpectation {
        let (stream, _peer) = UnixStream::pair().unwrap();
        let credentials =
            nix::sys::socket::getsockopt(&stream, nix::sys::socket::sockopt::PeerCredentials)
                .unwrap();
        PeerCredentialExpectation {
            uid: Some(credentials.uid()),
            gid: Some(credentials.gid()),
        }
    }

    fn socket_path(tag: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "cyrene-adapter-client-{}-{tag}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).unwrap();
        directory.join("adapter.sock")
    }

    #[test]
    fn exchange_completes_when_peer_matches_configured_identity() {
        let socket = socket_path("positive");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let payload = read_frame(&mut stream).unwrap();
            write_frame(&mut stream, &payload).unwrap();
        });
        let reply = exchange(
            "test-adapter",
            &socket,
            Duration::from_secs(2),
            own_peer_credentials(),
            b"ping",
        )
        .unwrap();
        assert_eq!(reply, b"ping");
        server.join().unwrap();
        let _ = fs::remove_dir_all(socket.parent().unwrap());
    }

    #[test]
    fn exchange_fails_closed_before_writing_when_peer_mismatches() {
        let socket = socket_path("negative");
        let listener = UnixListener::bind(&socket).unwrap();
        let (observed_tx, observed_rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_millis(500)))
                .unwrap();
            let observed = read_frame(&mut stream).map(|_| ());
            let _ = observed_tx.send(observed);
        });
        let mut expected = own_peer_credentials();
        expected.uid = expected.uid.map(|uid| uid.wrapping_add(1));
        let error = exchange(
            "test-adapter",
            &socket,
            Duration::from_secs(2),
            expected,
            b"ping",
        )
        .unwrap_err();
        assert_eq!(error.reason_code, "ADAPTER_PEER_CREDENTIAL_MISMATCH");
        // The server must never observe a protocol frame from the rejected
        // client: its bounded read has to time out.
        let observed = observed_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let error = observed.unwrap_err();
        assert!(
            error.kind() == std::io::ErrorKind::WouldBlock
                || error.kind() == std::io::ErrorKind::TimedOut,
            "server must time out waiting for a frame that was never sent, got {error}"
        );
        server.join().unwrap();
        let _ = fs::remove_dir_all(socket.parent().unwrap());
    }

    /// Forks a throwaway "hardware adapter" that runs as the unprivileged
    /// `nobody` account, binds `socket_path`, and accepts one connection. The
    /// Kernel-side client (this test's parent) verifies the connected peer;
    /// because the listener is owned by `nobody`, SO_PEERCRED reports a uid
    /// that differs from the trusted (root) expectation, so admission must
    /// fail closed. The directory backing `socket_path` must already be
    /// world-writable so `nobody` can create the socket inside it.
    fn spawn_nobody_adapter(socket_path: &std::path::Path) -> nix::unistd::Pid {
        let path = socket_path.to_path_buf();
        // SAFETY: forking a throwaway single-threaded test process before dropping privileges
        match unsafe { nix::unistd::fork() }.expect("fork") {
            nix::unistd::ForkResult::Child => {
                let nobody = nix::unistd::User::from_name("nobody")
                    .expect("resolve nobody")
                    .expect("nobody must exist on the acceptance host");
                nix::unistd::setgid(nobody.gid).expect("setgid(nobody)");
                nix::unistd::setuid(nobody.uid).expect("setuid(nobody)");
                let listener = UnixListener::bind(&path).expect("nobody bind");
                fs::write(path.with_extension("ready"), b"").expect("ready sentinel");
                let _ = listener.accept();
                std::process::exit(0);
            }
            nix::unistd::ForkResult::Parent { child } => child,
        }
    }

    /// End-to-end multi-account rejection (Kernel→Hardware adapter / outbound
    /// client direction): the Kernel client is configured to trust its own
    /// (root) uid, while a foreign local account (`nobody`) impersonates the
    /// hardware adapter. The peer-credential check must reject the connection
    /// before any adapter frame is exchanged. Requires root and a `nobody`
    /// account; runs in the Linux acceptance environment via
    /// `cargo test -p cy-adapter-client -- --ignored`.
    #[test]
    #[ignore = "requires root and a second local account; run in the Linux acceptance environment"]
    fn peer_credentials_reject_a_different_local_account() {
        let trusted = own_peer_credentials();
        let directory = std::env::temp_dir().join(format!(
            "cyrene-adapter-client-multiacct-{}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o777)).unwrap();
        let socket = directory.join("adapter.sock");
        let _ = fs::remove_file(&socket);

        let child = spawn_nobody_adapter(&socket);
        let ready = socket.with_extension("ready");
        while !ready.exists() {
            std::thread::sleep(Duration::from_millis(5));
        }

        let error = exchange(
            "test-adapter",
            &socket,
            Duration::from_secs(2),
            trusted,
            b"ping",
        )
        .unwrap_err();
        assert_eq!(error.reason_code, "ADAPTER_PEER_CREDENTIAL_MISMATCH");

        nix::sys::wait::waitpid(child, None).unwrap();
        let _ = fs::remove_dir_all(&directory);
    }
}
