//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 device_auth.rs                                                  │
//! │  Module: cy_workspace_fabric::device_auth                          │
//! │  Role: Relay connector mTLS identity and registry authorization.    │
//! │                                                                     │
//! │  模块职责：从 Relay mTLS 对端提取设备身份并校验注册表授权。               │
//! └─────────────────────────────────────────────────────────────────────┘

use std::sync::Arc;

use cy_proto::workspace_v1::{RelayHello, RelayParticipantRole};
use sha2::{Digest, Sha256};
use thiserror::Error;
#[cfg(test)]
use tonic::Request;
use x509_parser::parse_x509_certificate;

use crate::auth::{RelaySessionClaims, SessionPrincipal};
use crate::device_registry::{DeviceAuthorizationStatus, WorkspaceDeviceRegistry};
use crate::relay_peer_certificate_validation::{
    AuthenticatedRelayWorkspaceDevice, RelayPeerCertificateError, ValidatedRelayPeerCertificate,
};

/// Certificate identity derived from the peer certificate on a Tonic TLS connection.
///
/// Its fields and constructor stay private so callers cannot turn a header or a
/// `RelayHello` claim into a verified transport identity. Tonic only exposes
/// peer certificates from server-side TLS connection metadata; TLS chain
/// validation occurs before this parser reads the leaf certificate's expiry.
/// This is an inbound WorkspaceDevice identity for the Relay. It does not
/// authenticate the Relay service to a connector or authorize a receiver to
/// trust `caller_roles` carried in Relay-forwarded messages.
///
/// 该类型只表示 Relay 入站的 WorkspaceDevice 身份，不能认证 Relay 服务，也不能授权接收端信任转发消息中的 `caller_roles`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedClientCertificate {
    fingerprint_sha256: String,
    expires_at_unix_ms: u64,
}

impl VerifiedClientCertificate {
    /// Read the leaf certificate from Tonic's authenticated TLS peer metadata.
    ///
    /// This returns an error when the RPC did not arrive over a TLS connection
    /// with a peer certificate. No request metadata or forwarded header is read.
    ///
    /// 从 Tonic 已验证的 TLS 对端元数据读取叶子证书；缺失时拒绝，不读取转发 header。
    #[cfg(test)]
    pub(crate) fn from_tonic_request<T>(
        request: &Request<T>,
    ) -> Result<Self, WorkspaceDeviceAuthenticationError> {
        let certificates = request
            .peer_certs()
            .ok_or(WorkspaceDeviceAuthenticationError::MissingClientCertificate)?;
        let leaf = certificates
            .first()
            .ok_or(WorkspaceDeviceAuthenticationError::MissingClientCertificate)?;
        Self::from_tls_peer_der(leaf.as_ref())
    }

    #[cfg(test)]
    fn from_tls_peer_der(der: &[u8]) -> Result<Self, WorkspaceDeviceAuthenticationError> {
        Self::from_validated_der(der)
    }

    /// Build certificate identity from DER only after its transport has validated the chain.
    ///
    /// Direct Tonic TLS calls this after Rustls verification. The ACA adapter calls it only after
    /// `rustls-webpki` verifies the forwarded leaf against configured roots and intermediates.
    /// This crate-private boundary prevents external callers from converting arbitrary headers
    /// into a verified identity.
    pub(crate) fn from_validated_der(
        der: &[u8],
    ) -> Result<Self, WorkspaceDeviceAuthenticationError> {
        let (remaining, certificate) = parse_x509_certificate(der)
            .map_err(|_| WorkspaceDeviceAuthenticationError::InvalidClientCertificate)?;
        if !remaining.is_empty() {
            return Err(WorkspaceDeviceAuthenticationError::InvalidClientCertificate);
        }
        let expires_at_unix_ms = certificate
            .validity()
            .not_after
            .timestamp()
            .checked_mul(1_000)
            .and_then(|value| u64::try_from(value).ok())
            .ok_or(WorkspaceDeviceAuthenticationError::InvalidClientCertificate)?;

        Ok(Self {
            fingerprint_sha256: format!("{:x}", Sha256::digest(der)),
            expires_at_unix_ms,
        })
    }

    /// Lowercase SHA-256 hex over the exact certificate DER bytes.
    pub fn fingerprint_sha256(&self) -> &str {
        &self.fingerprint_sha256
    }

    /// Certificate `notAfter` time in Unix milliseconds.
    pub fn expires_at_unix_ms(&self) -> u64 {
        self.expires_at_unix_ms
    }
}

/// Failure before a Workspace connector can register with the Relay.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceDeviceAuthenticationError {
    #[error("WORKSPACE_DEVICE_TLS_CERTIFICATE_REQUIRED")]
    MissingClientCertificate,
    #[error("WORKSPACE_DEVICE_TLS_CERTIFICATE_INVALID")]
    InvalidClientCertificate,
    #[error("WORKSPACE_DEVICE_TLS_CERTIFICATE_EXPIRED")]
    ExpiredCertificate,
    #[error("WORKSPACE_DEVICE_CERTIFICATE_REVOCATION_UNKNOWN")]
    RevocationStatusUnknown,
    #[error("WORKSPACE_DEVICE_CERTIFICATE_NOT_REGISTERED")]
    UnregisteredCertificate,
    #[error("WORKSPACE_DEVICE_CERTIFICATE_REVOKED")]
    RevokedCertificate,
    #[error("WORKSPACE_DEVICE_IDENTITY_MISMATCH")]
    IdentityMismatch,
    #[error("WORKSPACE_DEVICE_REGISTRY_UNAVAILABLE")]
    RegistryUnavailable,
}

/// Authorize Relay connectors against externally approved certificate records.
///
/// The certificate fingerprint selects the registry record. Organization,
/// Workspace, and device IDs from `RelayHello` are only compared with that
/// record; `user`, `session_credential`, and `enrollment_state` are never
/// accepted as connector authority. The resulting `WorkspaceDevice` principal
/// is only the connector's identity to the Relay; it is never a Relay service
/// identity or a source of caller roles for downstream authorization.
///
/// 指纹选择注册表记录；`RelayHello` 的身份字段仅作范围匹配，用户、会话凭据和 enrollment 状态不构成授权。结果只表示 Connector 对 Relay 的设备身份，不表示 Relay 服务身份，也不提供下游 caller role。
pub struct RegistryWorkspaceDeviceVerifier {
    registry: Arc<dyn WorkspaceDeviceRegistry>,
}

impl RegistryWorkspaceDeviceVerifier {
    /// Create a verifier backed by the canonical Workspace device registry.
    pub fn new(registry: Arc<dyn WorkspaceDeviceRegistry>) -> Self {
        Self { registry }
    }

    /// Build connector claims only when the registered certificate is approved and in scope.
    #[allow(dead_code)] // Legacy fingerprint-only verifier stays crate-private and is never wired by Relay.
    pub(crate) fn authenticate(
        &self,
        hello: &RelayHello,
        certificate: &VerifiedClientCertificate,
        now_unix_ms: u64,
    ) -> Result<RelaySessionClaims, WorkspaceDeviceAuthenticationError> {
        if certificate.expires_at_unix_ms <= now_unix_ms {
            return Err(WorkspaceDeviceAuthenticationError::ExpiredCertificate);
        }
        if hello.role != RelayParticipantRole::WorkspaceConnector as i32 || hello.user.is_some() {
            return Err(WorkspaceDeviceAuthenticationError::IdentityMismatch);
        }
        let device = hello
            .device
            .as_ref()
            .ok_or(WorkspaceDeviceAuthenticationError::IdentityMismatch)?;

        let record = self
            .registry
            .find_device_by_certificate_fingerprint(certificate.fingerprint_sha256())
            .map_err(|_| WorkspaceDeviceAuthenticationError::RegistryUnavailable)?
            .ok_or(WorkspaceDeviceAuthenticationError::UnregisteredCertificate)?;
        if record.certificate_fingerprint_sha256 != certificate.fingerprint_sha256() {
            return Err(WorkspaceDeviceAuthenticationError::IdentityMismatch);
        }
        match record.authorization_status {
            DeviceAuthorizationStatus::Approved => {}
            DeviceAuthorizationStatus::Revoked => {
                return Err(WorkspaceDeviceAuthenticationError::RevokedCertificate);
            }
        }
        if record.key.organization_id.is_empty()
            || record.key.workspace_id.is_empty()
            || record.key.device_id.is_empty()
            || hello.organization_id != record.key.organization_id
            || hello.workspace_id != record.key.workspace_id
            || device.workspace_id != record.key.workspace_id
            || device.device_id != record.key.device_id
        {
            return Err(WorkspaceDeviceAuthenticationError::IdentityMismatch);
        }

        Ok(RelaySessionClaims {
            principal: SessionPrincipal::WorkspaceDevice {
                workspace_id: record.key.workspace_id.clone(),
                device_id: record.key.device_id.clone(),
            },
            organization_id: record.key.organization_id,
            workspace_id: record.key.workspace_id,
            expires_at_unix_ms: certificate.expires_at_unix_ms(),
        })
    }

    /// Authenticate a fully validated Tonic peer against a current active Registry binding.
    ///
    /// The certificate validator proves chain/profile/time and fresh signed
    /// revocation evidence before this method is called. This method then
    /// requires the Registry's current generation and immutable binding tuple;
    /// basic fingerprint records never authorize this path.
    pub(crate) fn authenticate_validated_peer(
        &self,
        hello: &RelayHello,
        certificate: &ValidatedRelayPeerCertificate,
        now_unix_ms: u64,
    ) -> Result<
        (RelaySessionClaims, AuthenticatedRelayWorkspaceDevice),
        WorkspaceDeviceAuthenticationError,
    > {
        if certificate.not_after_unix_ms() <= now_unix_ms {
            return Err(WorkspaceDeviceAuthenticationError::ExpiredCertificate);
        }
        if hello.role != RelayParticipantRole::WorkspaceConnector as i32 || hello.user.is_some() {
            return Err(WorkspaceDeviceAuthenticationError::IdentityMismatch);
        }
        let device = hello
            .device
            .as_ref()
            .ok_or(WorkspaceDeviceAuthenticationError::IdentityMismatch)?;

        let fingerprint = certificate.fingerprint_sha256_hex();
        let record = self
            .registry
            .find_current_device_certificate_identity(&fingerprint)
            .map_err(|_| WorkspaceDeviceAuthenticationError::RegistryUnavailable)?
            .ok_or(WorkspaceDeviceAuthenticationError::UnregisteredCertificate)?;
        let authenticated_peer = certificate
            .match_registry_identity(&record)
            .map_err(map_peer_certificate_error)?;
        let key = authenticated_peer.key();
        if key.organization_id.is_empty()
            || key.workspace_id.is_empty()
            || key.device_id.is_empty()
            || hello.organization_id != key.organization_id
            || hello.workspace_id != key.workspace_id
            || device.workspace_id != key.workspace_id
            || device.device_id != key.device_id
        {
            return Err(WorkspaceDeviceAuthenticationError::IdentityMismatch);
        }

        Ok((
            RelaySessionClaims {
                principal: SessionPrincipal::WorkspaceDevice {
                    workspace_id: key.workspace_id.clone(),
                    device_id: key.device_id.clone(),
                },
                organization_id: key.organization_id.clone(),
                workspace_id: key.workspace_id.clone(),
                expires_at_unix_ms: certificate.not_after_unix_ms(),
            },
            authenticated_peer,
        ))
    }

    /// Recheck a previously authenticated peer against fresh certificate and Registry facts.
    ///
    /// Relays call this before dispatching each new Workspace request. The returned identity
    /// must match the complete cached binding, so a revocation or generation rotation invalidates
    /// the established session instead of leaving it authorized until the TLS stream expires.
    pub(crate) fn revalidate_authenticated_peer(
        &self,
        expected: &AuthenticatedRelayWorkspaceDevice,
        certificate: &ValidatedRelayPeerCertificate,
        now_unix_ms: u64,
    ) -> Result<(), WorkspaceDeviceAuthenticationError> {
        if certificate.not_after_unix_ms() <= now_unix_ms {
            return Err(WorkspaceDeviceAuthenticationError::ExpiredCertificate);
        }

        let fingerprint = certificate.fingerprint_sha256_hex();
        let record = self
            .registry
            .find_current_device_certificate_identity(&fingerprint)
            .map_err(|_| WorkspaceDeviceAuthenticationError::RegistryUnavailable)?
            .ok_or(WorkspaceDeviceAuthenticationError::UnregisteredCertificate)?;
        let current = certificate
            .match_registry_identity(&record)
            .map_err(map_peer_certificate_error)?;
        if &current != expected {
            return Err(WorkspaceDeviceAuthenticationError::IdentityMismatch);
        }

        Ok(())
    }
}

fn map_peer_certificate_error(
    error: RelayPeerCertificateError,
) -> WorkspaceDeviceAuthenticationError {
    match error {
        RelayPeerCertificateError::MissingPeerCertificate => {
            WorkspaceDeviceAuthenticationError::MissingClientCertificate
        }
        RelayPeerCertificateError::ExpiredCertificate => {
            WorkspaceDeviceAuthenticationError::ExpiredCertificate
        }
        RelayPeerCertificateError::Revoked => {
            WorkspaceDeviceAuthenticationError::RevokedCertificate
        }
        RelayPeerCertificateError::RevocationStatusUnknown => {
            WorkspaceDeviceAuthenticationError::RevocationStatusUnknown
        }
        RelayPeerCertificateError::IdentityMismatch => {
            WorkspaceDeviceAuthenticationError::IdentityMismatch
        }
        RelayPeerCertificateError::InvalidCertificate => {
            WorkspaceDeviceAuthenticationError::InvalidClientCertificate
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use cy_proto::workspace_v1::{DeviceEnrollmentRef, UserIdentityRef};

    use super::*;
    use crate::device_registry::{
        ApprovedWorkspaceDeviceCertificate, WorkspaceDeviceKey, WorkspaceDeviceRecord,
    };
    use crate::directory::WorkspaceDirectoryError;

    const APPROVED_FINGERPRINT: &str =
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const NOW_UNIX_MS: u64 = 1_000_000;
    const TEST_CLIENT_CERTIFICATE_PEM: &str = "-----BEGIN CERTIFICATE-----\nMIIBuzCCAWGgAwIBAgIUR+G83pBbTQpMDYB4fyyTOr2SB3kwCgYIKoZIzj0EAwIw\nIjEgMB4GA1UEAwwXY3lyZW5lLWRldmljZS1hdXRoLXRlc3QwHhcNMjYwOTI2MTk1\nNDAxWhcNMzYwOTIzMTk1NDAxWjAiMSAwHgYDVQQDDBdjeXJlbmUtZGV2aWNlLWF1\ndGgtdGVzdDBZMBMGByqGSM49AgEGCCqGSM49AwEHA0IABJPP4Nx4Ft5DH5M3jHWy\nut76rf63vz4uKSbCEV5/wTn7fzzdfsyLmaAo4lqbYnSbUsKjYyqzo1WEVILz8zzp\n2gijdTBzMB0GA1UdDgQWBBROFe/aRJkBSynAXeG3Fos4znMyXzAfBgNVHSMEGDAW\ngBROFe/aRJkBSynAXeG3Fos4znMyXzAMBgNVHRMBAf8EAjAAMA4GA1UdDwEB/wQE\nAwIFoDATBgNVHSUEDDAKBggrBgEFBQcDAjAKBggqhkjOPQQDAgNIADBFAiEAvflb\nvbbUdzxQN+cik3SKJ50fEmx/xuE/b66T9lzUYxsCIHZcK8857dyo8mjRAIC8V0FS\nDH56qLy3KjptgQOqxX5P\n-----END CERTIFICATE-----\n";

    #[derive(Default)]
    struct TestDeviceRegistry {
        records: Mutex<BTreeMap<String, WorkspaceDeviceRecord>>,
    }

    impl TestDeviceRegistry {
        fn with_record(status: DeviceAuthorizationStatus) -> Self {
            let record = WorkspaceDeviceRecord {
                key: WorkspaceDeviceKey {
                    organization_id: "organization-1".to_string(),
                    workspace_id: "workspace-1".to_string(),
                    device_id: "device-1".to_string(),
                },
                certificate_fingerprint_sha256: APPROVED_FINGERPRINT.to_string(),
                authorization_status: status,
            };
            Self {
                records: Mutex::new(BTreeMap::from([(
                    record.certificate_fingerprint_sha256.clone(),
                    record,
                )])),
            }
        }
    }

    impl WorkspaceDeviceRegistry for TestDeviceRegistry {
        fn import_approved_device_certificate(
            &self,
            certificate: ApprovedWorkspaceDeviceCertificate,
        ) -> Result<WorkspaceDeviceRecord, WorkspaceDirectoryError> {
            let record = WorkspaceDeviceRecord {
                key: certificate.key,
                certificate_fingerprint_sha256: certificate.certificate_fingerprint_sha256,
                authorization_status: DeviceAuthorizationStatus::Approved,
            };
            self.records
                .lock()
                .map_err(|_| WorkspaceDirectoryError::Storage("test registry poisoned".into()))?
                .insert(
                    record.certificate_fingerprint_sha256.clone(),
                    record.clone(),
                );
            Ok(record)
        }

        fn revoke_device(
            &self,
            key: &WorkspaceDeviceKey,
        ) -> Result<WorkspaceDeviceRecord, WorkspaceDirectoryError> {
            let mut records = self
                .records
                .lock()
                .map_err(|_| WorkspaceDirectoryError::Storage("test registry poisoned".into()))?;
            let record = records
                .values_mut()
                .find(|record| &record.key == key)
                .ok_or_else(|| WorkspaceDirectoryError::Identity("missing test record".into()))?;
            record.authorization_status = DeviceAuthorizationStatus::Revoked;
            Ok(record.clone())
        }

        fn find_device(
            &self,
            key: &WorkspaceDeviceKey,
        ) -> Result<Option<WorkspaceDeviceRecord>, WorkspaceDirectoryError> {
            Ok(self
                .records
                .lock()
                .map_err(|_| WorkspaceDirectoryError::Storage("test registry poisoned".into()))?
                .values()
                .find(|record| &record.key == key)
                .cloned())
        }

        fn find_device_by_certificate_fingerprint(
            &self,
            fingerprint_sha256: &str,
        ) -> Result<Option<WorkspaceDeviceRecord>, WorkspaceDirectoryError> {
            Ok(self
                .records
                .lock()
                .map_err(|_| WorkspaceDirectoryError::Storage("test registry poisoned".into()))?
                .get(fingerprint_sha256)
                .cloned())
        }
    }

    fn certificate(fingerprint: &str, expires_at_unix_ms: u64) -> VerifiedClientCertificate {
        VerifiedClientCertificate {
            fingerprint_sha256: fingerprint.to_string(),
            expires_at_unix_ms,
        }
    }

    fn connector_hello(enrollment_state: &str) -> RelayHello {
        RelayHello {
            role: RelayParticipantRole::WorkspaceConnector as i32,
            session_credential: String::new(),
            user: None,
            organization_id: "organization-1".to_string(),
            workspace_id: "workspace-1".to_string(),
            device: Some(DeviceEnrollmentRef {
                device_id: "device-1".to_string(),
                workspace_id: "workspace-1".to_string(),
                enrollment_state: enrollment_state.to_string(),
            }),
        }
    }

    fn verifier(status: DeviceAuthorizationStatus) -> RegistryWorkspaceDeviceVerifier {
        RegistryWorkspaceDeviceVerifier::new(Arc::new(TestDeviceRegistry::with_record(status)))
    }

    #[test]
    fn tls_peer_extraction_ignores_spoofed_forwarded_certificate_metadata() {
        let mut request = Request::new(());
        request
            .metadata_mut()
            .insert("forwarded", "for=192.0.2.1;proto=https".parse().unwrap());
        request
            .metadata_mut()
            .insert("x-forwarded-client-cert", "Cert=spoofed".parse().unwrap());

        assert_eq!(
            VerifiedClientCertificate::from_tonic_request(&request),
            Err(WorkspaceDeviceAuthenticationError::MissingClientCertificate)
        );
    }

    #[test]
    fn tls_peer_der_produces_normalized_fingerprint_and_not_after() {
        let (_, pem) =
            x509_parser::pem::parse_x509_pem(TEST_CLIENT_CERTIFICATE_PEM.as_bytes()).unwrap();
        let certificate = VerifiedClientCertificate::from_tls_peer_der(&pem.contents).unwrap();

        assert_eq!(
            certificate.fingerprint_sha256(),
            "f303aaff1c210cbfa62c189ee71e0c4240dc8e0324fcaa5a864c8aec45162746"
        );
        assert_eq!(certificate.expires_at_unix_ms(), 2_105_812_441_000);
    }

    #[test]
    fn registry_approved_certificate_authenticates_connector_and_ignores_hello_state() {
        let claims = verifier(DeviceAuthorizationStatus::Approved)
            .authenticate(
                &connector_hello("spoofed-revoked-state"),
                &certificate(APPROVED_FINGERPRINT, NOW_UNIX_MS + 10),
                NOW_UNIX_MS,
            )
            .unwrap();

        assert_eq!(
            claims.principal,
            SessionPrincipal::WorkspaceDevice {
                workspace_id: "workspace-1".into(),
                device_id: "device-1".into(),
            }
        );
        assert_eq!(claims.organization_id, "organization-1");
        assert_eq!(claims.expires_at_unix_ms, NOW_UNIX_MS + 10);
    }

    #[test]
    fn approved_device_certificate_cannot_authenticate_as_frontend_role() {
        let mut hello = connector_hello("approved");
        hello.role = RelayParticipantRole::Frontend as i32;

        assert_eq!(
            verifier(DeviceAuthorizationStatus::Approved).authenticate(
                &hello,
                &certificate(APPROVED_FINGERPRINT, NOW_UNIX_MS + 10),
                NOW_UNIX_MS,
            ),
            Err(WorkspaceDeviceAuthenticationError::IdentityMismatch)
        );
    }

    #[test]
    fn claimed_approval_without_a_matching_registry_certificate_is_rejected() {
        let error = verifier(DeviceAuthorizationStatus::Approved)
            .authenticate(
                &connector_hello("approved"),
                &certificate(&"cd".repeat(32), NOW_UNIX_MS + 10),
                NOW_UNIX_MS,
            )
            .unwrap_err();

        assert_eq!(
            error,
            WorkspaceDeviceAuthenticationError::UnregisteredCertificate
        );
    }

    #[test]
    fn revoked_certificate_is_rejected_even_when_hello_claims_approval() {
        let error = verifier(DeviceAuthorizationStatus::Revoked)
            .authenticate(
                &connector_hello("approved"),
                &certificate(APPROVED_FINGERPRINT, NOW_UNIX_MS + 10),
                NOW_UNIX_MS,
            )
            .unwrap_err();

        assert_eq!(
            error,
            WorkspaceDeviceAuthenticationError::RevokedCertificate
        );
    }

    #[test]
    fn expired_certificate_is_rejected_before_registry_authorization() {
        let error = verifier(DeviceAuthorizationStatus::Approved)
            .authenticate(
                &connector_hello("approved"),
                &certificate(APPROVED_FINGERPRINT, NOW_UNIX_MS),
                NOW_UNIX_MS,
            )
            .unwrap_err();

        assert_eq!(
            error,
            WorkspaceDeviceAuthenticationError::ExpiredCertificate
        );
    }

    #[test]
    fn hello_user_and_workspace_scope_must_match_the_registered_device() {
        let mut hello = connector_hello("approved");
        hello.user = Some(UserIdentityRef {
            issuer: "https://identity.test".into(),
            subject: "user-1".into(),
        });
        assert_eq!(
            verifier(DeviceAuthorizationStatus::Approved).authenticate(
                &hello,
                &certificate(APPROVED_FINGERPRINT, NOW_UNIX_MS + 10),
                NOW_UNIX_MS,
            ),
            Err(WorkspaceDeviceAuthenticationError::IdentityMismatch)
        );

        let mut hello = connector_hello("approved");
        hello.organization_id = "organization-spoofed".into();
        assert_eq!(
            verifier(DeviceAuthorizationStatus::Approved).authenticate(
                &hello,
                &certificate(APPROVED_FINGERPRINT, NOW_UNIX_MS + 10),
                NOW_UNIX_MS,
            ),
            Err(WorkspaceDeviceAuthenticationError::IdentityMismatch)
        );

        let mut hello = connector_hello("approved");
        hello.workspace_id = "workspace-spoofed".into();
        assert_eq!(
            verifier(DeviceAuthorizationStatus::Approved).authenticate(
                &hello,
                &certificate(APPROVED_FINGERPRINT, NOW_UNIX_MS + 10),
                NOW_UNIX_MS,
            ),
            Err(WorkspaceDeviceAuthenticationError::IdentityMismatch)
        );
    }
}
