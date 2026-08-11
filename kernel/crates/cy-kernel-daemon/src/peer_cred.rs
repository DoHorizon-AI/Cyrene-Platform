//! Trusted transport identity: derive the authority `Principal` from the
//! Unix-socket peer credential (SO_PEERCRED) instead of trusting any
//! caller-supplied value in the request body.
//!
//! The v1 contract requires the Kernel to learn the caller's identity from the
//! authenticated transport, never from the payload. On a Unix domain socket
//! the kernel attests the peer's pid/uid/gid via `SO_PEERCRED`; we read it
//! once per accepted connection and surface it through hyper's connect-info
//! extension so every gRPC `Request` on the authority socket carries it.
//!
//! * `PeerCred` and the conversion/`principal_from_request` helpers are
//!   available on every platform (they contain no OS-specific code).
//! * The custom `Accept`/`Connected` wrapper that actually performs the
//!   `SO_PEERCRED` read is `#[cfg(unix)]`-gated; it cannot be compiled on
//!   Windows.

use cy_kernel_api::semantic;
use tonic::{Request, Status};

use crate::convert::common::semantic_status;

/// Credential attested by the kernel for the peer end of a Unix socket.
///
/// `Clone + Copy + Send + Sync + 'static` so it can live in a request
/// `Extensions` map and be retrieved by `principal_from_request`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerCred {
    pub pid: u32,
    pub uid: u32,
    pub gid: u32,
}

/// Build the contract `Principal` for an attested peer credential.
///
/// The identity `id` encodes the uid/gid/pid so it is stable and
/// introspectable; `generation` is the pid (never zero for a real peer, but
/// guarded anyway because `Identity::validate` rejects generation 0).
pub fn principal_from_peer_cred(cred: &PeerCred) -> semantic::Principal {
    semantic::Principal {
        identity: semantic::Identity {
            id: format!("unix://uid={}/gid={}/pid={}", cred.uid, cred.gid, cred.pid),
            generation: cred.pid.max(1) as u64,
        },
    }
}

/// Derive and attach the authenticated `Principal` for an authority request.
///
/// This is a tonic interceptor. It reads the `PeerCred` that the trusted
/// authority `Accept` inserted into the request extensions, derives the
/// contract `Principal`, and carries that Principal into the actual
/// `KernelAuthorityService` implementation. If `PeerCred` is missing the
/// request did not traverse the authenticated authority socket, so we fail
/// closed with `AUTHENTICATION_REQUIRED`.
///
/// It is platform-agnostic: it only inspects request extensions and therefore
/// type-checks (and is testable) on any platform.
pub fn inject_authority_principal(mut request: Request<()>) -> Result<Request<()>, Status> {
    let cred = request
        .extensions()
        .get::<PeerCred>()
        .copied()
        .ok_or_else(|| {
            semantic_status(
                tonic::Code::Unauthenticated,
                "AUTHENTICATION_REQUIRED",
                "authority calls require a trusted Unix peer credential (SO_PEERCRED); \
             the request did not arrive over the authenticated authority socket",
            )
        })?;
    request
        .extensions_mut()
        .insert(principal_from_peer_cred(&cred));
    Ok(request)
}

/// Recover the authenticated `Principal` attached by
/// [`inject_authority_principal`] for an incoming authority request.
///
/// Service methods deliberately require the derived `Principal`, rather than
/// accepting raw `PeerCred`. This prevents a future non-intercepted listener
/// from silently becoming an authority path.
pub fn principal_from_request<T>(request: &Request<T>) -> Result<semantic::Principal, Status> {
    request
        .extensions()
        .get::<semantic::Principal>()
        .cloned()
        .ok_or_else(|| {
            semantic_status(
                tonic::Code::Unauthenticated,
                "AUTHENTICATION_REQUIRED",
                "authority calls require a Principal derived by the trusted Unix peer credential \
                 (SO_PEERCRED) interceptor",
            )
        })
}

#[cfg(unix)]
pub use unix::{PeerCredAccept, PeerCredStream};

#[cfg(unix)]
mod unix {
    use std::{
        io,
        pin::Pin,
        task::{Context, Poll},
    };

    use nix::sys::socket::{getsockopt, sockopt::PeerCredentials};
    use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
    use tokio::net::UnixStream;
    use tokio_stream::Stream;
    use tonic::transport::server::Connected;

    use super::PeerCred;

    /// A `UnixStream` annotated with the peer credential read at accept time.
    ///
    /// Implements hyper's `Connected` so `connect_info()` yields the `PeerCred`,
    /// which hyper automatically inserts into the per-request `Extensions`.
    pub struct PeerCredStream {
        inner: UnixStream,
        peer_cred: PeerCred,
    }

    impl PeerCredStream {
        fn from_stream(inner: UnixStream) -> io::Result<Self> {
            let ucred = getsockopt(&inner, PeerCredentials).map_err(io::Error::other)?;
            Ok(Self {
                inner,
                peer_cred: PeerCred {
                    pid: ucred.pid() as u32,
                    uid: ucred.uid(),
                    gid: ucred.gid(),
                },
            })
        }
    }

    impl Connected for PeerCredStream {
        type ConnectInfo = PeerCred;

        fn connect_info(&self) -> Self::ConnectInfo {
            self.peer_cred
        }
    }

    impl AsyncRead for PeerCredStream {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            Pin::new(&mut self.inner).poll_read(cx, buf)
        }
    }

    impl AsyncWrite for PeerCredStream {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            Pin::new(&mut self.inner).poll_write(cx, buf)
        }

        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.inner).poll_flush(cx)
        }

        fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.inner).poll_shutdown(cx)
        }
    }

    /// A custom `Accept` (hyper `Stream` of connected streams) that wraps each
    /// accepted `UnixStream` with its `SO_PEERCRED`-derived `PeerCred`.
    ///
    /// Use this as the `incoming` for `serve_with_incoming` on the authority
    /// socket; the worker-control socket must keep `UnixListenerStream` so it
    /// never gains the ability to assert an authority `Principal`.
    pub struct PeerCredAccept<S> {
        inner: S,
    }

    impl<S> PeerCredAccept<S> {
        pub fn new(inner: S) -> Self {
            Self { inner }
        }
    }

    impl<S> Stream for PeerCredAccept<S>
    where
        S: Stream<Item = io::Result<UnixStream>> + Unpin,
    {
        type Item = io::Result<PeerCredStream>;

        fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            match Pin::new(&mut self.inner).poll_next(cx) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(None) => Poll::Ready(None),
                Poll::Ready(Some(Err(error))) => Poll::Ready(Some(Err(error))),
                Poll::Ready(Some(Ok(stream))) => {
                    Poll::Ready(Some(PeerCredStream::from_stream(stream)))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        inject_authority_principal, principal_from_peer_cred, principal_from_request, PeerCred,
    };
    use tonic::Request;

    #[test]
    fn principal_is_derived_from_the_attested_peer_credential() {
        let principal = principal_from_peer_cred(&PeerCred {
            pid: 4242,
            uid: 1000,
            gid: 1001,
        });

        assert_eq!(principal.identity.id, "unix://uid=1000/gid=1001/pid=4242");
        assert_eq!(principal.identity.generation, 4242);
    }

    #[test]
    fn missing_peer_credential_is_rejected() {
        let error = inject_authority_principal(Request::new(())).unwrap_err();

        assert_eq!(error.code(), tonic::Code::Unauthenticated);
    }

    #[test]
    fn service_requires_the_interceptor_derived_principal() {
        let mut request = Request::new(());
        request.extensions_mut().insert(PeerCred {
            pid: 4242,
            uid: 1000,
            gid: 1001,
        });

        let error = principal_from_request(&request).unwrap_err();
        assert_eq!(error.code(), tonic::Code::Unauthenticated);
    }

    #[test]
    fn interceptor_injects_principal_from_the_peer_credential() {
        let mut request = Request::new(());
        request.extensions_mut().insert(PeerCred {
            pid: 4242,
            uid: 1000,
            gid: 1001,
        });

        let request = inject_authority_principal(request).unwrap();
        let principal = principal_from_request(&request).unwrap();
        assert_eq!(principal.identity.id, "unix://uid=1000/gid=1001/pid=4242");
    }
}
