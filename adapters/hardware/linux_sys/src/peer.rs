//! Local UDS SO_PEERCRED peer-credential validation for the Linux system adapter.

#[cfg(unix)]
use std::os::unix::net::UnixStream;

pub fn client_peer_credentials_allowed(
    peer_uid: u32,
    peer_gid: u32,
    allowed_uid: Option<u32>,
    allowed_gid: Option<u32>,
) -> bool {
    if let Some(expected_uid) = allowed_uid {
        if peer_uid != expected_uid {
            return false;
        }
    }
    if let Some(expected_gid) = allowed_gid {
        if peer_gid != expected_gid {
            return false;
        }
    }
    true
}

#[cfg(target_os = "linux")]
pub fn verify_client_peer(
    stream: &UnixStream,
    allowed_uid: Option<u32>,
    allowed_gid: Option<u32>,
) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;

    let mut ucred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;

    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut ucred as *mut _ as *mut libc::c_void,
            &mut length,
        )
    };

    if result != 0 {
        return Err(std::io::Error::last_os_error());
    }

    if !client_peer_credentials_allowed(ucred.uid, ucred.gid, allowed_uid, allowed_gid) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "peer credentials rejected: uid={} gid={} pid={}",
                ucred.uid, ucred.gid, ucred.pid
            ),
        ));
    }

    Ok(())
}

#[cfg(all(unix, not(target_os = "linux")))]
pub fn verify_client_peer(
    _stream: &UnixStream,
    _allowed_uid: Option<u32>,
    _allowed_gid: Option<u32>,
) -> std::io::Result<()> {
    Ok(())
}
