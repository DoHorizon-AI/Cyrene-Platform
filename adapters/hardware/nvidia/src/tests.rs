// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: adapters/hardware/nvidia/src/tests.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! Unit tests for the adapter-side UDS peer-credential admission policy.

#[test]
fn client_peer_credential_policy_covers_all_expectation_shapes() {
    use crate::client_peer_credentials_allowed;

    // (expected_uid, expected_gid, actual_uid, actual_gid, accepted)
    // 中文：（expected_uid、expected_gid、actual_uid、actual_gid、accepted）。
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

/// Real Unix-domain-socket coverage for the adapter-side client admission
/// check (`--allowed-client-uid/gid`). Gated to Linux because SO_PEERCRED is
/// the supported credential source; compiled out on other hosts.
/// 中文：对适配器侧客户端准入检查（`--allowed-client-uid/gid`）进行真实 Unix 域套接字覆盖测试。该测试仅在 Linux 上启用，因为 SO_PEERCRED 是受支持的凭据来源；在其他主机上不会编译。
#[cfg(target_os = "linux")]
mod linux_uds {
    use std::{
        fs,
        io::{Read, Write},
        os::unix::net::{UnixListener, UnixStream},
        path::PathBuf,
        time::Duration,
    };

    use crate::verify_client_peer;

    /// SO_PEERCRED on a loopback socket pair reports this process, which is
    /// the ground truth the admission check compares against.
    /// 中文：SO_PEERCRED 在 loopback 套接字对上会报告当前进程，这就是准入检查所比较的实际凭据。
    fn own_credentials() -> (u32, u32) {
        let (stream, _peer) = UnixStream::pair().unwrap();
        let credentials =
            nix::sys::socket::getsockopt(&stream, nix::sys::socket::sockopt::PeerCredentials)
                .unwrap();
        (credentials.uid(), credentials.gid())
    }

    fn socket_path(tag: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "cyrene-nvidia-adapter-{}-{tag}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).unwrap();
        directory.join("nvidia-adapter.sock")
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
        write_frame(&mut client, b"inventory").unwrap();
        assert_eq!(read_frame(&mut client).unwrap(), b"inventory");
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
            // 中文：与 accept 循环一致，检查失败时会先关闭连接，绝不会调用 `read_frame`。
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
        // 中文：Kernel 等待响应帧时，会通过连接已关闭得知请求被拒绝。
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
    /// The adapter server (this test's parent) verifies the connected peer;
    /// because the client runs as `nobody`, SO_PEERCRED reports a uid that
    /// differs from the trusted (root) expectation, so admission must fail
    /// closed.
    /// 中文：派生一个临时的 Kernel 侧客户端进程；它会切换到无特权的 `nobody` 账户，连接 `socket_path` 并发送一个原始帧。适配器服务器（即本测试的父进程）会核验连接对端。由于客户端以 `nobody` 身份运行，SO_PEERCRED 报告的 uid 与受信任的 root 预期不同，因此准入检查必须失败关闭。
    fn spawn_nobody_client(socket_path: &std::path::Path) -> nix::unistd::Pid {
        let path = socket_path.to_path_buf();
        // SAFETY: forking a throwaway single-threaded test process before dropping privileges
        // 中文：安全性说明：在丢弃权限之前，派生一个临时的单线程测试进程。
        match unsafe { nix::unistd::fork() }.expect("fork") {
            nix::unistd::ForkResult::Child => {
                let nobody = nix::unistd::User::from_name("nobody")
                    .expect("resolve nobody")
                    .expect("nobody must exist on the acceptance host");
                nix::unistd::setgid(nobody.gid).expect("setgid(nobody)");
                nix::unistd::setuid(nobody.uid).expect("setuid(nobody)");
                let mut client = UnixStream::connect(&path).expect("nobody connect");
                let _ = write_frame(&mut client, b"inventory");
                std::process::exit(0);
            }
            nix::unistd::ForkResult::Parent { child } => child,
        }
    }

    /// End-to-end multi-account rejection (adapter / inbound server direction):
    /// the NVIDIA adapter is configured to trust the Kernel client's own (root)
    /// uid, while a foreign local account (`nobody`) opens the socket. The
    /// server-side peer check must reject the connection before any frame is
    /// processed. Requires root and a `nobody` account; runs in the Linux
    /// acceptance environment via
    /// `cargo test -p cyrene-nvidia-adapter -- --ignored`.
    /// 中文：端到端多账户拒绝测试（适配器／入站服务器方向）：NVIDIA 适配器配置为信任 Kernel 客户端自身的 root uid，而另一个本地账户（`nobody`）尝试连接套接字。服务器端的对端检查必须在处理任何帧之前拒绝该连接。该测试需要 root 权限和 `nobody` 账户；在 Linux 验收环境中通过 `cargo test -p cyrene-nvidia-adapter -- --ignored` 运行。
    #[test]
    #[ignore = "requires root and a second local account; run in the Linux acceptance environment"]
    fn peer_credentials_reject_a_different_local_account() {
        let socket = socket_path("multiacct");
        let listener = UnixListener::bind(&socket).unwrap();
        let (trusted_uid, _trusted_gid) = own_credentials();

        let child = spawn_nobody_client(&socket);
        let (stream, _) = listener.accept().unwrap();
        // The connecting peer is `nobody`; the trusted Kernel uid is our own
        // (root) identity, so the admission check must fail closed.
        // 中文：连接对端是 `nobody`；受信任的 Kernel uid 是当前 root 身份，因此准入检查必须失败关闭。
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
