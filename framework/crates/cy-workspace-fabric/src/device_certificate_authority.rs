//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 device_certificate_authority.rs                                 │
//! │  Module: cy_workspace_fabric::device_certificate_authority          │
//! │  Role: Idempotent device certificate issuer and revoker port.       │
//! │                                                                     │
//! │  模块职责：定义绑定稳定设备身份的 CA 签发、查询和撤销契约。             │
//! └─────────────────────────────────────────────────────────────────────┘
//!
//! This module is a contract, not a CA implementation. The repository has no
//! configured production CA, trust roots, or signing-key service. The default
//! implementation therefore fails closed. A future adapter must construct
//! requests from a trusted Directory registration binding in the same durable
//! orchestration boundary as the authorization record; these values must never
//! be accepted from a browser or inferred from an authorization ID. A separate
//! Directory bind followed by an unrelated authorization insert is not enough.
//!
//! CA response metadata is not proof of a valid certificate path. A configured
//! implementation must authenticate its CA service, verify the returned leaf
//! against configured trust roots and the exact SPKI, enforce current validity
//! and the device certificate profile, and preserve the binding attestation.
//! `x509-parser` in this crate is used for parsing only and is not an RFC 5280
//! path validator.

use std::fmt;

use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::device_authorization::{
    DeviceAuthorizationId, DeviceAuthorizationPortError, DeviceAuthorizationScope,
    DeviceCsrValidator,
};
use crate::device_registry::WorkspaceDeviceKey;
use crate::durable_directory::DeviceRegistrationBinding;

/// Largest CSR accepted by the Workspace device authorization flow.
pub(crate) const MAX_DEVICE_CSR_DER_BYTES: usize = 16 * 1024;

/// Maximum DER size for one certificate returned by an eventual CA adapter.
pub(crate) const MAX_DEVICE_CERTIFICATE_DER_BYTES: usize = 64 * 1024;

/// Maximum certificates in a returned CA chain.
pub(crate) const MAX_DEVICE_CERTIFICATE_CHAIN_LENGTH: usize = 8;

/// Stable values that scope one durable CA issuance operation.
///
/// This structure must be copied from a trusted Directory registration
/// binding. `authorization_id` identifies the persisted authorization attempt
/// and is only an idempotency key; it is not a device identity. `device_key`
/// provides the stable organization, Workspace, and device identity.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct DeviceCertificateIssuanceBinding {
    authorization_id: DeviceAuthorizationId,
    registration_binding_id: [u8; 16],
    device_key: WorkspaceDeviceKey,
    authorization_generation: u64,
    csr_sha256: [u8; 32],
    spki_sha256: [u8; 32],
}

impl fmt::Debug for DeviceCertificateIssuanceBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeviceCertificateIssuanceBinding")
            .field("authorization_id", &self.authorization_id)
            .field("registration_binding_id", &self.registration_binding_id)
            .field("device_key", &self.device_key)
            .field("authorization_generation", &self.authorization_generation)
            .field("csr_sha256", &self.csr_sha256)
            .field("spki_sha256", &self.spki_sha256)
            .finish()
    }
}

impl DeviceCertificateIssuanceBinding {
    pub(crate) fn authorization_id(&self) -> &DeviceAuthorizationId {
        &self.authorization_id
    }

    pub(crate) fn registration_binding_id(&self) -> &[u8; 16] {
        &self.registration_binding_id
    }

    pub(crate) fn device_key(&self) -> &WorkspaceDeviceKey {
        &self.device_key
    }

    pub(crate) fn authorization_generation(&self) -> u64 {
        self.authorization_generation
    }

    pub(crate) fn csr_sha256(&self) -> &[u8; 32] {
        &self.csr_sha256
    }

    pub(crate) fn spki_sha256(&self) -> &[u8; 32] {
        &self.spki_sha256
    }
}

/// Immutable input for one idempotent device certificate issuance.
///
/// Workspace scope is derived from `binding.device_key`; a caller cannot pass
/// a second, potentially conflicting scope. The constructor rechecks the CSR
/// digest and SPKI using the existing CSR validation port. Its binding must be
/// populated from the trusted Directory record, including the stable device
/// key and authorization generation.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct DeviceCertificateIssueRequest {
    binding: DeviceCertificateIssuanceBinding,
    csr_der: Vec<u8>,
    issued_at_unix_ms: u64,
    request_sha256: [u8; 32],
}

impl fmt::Debug for DeviceCertificateIssueRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeviceCertificateIssueRequest")
            .field("binding", &self.binding)
            .field("csr_der", &"[REDACTED]")
            .field("csr_der_len", &self.csr_der.len())
            .field("issued_at_unix_ms", &self.issued_at_unix_ms)
            .field("request_sha256", &self.request_sha256)
            .finish()
    }
}

impl DeviceCertificateIssueRequest {
    /// Copy identity and digests only from the trusted Directory binding.
    ///
    /// No caller-provided device ID or scope is accepted here. The exact CSR
    /// is checked against both Directory digests and the existing CSR
    /// validator before the CA request can be constructed. The binding and
    /// authorization ID must come from the transactionally composed enrollment
    /// operation; do not call Directory binding and authorization insertion as
    /// separate production steps.
    pub(crate) fn from_directory_binding(
        authorization_id: DeviceAuthorizationId,
        directory_binding: &DeviceRegistrationBinding,
        csr_der: Vec<u8>,
        issued_at_unix_ms: u64,
        csr_validator: &impl DeviceCsrValidator,
    ) -> Result<Self, DeviceCertificateAuthorityError> {
        let device_key = directory_binding.key().clone();
        if device_key.organization_id.is_empty()
            || device_key.workspace_id.is_empty()
            || device_key.device_id.is_empty()
        {
            return Err(DeviceCertificateAuthorityError::InvalidRequest);
        }

        let binding = DeviceCertificateIssuanceBinding {
            authorization_id,
            registration_binding_id: *directory_binding.binding_id(),
            device_key,
            authorization_generation: directory_binding.authorization_generation(),
            csr_sha256: *directory_binding.csr_sha256(),
            spki_sha256: *directory_binding.spki_sha256(),
        };
        Self::new(binding, csr_der, issued_at_unix_ms, csr_validator)
    }

    fn new(
        binding: DeviceCertificateIssuanceBinding,
        csr_der: Vec<u8>,
        issued_at_unix_ms: u64,
        csr_validator: &impl DeviceCsrValidator,
    ) -> Result<Self, DeviceCertificateAuthorityError> {
        if csr_der.is_empty() || csr_der.len() > MAX_DEVICE_CSR_DER_BYTES {
            return Err(DeviceCertificateAuthorityError::InvalidRequest);
        }
        if sha256(&csr_der) != binding.csr_sha256 {
            return Err(DeviceCertificateAuthorityError::BindingMismatch);
        }

        let validated_spki = csr_validator
            .validate_and_hash_spki(&csr_der)
            .map_err(map_csr_validation_error)?;
        if validated_spki != binding.spki_sha256 {
            return Err(DeviceCertificateAuthorityError::BindingMismatch);
        }

        let request_sha256 = issue_request_digest(&binding, issued_at_unix_ms);
        Ok(Self {
            binding,
            csr_der,
            issued_at_unix_ms,
            request_sha256,
        })
    }

    pub(crate) fn binding(&self) -> &DeviceCertificateIssuanceBinding {
        &self.binding
    }

    pub(crate) fn scope(&self) -> DeviceAuthorizationScope {
        DeviceAuthorizationScope {
            organization_id: self.binding.device_key.organization_id.clone(),
            workspace_id: self.binding.device_key.workspace_id.clone(),
        }
    }

    pub(crate) fn csr_der(&self) -> &[u8] {
        &self.csr_der
    }

    pub(crate) fn issued_at_unix_ms(&self) -> u64 {
        self.issued_at_unix_ms
    }

    pub(crate) fn request_sha256(&self) -> &[u8; 32] {
        &self.request_sha256
    }
}

/// Request for idempotently revoking one exact issued certificate.
///
/// The stable key, registration binding, generation, serial, and leaf DER
/// fingerprint identify the certificate. Registry-only revocation is not CA
/// revocation, and a missing or pending CA result is never confirmation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeviceCertificateRevocationRequest {
    authorization_id: DeviceAuthorizationId,
    registration_binding_id: [u8; 16],
    device_key: WorkspaceDeviceKey,
    authorization_generation: u64,
    serial_number: Vec<u8>,
    certificate_fingerprint_sha256: [u8; 32],
    idempotency_key_sha256: [u8; 32],
}

impl DeviceCertificateRevocationRequest {
    pub(crate) fn new(
        authorization_id: DeviceAuthorizationId,
        registration_binding_id: [u8; 16],
        device_key: WorkspaceDeviceKey,
        authorization_generation: u64,
        serial_number: Vec<u8>,
        certificate_fingerprint_sha256: [u8; 32],
    ) -> Result<Self, DeviceCertificateAuthorityError> {
        if device_key.organization_id.is_empty()
            || device_key.workspace_id.is_empty()
            || device_key.device_id.is_empty()
            || serial_number.is_empty()
            || serial_number.len() > 64
        {
            return Err(DeviceCertificateAuthorityError::InvalidRequest);
        }

        let idempotency_key_sha256 = revocation_request_digest(
            &authorization_id,
            &registration_binding_id,
            &device_key,
            authorization_generation,
            &serial_number,
            &certificate_fingerprint_sha256,
        );

        Ok(Self {
            authorization_id,
            registration_binding_id,
            device_key,
            authorization_generation,
            serial_number,
            certificate_fingerprint_sha256,
            idempotency_key_sha256,
        })
    }

    pub(crate) fn authorization_id(&self) -> &DeviceAuthorizationId {
        &self.authorization_id
    }

    pub(crate) fn registration_binding_id(&self) -> &[u8; 16] {
        &self.registration_binding_id
    }

    pub(crate) fn device_key(&self) -> &WorkspaceDeviceKey {
        &self.device_key
    }

    pub(crate) fn authorization_generation(&self) -> u64 {
        self.authorization_generation
    }

    pub(crate) fn serial_number(&self) -> &[u8] {
        &self.serial_number
    }

    pub(crate) fn certificate_fingerprint_sha256(&self) -> &[u8; 32] {
        &self.certificate_fingerprint_sha256
    }

    pub(crate) fn idempotency_key_sha256(&self) -> &[u8; 32] {
        &self.idempotency_key_sha256
    }
}

/// CA-issued certificate metadata and bytes returned for one request.
///
/// This receipt is only an untrusted service response. A concrete adapter must
/// verify its echo against the request and validate the leaf certificate path,
/// validity, client-auth usage, and SPKI against configured trust roots before
/// converting it into approved Workspace certificate metadata.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct UnverifiedDeviceCertificateIssueReceipt {
    pub(crate) authorization_id: DeviceAuthorizationId,
    pub(crate) registration_binding_id: [u8; 16],
    pub(crate) device_key: WorkspaceDeviceKey,
    pub(crate) authorization_generation: u64,
    pub(crate) csr_sha256: [u8; 32],
    pub(crate) spki_sha256: [u8; 32],
    pub(crate) request_sha256: [u8; 32],
    pub(crate) certificate_der: Vec<u8>,
    pub(crate) certificate_chain_der: Vec<Vec<u8>>,
    pub(crate) serial_number: Vec<u8>,
    pub(crate) not_after_unix_ms: u64,
}

impl fmt::Debug for UnverifiedDeviceCertificateIssueReceipt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UnverifiedDeviceCertificateIssueReceipt")
            .field("authorization_id", &self.authorization_id)
            .field("registration_binding_id", &self.registration_binding_id)
            .field("device_key", &self.device_key)
            .field("authorization_generation", &self.authorization_generation)
            .field("csr_sha256", &self.csr_sha256)
            .field("spki_sha256", &self.spki_sha256)
            .field("request_sha256", &self.request_sha256)
            .field("certificate_der_len", &self.certificate_der.len())
            .field("certificate_chain_len", &self.certificate_chain_der.len())
            .field("serial_number", &self.serial_number)
            .field("not_after_unix_ms", &self.not_after_unix_ms)
            .finish()
    }
}

impl UnverifiedDeviceCertificateIssueReceipt {
    pub(crate) fn echoes_request(&self, request: &DeviceCertificateIssueRequest) -> bool {
        self.authorization_id == request.binding.authorization_id
            && self.registration_binding_id == request.binding.registration_binding_id
            && self.device_key == request.binding.device_key
            && self.authorization_generation == request.binding.authorization_generation
            && self.csr_sha256 == request.binding.csr_sha256
            && self.spki_sha256 == request.binding.spki_sha256
            && self.request_sha256 == request.request_sha256
            && !self.certificate_der.is_empty()
            && self.certificate_der.len() <= MAX_DEVICE_CERTIFICATE_DER_BYTES
            && self.certificate_chain_der.len() <= MAX_DEVICE_CERTIFICATE_CHAIN_LENGTH
            && self
                .certificate_chain_der
                .iter()
                .all(|der| !der.is_empty() && der.len() <= MAX_DEVICE_CERTIFICATE_DER_BYTES)
            && !self.serial_number.is_empty()
            && self.serial_number.len() <= 64
            && self.not_after_unix_ms > request.issued_at_unix_ms
    }
}

/// Confirmed revocation response from the CA service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeviceCertificateRevocationReceipt {
    pub(crate) authorization_id: DeviceAuthorizationId,
    pub(crate) registration_binding_id: [u8; 16],
    pub(crate) device_key: WorkspaceDeviceKey,
    pub(crate) authorization_generation: u64,
    pub(crate) serial_number: Vec<u8>,
    pub(crate) certificate_fingerprint_sha256: [u8; 32],
    pub(crate) idempotency_key_sha256: [u8; 32],
}

impl DeviceCertificateRevocationReceipt {
    pub(crate) fn echoes_request(&self, request: &DeviceCertificateRevocationRequest) -> bool {
        self.authorization_id == request.authorization_id
            && self.registration_binding_id == request.registration_binding_id
            && self.device_key == request.device_key
            && self.authorization_generation == request.authorization_generation
            && self.serial_number == request.serial_number
            && self.certificate_fingerprint_sha256 == request.certificate_fingerprint_sha256
            && self.idempotency_key_sha256 == request.idempotency_key_sha256
    }
}

/// Deterministic policy rejection codes returned by an authenticated CA.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeviceCertificateAuthorityRejection {
    InvalidCsr,
    ScopeNotAllowed,
    UnsupportedCertificateProfile,
    PolicyDenied,
}

/// Result of an idempotent issuance attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DeviceCertificateIssueOutcome {
    /// The CA reports issuance; the caller still has to validate the receipt.
    Issued(Box<UnverifiedDeviceCertificateIssueReceipt>),
    /// Deterministic rejection; this idempotency key did not create a cert.
    Rejected(DeviceCertificateAuthorityRejection),
    /// The CA accepted work but has not finished it.
    Pending,
}

/// Authoritative result of querying an issuance idempotency key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DeviceCertificateIssueLookup {
    /// The CA reports issuance; the caller still has to validate the receipt.
    Issued(Box<UnverifiedDeviceCertificateIssueReceipt>),
    /// The CA durably recorded a deterministic rejection.
    Rejected(DeviceCertificateAuthorityRejection),
    /// An issuance operation exists and is not terminal.
    Pending,
    /// The authoritative CA lookup has no operation for this request key.
    NotFound,
}

/// Result of an idempotent certificate revocation attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DeviceCertificateRevocationOutcome {
    /// The CA confirmed revocation for the exact request binding.
    Confirmed(DeviceCertificateRevocationReceipt),
    /// Deterministic rejection; no revocation was applied for this key.
    Rejected(DeviceCertificateAuthorityRejection),
    /// The CA accepted work but has not confirmed revocation.
    Pending,
}

/// Authoritative result of querying a certificate revocation idempotency key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DeviceCertificateRevocationLookup {
    /// The CA durably confirmed revocation for the exact request binding.
    Confirmed(DeviceCertificateRevocationReceipt),
    /// A revocation operation exists and is not terminal.
    Pending,
    /// No operation exists; this is not evidence of revocation.
    NotRequested,
}

/// Idempotent external CA boundary required by a production deployment.
///
/// Implementations must durably deduplicate issuance by `authorization_id` and
/// bind that key to the complete request digest. Reusing an authorization ID
/// with different binding data must be rejected without signing. If an
/// operation may have committed but its response is lost, return
/// `OutcomeUnknown`; callers must query or retry the exact same request. Query
/// results must be authoritative for the same idempotency key.
///
/// Revocation is complete only after returning `Confirmed` with an exact match
/// for the requested binding, serial, and leaf fingerprint. `NotRequested`,
/// `Pending`, registry metadata changes, and unknown outcomes are not CA
/// revocation confirmation.
pub(crate) trait DeviceCertificateAuthorityPort: Send + Sync {
    fn issue_or_get(
        &self,
        request: &DeviceCertificateIssueRequest,
    ) -> Result<DeviceCertificateIssueOutcome, DeviceCertificateAuthorityError>;

    fn lookup_issuance(
        &self,
        request: &DeviceCertificateIssueRequest,
    ) -> Result<DeviceCertificateIssueLookup, DeviceCertificateAuthorityError>;

    fn revoke_or_get(
        &self,
        request: &DeviceCertificateRevocationRequest,
    ) -> Result<DeviceCertificateRevocationOutcome, DeviceCertificateAuthorityError>;

    fn lookup_revocation(
        &self,
        request: &DeviceCertificateRevocationRequest,
    ) -> Result<DeviceCertificateRevocationLookup, DeviceCertificateAuthorityError>;
}

/// Safe default while production CA service identity, roots, and credentials
/// are not configured. It never attempts to sign or revoke certificates.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct UnconfiguredDeviceCertificateAuthority;

impl DeviceCertificateAuthorityPort for UnconfiguredDeviceCertificateAuthority {
    fn issue_or_get(
        &self,
        _request: &DeviceCertificateIssueRequest,
    ) -> Result<DeviceCertificateIssueOutcome, DeviceCertificateAuthorityError> {
        Err(DeviceCertificateAuthorityError::Unavailable)
    }

    fn lookup_issuance(
        &self,
        _request: &DeviceCertificateIssueRequest,
    ) -> Result<DeviceCertificateIssueLookup, DeviceCertificateAuthorityError> {
        Err(DeviceCertificateAuthorityError::Unavailable)
    }

    fn revoke_or_get(
        &self,
        _request: &DeviceCertificateRevocationRequest,
    ) -> Result<DeviceCertificateRevocationOutcome, DeviceCertificateAuthorityError> {
        Err(DeviceCertificateAuthorityError::Unavailable)
    }

    fn lookup_revocation(
        &self,
        _request: &DeviceCertificateRevocationRequest,
    ) -> Result<DeviceCertificateRevocationLookup, DeviceCertificateAuthorityError> {
        Err(DeviceCertificateAuthorityError::Unavailable)
    }
}

/// Local validation, CA rejection, and transport uncertainty are separate
/// so callers cannot mistake a lost response for a deterministic rejection.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeviceCertificateAuthorityError {
    #[error("invalid device certificate authority request")]
    InvalidRequest,
    #[error("device certificate request does not match Directory binding")]
    BindingMismatch,
    #[error("device CSR rejected by the configured validator")]
    CsrRejected,
    #[error("device CSR validator unavailable")]
    CsrValidatorUnavailable,
    /// No operation was sent to the CA; it is safe to retry with the same key.
    #[error("device certificate authority is not configured or unavailable")]
    Unavailable,
    /// The request may have committed; reconcile using the same idempotency key.
    #[error("device certificate authority operation outcome is unknown")]
    OutcomeUnknown,
}

fn map_csr_validation_error(
    error: DeviceAuthorizationPortError,
) -> DeviceCertificateAuthorityError {
    match error {
        DeviceAuthorizationPortError::Rejected => DeviceCertificateAuthorityError::CsrRejected,
        DeviceAuthorizationPortError::Unavailable => {
            DeviceCertificateAuthorityError::CsrValidatorUnavailable
        }
    }
}

fn issue_request_digest(
    binding: &DeviceCertificateIssuanceBinding,
    issued_at_unix_ms: u64,
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"CYRENE-WORKSPACE-DEVICE-CERT-ISSUE-V1\0");
    hasher.update(binding.authorization_id);
    hasher.update(binding.registration_binding_id);
    hasher.update(binding.authorization_generation.to_be_bytes());
    hash_field(&mut hasher, binding.device_key.organization_id.as_bytes());
    hash_field(&mut hasher, binding.device_key.workspace_id.as_bytes());
    hash_field(&mut hasher, binding.device_key.device_id.as_bytes());
    hasher.update(binding.csr_sha256);
    hasher.update(binding.spki_sha256);
    hasher.update(issued_at_unix_ms.to_be_bytes());
    hasher.finalize().into()
}

fn revocation_request_digest(
    authorization_id: &DeviceAuthorizationId,
    registration_binding_id: &[u8; 16],
    device_key: &WorkspaceDeviceKey,
    authorization_generation: u64,
    serial_number: &[u8],
    certificate_fingerprint_sha256: &[u8; 32],
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"CYRENE-WORKSPACE-DEVICE-CERT-REVOKE-V1\0");
    hasher.update(authorization_id);
    hasher.update(registration_binding_id);
    hasher.update(authorization_generation.to_be_bytes());
    hash_field(&mut hasher, device_key.organization_id.as_bytes());
    hash_field(&mut hasher, device_key.workspace_id.as_bytes());
    hash_field(&mut hasher, device_key.device_id.as_bytes());
    hash_field(&mut hasher, serial_number);
    hasher.update(certificate_fingerprint_sha256);
    hasher.finalize().into()
}

fn hash_field(hasher: &mut Sha256, field: &[u8]) {
    hasher.update((field.len() as u64).to_be_bytes());
    hasher.update(field);
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestCsrValidator([u8; 32]);

    impl DeviceCsrValidator for TestCsrValidator {
        fn validate_and_hash_spki(
            &self,
            _csr_der: &[u8],
        ) -> Result<[u8; 32], DeviceAuthorizationPortError> {
            Ok(self.0)
        }
    }

    fn directory_binding(csr_der: &[u8], spki_sha256: [u8; 32]) -> DeviceRegistrationBinding {
        DeviceRegistrationBinding::test_fixture(
            [2; 16],
            WorkspaceDeviceKey {
                organization_id: "org-a".into(),
                workspace_id: "workspace-a".into(),
                device_id: "device-a".into(),
            },
            3,
            sha256(csr_der),
            spki_sha256,
        )
    }

    fn request() -> DeviceCertificateIssueRequest {
        let csr_der = vec![0x30, 0x00];
        let spki_sha256 = [7; 32];
        let binding = directory_binding(&csr_der, spki_sha256);
        DeviceCertificateIssueRequest::from_directory_binding(
            [1; 16],
            &binding,
            csr_der,
            1_000,
            &TestCsrValidator(spki_sha256),
        )
        .expect("valid request")
    }

    #[test]
    fn issue_request_binds_stable_device_scope_and_redacts_csr_debug() {
        let request = request();
        let scope = request.scope();

        assert_eq!(scope.organization_id, "org-a");
        assert_eq!(scope.workspace_id, "workspace-a");
        assert_eq!(request.binding().device_key().device_id, "device-a");
        assert_eq!(request.binding().authorization_id(), &[1; 16]);
        assert_eq!(request.binding().registration_binding_id(), &[2; 16]);
        assert_eq!(request.binding().authorization_generation(), 3);
        assert!(!format!("{request:?}").contains("48, 0"));
        assert!(request.request_sha256() != &[0; 32]);
    }

    #[test]
    fn issue_request_rejects_csr_digest_mismatch() {
        let csr_der = vec![0x30, 0x01, 0x00];
        let binding = directory_binding(&[0x30, 0x00], [7; 32]);

        assert_eq!(
            DeviceCertificateIssueRequest::from_directory_binding(
                [1; 16],
                &binding,
                csr_der,
                1_000,
                &TestCsrValidator([7; 32]),
            ),
            Err(DeviceCertificateAuthorityError::BindingMismatch)
        );
    }

    #[test]
    fn issue_request_rejects_spki_binding_mismatch() {
        let csr_der = vec![0x30, 0x01, 0x00];
        let binding = directory_binding(&csr_der, [8; 32]);

        assert_eq!(
            DeviceCertificateIssueRequest::from_directory_binding(
                [1; 16],
                &binding,
                csr_der,
                1_000,
                &TestCsrValidator([7; 32]),
            ),
            Err(DeviceCertificateAuthorityError::BindingMismatch)
        );
    }

    #[test]
    fn unconfigured_authority_never_issues_or_confirms_revocation() {
        let authority = UnconfiguredDeviceCertificateAuthority;
        let request = request();
        assert_eq!(
            authority.issue_or_get(&request),
            Err(DeviceCertificateAuthorityError::Unavailable)
        );
        assert_eq!(
            authority.lookup_issuance(&request),
            Err(DeviceCertificateAuthorityError::Unavailable)
        );

        let revoke = DeviceCertificateRevocationRequest::new(
            *request.binding().authorization_id(),
            *request.binding().registration_binding_id(),
            request.binding().device_key().clone(),
            request.binding().authorization_generation(),
            vec![1, 2, 3],
            [4; 32],
        )
        .expect("valid revocation request");
        assert_eq!(
            authority.revoke_or_get(&revoke),
            Err(DeviceCertificateAuthorityError::Unavailable)
        );
        assert_eq!(
            authority.lookup_revocation(&revoke),
            Err(DeviceCertificateAuthorityError::Unavailable)
        );
    }

    #[test]
    fn issue_receipt_must_echo_the_full_directory_and_csr_binding() {
        let request = request();
        let receipt = UnverifiedDeviceCertificateIssueReceipt {
            authorization_id: *request.binding.authorization_id(),
            registration_binding_id: *request.binding.registration_binding_id(),
            device_key: request.binding.device_key().clone(),
            authorization_generation: request.binding.authorization_generation(),
            csr_sha256: *request.binding.csr_sha256(),
            spki_sha256: *request.binding.spki_sha256(),
            request_sha256: *request.request_sha256(),
            certificate_der: vec![1],
            certificate_chain_der: vec![vec![2]],
            serial_number: vec![3],
            not_after_unix_ms: 2_000,
        };
        assert!(receipt.echoes_request(&request));

        let mut wrong_generation = receipt.clone();
        wrong_generation.authorization_generation += 1;
        assert!(!wrong_generation.echoes_request(&request));
    }

    #[test]
    fn revocation_requires_exact_serial_and_fingerprint_confirmation() {
        let issue = request();
        let request = DeviceCertificateRevocationRequest::new(
            *issue.binding().authorization_id(),
            *issue.binding().registration_binding_id(),
            issue.binding().device_key().clone(),
            issue.binding().authorization_generation(),
            vec![1, 2, 3],
            [4; 32],
        )
        .expect("valid revocation request");
        let receipt = DeviceCertificateRevocationReceipt {
            authorization_id: *request.authorization_id(),
            registration_binding_id: *request.registration_binding_id(),
            device_key: request.device_key().clone(),
            authorization_generation: request.authorization_generation(),
            serial_number: request.serial_number().to_vec(),
            certificate_fingerprint_sha256: *request.certificate_fingerprint_sha256(),
            idempotency_key_sha256: *request.idempotency_key_sha256(),
        };
        assert!(receipt.echoes_request(&request));

        let mut wrong_fingerprint = receipt;
        wrong_fingerprint.certificate_fingerprint_sha256 = [5; 32];
        assert!(!wrong_fingerprint.echoes_request(&request));
    }

    #[test]
    fn revocation_idempotency_key_covers_exact_certificate_and_device() {
        let key = WorkspaceDeviceKey {
            organization_id: "org-a".into(),
            workspace_id: "workspace-a".into(),
            device_id: "device-a".into(),
        };
        let first = DeviceCertificateRevocationRequest::new(
            [1; 16],
            [2; 16],
            key.clone(),
            3,
            vec![1, 2, 3],
            [4; 32],
        )
        .expect("valid request");
        let retry = DeviceCertificateRevocationRequest::new(
            [1; 16],
            [2; 16],
            key.clone(),
            3,
            vec![1, 2, 3],
            [4; 32],
        )
        .expect("valid retry");
        let different_certificate = DeviceCertificateRevocationRequest::new(
            [1; 16],
            [2; 16],
            key,
            3,
            vec![1, 2, 4],
            [4; 32],
        )
        .expect("valid different certificate");

        assert_eq!(
            first.idempotency_key_sha256(),
            retry.idempotency_key_sha256()
        );
        assert_ne!(
            first.idempotency_key_sha256(),
            different_certificate.idempotency_key_sha256()
        );
    }
}
