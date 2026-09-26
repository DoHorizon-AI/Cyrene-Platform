//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 aca_forwarded_certificate.rs                                   │
//! │  Module: cy_workspace_fabric::aca_forwarded_certificate             │
//! │  Role: Validate device certificates forwarded by ACA Envoy.         │
//! │                                                                     │
//! │  模块职责：校验 ACA Envoy 转发的设备客户端证书。                        │
//! └─────────────────────────────────────────────────────────────────────┘
//!
//! XFCC is an identity source only when this adapter is explicitly selected by
//! a server running behind ACA HTTP/2 ingress. ACA overwrites a client-sent
//! XFCC header, but the application still validates the configured CA path,
//! certificate time, client-auth EKU, and registered device scope.

use std::collections::BTreeSet;
use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cy_proto::workspace_v1::RelayHello;
use rustls::pki_types::{CertificateDer, UnixTime};
use rustls::RootCertStore;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tonic::Request;
use webpki::{EndEntityCert, KeyUsage};
use x509_parser::parse_x509_certificate;
use x509_parser::pem::parse_x509_pem;

use crate::auth::RelaySessionClaims;
use crate::device_auth::{
    RegistryWorkspaceDeviceVerifier, VerifiedClientCertificate, WorkspaceDeviceAuthenticationError,
};

const XFCC_HEADER: &str = "x-forwarded-client-cert";
const MAX_XFCC_HEADER_BYTES: usize = 64 * 1024;
const MAX_TRUST_ROOT_BUNDLE_BYTES: usize = 1024 * 1024;
const MAX_TRUST_ROOTS: usize = 32;
const MAX_CHAIN_CERTIFICATES: usize = 8;
const MAX_CERTIFICATE_DER_BYTES: usize = 32 * 1024;
const CLIENT_AUTH_EKU_DER: &[u8] = &[0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x02];

/// Invalid private CA configuration supplied when explicitly enabling ACA ingress.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum AcaForwardedCertificateConfigError {
    /// The configured root bundle is empty, malformed, out of policy, or not a current CA.
    #[error("WORKSPACE_DEVICE_CERTIFICATE_TRUST_CONFIG_INVALID")]
    InvalidTrustRoots,
}

/// ACA HTTP/2 ingress certificate adapter with an explicit, private client-CA trust bundle.
///
/// Construct this at server startup only for a Relay behind ACA ingress configured with
/// `clientCertificateMode=require`. It must not be installed for ordinary HTTP ingress.
pub struct AcaForwardedCertificateAdapter {
    roots: RootCertStore,
    root_fingerprints: BTreeSet<[u8; 32]>,
}

impl AcaForwardedCertificateAdapter {
    /// Build an ACA-only verifier from a bounded PEM bundle of trusted client CA certificates.
    ///
    /// Every configured trust root must be current, marked as a CA, and permit certificate
    /// signing. The bundle is private server configuration; request callers cannot supply roots.
    pub fn new(
        trusted_client_ca_bundle_pem: &[u8],
    ) -> Result<Self, AcaForwardedCertificateConfigError> {
        if trusted_client_ca_bundle_pem.is_empty()
            || trusted_client_ca_bundle_pem.len() > MAX_TRUST_ROOT_BUNDLE_BYTES
        {
            return Err(AcaForwardedCertificateConfigError::InvalidTrustRoots);
        }

        let now_seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| AcaForwardedCertificateConfigError::InvalidTrustRoots)?
            .as_secs();
        let now_seconds = i64::try_from(now_seconds)
            .map_err(|_| AcaForwardedCertificateConfigError::InvalidTrustRoots)?;
        let root_der = parse_pem_certificate_bundle(
            trusted_client_ca_bundle_pem,
            MAX_TRUST_ROOTS,
            MAX_TRUST_ROOT_BUNDLE_BYTES,
        )
        .map_err(|_| AcaForwardedCertificateConfigError::InvalidTrustRoots)?;
        if root_der.is_empty() || root_der.len() > MAX_TRUST_ROOTS {
            return Err(AcaForwardedCertificateConfigError::InvalidTrustRoots);
        }

        let mut roots = RootCertStore::empty();
        let mut root_fingerprints = BTreeSet::new();
        for der in root_der {
            validate_ca_certificate(&der, now_seconds)
                .map_err(|_| AcaForwardedCertificateConfigError::InvalidTrustRoots)?;
            let fingerprint = sha256_digest(&der);
            if !root_fingerprints.insert(fingerprint) {
                return Err(AcaForwardedCertificateConfigError::InvalidTrustRoots);
            }
            roots
                .add(CertificateDer::from(der))
                .map_err(|_| AcaForwardedCertificateConfigError::InvalidTrustRoots)?;
        }

        if roots.is_empty() {
            return Err(AcaForwardedCertificateConfigError::InvalidTrustRoots);
        }

        Ok(Self {
            roots,
            root_fingerprints,
        })
    }

    /// Validate one ACA-overwritten XFCC certificate, then apply the existing device registry.
    ///
    /// This preserves the canonical fingerprint approval, revocation, organization, Workspace,
    /// and device checks. The caller must select this method only for the explicitly trusted ACA
    /// ingress mode; ordinary ingress continues to use Tonic's TLS peer-certificate path.
    pub fn authenticate_request<T>(
        &self,
        request: &Request<T>,
        hello: &RelayHello,
        registry_verifier: &RegistryWorkspaceDeviceVerifier,
        now_unix_ms: u64,
    ) -> Result<RelaySessionClaims, WorkspaceDeviceAuthenticationError> {
        let certificate = self.verified_certificate_from_request(request, now_unix_ms)?;
        registry_verifier.authenticate(hello, &certificate, now_unix_ms)
    }

    fn verified_certificate_from_request<T>(
        &self,
        request: &Request<T>,
        now_unix_ms: u64,
    ) -> Result<VerifiedClientCertificate, WorkspaceDeviceAuthenticationError> {
        let header = single_xfcc_header(request)?;
        let forwarded = parse_xfcc(header)?;
        if !hex_digest_matches(&forwarded.declared_hash, &forwarded.leaf_der) {
            return Err(WorkspaceDeviceAuthenticationError::InvalidClientCertificate);
        }

        let mut intermediate_der = Vec::with_capacity(forwarded.intermediate_der.len());
        let mut seen = BTreeSet::new();
        for der in forwarded.intermediate_der {
            let fingerprint = sha256_digest(&der);
            if fingerprint == sha256_digest(&forwarded.leaf_der)
                || self.root_fingerprints.contains(&fingerprint)
                || !seen.insert(fingerprint)
            {
                return Err(WorkspaceDeviceAuthenticationError::InvalidClientCertificate);
            }
            validate_ca_certificate(&der, unix_seconds(now_unix_ms)?)
                .map_err(|_| WorkspaceDeviceAuthenticationError::InvalidClientCertificate)?;
            intermediate_der.push(CertificateDer::from(der));
        }

        let leaf_der = CertificateDer::from(forwarded.leaf_der);
        if !has_explicit_client_auth_eku(leaf_der.as_ref()) {
            return Err(WorkspaceDeviceAuthenticationError::InvalidClientCertificate);
        }
        let leaf = EndEntityCert::try_from(&leaf_der)
            .map_err(|_| WorkspaceDeviceAuthenticationError::InvalidClientCertificate)?;
        let validation_time = UnixTime::since_unix_epoch(Duration::from_millis(now_unix_ms));
        let supported_algorithms = rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .all;
        leaf.verify_for_usage(
            supported_algorithms,
            &self.roots.roots,
            &intermediate_der,
            validation_time,
            // WebPKI applies the validator to every certificate in the chain. Require the EKU
            // on the end entity explicitly below; here, preserve RFC 5280 path constraints for
            // intermediates that carry their own EKU restriction.
            KeyUsage::required_if_present(CLIENT_AUTH_EKU_DER),
            None,
            None,
        )
        .map_err(|_| WorkspaceDeviceAuthenticationError::InvalidClientCertificate)?;

        VerifiedClientCertificate::from_validated_der(leaf_der.as_ref())
    }
}

impl fmt::Debug for AcaForwardedCertificateAdapter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AcaForwardedCertificateAdapter")
            .field("trusted_root_count", &self.roots.len())
            .finish()
    }
}

struct ForwardedCertificate {
    declared_hash: String,
    leaf_der: Vec<u8>,
    intermediate_der: Vec<Vec<u8>>,
}

fn single_xfcc_header<T>(request: &Request<T>) -> Result<&str, WorkspaceDeviceAuthenticationError> {
    let mut values = request.metadata().get_all(XFCC_HEADER).iter();
    let value = values
        .next()
        .ok_or(WorkspaceDeviceAuthenticationError::MissingClientCertificate)?;
    if values.next().is_some() {
        return Err(WorkspaceDeviceAuthenticationError::InvalidClientCertificate);
    }
    let value = value
        .to_str()
        .map_err(|_| WorkspaceDeviceAuthenticationError::InvalidClientCertificate)?;
    if value.is_empty()
        || value.len() > MAX_XFCC_HEADER_BYTES
        || !value.is_ascii()
        || value.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(WorkspaceDeviceAuthenticationError::InvalidClientCertificate);
    }
    Ok(value)
}

fn parse_xfcc(value: &str) -> Result<ForwardedCertificate, WorkspaceDeviceAuthenticationError> {
    let fields = parse_xfcc_fields(value)?;
    let declared_hash = fields
        .get("hash")
        .cloned()
        .ok_or(WorkspaceDeviceAuthenticationError::InvalidClientCertificate)?;
    if declared_hash.len() != 64 || !declared_hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(WorkspaceDeviceAuthenticationError::InvalidClientCertificate);
    }

    let cert_value = fields
        .get("cert")
        .ok_or(WorkspaceDeviceAuthenticationError::MissingClientCertificate)?;
    let leaf_der = parse_single_forwarded_pem(cert_value)?;
    let intermediate_der = match fields.get("chain") {
        Some(chain) => parse_forwarded_pem_bundle(chain, MAX_CHAIN_CERTIFICATES)?,
        None => Vec::new(),
    };
    if intermediate_der.len() > MAX_CHAIN_CERTIFICATES {
        return Err(WorkspaceDeviceAuthenticationError::InvalidClientCertificate);
    }

    Ok(ForwardedCertificate {
        declared_hash,
        leaf_der,
        intermediate_der,
    })
}

fn parse_xfcc_fields(
    value: &str,
) -> Result<std::collections::BTreeMap<String, String>, WorkspaceDeviceAuthenticationError> {
    let mut fields = std::collections::BTreeMap::new();
    let mut start = 0;
    let mut quoted = false;
    let bytes = value.as_bytes();
    for (index, byte) in bytes.iter().copied().enumerate() {
        match byte {
            b'\\' => return Err(WorkspaceDeviceAuthenticationError::InvalidClientCertificate),
            b'"' => quoted = !quoted,
            b',' if !quoted => {
                return Err(WorkspaceDeviceAuthenticationError::InvalidClientCertificate);
            }
            b';' if !quoted => {
                insert_xfcc_field(&mut fields, &value[start..index])?;
                start = index + 1;
            }
            _ => {}
        }
    }
    if quoted {
        return Err(WorkspaceDeviceAuthenticationError::InvalidClientCertificate);
    }
    insert_xfcc_field(&mut fields, &value[start..])?;
    if !fields.contains_key("hash") || !fields.contains_key("cert") {
        return Err(WorkspaceDeviceAuthenticationError::MissingClientCertificate);
    }
    Ok(fields)
}

fn insert_xfcc_field(
    fields: &mut std::collections::BTreeMap<String, String>,
    segment: &str,
) -> Result<(), WorkspaceDeviceAuthenticationError> {
    let (key, raw_value) = segment
        .split_once('=')
        .ok_or(WorkspaceDeviceAuthenticationError::InvalidClientCertificate)?;
    let key = key.trim().to_ascii_lowercase();
    if !matches!(key.as_str(), "hash" | "cert" | "chain") {
        return Err(WorkspaceDeviceAuthenticationError::InvalidClientCertificate);
    }

    let raw_value = raw_value.trim();
    let value = if key == "hash" {
        if raw_value.starts_with('"') || raw_value.ends_with('"') {
            return Err(WorkspaceDeviceAuthenticationError::InvalidClientCertificate);
        }
        raw_value.to_owned()
    } else {
        if raw_value.len() < 2 || !raw_value.starts_with('"') || !raw_value.ends_with('"') {
            return Err(WorkspaceDeviceAuthenticationError::InvalidClientCertificate);
        }
        let inner = &raw_value[1..raw_value.len() - 1];
        if inner.contains('"') || inner.is_empty() {
            return Err(WorkspaceDeviceAuthenticationError::InvalidClientCertificate);
        }
        inner.to_owned()
    };

    if fields.insert(key, value).is_some() {
        return Err(WorkspaceDeviceAuthenticationError::InvalidClientCertificate);
    }
    Ok(())
}

fn parse_single_forwarded_pem(
    encoded_pem: &str,
) -> Result<Vec<u8>, WorkspaceDeviceAuthenticationError> {
    let decoded = decode_xfcc_value(encoded_pem)?;
    let mut certificates = parse_pem_certificate_bundle(&decoded, 1, MAX_CERTIFICATE_DER_BYTES)
        .map_err(|_| WorkspaceDeviceAuthenticationError::InvalidClientCertificate)?;
    if certificates.len() != 1 {
        return Err(WorkspaceDeviceAuthenticationError::InvalidClientCertificate);
    }
    Ok(certificates.remove(0))
}

fn parse_forwarded_pem_bundle(
    encoded_pem: &str,
    max_certificates: usize,
) -> Result<Vec<Vec<u8>>, WorkspaceDeviceAuthenticationError> {
    let decoded = decode_xfcc_value(encoded_pem)?;
    let certificates =
        parse_pem_certificate_bundle(&decoded, max_certificates, MAX_XFCC_HEADER_BYTES)
            .map_err(|_| WorkspaceDeviceAuthenticationError::InvalidClientCertificate)?;
    if certificates.is_empty() {
        return Err(WorkspaceDeviceAuthenticationError::InvalidClientCertificate);
    }
    Ok(certificates)
}

fn decode_xfcc_value(value: &str) -> Result<Vec<u8>, WorkspaceDeviceAuthenticationError> {
    let input = value.as_bytes();
    let mut decoded = Vec::with_capacity(input.len());
    let mut index = 0;
    while index < input.len() {
        let byte = if input[index] == b'%' {
            if index + 2 >= input.len() {
                return Err(WorkspaceDeviceAuthenticationError::InvalidClientCertificate);
            }
            let high = hex_nibble(input[index + 1])
                .ok_or(WorkspaceDeviceAuthenticationError::InvalidClientCertificate)?;
            let low = hex_nibble(input[index + 2])
                .ok_or(WorkspaceDeviceAuthenticationError::InvalidClientCertificate)?;
            index += 3;
            (high << 4) | low
        } else {
            let byte = input[index];
            if !byte.is_ascii() {
                return Err(WorkspaceDeviceAuthenticationError::InvalidClientCertificate);
            }
            index += 1;
            byte
        };
        if decoded.len() >= MAX_XFCC_HEADER_BYTES {
            return Err(WorkspaceDeviceAuthenticationError::InvalidClientCertificate);
        }
        decoded.push(byte);
    }
    Ok(decoded)
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn parse_pem_certificate_bundle(
    input: &[u8],
    max_certificates: usize,
    max_total_bytes: usize,
) -> Result<Vec<Vec<u8>>, ()> {
    if input.is_empty() || input.len() > max_total_bytes || !input.is_ascii() {
        return Err(());
    }

    let mut remaining = input;
    let mut certificates = Vec::new();
    let mut total_der_bytes = 0_usize;
    loop {
        remaining = trim_ascii_start(remaining);
        if remaining.is_empty() {
            break;
        }
        if certificates.len() >= max_certificates {
            return Err(());
        }
        let previous_len = remaining.len();
        let (next, pem) = parse_x509_pem(remaining).map_err(|_| ())?;
        if next.len() >= previous_len || pem.label != "CERTIFICATE" {
            return Err(());
        }
        if pem.contents.is_empty() || pem.contents.len() > MAX_CERTIFICATE_DER_BYTES {
            return Err(());
        }
        let (der_remaining, _) = parse_x509_certificate(&pem.contents).map_err(|_| ())?;
        if !der_remaining.is_empty() {
            return Err(());
        }
        total_der_bytes = total_der_bytes.checked_add(pem.contents.len()).ok_or(())?;
        if total_der_bytes > max_total_bytes {
            return Err(());
        }
        certificates.push(pem.contents);
        remaining = next;
    }
    Ok(certificates)
}

fn trim_ascii_start(input: &[u8]) -> &[u8] {
    let leading_whitespace = input
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(input.len());
    &input[leading_whitespace..]
}

fn validate_ca_certificate(der: &[u8], now_unix_seconds: i64) -> Result<(), ()> {
    let (remaining, certificate) = parse_x509_certificate(der).map_err(|_| ())?;
    if !remaining.is_empty()
        || !certificate
            .basic_constraints()
            .map_err(|_| ())?
            .is_some_and(|constraints| constraints.value.ca)
        || !certificate
            .key_usage()
            .map_err(|_| ())?
            .is_some_and(|key_usage| key_usage.value.key_cert_sign())
        || certificate.validity().not_before.timestamp() > now_unix_seconds
        || certificate.validity().not_after.timestamp() <= now_unix_seconds
    {
        return Err(());
    }
    Ok(())
}

fn has_explicit_client_auth_eku(der: &[u8]) -> bool {
    let Ok((remaining, certificate)) = parse_x509_certificate(der) else {
        return false;
    };
    remaining.is_empty()
        && certificate
            .extended_key_usage()
            .ok()
            .flatten()
            .is_some_and(|usage| usage.value.client_auth)
}

fn unix_seconds(unix_ms: u64) -> Result<i64, WorkspaceDeviceAuthenticationError> {
    i64::try_from(unix_ms / 1_000)
        .map_err(|_| WorkspaceDeviceAuthenticationError::InvalidClientCertificate)
}

fn sha256_digest(der: &[u8]) -> [u8; 32] {
    Sha256::digest(der).into()
}

fn hex_digest_matches(expected: &str, der: &[u8]) -> bool {
    let actual = sha256_digest(der);
    if expected.len() != actual.len() * 2 {
        return false;
    }
    expected
        .as_bytes()
        .chunks_exact(2)
        .zip(actual)
        .all(|(pair, byte)| {
            let Some(high) = hex_nibble(pair[0]) else {
                return false;
            };
            let Some(low) = hex_nibble(pair[1]) else {
                return false;
            };
            (high << 4) | low == byte
        })
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use cy_proto::workspace_v1::{DeviceEnrollmentRef, RelayParticipantRole};
    use rcgen::{
        date_time_ymd, BasicConstraints, CertificateParams, CertifiedIssuer, DistinguishedName,
        DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose,
    };

    use crate::device_auth::WorkspaceDeviceAuthenticationError;
    use crate::device_registry::{
        ApprovedWorkspaceDeviceCertificate, DeviceAuthorizationStatus, WorkspaceDeviceKey,
        WorkspaceDeviceRecord, WorkspaceDeviceRegistry,
    };
    use crate::directory::WorkspaceDirectoryError;

    use super::*;

    const TEST_NOW_UNIX_MS: u64 = 1_789_934_400_000;

    struct TestCertificateChain {
        root_pem: String,
        leaf_pem: String,
        intermediate_pem: String,
        leaf_der: Vec<u8>,
    }

    fn test_certificate_chain(with_client_auth: bool) -> TestCertificateChain {
        test_certificate_chain_with_leaf_validity(with_client_auth, false)
    }

    fn test_certificate_chain_with_leaf_validity(
        with_client_auth: bool,
        expired: bool,
    ) -> TestCertificateChain {
        let root_key = KeyPair::generate().unwrap();
        let mut root_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        root_params.distinguished_name = DistinguishedName::new();
        root_params
            .distinguished_name
            .push(DnType::CommonName, "Test Workspace Device Root");
        root_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        root_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let root = CertifiedIssuer::self_signed(root_params, root_key).unwrap();

        let intermediate_key = KeyPair::generate().unwrap();
        let mut intermediate_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        intermediate_params.distinguished_name = DistinguishedName::new();
        intermediate_params
            .distinguished_name
            .push(DnType::CommonName, "Test Workspace Device Intermediate");
        intermediate_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        intermediate_params.key_usages =
            vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let intermediate =
            CertifiedIssuer::signed_by(intermediate_params, intermediate_key, &root).unwrap();

        let leaf_key = KeyPair::generate().unwrap();
        let mut leaf_params = CertificateParams::new(vec!["device.test".to_string()]).unwrap();
        if expired {
            leaf_params.not_before = date_time_ymd(2020, 1, 1);
            leaf_params.not_after = date_time_ymd(2025, 1, 1);
        } else {
            leaf_params.not_before = date_time_ymd(2020, 1, 1);
            leaf_params.not_after = date_time_ymd(2030, 1, 1);
        }
        leaf_params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        if with_client_auth {
            leaf_params
                .extended_key_usages
                .push(ExtendedKeyUsagePurpose::ClientAuth);
        }
        let leaf = leaf_params.signed_by(&leaf_key, &intermediate).unwrap();

        TestCertificateChain {
            root_pem: root.pem(),
            leaf_pem: leaf.pem(),
            intermediate_pem: intermediate.pem(),
            leaf_der: leaf.der().as_ref().to_vec(),
        }
    }

    fn encode_xfcc_pem(value: &str) -> String {
        let mut encoded = String::with_capacity(value.len());
        for byte in value.bytes() {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
                encoded.push(char::from(byte));
            } else {
                encoded.push('%');
                encoded.push(char::from(hex_char(byte >> 4)));
                encoded.push(char::from(hex_char(byte & 0x0f)));
            }
        }
        encoded
    }

    fn hex_char(value: u8) -> u8 {
        match value {
            0..=9 => b'0' + value,
            _ => b'A' + value - 10,
        }
    }

    fn xfcc_header(chain: &TestCertificateChain) -> String {
        let hash = format!("{:x}", Sha256::digest(&chain.leaf_der));
        format!(
            "Hash={hash};Cert=\"{}\";Chain=\"{}\"",
            encode_xfcc_pem(&chain.leaf_pem),
            encode_xfcc_pem(&chain.intermediate_pem),
        )
    }

    fn request_with_xfcc(header: &str) -> Request<()> {
        let mut request = Request::new(());
        request
            .metadata_mut()
            .insert(XFCC_HEADER, header.parse().unwrap());
        request
    }

    fn adapter(chain: &TestCertificateChain) -> AcaForwardedCertificateAdapter {
        AcaForwardedCertificateAdapter::new(chain.root_pem.as_bytes()).unwrap()
    }

    fn hello() -> RelayHello {
        RelayHello {
            role: RelayParticipantRole::WorkspaceConnector as i32,
            session_credential: String::new(),
            user: None,
            organization_id: "organization-1".to_string(),
            workspace_id: "workspace-1".to_string(),
            device: Some(DeviceEnrollmentRef {
                device_id: "device-1".to_string(),
                workspace_id: "workspace-1".to_string(),
                enrollment_state: "approved".to_string(),
            }),
        }
    }

    #[derive(Default)]
    struct TestDeviceRegistry {
        record: Mutex<Option<WorkspaceDeviceRecord>>,
    }

    impl TestDeviceRegistry {
        fn approved(fingerprint: &str) -> Self {
            Self {
                record: Mutex::new(Some(WorkspaceDeviceRecord {
                    key: WorkspaceDeviceKey {
                        organization_id: "organization-1".to_string(),
                        workspace_id: "workspace-1".to_string(),
                        device_id: "device-1".to_string(),
                    },
                    certificate_fingerprint_sha256: fingerprint.to_string(),
                    authorization_status: DeviceAuthorizationStatus::Approved,
                })),
            }
        }

        fn with_status(fingerprint: &str, status: DeviceAuthorizationStatus) -> Self {
            let registry = Self::approved(fingerprint);
            registry
                .record
                .lock()
                .unwrap()
                .as_mut()
                .unwrap()
                .authorization_status = status;
            registry
        }
    }

    impl WorkspaceDeviceRegistry for TestDeviceRegistry {
        fn import_approved_device_certificate(
            &self,
            _certificate: ApprovedWorkspaceDeviceCertificate,
        ) -> Result<WorkspaceDeviceRecord, WorkspaceDirectoryError> {
            Err(WorkspaceDirectoryError::Storage(
                "unused test method".into(),
            ))
        }

        fn revoke_device(
            &self,
            _key: &WorkspaceDeviceKey,
        ) -> Result<WorkspaceDeviceRecord, WorkspaceDirectoryError> {
            Err(WorkspaceDirectoryError::Storage(
                "unused test method".into(),
            ))
        }

        fn find_device(
            &self,
            _key: &WorkspaceDeviceKey,
        ) -> Result<Option<WorkspaceDeviceRecord>, WorkspaceDirectoryError> {
            Ok(None)
        }

        fn find_device_by_certificate_fingerprint(
            &self,
            fingerprint: &str,
        ) -> Result<Option<WorkspaceDeviceRecord>, WorkspaceDirectoryError> {
            Ok(self
                .record
                .lock()
                .map_err(|_| WorkspaceDirectoryError::Storage("test registry poisoned".into()))?
                .as_ref()
                .filter(|record| record.certificate_fingerprint_sha256 == fingerprint)
                .cloned())
        }
    }

    #[test]
    fn trusted_aca_request_validates_chain_hash_and_reuses_device_scope() {
        let chain = test_certificate_chain(true);
        let fingerprint = format!("{:x}", Sha256::digest(&chain.leaf_der));
        let header = xfcc_header(&chain);
        let parsed = parse_xfcc(&header).unwrap();
        assert_eq!(parsed.leaf_der, chain.leaf_der);
        assert!(hex_digest_matches(&parsed.declared_hash, &parsed.leaf_der));
        assert!(parsed
            .intermediate_der
            .iter()
            .all(
                |der| validate_ca_certificate(der, unix_seconds(TEST_NOW_UNIX_MS).unwrap()).is_ok()
            ));
        let request = request_with_xfcc(&header);
        let registry = RegistryWorkspaceDeviceVerifier::new(Arc::new(
            TestDeviceRegistry::approved(&fingerprint),
        ));

        let claims = adapter(&chain)
            .authenticate_request(&request, &hello(), &registry, TEST_NOW_UNIX_MS)
            .unwrap();

        assert_eq!(claims.organization_id, "organization-1");
        assert_eq!(claims.workspace_id, "workspace-1");
        assert_eq!(
            claims.principal,
            crate::SessionPrincipal::WorkspaceDevice {
                workspace_id: "workspace-1".into(),
                device_id: "device-1".into(),
            }
        );
    }

    #[test]
    fn trusted_aca_request_rejects_registry_revocation_and_scope_mismatch() {
        let chain = test_certificate_chain(true);
        let fingerprint = format!("{:x}", Sha256::digest(&chain.leaf_der));
        let request = request_with_xfcc(&xfcc_header(&chain));
        let revoked = RegistryWorkspaceDeviceVerifier::new(Arc::new(
            TestDeviceRegistry::with_status(&fingerprint, DeviceAuthorizationStatus::Revoked),
        ));
        assert_eq!(
            adapter(&chain).authenticate_request(&request, &hello(), &revoked, TEST_NOW_UNIX_MS),
            Err(WorkspaceDeviceAuthenticationError::RevokedCertificate)
        );

        let approved = RegistryWorkspaceDeviceVerifier::new(Arc::new(
            TestDeviceRegistry::approved(&fingerprint),
        ));
        let mut mismatched_hello = hello();
        mismatched_hello.workspace_id = "workspace-other".to_string();
        assert_eq!(
            adapter(&chain).authenticate_request(
                &request,
                &mismatched_hello,
                &approved,
                TEST_NOW_UNIX_MS
            ),
            Err(WorkspaceDeviceAuthenticationError::IdentityMismatch)
        );
    }

    #[test]
    fn invalid_chain_or_missing_client_auth_eku_is_rejected() {
        let chain = test_certificate_chain(true);
        let unrelated_root = test_certificate_chain(true);
        let request = request_with_xfcc(&xfcc_header(&chain));
        let wrong_roots =
            AcaForwardedCertificateAdapter::new(unrelated_root.root_pem.as_bytes()).unwrap();
        let registry =
            RegistryWorkspaceDeviceVerifier::new(Arc::new(TestDeviceRegistry::approved("unused")));
        assert_eq!(
            wrong_roots.authenticate_request(&request, &hello(), &registry, TEST_NOW_UNIX_MS),
            Err(WorkspaceDeviceAuthenticationError::InvalidClientCertificate)
        );

        let no_eku_chain = test_certificate_chain(false);
        let no_eku_request = request_with_xfcc(&xfcc_header(&no_eku_chain));
        assert_eq!(
            adapter(&no_eku_chain).authenticate_request(
                &no_eku_request,
                &hello(),
                &registry,
                TEST_NOW_UNIX_MS
            ),
            Err(WorkspaceDeviceAuthenticationError::InvalidClientCertificate)
        );

        let expired_chain = test_certificate_chain_with_leaf_validity(true, true);
        let expired_request = request_with_xfcc(&xfcc_header(&expired_chain));
        assert_eq!(
            adapter(&expired_chain).authenticate_request(
                &expired_request,
                &hello(),
                &registry,
                TEST_NOW_UNIX_MS
            ),
            Err(WorkspaceDeviceAuthenticationError::InvalidClientCertificate)
        );
    }

    #[test]
    fn missing_duplicate_multielement_and_multiple_leaf_xfcc_fail_closed() {
        let chain = test_certificate_chain(true);
        let adapter = adapter(&chain);
        let registry =
            RegistryWorkspaceDeviceVerifier::new(Arc::new(TestDeviceRegistry::approved("unused")));
        let no_header = Request::new(());
        assert_eq!(
            adapter.authenticate_request(&no_header, &hello(), &registry, TEST_NOW_UNIX_MS),
            Err(WorkspaceDeviceAuthenticationError::MissingClientCertificate)
        );

        let mut duplicate_header = Request::new(());
        duplicate_header
            .metadata_mut()
            .append(XFCC_HEADER, xfcc_header(&chain).parse().unwrap());
        duplicate_header
            .metadata_mut()
            .append(XFCC_HEADER, xfcc_header(&chain).parse().unwrap());
        assert_eq!(
            adapter.authenticate_request(&duplicate_header, &hello(), &registry, TEST_NOW_UNIX_MS),
            Err(WorkspaceDeviceAuthenticationError::InvalidClientCertificate)
        );

        let multiple_elements = format!("{},{}", xfcc_header(&chain), xfcc_header(&chain));
        assert_eq!(
            adapter.authenticate_request(
                &request_with_xfcc(&multiple_elements),
                &hello(),
                &registry,
                TEST_NOW_UNIX_MS
            ),
            Err(WorkspaceDeviceAuthenticationError::InvalidClientCertificate)
        );

        let mut two_leaf_chain = chain;
        two_leaf_chain.leaf_pem = format!("{}{}", two_leaf_chain.leaf_pem, two_leaf_chain.leaf_pem);
        assert_eq!(
            adapter.authenticate_request(
                &request_with_xfcc(&xfcc_header(&two_leaf_chain)),
                &hello(),
                &registry,
                TEST_NOW_UNIX_MS
            ),
            Err(WorkspaceDeviceAuthenticationError::InvalidClientCertificate)
        );
    }

    #[test]
    fn malformed_fields_hashes_and_unexpected_chain_certificates_are_rejected() {
        let chain = test_certificate_chain(true);
        let adapter = adapter(&chain);
        let registry =
            RegistryWorkspaceDeviceVerifier::new(Arc::new(TestDeviceRegistry::approved("unused")));
        let valid = xfcc_header(&chain);
        let invalid_values = vec![
            "Hash=abcd;Cert=\"missing-quote\"".to_string(),
            valid.replace("Hash=", "Hash=not-hex;Hash="),
            valid.replace("Hash=", "Unknown=x;Hash="),
            valid.replace("Hash=", "Hash=00"),
        ];
        for invalid in invalid_values {
            assert_eq!(
                adapter.authenticate_request(
                    &request_with_xfcc(&invalid),
                    &hello(),
                    &registry,
                    TEST_NOW_UNIX_MS
                ),
                Err(WorkspaceDeviceAuthenticationError::InvalidClientCertificate)
            );
        }

        let hash = format!("{:x}", Sha256::digest(&chain.leaf_der));
        let with_leaf_in_chain = format!(
            "Hash={hash};Cert=\"{}\";Chain=\"{}\"",
            encode_xfcc_pem(&chain.leaf_pem),
            encode_xfcc_pem(&chain.leaf_pem),
        );
        assert_eq!(
            adapter.authenticate_request(
                &request_with_xfcc(&with_leaf_in_chain),
                &hello(),
                &registry,
                TEST_NOW_UNIX_MS
            ),
            Err(WorkspaceDeviceAuthenticationError::InvalidClientCertificate)
        );
    }

    #[test]
    fn invalid_or_non_ca_trust_roots_are_rejected_at_startup() {
        assert!(matches!(
            AcaForwardedCertificateAdapter::new(b""),
            Err(AcaForwardedCertificateConfigError::InvalidTrustRoots)
        ));

        let leaf = test_certificate_chain(true);
        assert!(matches!(
            AcaForwardedCertificateAdapter::new(leaf.leaf_pem.as_bytes()),
            Err(AcaForwardedCertificateConfigError::InvalidTrustRoots)
        ));
    }

    #[test]
    fn forwarded_certificate_debug_output_contains_no_certificate_data() {
        let chain = test_certificate_chain(true);
        let adapter = adapter(&chain);
        let debug = format!("{adapter:?}");
        assert!(!debug.contains("BEGIN CERTIFICATE"));
        assert!(!debug.contains("device.test"));
    }
}
