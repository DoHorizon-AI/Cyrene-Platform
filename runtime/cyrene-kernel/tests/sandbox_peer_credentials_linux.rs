// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: runtime/cyrene-kernel/tests/sandbox_peer_credentials_linux.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! Linux acceptance coverage for the Kernel-to-sandboxd UDS identity boundary.
//!
//! This stays in the host-composition crate rather than `kernel/`: running a
//! second Unix account is privileged acceptance setup, not Kernel behavior.

#[cfg(target_os = "linux")]
mod linux_uds {
    use std::{
        fs,
        io::Read,
        os::unix::{fs::PermissionsExt, net::UnixListener},
        path::Path,
        process::{Child, Command},
        time::{Duration, Instant},
    };

    use cy_adapter_client::PeerCredentialExpectation;
    use cy_kernel_api::ProcessRuntime;
    use cy_sandbox_client::{SandboxAdapterEndpoint, UdsSandboxAdapterClient};

    const ADAPTER_ID: &str = "sandboxd-test";
    const NOBODY_ADAPTER_SOCKET_ENV: &str = "CYRENE_SANDBOX_CLIENT_NOBODY_SOCKET";

    /// Starts an ignored helper as `nobody`. The privileged test-harness action
    /// is intentionally outside the pure-safe Kernel crate.
    fn spawn_nobody_adapter(socket_path: &Path) -> Child {
        let test_binary = std::env::current_exe().expect("locate test binary");
        Command::new("runuser")
            .args(["--preserve-environment", "--user", "nobody", "--"])
            .env(NOBODY_ADAPTER_SOCKET_ENV, socket_path)
            .arg(test_binary)
            .args([
                "--exact",
                "linux_uds::nobody_adapter_helper",
                "--ignored",
                "--nocapture",
            ])
            .spawn()
            .expect("start nobody adapter helper through runuser")
    }

    /// Runs only when started through [`spawn_nobody_adapter`]. It proves the
    /// rejected client sent no protocol byte before disconnecting.
    #[test]
    #[ignore = "internal helper for the multi-account UDS acceptance test"]
    fn nobody_adapter_helper() {
        let Some(socket_path) = std::env::var_os(NOBODY_ADAPTER_SOCKET_ENV) else {
            return;
        };
        let socket_path = std::path::PathBuf::from(socket_path);
        let listener = UnixListener::bind(&socket_path).expect("nobody bind");
        fs::write(socket_path.with_extension("ready"), b"").expect("ready sentinel");

        let (mut stream, _) = listener.accept().expect("accept kernel client");
        stream
            .set_read_timeout(Some(Duration::from_millis(500)))
            .expect("set read timeout");
        let mut byte = [0_u8; 1];
        let no_protocol_frame = match stream.read(&mut byte) {
            Ok(0) => true,
            Ok(_) => false,
            Err(error) => matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ),
        };
        let result = if no_protocol_frame {
            &b"no-frame"[..]
        } else {
            &b"frame"[..]
        };
        fs::write(socket_path.with_extension("result"), result).expect("write result sentinel");
    }

    fn wait_for(path: &Path, description: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !path.exists() {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {description}: {}",
                path.display()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// End-to-end multi-account rejection (Kernel-to-Adapter / outbound client
    /// direction): the client trusts root while an untrusted `nobody` account
    /// owns the UDS listener. `SO_PEERCRED` must reject it before any protocol
    /// frame is written. Requires root, a `nobody` account, and `runuser`; run
    /// in the Linux acceptance environment with:
    /// `cargo test -p cyrene-kernel --test sandbox_peer_credentials_linux -- --ignored`.
    #[test]
    #[ignore = "requires root and a second local account; run in the Linux acceptance environment"]
    fn peer_credentials_reject_a_different_local_account() {
        let directory = std::env::temp_dir().join(format!(
            "cyrene-sandbox-client-multiacct-{}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).expect("create shared test directory");
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o777))
            .expect("make shared test directory writable");
        let socket = directory.join("sandboxd.sock");
        let ready = socket.with_extension("ready");
        let result = socket.with_extension("result");

        let mut child = spawn_nobody_adapter(&socket);
        wait_for(&ready, "nobody adapter readiness");

        let mut endpoint = SandboxAdapterEndpoint::new(ADAPTER_ID, &socket);
        endpoint.peer_credentials = PeerCredentialExpectation {
            uid: Some(0),
            gid: None,
        };
        let client = UdsSandboxAdapterClient::from_endpoint(endpoint).expect("valid endpoint");
        let capabilities = client.preflight();
        assert!(!capabilities.ready);
        assert!(capabilities
            .facts
            .iter()
            .any(|fact| { fact.detail.starts_with("SANDBOX_PEER_CREDENTIAL_MISMATCH:") }));

        assert!(child.wait().expect("wait for nobody adapter").success());
        wait_for(&result, "nobody adapter result");
        assert_eq!(
            fs::read(&result).expect("read result sentinel"),
            b"no-frame"
        );
        fs::remove_dir_all(&directory).expect("remove test directory");
    }
}
