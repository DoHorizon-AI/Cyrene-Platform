//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 lib.rs                                                          │
//! │  Module: cy_mtls_channel_client                                     │
//! │  Role: Strict outbound mTLS Tonic channel construction.             │
//! │                                                                     │
//! │  模块职责：构造严格校验服务端身份的出站 mTLS Tonic channel。             │
//! └─────────────────────────────────────────────────────────────────────┘

#![forbid(unsafe_code)]

use std::time::Duration;

use thiserror::Error;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint, Identity};

/// Failure while validating or opening an outbound mTLS channel.
#[derive(Debug, Error)]
pub enum MtlsChannelConnectError {
    #[error("MTLS_ENDPOINT_INVALID: {0}")]
    Endpoint(String),
    #[error("MTLS_TRANSPORT_FAILED: {0}")]
    Transport(String),
}

/// Opens an HTTPS Tonic channel with server CA validation and a client identity.
///
/// The caller supplies endpoint policy, server name, and identity material. This
/// helper does not issue identities, authorize a peer, or select application routes.
///
/// # Errors
/// Returns [`MtlsChannelConnectError::Endpoint`] for an invalid HTTPS endpoint or
/// empty server name, and [`MtlsChannelConnectError::Transport`] when TLS setup or
/// connection establishment fails.
pub async fn connect_mtls_channel(
    address: &str,
    server_name: &str,
    ca_certificate_pem: &[u8],
    client_certificate_pem: &[u8],
    client_key_pem: &[u8],
    connect_timeout: Option<Duration>,
) -> Result<Channel, MtlsChannelConnectError> {
    if !address.starts_with("https://") || server_name.trim().is_empty() {
        return Err(MtlsChannelConnectError::Endpoint(
            "mTLS endpoint requires HTTPS and a server name".to_string(),
        ));
    }

    let mut endpoint = Endpoint::from_shared(address.to_string())
        .map_err(|error| MtlsChannelConnectError::Endpoint(error.to_string()))?;
    if let Some(timeout) = connect_timeout {
        endpoint = endpoint.connect_timeout(timeout);
    }
    let tls = ClientTlsConfig::new()
        .domain_name(server_name.to_string())
        .ca_certificate(Certificate::from_pem(ca_certificate_pem))
        .identity(Identity::from_pem(client_certificate_pem, client_key_pem));

    endpoint
        .tls_config(tls)
        .map_err(|error| MtlsChannelConnectError::Transport(error.to_string()))?
        .connect()
        .await
        .map_err(|error| MtlsChannelConnectError::Transport(error.to_string()))
}
