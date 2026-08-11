//! Unit tests for the adapter-side UDS peer-credential admission policy.

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

/// Real Unix-domain-socket coverage for the adapter-side client admission
/// check (`--allowed-client-uid/gid`). Gated to Linux because SO_PEERCRED is
/// the supported credential source; compiled out on other hosts.
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
    fn own_credentials() -> (u32, u32) {
        let (stream, _peer) = UnixStream::pair().unwrap();
        let credentials = nix::sys::socket::getsockopt(
            &stream,
            nix::sys::socket::sockopt::PeerCredentials,
        )
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
    /// The adapter server (this test's parent) verifies the connected peer;
    /// because the client runs as `nobody`, SO_PEERCRED reports a uid that
    /// differs from the trusted (root) expectation, so admission must fail
    /// closed.
    fn spawn_nobody_client(socket_path: &std::path::Path) -> nix::unistd::Pid {
        let path = socket_path.to_path_buf();
        match nix::unistd::fork().expect("fork") {
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
