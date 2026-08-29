//! UDS peer-credential admission for the privileged Sandbox Adapter Host.
//!
//! The pure decision lives in `client_peer_credentials_allowed`; the
//! SO_PEERCRED lookup is confined to the thin `verify_client_peer` shell so
//! the admission policy is testable without a socket.

#[cfg(unix)]
use std::os::unix::net::UnixStream;

/// Pure admission decision for an accepted Kernel client connection, checked
/// against the configured `--allowed-client-uid/gid` identity.
pub fn client_peer_credentials_allowed(
    expected_uid: Option<u32>,
    expected_gid: Option<u32>,
    actual_uid: u32,
    actual_gid: u32,
) -> std::io::Result<()> {
    if expected_uid.is_some_and(|uid| uid != actual_uid)
        || expected_gid.is_some_and(|gid| gid != actual_gid)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "UDS peer credentials do not match configured Kernel identity",
        ));
    }
    Ok(())
}

/// Verifies an accepted Kernel client against the configured peer identity,
/// after accept and before any request frame is read; a mismatch fails
/// closed and the caller drops the connection.
#[cfg(unix)]
pub fn verify_client_peer(
    stream: &UnixStream,
    expected_uid: Option<u32>,
    expected_gid: Option<u32>,
) -> std::io::Result<()> {
    if expected_uid.is_none() && expected_gid.is_none() {
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    {
        let credentials =
            nix::sys::socket::getsockopt(stream, nix::sys::socket::sockopt::PeerCredentials)
                .map_err(std::io::Error::other)?;
        client_peer_credentials_allowed(
            expected_uid,
            expected_gid,
            credentials.uid(),
            credentials.gid(),
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = stream;
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "UDS peer credential checks require Linux",
        ))
    }
}
