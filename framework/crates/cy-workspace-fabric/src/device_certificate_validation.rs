//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 device_certificate_validation.rs                               │
//! │  Module: cy_workspace_fabric::device_certificate_validation         │
//! │  Role: Validate issued Workspace device certificate responses.      │
//! │                                                                     │
//! │  模块职责：验收 Workspace 设备证书响应并拒绝未验证的撤销状态。          │
//! └─────────────────────────────────────────────────────────────────────┘
//!
//! This is a validation seam, not a configured CA or revocation service. A
//! successful result requires configured trust roots and an injected checker
//! that proves the exact certificate currently has good revocation status.

use std::collections::BTreeSet;
use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use rustls::pki_types::{CertificateDer, UnixTime};
use rustls::RootCertStore;
use sha2::{Digest, Sha256};
use thiserror::Error;
use webpki::{EndEntityCert, KeyUsage};
use x509_parser::certificate::X509Certificate;
use x509_parser::extensions::ParsedExtension;
use x509_parser::parse_x509_certificate;

use crate::device_authorization::{
    DeviceAuthorizationDeviceKey, DeviceAuthorizationScope, IssuedDeviceCertificate,
};
use crate::device_certificate_authority::DeviceCertificateIssuanceBinding;
use crate::device_registry::WorkspaceDeviceKey;

const MAX_TRUST_ROOTS: usize = 32;
const MAX_TRUST_ROOT_BUNDLE_BYTES: usize = 1024 * 1024;
const MAX_CHAIN_CERTIFICATES: usize = 8;
const MAX_CERTIFICATE_DER_BYTES: usize = 64 * 1024;
const MAX_CHAIN_DER_BYTES: usize = 256 * 1024;
const MAX_DEVICE_IDENTITY_URI_BYTES: usize = 8 * 1024;
const CLIENT_AUTH_EKU_DER: &[u8] = &[0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x02];

/// Configuration error returned when the private client CA trust roots are absent or invalid.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeviceCertificateValidationConfigError {
    #[error("WORKSPACE_DEVICE_CERTIFICATE_TRUST_CONFIG_INVALID")]
    InvalidTrustRoots,
}

/// Validation failure that deliberately omits certificate and CA diagnostics.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeviceCertificateValidationError {
    #[error("WORKSPACE_DEVICE_CERTIFICATE_INVALID")]
    InvalidCertificate,
    #[error("WORKSPACE_DEVICE_CERTIFICATE_REVOKED")]
    Revoked,
    #[error("WORKSPACE_DEVICE_CERTIFICATE_REVOCATION_STATUS_UNKNOWN")]
    RevocationStatusUnknown,
}

/// Revocation outcomes returned by a separately trusted status adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub(crate) enum DeviceCertificateRevocationCheckError {
    #[error("certificate is revoked")]
    Revoked,
    #[error("current revocation status is unknown or unavailable")]
    Unknown,
}

/// Exact certificate tuple presented to the configured revocation checker.
///
/// The checker must authenticate and verify its evidence (for example a
/// current signed CRL or OCSP response) against the configured CA path. A
/// cache miss, stale response, timeout, malformed evidence, or unavailable
/// checker must return `Unknown`; it must never be treated as `Good`.
pub(crate) struct DeviceCertificateRevocationQuery<'a> {
    certificate_der: &'a [u8],
    ca_chain_der: &'a [Vec<u8>],
    trusted_roots_der: &'a [Vec<u8>],
    serial_number: &'a [u8],
    certificate_sha256: [u8; 32],
    checked_at_unix_ms: u64,
}

impl DeviceCertificateRevocationQuery<'_> {
    pub(crate) fn certificate_der(&self) -> &[u8] {
        self.certificate_der
    }

    pub(crate) fn ca_chain_der(&self) -> &[Vec<u8>] {
        self.ca_chain_der
    }

    pub(crate) fn trusted_roots_der(&self) -> &[Vec<u8>] {
        self.trusted_roots_der
    }

    /// Canonical positive serial bytes, without the DER INTEGER sign octet.
    pub(crate) fn serial_number(&self) -> &[u8] {
        self.serial_number
    }

    pub(crate) fn certificate_sha256(&self) -> &[u8; 32] {
        &self.certificate_sha256
    }

    pub(crate) fn checked_at_unix_ms(&self) -> u64 {
        self.checked_at_unix_ms
    }
}

/// Trusted online or local revocation-status boundary.
///
/// Implementations must return `Ok(())` only after proving current good
/// status for this exact DER and issuer path. There is intentionally no default
/// implementation: absence of an OCSP/CRL/status integration denies issuance.
pub(crate) trait DeviceCertificateRevocationChecker: Send + Sync {
    fn require_current_good_status(
        &self,
        query: &DeviceCertificateRevocationQuery<'_>,
    ) -> Result<(), DeviceCertificateRevocationCheckError>;
}

/// Cryptographic facts derived from a fully validated issuer response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ValidatedIssuedDeviceCertificate {
    certificate_sha256: [u8; 32],
    serial_number: Vec<u8>,
    not_after_unix_ms: u64,
}

impl ValidatedIssuedDeviceCertificate {
    pub(crate) fn certificate_sha256(&self) -> &[u8; 32] {
        &self.certificate_sha256
    }

    pub(crate) fn serial_number(&self) -> &[u8] {
        &self.serial_number
    }

    pub(crate) fn not_after_unix_ms(&self) -> u64 {
        self.not_after_unix_ms
    }
}

/// Response verifier initialized only with private, configured client CA roots.
pub(crate) struct DeviceCertificateResponseValidator {
    roots: RootCertStore,
    trusted_roots_der: Vec<Vec<u8>>,
    root_fingerprints: BTreeSet<[u8; 32]>,
}

impl DeviceCertificateResponseValidator {
    /// Construct from DER trust anchors supplied by trusted runtime configuration.
    ///
    /// An empty, oversized, malformed, duplicate, or non-CA bundle fails startup.
    pub(crate) fn new(
        trusted_roots_der: Vec<Vec<u8>>,
    ) -> Result<Self, DeviceCertificateValidationConfigError> {
        let total_bytes = trusted_roots_der
            .iter()
            .try_fold(0_usize, |total, certificate| {
                total.checked_add(certificate.len())
            })
            .ok_or(DeviceCertificateValidationConfigError::InvalidTrustRoots)?;
        if trusted_roots_der.is_empty()
            || trusted_roots_der.len() > MAX_TRUST_ROOTS
            || total_bytes > MAX_TRUST_ROOT_BUNDLE_BYTES
        {
            return Err(DeviceCertificateValidationConfigError::InvalidTrustRoots);
        }

        let mut roots = RootCertStore::empty();
        let mut root_fingerprints = BTreeSet::new();
        for certificate_der in &trusted_roots_der {
            if certificate_der.is_empty() || certificate_der.len() > MAX_CERTIFICATE_DER_BYTES {
                return Err(DeviceCertificateValidationConfigError::InvalidTrustRoots);
            }
            let certificate = parse_certificate(certificate_der)
                .map_err(|_| DeviceCertificateValidationConfigError::InvalidTrustRoots)?;
            validate_ca_certificate(&certificate, None)
                .map_err(|_| DeviceCertificateValidationConfigError::InvalidTrustRoots)?;
            if !root_fingerprints.insert(sha256(certificate_der)) {
                return Err(DeviceCertificateValidationConfigError::InvalidTrustRoots);
            }
            roots
                .add(CertificateDer::from(certificate_der.clone()))
                .map_err(|_| DeviceCertificateValidationConfigError::InvalidTrustRoots)?;
        }

        if roots.is_empty() {
            return Err(DeviceCertificateValidationConfigError::InvalidTrustRoots);
        }

        Ok(Self {
            roots,
            trusted_roots_der,
            root_fingerprints,
        })
    }

    /// Validate path, private identity profile, issuer metadata, and current revocation status.
    ///
    /// The expected binding must come from the trusted Directory result used
    /// to authorize signing. The explicit checker parameter has no fallback;
    /// its errors reject the certificate.
    pub(crate) fn validate_issued_certificate(
        &self,
        issued: &IssuedDeviceCertificate,
        binding: &DeviceCertificateIssuanceBinding,
        now_unix_ms: u64,
        revocation_checker: &impl DeviceCertificateRevocationChecker,
    ) -> Result<ValidatedIssuedDeviceCertificate, DeviceCertificateValidationError> {
        if issued.certificate_der.is_empty()
            || issued.certificate_der.len() > MAX_CERTIFICATE_DER_BYTES
            || issued.ca_chain_der.len() > MAX_CHAIN_CERTIFICATES
            || issued
                .ca_chain_der
                .iter()
                .any(|der| der.is_empty() || der.len() > MAX_CERTIFICATE_DER_BYTES)
            || issued.ca_chain_der.iter().map(Vec::len).sum::<usize>() > MAX_CHAIN_DER_BYTES
            || !response_metadata_matches_binding(issued, binding)
        {
            return Err(DeviceCertificateValidationError::InvalidCertificate);
        }

        let now_seconds = unix_seconds(now_unix_ms)
            .ok_or(DeviceCertificateValidationError::InvalidCertificate)?;
        let leaf = parse_certificate(&issued.certificate_der)
            .map_err(|_| DeviceCertificateValidationError::InvalidCertificate)?;
        let leaf_fingerprint = sha256(&issued.certificate_der);
        let serial_number = canonical_serial(leaf.tbs_certificate.raw_serial())
            .ok_or(DeviceCertificateValidationError::InvalidCertificate)?;
        let not_after_unix_ms = timestamp_millis(leaf.validity().not_after.timestamp())
            .ok_or(DeviceCertificateValidationError::InvalidCertificate)?;

        if serial_number != issued.serial_number
            || not_after_unix_ms != issued.not_after_unix_ms
            || issued.not_after_unix_ms <= now_unix_ms
            || leaf.validity().not_before.timestamp() > now_seconds
            || leaf.validity().not_after.timestamp() <= now_seconds
            || sha256(leaf.tbs_certificate.subject_pki.raw) != *binding.spki_sha256()
            || !leaf_matches_device_profile(&leaf, binding)
        {
            return Err(DeviceCertificateValidationError::InvalidCertificate);
        }

        self.verify_rfc5280_path(issued, now_unix_ms, now_seconds)?;

        let query = DeviceCertificateRevocationQuery {
            certificate_der: &issued.certificate_der,
            ca_chain_der: &issued.ca_chain_der,
            trusted_roots_der: &self.trusted_roots_der,
            serial_number: &serial_number,
            certificate_sha256: leaf_fingerprint,
            checked_at_unix_ms: now_unix_ms,
        };
        revocation_checker
            .require_current_good_status(&query)
            .map_err(|error| match error {
                DeviceCertificateRevocationCheckError::Revoked => {
                    DeviceCertificateValidationError::Revoked
                }
                DeviceCertificateRevocationCheckError::Unknown => {
                    DeviceCertificateValidationError::RevocationStatusUnknown
                }
            })?;

        Ok(ValidatedIssuedDeviceCertificate {
            certificate_sha256: leaf_fingerprint,
            serial_number,
            not_after_unix_ms,
        })
    }

    fn verify_rfc5280_path(
        &self,
        issued: &IssuedDeviceCertificate,
        now_unix_ms: u64,
        now_seconds: i64,
    ) -> Result<(), DeviceCertificateValidationError> {
        for root_der in &self.trusted_roots_der {
            let root = parse_certificate(root_der)
                .map_err(|_| DeviceCertificateValidationError::InvalidCertificate)?;
            validate_ca_certificate(&root, Some(now_seconds))
                .map_err(|_| DeviceCertificateValidationError::InvalidCertificate)?;
        }

        let mut seen = BTreeSet::new();
        let mut intermediate_der = Vec::with_capacity(issued.ca_chain_der.len());
        let mut parsed_intermediates = Vec::with_capacity(issued.ca_chain_der.len());
        for der in &issued.ca_chain_der {
            let fingerprint = sha256(der);
            if fingerprint == sha256(&issued.certificate_der)
                || self.root_fingerprints.contains(&fingerprint)
                || !seen.insert(fingerprint)
            {
                return Err(DeviceCertificateValidationError::InvalidCertificate);
            }
            let certificate = parse_certificate(der)
                .map_err(|_| DeviceCertificateValidationError::InvalidCertificate)?;
            validate_ca_certificate(&certificate, Some(now_seconds))
                .map_err(|_| DeviceCertificateValidationError::InvalidCertificate)?;
            parsed_intermediates.push(certificate);
            intermediate_der.push(CertificateDer::from(der.clone()));
        }

        // Require every supplied intermediate to form one ordered path and to
        // terminate at exactly one configured root. This prevents unused or
        // attacker-added CA certificates from being silently ignored.
        let leaf = parse_certificate(&issued.certificate_der)
            .map_err(|_| DeviceCertificateValidationError::InvalidCertificate)?;
        let mut next_issuer = leaf.issuer().as_raw();
        for intermediate in &parsed_intermediates {
            if next_issuer != intermediate.subject().as_raw() {
                return Err(DeviceCertificateValidationError::InvalidCertificate);
            }
            next_issuer = intermediate.issuer().as_raw();
        }
        let matching_roots = self
            .trusted_roots_der
            .iter()
            .filter_map(|der| parse_certificate(der).ok())
            .filter(|root| root.subject().as_raw() == next_issuer)
            .count();
        if matching_roots != 1 {
            return Err(DeviceCertificateValidationError::InvalidCertificate);
        }

        let leaf_der = CertificateDer::from(issued.certificate_der.clone());
        let leaf = EndEntityCert::try_from(&leaf_der)
            .map_err(|_| DeviceCertificateValidationError::InvalidCertificate)?;
        let validation_time = UnixTime::since_unix_epoch(Duration::from_millis(now_unix_ms));
        let supported_algorithms = rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .all;
        leaf.verify_for_usage(
            supported_algorithms,
            &self.roots.roots,
            &intermediate_der,
            validation_time,
            KeyUsage::required_if_present(CLIENT_AUTH_EKU_DER),
            None,
            None,
        )
        .map_err(|_| DeviceCertificateValidationError::InvalidCertificate)?;

        Ok(())
    }
}

fn response_metadata_matches_binding(
    issued: &IssuedDeviceCertificate,
    binding: &DeviceCertificateIssuanceBinding,
) -> bool {
    let expected_key = binding.device_key();
    issued.registration_binding_id == *binding.registration_binding_id()
        && issued.device_key
            == (DeviceAuthorizationDeviceKey {
                organization_id: expected_key.organization_id.clone(),
                workspace_id: expected_key.workspace_id.clone(),
                device_id: expected_key.device_id.clone(),
            })
        && issued.authorization_generation == binding.authorization_generation()
        && issued.scope
            == (DeviceAuthorizationScope {
                organization_id: expected_key.organization_id.clone(),
                workspace_id: expected_key.workspace_id.clone(),
            })
        && issued.spki_sha256 == *binding.spki_sha256()
}

fn leaf_matches_device_profile(
    leaf: &X509Certificate<'_>,
    binding: &DeviceCertificateIssuanceBinding,
) -> bool {
    if has_duplicate_extensions(leaf)
        || leaf.extensions().iter().any(|extension| {
            matches!(
                extension.parsed_extension(),
                ParsedExtension::ParseError { .. }
            )
        })
        || leaf.subject().iter_attributes().next().is_some()
    {
        return false;
    }

    let Ok(Some(basic_constraints)) = leaf.basic_constraints() else {
        return false;
    };
    if basic_constraints.value.ca || !basic_constraints.critical {
        return false;
    }

    let Ok(Some(key_usage)) = leaf.key_usage() else {
        return false;
    };
    if !key_usage.critical || key_usage.value.flags != 1 {
        return false;
    }

    let Ok(Some(extended_key_usage)) = leaf.extended_key_usage() else {
        return false;
    };
    let usage = extended_key_usage.value;
    if extended_key_usage.critical
        || !usage.client_auth
        || usage.any
        || usage.server_auth
        || usage.code_signing
        || usage.email_protection
        || usage.time_stamping
        || usage.ocsp_signing
        || !usage.other.is_empty()
    {
        return false;
    }

    let Ok(Some(subject_alt_name)) = leaf.subject_alternative_name() else {
        return false;
    };
    if !subject_alt_name.critical || subject_alt_name.value.general_names.len() != 1 {
        return false;
    }
    let Some(x509_parser::extensions::GeneralName::URI(uri)) =
        subject_alt_name.value.general_names.first()
    else {
        return false;
    };
    if uri.len() > MAX_DEVICE_IDENTITY_URI_BYTES {
        return false;
    }

    let expected_uri = match device_identity_uri(
        binding.device_key(),
        binding.authorization_generation(),
        binding.csr_sha256(),
    ) {
        Ok(uri) => uri,
        Err(()) => return false,
    };
    *uri == expected_uri
}

fn has_duplicate_extensions(certificate: &X509Certificate<'_>) -> bool {
    let mut seen = BTreeSet::new();
    certificate
        .extensions()
        .iter()
        .any(|extension| !seen.insert(extension.oid.to_id_string()))
}

fn validate_ca_certificate(
    certificate: &X509Certificate<'_>,
    now_unix_seconds: Option<i64>,
) -> Result<(), ()> {
    if has_duplicate_extensions(certificate)
        || certificate.extensions().iter().any(|extension| {
            matches!(
                extension.parsed_extension(),
                ParsedExtension::ParseError { .. }
            )
        })
    {
        return Err(());
    }
    let basic_constraints = certificate.basic_constraints().map_err(|_| ())?.ok_or(())?;
    let key_usage = certificate.key_usage().map_err(|_| ())?.ok_or(())?;
    if !basic_constraints.value.ca
        || !basic_constraints.critical
        || !key_usage.critical
        || !key_usage.value.key_cert_sign()
    {
        return Err(());
    }
    if let Some(now) = now_unix_seconds {
        if certificate.validity().not_before.timestamp() > now
            || certificate.validity().not_after.timestamp() <= now
        {
            return Err(());
        }
    }
    Ok(())
}

/// Encode the private URI SAN that a configured issuer must derive from Directory data.
///
/// The URI scheme is deliberately project-private and is not an IANA-registered
/// protocol identifier. It is not a claim of generic certificate interoperability.
pub(crate) fn device_identity_uri(
    key: &WorkspaceDeviceKey,
    authorization_generation: u64,
    csr_sha256: &[u8; 32],
) -> Result<String, ()> {
    if key.organization_id.is_empty()
        || key.workspace_id.is_empty()
        || key.device_id.is_empty()
        || authorization_generation == 0
    {
        return Err(());
    }
    let uri = format!(
        "cyrene-device:v1:{}.{}.{}.{}.{}",
        URL_SAFE_NO_PAD.encode(key.organization_id.as_bytes()),
        URL_SAFE_NO_PAD.encode(key.workspace_id.as_bytes()),
        URL_SAFE_NO_PAD.encode(key.device_id.as_bytes()),
        authorization_generation,
        lowercase_hex(csr_sha256),
    );
    if uri.len() > MAX_DEVICE_IDENTITY_URI_BYTES {
        return Err(());
    }
    Ok(uri)
}

/// Parse only the canonical identity format emitted by [`device_identity_uri`].
#[allow(dead_code)]
fn parse_device_identity_uri(uri: &str) -> Result<(WorkspaceDeviceKey, u64, [u8; 32]), ()> {
    let rest = uri.strip_prefix("cyrene-device:v1:").ok_or(())?;
    if uri.len() > MAX_DEVICE_IDENTITY_URI_BYTES {
        return Err(());
    }
    let parts = rest.split('.').collect::<Vec<_>>();
    if parts.len() != 5 {
        return Err(());
    }
    let organization_id = decode_identity_field(parts[0])?;
    let workspace_id = decode_identity_field(parts[1])?;
    let device_id = decode_identity_field(parts[2])?;
    let generation = parts[3].parse::<u64>().map_err(|_| ())?;
    if generation == 0 || generation.to_string() != parts[3] {
        return Err(());
    }
    let csr_sha256 = parse_lowercase_sha256(parts[4])?;
    Ok((
        WorkspaceDeviceKey {
            organization_id,
            workspace_id,
            device_id,
        },
        generation,
        csr_sha256,
    ))
}

fn decode_identity_field(value: &str) -> Result<String, ()> {
    if value.is_empty() {
        return Err(());
    }
    let bytes = URL_SAFE_NO_PAD.decode(value).map_err(|_| ())?;
    if URL_SAFE_NO_PAD.encode(&bytes) != value {
        return Err(());
    }
    let decoded = String::from_utf8(bytes).map_err(|_| ())?;
    if decoded.is_empty() {
        return Err(());
    }
    Ok(decoded)
}

fn parse_lowercase_sha256(value: &str) -> Result<[u8; 32], ()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(());
    }
    let mut digest = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        digest[index] = (hex_nibble(pair[0]).ok_or(())? << 4) | hex_nibble(pair[1]).ok_or(())?;
    }
    Ok(digest)
}

fn lowercase_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[(byte >> 4) as usize]));
        output.push(char::from(HEX[(byte & 0x0f) as usize]));
    }
    output
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

fn canonical_serial(raw_der_integer: &[u8]) -> Option<Vec<u8>> {
    if raw_der_integer.is_empty() || raw_der_integer.len() > 21 {
        return None;
    }
    let serial = if raw_der_integer[0] == 0 {
        if raw_der_integer.len() < 2 || raw_der_integer[1] & 0x80 == 0 {
            return None;
        }
        &raw_der_integer[1..]
    } else {
        raw_der_integer
    };
    if serial.is_empty()
        || serial.len() > 20
        || serial[0] & 0x80 != 0
        || serial.iter().all(|byte| *byte == 0)
    {
        return None;
    }
    Some(serial.to_vec())
}

fn parse_certificate(der: &[u8]) -> Result<X509Certificate<'_>, ()> {
    let (remaining, certificate) = parse_x509_certificate(der).map_err(|_| ())?;
    if !remaining.is_empty() {
        return Err(());
    }
    Ok(certificate)
}

fn unix_seconds(unix_ms: u64) -> Option<i64> {
    i64::try_from(unix_ms / 1_000).ok()
}

fn timestamp_millis(unix_seconds: i64) -> Option<u64> {
    u64::try_from(unix_seconds).ok()?.checked_mul(1_000)
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

#[cfg(test)]
mod tests {
    use rcgen::{
        date_time_ymd, BasicConstraints, CertificateParams, CertifiedIssuer, CustomExtension,
        DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose,
    };

    use crate::device_authorization::{DeviceAuthorizationPortError, DeviceCsrValidator};
    use crate::device_certificate_authority::DeviceCertificateIssueRequest;
    use crate::durable_directory::DeviceRegistrationBinding;

    use super::*;

    const TEST_NOW_UNIX_MS: u64 = 1_789_934_400_000;

    struct TestCertificate {
        issued: IssuedDeviceCertificate,
        root_der: Vec<u8>,
        alternate_root_der: Vec<u8>,
        binding: DeviceCertificateIssuanceBinding,
        key: WorkspaceDeviceKey,
        csr_der: Vec<u8>,
        generation: u64,
    }

    struct TestRevocationChecker(Result<(), DeviceCertificateRevocationCheckError>);

    impl DeviceCertificateRevocationChecker for TestRevocationChecker {
        fn require_current_good_status(
            &self,
            query: &DeviceCertificateRevocationQuery<'_>,
        ) -> Result<(), DeviceCertificateRevocationCheckError> {
            assert!(!query.certificate_der().is_empty());
            assert!(!query.ca_chain_der().is_empty());
            assert!(!query.serial_number().is_empty());
            assert_ne!(query.certificate_sha256(), &[0; 32]);
            assert_eq!(query.checked_at_unix_ms(), TEST_NOW_UNIX_MS);
            self.0
        }
    }

    struct TestCsrValidator([u8; 32]);

    impl DeviceCsrValidator for TestCsrValidator {
        fn validate_and_hash_spki(
            &self,
            _csr_der: &[u8],
        ) -> Result<[u8; 32], DeviceAuthorizationPortError> {
            Ok(self.0)
        }
    }

    fn test_certificate(expired: bool, with_client_auth: bool) -> TestCertificate {
        let root_key = KeyPair::generate().expect("root key");
        let root_params = ca_params("Workspace device test root");
        let root =
            CertifiedIssuer::self_signed(root_params.clone(), root_key).expect("self signed root");

        let alternate_root_key = KeyPair::generate().expect("alternate root key");
        let alternate_root = CertifiedIssuer::self_signed(root_params.clone(), alternate_root_key)
            .expect("alternate self signed root");

        let intermediate_key = KeyPair::generate().expect("intermediate key");
        let intermediate_params = ca_params("Workspace device test intermediate");
        let intermediate = CertifiedIssuer::signed_by(intermediate_params, intermediate_key, &root)
            .expect("signed intermediate");

        let key = WorkspaceDeviceKey {
            organization_id: "org-α".into(),
            workspace_id: "workspace-β".into(),
            device_id: "device-🙂".into(),
        };
        let generation = 3;
        let csr_der = b"test-only CSR tuple".to_vec();
        let csr_sha256 = sha256(&csr_der);
        let uri = device_identity_uri(&key, generation, &csr_sha256).expect("profile URI");
        let leaf_key = KeyPair::generate().expect("leaf key");
        let mut leaf_params = CertificateParams::new(Vec::<String>::new()).expect("leaf params");
        leaf_params.distinguished_name = DistinguishedName::new();
        leaf_params.not_before = date_time_ymd(2020, 1, 1);
        leaf_params.not_after = if expired {
            date_time_ymd(2025, 1, 1)
        } else {
            date_time_ymd(2030, 1, 1)
        };
        leaf_params.is_ca = IsCa::ExplicitNoCa;
        leaf_params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        if with_client_auth {
            leaf_params
                .extended_key_usages
                .push(ExtendedKeyUsagePurpose::ClientAuth);
        }
        leaf_params
            .custom_extensions
            .push(critical_subject_alt_name_uri(&uri));
        let leaf = leaf_params
            .signed_by(&leaf_key, &intermediate)
            .expect("signed leaf");
        let leaf_der = leaf.der().as_ref().to_vec();
        let parsed_leaf = parse_certificate(&leaf_der).expect("parse leaf");
        let spki_sha256 = sha256(parsed_leaf.tbs_certificate.subject_pki.raw);
        let binding = binding_for(key.clone(), generation, csr_der.clone(), spki_sha256);
        let serial_number = canonical_serial(parsed_leaf.tbs_certificate.raw_serial())
            .expect("positive test serial");
        let not_after_unix_ms =
            timestamp_millis(parsed_leaf.validity().not_after.timestamp()).expect("test notAfter");
        let device_key = binding.device_key();

        let issued = IssuedDeviceCertificate {
            certificate_der: leaf_der,
            ca_chain_der: vec![intermediate.der().as_ref().to_vec()],
            serial_number,
            registration_binding_id: *binding.registration_binding_id(),
            device_key: DeviceAuthorizationDeviceKey {
                organization_id: device_key.organization_id.clone(),
                workspace_id: device_key.workspace_id.clone(),
                device_id: device_key.device_id.clone(),
            },
            authorization_generation: binding.authorization_generation(),
            scope: DeviceAuthorizationScope {
                organization_id: device_key.organization_id.clone(),
                workspace_id: device_key.workspace_id.clone(),
            },
            spki_sha256,
            not_after_unix_ms,
        };

        TestCertificate {
            issued,
            root_der: root.der().as_ref().to_vec(),
            alternate_root_der: alternate_root.der().as_ref().to_vec(),
            binding,
            key,
            csr_der,
            generation,
        }
    }

    fn ca_params(common_name: &str) -> CertificateParams {
        let mut params = CertificateParams::new(Vec::<String>::new()).expect("CA params");
        params
            .distinguished_name
            .push(DnType::CommonName, common_name);
        params.not_before = date_time_ymd(2020, 1, 1);
        params.not_after = date_time_ymd(2035, 1, 1);
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        params
    }

    fn critical_subject_alt_name_uri(uri: &str) -> CustomExtension {
        let uri_name = der_tlv(0x86, uri.as_bytes());
        let general_names = der_tlv(0x30, &uri_name);
        let mut extension = CustomExtension::from_oid_content(&[2, 5, 29, 17], general_names);
        extension.set_criticality(true);
        extension
    }

    fn der_tlv(tag: u8, value: &[u8]) -> Vec<u8> {
        let mut encoded = Vec::with_capacity(value.len() + 6);
        encoded.push(tag);
        if value.len() < 128 {
            encoded.push(value.len() as u8);
        } else {
            let bytes = value.len().to_be_bytes();
            let first = bytes
                .iter()
                .position(|byte| *byte != 0)
                .expect("nonempty DER length");
            let length_bytes = &bytes[first..];
            encoded.push(0x80 | length_bytes.len() as u8);
            encoded.extend_from_slice(length_bytes);
        }
        encoded.extend_from_slice(value);
        encoded
    }

    fn binding_for(
        key: WorkspaceDeviceKey,
        generation: u64,
        csr_der: Vec<u8>,
        spki_sha256: [u8; 32],
    ) -> DeviceCertificateIssuanceBinding {
        let directory_binding = DeviceRegistrationBinding::test_fixture(
            [9; 16],
            key,
            generation,
            sha256(&csr_der),
            spki_sha256,
        );
        DeviceCertificateIssueRequest::from_directory_binding(
            [8; 16],
            &directory_binding,
            csr_der,
            TEST_NOW_UNIX_MS,
            &TestCsrValidator(spki_sha256),
        )
        .expect("trusted test binding")
        .binding()
        .clone()
    }

    fn issued_for_binding(
        original: &IssuedDeviceCertificate,
        binding: &DeviceCertificateIssuanceBinding,
    ) -> IssuedDeviceCertificate {
        let key = binding.device_key();
        IssuedDeviceCertificate {
            certificate_der: original.certificate_der.clone(),
            ca_chain_der: original.ca_chain_der.clone(),
            serial_number: original.serial_number.clone(),
            registration_binding_id: *binding.registration_binding_id(),
            device_key: DeviceAuthorizationDeviceKey {
                organization_id: key.organization_id.clone(),
                workspace_id: key.workspace_id.clone(),
                device_id: key.device_id.clone(),
            },
            authorization_generation: binding.authorization_generation(),
            scope: DeviceAuthorizationScope {
                organization_id: key.organization_id.clone(),
                workspace_id: key.workspace_id.clone(),
            },
            spki_sha256: *binding.spki_sha256(),
            not_after_unix_ms: original.not_after_unix_ms,
        }
    }

    fn validator(root_der: Vec<u8>) -> DeviceCertificateResponseValidator {
        DeviceCertificateResponseValidator::new(vec![root_der]).expect("trusted root")
    }

    fn good_revocation() -> TestRevocationChecker {
        TestRevocationChecker(Ok(()))
    }

    #[test]
    fn valid_path_identity_spki_serial_and_expiry_produce_derived_facts() {
        let fixture = test_certificate(false, true);
        let verifier = validator(fixture.root_der.clone());
        let leaf = parse_certificate(&fixture.issued.certificate_der).expect("test leaf parses");
        assert!(response_metadata_matches_binding(
            &fixture.issued,
            &fixture.binding
        ));
        assert!(leaf_matches_device_profile(&leaf, &fixture.binding));
        assert_eq!(
            canonical_serial(leaf.tbs_certificate.raw_serial()),
            Some(fixture.issued.serial_number.clone())
        );
        assert_eq!(
            timestamp_millis(leaf.validity().not_after.timestamp()),
            Some(fixture.issued.not_after_unix_ms)
        );
        verifier
            .verify_rfc5280_path(
                &fixture.issued,
                TEST_NOW_UNIX_MS,
                unix_seconds(TEST_NOW_UNIX_MS).unwrap(),
            )
            .expect("test path validates");
        let validated = verifier
            .validate_issued_certificate(
                &fixture.issued,
                &fixture.binding,
                TEST_NOW_UNIX_MS,
                &good_revocation(),
            )
            .expect("valid certificate response");

        assert_eq!(
            validated.certificate_sha256(),
            &sha256(&fixture.issued.certificate_der)
        );
        assert_eq!(validated.serial_number(), fixture.issued.serial_number);
        assert_eq!(
            validated.not_after_unix_ms(),
            fixture.issued.not_after_unix_ms
        );
    }

    #[test]
    fn no_root_or_unrelated_chain_fails_closed() {
        assert!(matches!(
            DeviceCertificateResponseValidator::new(Vec::new()),
            Err(DeviceCertificateValidationConfigError::InvalidTrustRoots)
        ));

        let fixture = test_certificate(false, true);
        assert_eq!(
            validator(fixture.alternate_root_der.clone()).validate_issued_certificate(
                &fixture.issued,
                &fixture.binding,
                TEST_NOW_UNIX_MS,
                &good_revocation(),
            ),
            Err(DeviceCertificateValidationError::InvalidCertificate)
        );
    }

    #[test]
    fn certificate_identity_must_match_each_directory_binding_component() {
        let fixture = test_certificate(false, true);
        let mut alternates = Vec::new();

        for field in 0..5 {
            let mut key = fixture.key.clone();
            let mut generation = fixture.generation;
            let mut csr_der = fixture.csr_der.clone();
            let spki = *fixture.binding.spki_sha256();
            match field {
                0 => key.organization_id.push_str("-other"),
                1 => key.workspace_id.push_str("-other"),
                2 => key.device_id.push_str("-other"),
                3 => generation += 1,
                4 => csr_der.push(0x01),
                _ => unreachable!(),
            }
            alternates.push(binding_for(key, generation, csr_der, spki));
        }
        let mut wrong_spki = *fixture.binding.spki_sha256();
        wrong_spki[0] ^= 0xff;
        alternates.push(binding_for(
            fixture.key.clone(),
            fixture.generation,
            fixture.csr_der.clone(),
            wrong_spki,
        ));

        for binding in alternates {
            let issued = issued_for_binding(&fixture.issued, &binding);
            assert_eq!(
                validator(fixture.root_der.clone()).validate_issued_certificate(
                    &issued,
                    &binding,
                    TEST_NOW_UNIX_MS,
                    &good_revocation(),
                ),
                Err(DeviceCertificateValidationError::InvalidCertificate)
            );
        }
    }

    #[test]
    fn metadata_expiry_and_der_sizes_must_match_the_signed_leaf() {
        let fixture = test_certificate(false, true);
        let verifier = validator(fixture.root_der.clone());

        let mut wrong_serial = fixture.issued.clone();
        wrong_serial.serial_number[0] ^= 0x01;
        assert_eq!(
            verifier.validate_issued_certificate(
                &wrong_serial,
                &fixture.binding,
                TEST_NOW_UNIX_MS,
                &good_revocation(),
            ),
            Err(DeviceCertificateValidationError::InvalidCertificate)
        );

        let mut wrong_expiry = fixture.issued.clone();
        wrong_expiry.not_after_unix_ms += 1_000;
        assert_eq!(
            verifier.validate_issued_certificate(
                &wrong_expiry,
                &fixture.binding,
                TEST_NOW_UNIX_MS,
                &good_revocation(),
            ),
            Err(DeviceCertificateValidationError::InvalidCertificate)
        );

        let mut oversized_leaf = fixture.issued.clone();
        oversized_leaf
            .certificate_der
            .resize(MAX_CERTIFICATE_DER_BYTES + 1, 0);
        assert_eq!(
            verifier.validate_issued_certificate(
                &oversized_leaf,
                &fixture.binding,
                TEST_NOW_UNIX_MS,
                &good_revocation(),
            ),
            Err(DeviceCertificateValidationError::InvalidCertificate)
        );

        let mut oversized_chain = fixture.issued.clone();
        oversized_chain
            .ca_chain_der
            .push(vec![0; MAX_CHAIN_DER_BYTES + 1]);
        assert_eq!(
            verifier.validate_issued_certificate(
                &oversized_chain,
                &fixture.binding,
                TEST_NOW_UNIX_MS,
                &good_revocation(),
            ),
            Err(DeviceCertificateValidationError::InvalidCertificate)
        );
    }

    #[test]
    fn expired_or_non_client_auth_leaf_is_rejected() {
        let expired = test_certificate(true, true);
        assert_eq!(
            validator(expired.root_der.clone()).validate_issued_certificate(
                &expired.issued,
                &expired.binding,
                TEST_NOW_UNIX_MS,
                &good_revocation(),
            ),
            Err(DeviceCertificateValidationError::InvalidCertificate)
        );

        let no_client_auth = test_certificate(false, false);
        assert_eq!(
            validator(no_client_auth.root_der.clone()).validate_issued_certificate(
                &no_client_auth.issued,
                &no_client_auth.binding,
                TEST_NOW_UNIX_MS,
                &good_revocation(),
            ),
            Err(DeviceCertificateValidationError::InvalidCertificate)
        );
    }

    #[test]
    fn revoked_and_unknown_status_never_accept_the_certificate() {
        let fixture = test_certificate(false, true);
        let validator = validator(fixture.root_der.clone());

        assert_eq!(
            validator.validate_issued_certificate(
                &fixture.issued,
                &fixture.binding,
                TEST_NOW_UNIX_MS,
                &TestRevocationChecker(Err(DeviceCertificateRevocationCheckError::Revoked)),
            ),
            Err(DeviceCertificateValidationError::Revoked)
        );
        assert_eq!(
            validator.validate_issued_certificate(
                &fixture.issued,
                &fixture.binding,
                TEST_NOW_UNIX_MS,
                &TestRevocationChecker(Err(DeviceCertificateRevocationCheckError::Unknown)),
            ),
            Err(DeviceCertificateValidationError::RevocationStatusUnknown)
        );
    }

    #[test]
    fn identity_uri_is_canonical_and_round_trips_exact_utf8_bytes() {
        let fixture = test_certificate(false, true);
        let uri = device_identity_uri(
            &fixture.key,
            fixture.generation,
            fixture.binding.csr_sha256(),
        )
        .unwrap();
        assert_eq!(
            parse_device_identity_uri(&uri),
            Ok((
                fixture.key,
                fixture.generation,
                *fixture.binding.csr_sha256()
            ))
        );
        assert!(parse_device_identity_uri(&uri.replace(".3.", ".03.")).is_err());
        assert!(parse_device_identity_uri(&uri.to_uppercase()).is_err());
    }
}
