//! Unit tests for cgroup v2 runtime, cleanup, telemetry, and device filtering.

use std::{collections::BTreeMap, fs, fs::File, path::PathBuf};

use cy_kernel_api::{
    DeviceBinding, DeviceMapper, EnforcementMode, ProcessHandle, ProcessRuntime,
    RuntimeProcessEvidence, SandboxBackend,
};

#[cfg(target_os = "linux")]
use crate::bpf::{
    build_device_filter_program, DeviceRule, BPF_ALU64_MOV_K, BPF_DEVCG_DEV_CHAR, BPF_JMP_EXIT,
};
use crate::{bpf::LinuxDeviceMapper, config::CgroupV2Config, runtime::CgroupV2Runtime};

fn config(root: PathBuf) -> CgroupV2Config {
    CgroupV2Config {
        root,
        transport_root: std::env::temp_dir()
            .join(format!("cyrene-transport-{}", std::process::id())),
        device_bpf_enabled: false,
        dev_mode: false,
    }
}

#[test]
fn dev_mode_preflight_reports_ready_without_cgroups() {
    let root = std::env::temp_dir().join(format!("cyrene-cgroup-dev-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let mut dev_cfg = config(root.clone());
    dev_cfg.dev_mode = true;
    let runtime = CgroupV2Runtime::new(dev_cfg);
    let caps = runtime.preflight_report();
    assert!(
        caps.ready,
        "dev_mode preflight must report ready even without cgroup mounts"
    );
    assert_eq!(caps.enforcement[0].mode, EnforcementMode::Unenforced);
    assert_eq!(caps.enforcement[0].reason_code, "DEV_MODE_UNENFORCED");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn initialization_preserves_unclassified_instance_dirs() {
    let root = std::env::temp_dir().join(format!("cyrene-cgroup-dev-clean-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let mut dev_cfg = config(root.clone());
    dev_cfg.dev_mode = true;
    let runtime = CgroupV2Runtime::new(dev_cfg);

    // Pre-create stale instance dir
    let stale_dir = root.join("instance-stale");
    fs::create_dir_all(&stale_dir).unwrap();
    fs::write(stale_dir.join("dummy.txt"), "stale").unwrap();

    runtime.initialize_owned_root().unwrap();
    assert!(
        stale_dir.exists(),
        "sandboxd must not delete a cgroup before Kernel journal classification"
    );

    // First create succeeds
    let res = runtime.create_instance_cgroup("instance-test");
    assert!(res.is_ok(), "creating instance cgroup must succeed");

    // Second create with same name fails with CGROUP_CREATE_FAILED
    let res_dup = runtime.create_instance_cgroup("instance-test");
    assert!(
        res_dup.is_err(),
        "duplicate instance name must be rejected in dev mode"
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn recovery_rejects_foreign_or_mismatched_evidence_before_cleanup() {
    let root = std::env::temp_dir().join(format!("cyrene-recovery-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("foreign")).unwrap();
    let runtime = CgroupV2Runtime::new(config(root.clone()));
    let start_time_ticks = crate::sys::proc_start_time(std::process::id()).unwrap();

    let error = runtime
        .recover_stale_process(&RuntimeProcessEvidence {
            cgroup_name: "foreign".to_string(),
            pid: std::process::id(),
            start_time_ticks,
        })
        .unwrap_err();
    assert_eq!(error.reason_code, "INVALID_CGROUP_NAME");
    assert!(
        root.join("foreign").exists(),
        "foreign state must never be killed"
    );

    let stale = root.join("instance-stale");
    fs::create_dir_all(&stale).unwrap();
    fs::write(stale.join("cgroup.procs"), std::process::id().to_string()).unwrap();
    fs::write(stale.join("cgroup.kill"), "unchanged").unwrap();
    let error = runtime
        .recover_stale_process(&RuntimeProcessEvidence {
            cgroup_name: "instance-stale".to_string(),
            pid: std::process::id(),
            start_time_ticks: start_time_ticks + 1,
        })
        .unwrap_err();
    assert_eq!(error.reason_code, "RECOVERY_EVIDENCE_MISMATCH");
    assert_eq!(
        fs::read_to_string(stale.join("cgroup.kill")).unwrap(),
        "unchanged"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn preflight_requires_cgroup_v2_and_cgroup_kill() {
    let root = std::env::temp_dir().join(format!("cyrene-cgroup-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("cgroup.controllers"), "cpu memory pids").unwrap();
    File::create(root.join("cgroup.kill")).unwrap();
    assert!(CgroupV2Runtime::new(config(root.clone())).preflight().ready);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn initialization_preserves_foreign_and_unclassified_children() {
    let root = std::env::temp_dir().join(format!("cyrene-cleanup-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("foreign")).unwrap();
    fs::create_dir_all(root.join("instance-stale")).unwrap();
    File::create(root.join("instance-stale/cgroup.procs")).unwrap();
    File::create(root.join("instance-stale/cgroup.kill")).unwrap();
    CgroupV2Runtime::new(config(root.clone()))
        .initialize_owned_root()
        .unwrap();
    assert!(root.join("foreign").exists());
    assert!(root.join("instance-stale").exists());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn telemetry_uses_kernel_files_without_estimation() {
    let root = std::env::temp_dir().join(format!("cyrene-telemetry-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("memory.current"), "42\n").unwrap();
    fs::write(root.join("memory.events.local"), "oom_kill 7\n").unwrap();
    fs::write(
        root.join("cpu.stat"),
        "usage_usec 11\nuser_usec 8\nsystem_usec 3\n",
    )
    .unwrap();
    let telemetry = CgroupV2Runtime::new(config(root.clone())).telemetry(&ProcessHandle {
        pid: 1,
        cgroup_path: root.clone(),
        start_time_ticks: None,
        transport_socket: None,
    });
    assert_eq!(telemetry.memory_current_bytes, Some(42));
    assert_eq!(telemetry.cpu_usage_usec, Some(11));
    assert_eq!(telemetry.oom_kill_count, 7);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn hard_device_request_fails_closed_when_disabled() {
    let mapper = LinuxDeviceMapper {
        device_bpf_enabled: false,
    };
    let binding = DeviceBinding {
        resource_id: "GPU-0".to_string(),
        nodes: Vec::new(),
        environment: BTreeMap::new(),
        joinable_environment_keys: Default::default(),
        required_gids: Vec::new(),
        enforcement: EnforcementMode::Hard,
        adapter_id: "test".to_string(),
        reason_code: "test".to_string(),
    };
    assert_eq!(
        mapper
            .enforce(&binding, EnforcementMode::Hard)
            .unwrap_err()
            .reason_code,
        "HARD_ENFORCEMENT_UNAVAILABLE"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn device_filter_program_is_default_deny_and_has_allow_path() {
    let program = build_device_filter_program(&[DeviceRule {
        kind: BPF_DEVCG_DEV_CHAR,
        major: 195,
        minor: 0,
    }]);
    assert_eq!(program.last().unwrap().code, BPF_JMP_EXIT);
    assert_eq!(program[program.len() - 2].imm, 0);
    assert!(program
        .iter()
        .any(|instruction| instruction.code == BPF_ALU64_MOV_K && instruction.imm == 1));
}

#[test]
fn client_peer_credential_policy_covers_all_expectation_shapes() {
    use crate::client_peer_credentials_allowed;

    // (expected_uid, expected_gid, actual_uid, actual_gid, accepted)
    let cases = [
        (Some(1000), Some(2000), 1000, 2000, true),
        (Some(1000), Some(2000), 1001, 2000, false),
        (Some(1000), Some(2000), 1000, 2001, false),
        (Some(1000), Some(2000), 1001, 2001, false),
        (Some(1000), None, 1000, 9999, true),
        (Some(1000), None, 1001, 1000, false),
        (None, Some(2000), 9999, 2000, true),
        (None, Some(2000), 2000, 2001, false),
        (None, None, 0, 0, true),
    ];
    for (expected_uid, expected_gid, actual_uid, actual_gid, accepted) in cases {
        let verdict =
            client_peer_credentials_allowed(expected_uid, expected_gid, actual_uid, actual_gid);
        assert_eq!(
            verdict.is_ok(),
            accepted,
            "unexpected verdict for expectation ({expected_uid:?}, {expected_gid:?}) against ({actual_uid}, {actual_gid})"
        );
        if !accepted {
            assert_eq!(
                verdict.unwrap_err().kind(),
                std::io::ErrorKind::PermissionDenied
            );
        }
    }
}

/// Real Unix-domain-socket coverage for the sandboxd-side client admission
/// check (`--allowed-client-uid/gid`). Gated to Linux because SO_PEERCRED is
/// the supported credential source; compiled out on other hosts.
#[cfg(target_os = "linux")]
mod linux_uds {
    use std::{
        fs,
        io::{Read, Write},
        os::unix::{
            fs::PermissionsExt,
            net::{UnixListener, UnixStream},
        },
        path::PathBuf,
        time::Duration,
    };

    use crate::verify_client_peer;

    /// SO_PEERCRED on a loopback socket pair reports this process, which is
    /// the ground truth the admission check compares against.
    fn own_credentials() -> (u32, u32) {
        let (stream, _peer) = UnixStream::pair().unwrap();
        let credentials =
            nix::sys::socket::getsockopt(&stream, nix::sys::socket::sockopt::PeerCredentials)
                .unwrap();
        (credentials.uid(), credentials.gid())
    }

    fn socket_path(tag: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("cyrene-sandboxd-{}-{tag}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        directory.join("sandboxd.sock")
    }

    fn write_frame(mut writer: impl Write, payload: &[u8]) -> std::io::Result<()> {
        writer.write_all(&(payload.len() as u32).to_be_bytes())?;
        writer.write_all(payload)
    }

    fn read_frame(mut reader: impl Read) -> std::io::Result<Vec<u8>> {
        let mut length = [0_u8; 4];
        reader.read_exact(&mut length)?;
        let mut payload = vec![0; u32::from_be_bytes(length) as usize];
        reader.read_exact(&mut payload)?;
        Ok(payload)
    }

    #[test]
    fn admitted_client_completes_a_request_response_exchange() {
        let socket = socket_path("positive");
        let listener = UnixListener::bind(&socket).unwrap();
        let (uid, gid) = own_credentials();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            verify_client_peer(&stream, Some(uid), Some(gid)).unwrap();
            let payload = read_frame(&mut stream).unwrap();
            write_frame(&mut stream, &payload).unwrap();
        });
        let mut client = UnixStream::connect(&socket).unwrap();
        write_frame(&mut client, b"preflight").unwrap();
        assert_eq!(read_frame(&mut client).unwrap(), b"preflight");
        server.join().unwrap();
        let _ = fs::remove_dir_all(socket.parent().unwrap());
    }

    #[test]
    fn rejected_client_is_dropped_before_any_frame_is_read() {
        let socket = socket_path("negative");
        let listener = UnixListener::bind(&socket).unwrap();
        let (uid, gid) = own_credentials();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            // Mirror the accept loop: a failed check closes the connection
            // before `read_frame` is ever called.
            let verdict = verify_client_peer(&stream, Some(uid.wrapping_add(1)), Some(gid));
            assert_eq!(
                verdict.as_ref().unwrap_err().kind(),
                std::io::ErrorKind::PermissionDenied
            );
            drop(stream);
        });
        let mut client = UnixStream::connect(&socket).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        // The Kernel side learns about the rejection as a closed connection
        // when it waits for the response frame.
        let error = read_frame(&mut client).unwrap_err();
        assert!(
            error.kind() == std::io::ErrorKind::UnexpectedEof
                || error.kind() == std::io::ErrorKind::ConnectionReset,
            "rejected client must observe a closed connection, got {error}"
        );
        server.join().unwrap();
        let _ = fs::remove_dir_all(socket.parent().unwrap());
    }

    /// Forks a throwaway Kernel-side client that drops to the unprivileged
    /// `nobody` account and connects to `socket_path`, sending one raw frame.
    /// The sandboxd server (this test's parent) verifies the connected peer;
    /// because the client runs as `nobody`, SO_PEERCRED reports a uid that
    /// differs from the trusted (root) expectation, so admission must fail
    /// closed.
    fn spawn_nobody_client(socket_path: &std::path::Path) -> nix::unistd::Pid {
        let path = socket_path.to_path_buf();
        // SAFETY: forking a throwaway single-threaded test process before dropping privileges
        match unsafe { nix::unistd::fork() }.expect("fork") {
            nix::unistd::ForkResult::Child => {
                let nobody = nix::unistd::User::from_name("nobody")
                    .expect("resolve nobody")
                    .expect("nobody must exist on the acceptance host");
                nix::unistd::setgid(nobody.gid).expect("setgid(nobody)");
                nix::unistd::setuid(nobody.uid).expect("setuid(nobody)");
                let mut client = UnixStream::connect(&path).expect("nobody connect");
                let _ = write_frame(&mut client, b"preflight");
                std::process::exit(0);
            }
            nix::unistd::ForkResult::Parent { child } => child,
        }
    }

    /// End-to-end multi-account rejection (adapter / inbound server direction):
    /// sandboxd is configured to trust the Kernel client's own (root) uid, while
    /// a foreign local account (`nobody`) opens the socket. The server-side
    /// peer check must reject the connection before any frame is processed.
    /// Requires root and a `nobody` account; runs in the Linux acceptance
    /// environment via `cargo test -p cyrene-sandboxd -- --ignored`.
    #[test]
    #[ignore = "requires root and a second local account; run in the Linux acceptance environment"]
    fn peer_credentials_reject_a_different_local_account() {
        let socket = socket_path("multiacct");
        let listener = UnixListener::bind(&socket).unwrap();
        let (trusted_uid, _trusted_gid) = own_credentials();
        if trusted_uid != 0 {
            eprintln!(
                "SKIP: multi-account UDS acceptance requires root to drop the child to nobody; current uid={trusted_uid}"
            );
            drop(listener);
            let _ = fs::remove_dir_all(socket.parent().unwrap());
            return;
        }
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o777)).unwrap();

        let child = spawn_nobody_client(&socket);
        let (stream, _) = listener.accept().unwrap();
        // The connecting peer is `nobody`; the trusted Kernel uid is our own
        // (root) identity, so the admission check must fail closed.
        let verdict = verify_client_peer(&stream, Some(trusted_uid), None);
        assert!(verdict.is_err(), "a foreign local account must be rejected");
        assert_eq!(
            verdict.unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );

        nix::sys::wait::waitpid(child, None).unwrap();
        let _ = fs::remove_dir_all(socket.parent().unwrap());
    }
}
