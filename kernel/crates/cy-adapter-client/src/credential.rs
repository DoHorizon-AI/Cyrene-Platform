#[cfg(any(test, unix, target_os = "linux"))]
use cy_kernel_api::ProviderError;

/// Optional UDS peer identity constraint from static node configuration. The
/// Kernel verifies it after connect, before sending an adapter request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PeerCredentialExpectation {
    pub uid: Option<u32>,
    pub gid: Option<u32>,
}

impl PeerCredentialExpectation {
    pub fn is_configured(self) -> bool {
        self.uid.is_some() || self.gid.is_some()
    }

    #[cfg(any(test, target_os = "linux"))]
    pub(crate) fn verify(
        self,
        adapter_id: &str,
        actual_uid: u32,
        actual_gid: u32,
    ) -> Result<(), ProviderError> {
        if self.uid.is_some_and(|uid| uid != actual_uid)
            || self.gid.is_some_and(|gid| gid != actual_gid)
        {
            return Err(ProviderError::new(
                adapter_id,
                "ADAPTER_PEER_CREDENTIAL_MISMATCH",
                "UDS peer credentials do not match the configured adapter identity",
            ));
        }
        Ok(())
    }
}

#[cfg(unix)]
pub(crate) fn verify_connected_peer(
    adapter_id: &str,
    stream: &std::os::unix::net::UnixStream,
    expected: PeerCredentialExpectation,
) -> Result<(), ProviderError> {
    if !expected.is_configured() {
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    {
        let credentials =
            nix::sys::socket::getsockopt(stream, nix::sys::socket::sockopt::PeerCredentials)
                .map_err(|error| {
                    ProviderError::new(
                        adapter_id,
                        "ADAPTER_PEER_CREDENTIAL_UNAVAILABLE",
                        &error.to_string(),
                    )
                })?;
        expected.verify(adapter_id, credentials.uid(), credentials.gid())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = stream;
        Err(ProviderError::new(
            adapter_id,
            "ADAPTER_PEER_CREDENTIAL_UNSUPPORTED",
            "configured UDS peer credential checks require a Linux Kernel host",
        ))
    }
}
