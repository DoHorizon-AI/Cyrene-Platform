//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 aca_forwarded_bff_workload.rs                                   │
//! │  Module: cy_workspace_fabric::aca_forwarded_bff_workload            │
//! │  Role: Authenticate the Web BFF ACA workload certificate.          │
//! │                                                                     │
//! │  模块职责：验证 ACA 转发的 Web BFF workload client certificate。       │
//! └─────────────────────────────────────────────────────────────────────┘
//!
//! XFCC is trusted only when the application is reachable through ACA HTTP/2
//! ingress configured to require client certificates and overwrite the header.
//! The host must also block direct access to the app target port.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cy_proto::workspace_v1::RelayHello;
use rustls::pki_types::{CertificateDer, UnixTime};
use rustls::RootCertStore;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tonic::Request;
use webpki::{EndEntityCert, KeyUsage};
use x509_parser::parse_x509_certificate;
use x509_parser::pem::parse_x509_pem;

const XFCC_HEADER: &str = "x-forwarded-client-cert";
const MAX_XFCC_HEADER_BYTES: usize = 64 * 1024;
const MAX_CA_BUNDLE_BYTES: usize = 1024 * 1024;
const MAX_CA_CERTIFICATES: usize = 32;
const MAX_CHAIN_CERTIFICATES: usize = 8;
const MAX_CERTIFICATE_DER_BYTES: usize = 32 * 1024;
const MAX_CERTIFICATE_PINS: usize = 32;
const MAX_SUBJECT_BYTES: usize = 2048;
const CLIENT_AUTH_EKU_DER: &[u8] = &[0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x02];

/// Stable failures for BFF workload certificate configuration and verification.
///
/// Error text never includes the XFCC value, certificate bytes, subject, or trust roots.
///
/// BFF workload certificate 配置与验证的稳定错误；错误文本不包含 XFCC、证书、subject 或 CA 内容。
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum BffWorkloadCertificateError {
    /// Trusted CA or exact certificate allowlist configuration is missing or invalid.
    #[error("BFF workload certificate trust is not configured")]
    Configuration,
    /// ACA did not forward exactly one client certificate.
    #[error("BFF workload client certificate is missing")]
    MissingCertificate,
    /// XFCC syntax, certificate encoding, or certificate path is invalid.
    #[error("BFF workload client certificate is invalid")]
    InvalidCertificate,
    /// The certificate has expired or is not yet valid.
    #[error("BFF workload client certificate is not current")]
    CertificateNotCurrent,
    /// The certificate fingerprint is explicitly revoked by the active allowlist.
    #[error("BFF workload client certificate is revoked")]
    RevokedCertificate,
    /// The certificate chain is valid but its fingerprint is not allowlisted.
    #[error("BFF workload client certificate is not allowlisted")]
    CertificateNotAllowlisted,
    /// The certificate fingerprint is allowlisted but its exact subject differs.
    #[error("BFF workload client certificate subject does not match")]
    SubjectMismatch,
    /// A caller selected the Frontend verifier for another Relay participant role.
    #[error("BFF workload certificate is only valid for Frontend participants")]
    WrongRole,
}

/// One exact BFF workload certificate pin loaded from private host configuration.
///
/// The fingerprint is lowercase or uppercase SHA-256 hex over the leaf DER. `subject`
/// must exactly match the X.509 subject's canonical parser rendering. A revoked pin
/// remains in the file for audit but is never accepted; removing a pin also denies it.
///
/// 私有 host 配置中的一个 BFF workload 证书 pin。撤销项保留用于审计但永不接受，删除 pin 也会拒绝。
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BffWorkloadCertificatePin {
    #[serde(rename = "sha256Fingerprint")]
    fingerprint_sha256: String,
    subject: String,
    revoked: bool,
}

impl BffWorkloadCertificatePin {
    /// Creates one fingerprint, exact-subject, and revocation entry.
    pub fn new(
        fingerprint_sha256: impl Into<String>,
        subject: impl Into<String>,
        revoked: bool,
    ) -> Self {
        Self {
            fingerprint_sha256: fingerprint_sha256.into(),
            subject: subject.into(),
            revoked,
        }
    }
}

/// Verified BFF transport identity for audit-safe correlation.
///
/// Only the leaf fingerprint is exposed. Certificate or XFCC contents are not retained.
///
/// 已验证的 BFF transport identity；只暴露叶子证书指纹，不保留证书或 XFCC 内容。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedBffWorkloadIdentity {
    fingerprint_sha256: String,
}

impl VerifiedBffWorkloadIdentity {
    /// Returns lowercase SHA-256 hex over the forwarded leaf certificate DER.
    pub fn fingerprint_sha256(&self) -> &str {
        &self.fingerprint_sha256
    }
}

/// ACA XFCC verifier for the BFF service identity, independent of device CA and registry.
pub struct AcaForwardedBffWorkloadCertificateAdapter {
    roots: RootCertStore,
    root_fingerprints: BTreeSet<[u8; 32]>,
    pins: BTreeMap<[u8; 32], BffWorkloadCertificatePin>,
}

impl AcaForwardedBffWorkloadCertificateAdapter {
    /// Builds an adapter from a dedicated BFF client CA bundle and exact leaf pins.
    ///
    /// The CA bundle must be private BFF-service trust, not the Workspace-device CA.
    /// Empty, duplicate, malformed, expired, or non-CA trust entries fail at startup.
    ///
    /// 使用独立 BFF service CA 与精确叶子 pin 创建 adapter；不得复用 Workspace device CA。
    pub fn new(
        trusted_bff_client_ca_bundle_pem: &[u8],
        pins: impl IntoIterator<Item = BffWorkloadCertificatePin>,
    ) -> Result<Self, BffWorkloadCertificateError> {
        if trusted_bff_client_ca_bundle_pem.is_empty()
            || trusted_bff_client_ca_bundle_pem.len() > MAX_CA_BUNDLE_BYTES
        {
            return Err(BffWorkloadCertificateError::Configuration);
        }
        let now_seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| BffWorkloadCertificateError::Configuration)?
            .as_secs();
        let now_seconds =
            i64::try_from(now_seconds).map_err(|_| BffWorkloadCertificateError::Configuration)?;
        let root_der = parse_pem_certificate_bundle(
            trusted_bff_client_ca_bundle_pem,
            MAX_CA_CERTIFICATES,
            MAX_CA_BUNDLE_BYTES,
        )
        .map_err(|_| BffWorkloadCertificateError::Configuration)?;
        if root_der.is_empty() || root_der.len() > MAX_CA_CERTIFICATES {
            return Err(BffWorkloadCertificateError::Configuration);
        }

        let mut roots = RootCertStore::empty();
        let mut root_fingerprints = BTreeSet::new();
        for der in root_der {
            validate_ca_certificate(&der, now_seconds)
                .map_err(|_| BffWorkloadCertificateError::Configuration)?;
            let fingerprint = sha256_digest(&der);
            if !root_fingerprints.insert(fingerprint) {
                return Err(BffWorkloadCertificateError::Configuration);
            }
            roots
                .add(CertificateDer::from(der))
                .map_err(|_| BffWorkloadCertificateError::Configuration)?;
        }
        if roots.is_empty() {
            return Err(BffWorkloadCertificateError::Configuration);
        }

        let mut parsed_pins = BTreeMap::new();
        for pin in pins {
            if parsed_pins.len() >= MAX_CERTIFICATE_PINS || !valid_subject(&pin.subject) {
                return Err(BffWorkloadCertificateError::Configuration);
            }
            let fingerprint = parse_sha256_hex(&pin.fingerprint_sha256)
                .ok_or(BffWorkloadCertificateError::Configuration)?;
            if parsed_pins.insert(fingerprint, pin).is_some() {
                return Err(BffWorkloadCertificateError::Configuration);
            }
        }
        if parsed_pins.is_empty() {
            return Err(BffWorkloadCertificateError::Configuration);
        }

        Ok(Self {
            roots,
            root_fingerprints,
            pins: parsed_pins,
        })
    }

    /// Verifies ACA-overwritten XFCC, BFF service CA, current validity, exact subject and pin.
    ///
    /// Call only for the Frontend role while processing the same Relay RPC request that carried
    /// its first `RelayHello`. The deployment must force every request through ACA ingress; a
    /// local spoofed header cannot be distinguished from ingress metadata without that boundary.
    ///
    /// 校验同一 Relay RPC 的 ACA XFCC、专用 CA、有效期、精确 subject 与指纹 pin；缺少可信 ingress 边界时不得调用。
    pub fn authenticate_request<T>(
        &self,
        request: &Request<T>,
        hello: &RelayHello,
        now_unix_ms: u64,
    ) -> Result<VerifiedBffWorkloadIdentity, BffWorkloadCertificateError> {
        if hello.role != cy_proto::workspace_v1::RelayParticipantRole::Frontend as i32 {
            return Err(BffWorkloadCertificateError::WrongRole);
        }
        let header = single_xfcc_header(request)?;
        let forwarded = parse_xfcc(header)?;
        self.authenticate_forwarded_certificate(forwarded, hello, now_unix_ms)
    }

    fn authenticate_forwarded_certificate(
        &self,
        forwarded: ForwardedCertificate,
        hello: &RelayHello,
        now_unix_ms: u64,
    ) -> Result<VerifiedBffWorkloadIdentity, BffWorkloadCertificateError> {
        if hello.role != cy_proto::workspace_v1::RelayParticipantRole::Frontend as i32 {
            return Err(BffWorkloadCertificateError::WrongRole);
        }
        let leaf_fingerprint = sha256_digest(&forwarded.leaf_der);
        if forwarded.declared_hash != leaf_fingerprint {
            return Err(BffWorkloadCertificateError::InvalidCertificate);
        }
        let pin = self
            .pins
            .get(&leaf_fingerprint)
            .ok_or(BffWorkloadCertificateError::CertificateNotAllowlisted)?;
        if pin.revoked {
            return Err(BffWorkloadCertificateError::RevokedCertificate);
        }

        let now_seconds = unix_seconds(now_unix_ms)?;
        let (remaining, leaf_certificate) = parse_x509_certificate(&forwarded.leaf_der)
            .map_err(|_| BffWorkloadCertificateError::InvalidCertificate)?;
        if !remaining.is_empty() || !has_explicit_client_auth_eku(&forwarded.leaf_der) {
            return Err(BffWorkloadCertificateError::InvalidCertificate);
        }
        if leaf_certificate.validity().not_before.timestamp() > now_seconds
            || leaf_certificate.validity().not_after.timestamp() <= now_seconds
        {
            return Err(BffWorkloadCertificateError::CertificateNotCurrent);
        }
        if leaf_certificate.subject().to_string() != pin.subject {
            return Err(BffWorkloadCertificateError::SubjectMismatch);
        }
        if leaf_certificate
            .basic_constraints()
            .map_err(|_| BffWorkloadCertificateError::InvalidCertificate)?
            .is_some_and(|constraints| constraints.value.ca)
        {
            return Err(BffWorkloadCertificateError::InvalidCertificate);
        }

        let mut intermediates = Vec::with_capacity(forwarded.intermediate_der.len());
        let mut seen = BTreeSet::new();
        for der in forwarded.intermediate_der {
            let fingerprint = sha256_digest(&der);
            if fingerprint == leaf_fingerprint
                || self.root_fingerprints.contains(&fingerprint)
                || !seen.insert(fingerprint)
            {
                return Err(BffWorkloadCertificateError::InvalidCertificate);
            }
            validate_ca_certificate(&der, now_seconds)
                .map_err(|_| BffWorkloadCertificateError::InvalidCertificate)?;
            intermediates.push(CertificateDer::from(der));
        }

        let leaf_der = CertificateDer::from(forwarded.leaf_der);
        let leaf = EndEntityCert::try_from(&leaf_der)
            .map_err(|_| BffWorkloadCertificateError::InvalidCertificate)?;
        let validation_time = UnixTime::since_unix_epoch(Duration::from_millis(now_unix_ms));
        let supported_algorithms = rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .all;
        leaf.verify_for_usage(
            supported_algorithms,
            &self.roots.roots,
            &intermediates,
            validation_time,
            KeyUsage::required_if_present(CLIENT_AUTH_EKU_DER),
            None,
            None,
        )
        .map_err(|_| BffWorkloadCertificateError::InvalidCertificate)?;

        Ok(VerifiedBffWorkloadIdentity {
            fingerprint_sha256: hex_encode(&leaf_fingerprint),
        })
    }
}

/// Native Tonic TLS adapter for a separately pinned BFF workload identity.
///
/// It shares certificate profile validation with the ACA adapter but obtains certificate bytes
/// exclusively from Tonic's peer-certificate extension. The two host modes have separate types
/// so a native endpoint cannot accidentally trust an XFCC header.
pub struct TonicBffWorkloadCertificateAdapter {
    verifier: AcaForwardedBffWorkloadCertificateAdapter,
}

impl TonicBffWorkloadCertificateAdapter {
    /// Builds a verifier from a dedicated BFF CA bundle and exact leaf pins.
    pub fn new(
        trusted_bff_client_ca_bundle_pem: &[u8],
        pins: Vec<BffWorkloadCertificatePin>,
    ) -> Result<Self, BffWorkloadCertificateError> {
        Ok(Self {
            verifier: AcaForwardedBffWorkloadCertificateAdapter::new(
                trusted_bff_client_ca_bundle_pem,
                pins,
            )?,
        })
    }

    /// Authenticates only the Frontend role using Tonic's TLS peer certificate chain.
    pub fn authenticate_request<T>(
        &self,
        request: &Request<T>,
        hello: &RelayHello,
        now_unix_ms: u64,
    ) -> Result<VerifiedBffWorkloadIdentity, BffWorkloadCertificateError> {
        let certificates = request
            .peer_certs()
            .ok_or(BffWorkloadCertificateError::MissingCertificate)?;
        let leaf_der = certificates
            .first()
            .ok_or(BffWorkloadCertificateError::MissingCertificate)?
            .as_ref()
            .to_vec();
        let intermediate_der: Vec<Vec<u8>> = certificates
            .iter()
            .skip(1)
            .map(|certificate| certificate.as_ref().to_vec())
            .collect();
        self.authenticate_peer_certificate_chain(&leaf_der, &intermediate_der, hello, now_unix_ms)
    }

    pub(crate) fn authenticate_peer_certificate_chain(
        &self,
        leaf_der: &[u8],
        intermediate_der: &[Vec<u8>],
        hello: &RelayHello,
        now_unix_ms: u64,
    ) -> Result<VerifiedBffWorkloadIdentity, BffWorkloadCertificateError> {
        self.verifier.authenticate_forwarded_certificate(
            ForwardedCertificate {
                declared_hash: sha256_digest(leaf_der),
                leaf_der: leaf_der.to_vec(),
                intermediate_der: intermediate_der.to_vec(),
            },
            hello,
            now_unix_ms,
        )
    }
}

impl fmt::Debug for TonicBffWorkloadCertificateAdapter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TonicBffWorkloadCertificateAdapter")
            .field("verifier", &self.verifier)
            .finish()
    }
}

impl fmt::Debug for AcaForwardedBffWorkloadCertificateAdapter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AcaForwardedBffWorkloadCertificateAdapter")
            .field("trusted_root_count", &self.roots.len())
            .field("certificate_pin_count", &self.pins.len())
            .finish()
    }
}

struct ForwardedCertificate {
    declared_hash: [u8; 32],
    leaf_der: Vec<u8>,
    intermediate_der: Vec<Vec<u8>>,
}

fn single_xfcc_header<T>(request: &Request<T>) -> Result<&str, BffWorkloadCertificateError> {
    let mut values = request.metadata().get_all(XFCC_HEADER).iter();
    let value = values
        .next()
        .ok_or(BffWorkloadCertificateError::MissingCertificate)?;
    if values.next().is_some() {
        return Err(BffWorkloadCertificateError::InvalidCertificate);
    }
    let value = value
        .to_str()
        .map_err(|_| BffWorkloadCertificateError::InvalidCertificate)?;
    if value.is_empty()
        || value.len() > MAX_XFCC_HEADER_BYTES
        || !value.is_ascii()
        || value.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(BffWorkloadCertificateError::InvalidCertificate);
    }
    Ok(value)
}

fn parse_xfcc(value: &str) -> Result<ForwardedCertificate, BffWorkloadCertificateError> {
    let fields = parse_xfcc_fields(value)?;
    let declared_hash = fields
        .get("hash")
        .and_then(|hash| parse_sha256_hex(hash))
        .ok_or(BffWorkloadCertificateError::InvalidCertificate)?;
    let leaf_pem = fields
        .get("cert")
        .ok_or(BffWorkloadCertificateError::MissingCertificate)?;
    let leaf_der = parse_single_forwarded_pem(leaf_pem)?;
    let intermediate_der = match fields.get("chain") {
        Some(chain) => parse_forwarded_pem_bundle(chain, MAX_CHAIN_CERTIFICATES)?,
        None => Vec::new(),
    };
    Ok(ForwardedCertificate {
        declared_hash,
        leaf_der,
        intermediate_der,
    })
}

fn parse_xfcc_fields(value: &str) -> Result<BTreeMap<String, String>, BffWorkloadCertificateError> {
    let mut fields = BTreeMap::new();
    let mut start = 0;
    let mut quoted = false;
    for (index, byte) in value.bytes().enumerate() {
        match byte {
            b'\\' => return Err(BffWorkloadCertificateError::InvalidCertificate),
            b'"' => quoted = !quoted,
            b',' if !quoted => return Err(BffWorkloadCertificateError::InvalidCertificate),
            b';' if !quoted => {
                insert_xfcc_field(&mut fields, &value[start..index])?;
                start = index + 1;
            }
            _ => {}
        }
    }
    if quoted {
        return Err(BffWorkloadCertificateError::InvalidCertificate);
    }
    insert_xfcc_field(&mut fields, &value[start..])?;
    if !fields.contains_key("hash") || !fields.contains_key("cert") {
        return Err(BffWorkloadCertificateError::MissingCertificate);
    }
    Ok(fields)
}

fn insert_xfcc_field(
    fields: &mut BTreeMap<String, String>,
    segment: &str,
) -> Result<(), BffWorkloadCertificateError> {
    let (key, raw_value) = segment
        .split_once('=')
        .ok_or(BffWorkloadCertificateError::InvalidCertificate)?;
    let key = key.trim().to_ascii_lowercase();
    if !matches!(key.as_str(), "hash" | "cert" | "chain") {
        return Err(BffWorkloadCertificateError::InvalidCertificate);
    }
    let raw_value = raw_value.trim();
    let value = if key == "hash" {
        if raw_value.starts_with('"') || raw_value.ends_with('"') {
            return Err(BffWorkloadCertificateError::InvalidCertificate);
        }
        raw_value.to_owned()
    } else {
        if raw_value.len() < 2 || !raw_value.starts_with('"') || !raw_value.ends_with('"') {
            return Err(BffWorkloadCertificateError::InvalidCertificate);
        }
        let inner = &raw_value[1..raw_value.len() - 1];
        if inner.is_empty() || inner.contains('"') {
            return Err(BffWorkloadCertificateError::InvalidCertificate);
        }
        inner.to_owned()
    };
    if fields.insert(key, value).is_some() {
        return Err(BffWorkloadCertificateError::InvalidCertificate);
    }
    Ok(())
}

fn parse_single_forwarded_pem(value: &str) -> Result<Vec<u8>, BffWorkloadCertificateError> {
    let decoded = decode_xfcc_value(value)?;
    let mut certificates = parse_pem_certificate_bundle(&decoded, 1, MAX_CERTIFICATE_DER_BYTES)?;
    if certificates.len() != 1 {
        return Err(BffWorkloadCertificateError::InvalidCertificate);
    }
    Ok(certificates.remove(0))
}

fn parse_forwarded_pem_bundle(
    value: &str,
    max_certificates: usize,
) -> Result<Vec<Vec<u8>>, BffWorkloadCertificateError> {
    let decoded = decode_xfcc_value(value)?;
    let certificates =
        parse_pem_certificate_bundle(&decoded, max_certificates, MAX_XFCC_HEADER_BYTES)?;
    if certificates.is_empty() {
        return Err(BffWorkloadCertificateError::InvalidCertificate);
    }
    Ok(certificates)
}

fn decode_xfcc_value(value: &str) -> Result<Vec<u8>, BffWorkloadCertificateError> {
    let input = value.as_bytes();
    let mut decoded = Vec::with_capacity(input.len());
    let mut index = 0;
    while index < input.len() {
        let byte = if input[index] == b'%' {
            if index + 2 >= input.len() {
                return Err(BffWorkloadCertificateError::InvalidCertificate);
            }
            let high = hex_nibble(input[index + 1])
                .ok_or(BffWorkloadCertificateError::InvalidCertificate)?;
            let low = hex_nibble(input[index + 2])
                .ok_or(BffWorkloadCertificateError::InvalidCertificate)?;
            index += 3;
            (high << 4) | low
        } else {
            let byte = input[index];
            if !byte.is_ascii() {
                return Err(BffWorkloadCertificateError::InvalidCertificate);
            }
            index += 1;
            byte
        };
        if decoded.len() >= MAX_XFCC_HEADER_BYTES {
            return Err(BffWorkloadCertificateError::InvalidCertificate);
        }
        decoded.push(byte);
    }
    Ok(decoded)
}

pub(crate) fn parse_pem_certificate_bundle(
    input: &[u8],
    max_certificates: usize,
    max_total_bytes: usize,
) -> Result<Vec<Vec<u8>>, BffWorkloadCertificateError> {
    if input.is_empty() || input.len() > max_total_bytes || !input.is_ascii() {
        return Err(BffWorkloadCertificateError::InvalidCertificate);
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
            return Err(BffWorkloadCertificateError::InvalidCertificate);
        }
        let previous_len = remaining.len();
        let (next, pem) = parse_x509_pem(remaining)
            .map_err(|_| BffWorkloadCertificateError::InvalidCertificate)?;
        if next.len() >= previous_len || pem.label != "CERTIFICATE" {
            return Err(BffWorkloadCertificateError::InvalidCertificate);
        }
        if pem.contents.is_empty() || pem.contents.len() > MAX_CERTIFICATE_DER_BYTES {
            return Err(BffWorkloadCertificateError::InvalidCertificate);
        }
        let (der_remaining, _) = parse_x509_certificate(&pem.contents)
            .map_err(|_| BffWorkloadCertificateError::InvalidCertificate)?;
        if !der_remaining.is_empty() {
            return Err(BffWorkloadCertificateError::InvalidCertificate);
        }
        total_der_bytes = total_der_bytes
            .checked_add(pem.contents.len())
            .ok_or(BffWorkloadCertificateError::InvalidCertificate)?;
        if total_der_bytes > max_total_bytes {
            return Err(BffWorkloadCertificateError::InvalidCertificate);
        }
        certificates.push(pem.contents);
        remaining = next;
    }
    Ok(certificates)
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

fn parse_sha256_hex(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64 {
        return None;
    }
    let mut output = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = hex_nibble(pair[0])?;
        let low = hex_nibble(pair[1])?;
        output[index] = (high << 4) | low;
    }
    Some(output)
}

fn hex_encode(value: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(value.len() * 2);
    for byte in value {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn sha256_digest(der: &[u8]) -> [u8; 32] {
    Sha256::digest(der).into()
}

fn unix_seconds(unix_ms: u64) -> Result<i64, BffWorkloadCertificateError> {
    i64::try_from(unix_ms / 1_000).map_err(|_| BffWorkloadCertificateError::InvalidCertificate)
}

fn trim_ascii_start(input: &[u8]) -> &[u8] {
    let leading_whitespace = input
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(input.len());
    &input[leading_whitespace..]
}

fn valid_subject(subject: &str) -> bool {
    !subject.is_empty()
        && subject.len() <= MAX_SUBJECT_BYTES
        && subject.trim() == subject
        && !subject.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use rcgen::{
        date_time_ymd, BasicConstraints, CertificateParams, CertifiedIssuer, DistinguishedName,
        DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose,
    };

    use super::*;

    const TEST_NOW_UNIX_MS: u64 = 1_789_934_400_000;
    struct TestCertificateChain {
        root_pem: String,
        leaf_pem: String,
        intermediate_pem: String,
        leaf_der: Vec<u8>,
    }

    fn test_certificate_chain(with_client_auth: bool, expired: bool) -> TestCertificateChain {
        let root_key = KeyPair::generate().unwrap();
        let mut root_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        root_params.distinguished_name = DistinguishedName::new();
        root_params
            .distinguished_name
            .push(DnType::CommonName, "Cyrene BFF Workload Root");
        root_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        root_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        root_params.not_before = date_time_ymd(2025, 1, 1);
        root_params.not_after = date_time_ymd(2035, 1, 1);
        let root = CertifiedIssuer::self_signed(root_params, root_key).unwrap();

        let intermediate_key = KeyPair::generate().unwrap();
        let mut intermediate_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        intermediate_params.distinguished_name = DistinguishedName::new();
        intermediate_params
            .distinguished_name
            .push(DnType::CommonName, "Cyrene BFF Workload Intermediate");
        intermediate_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        intermediate_params.key_usages =
            vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        intermediate_params.not_before = date_time_ymd(2025, 1, 1);
        intermediate_params.not_after = date_time_ymd(2034, 1, 1);
        let intermediate =
            CertifiedIssuer::signed_by(intermediate_params, intermediate_key, &root).unwrap();

        let leaf_key = KeyPair::generate().unwrap();
        let mut leaf_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        leaf_params.distinguished_name = DistinguishedName::new();
        leaf_params
            .distinguished_name
            .push(DnType::CommonName, "Cyrene Web BFF Workload");
        leaf_params.is_ca = IsCa::NoCa;
        leaf_params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        leaf_params.not_before = date_time_ymd(2025, 1, 1);
        leaf_params.not_after = if expired {
            date_time_ymd(2026, 1, 1)
        } else {
            date_time_ymd(2033, 1, 1)
        };
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

    fn subject(chain: &TestCertificateChain) -> String {
        let (_, cert) = parse_x509_certificate(&chain.leaf_der).unwrap();
        cert.subject().to_string()
    }

    fn xfcc_header(chain: &TestCertificateChain) -> String {
        format!(
            "Hash={};Cert=\"{}\";Chain=\"{}\"",
            hex_encode(&sha256_digest(&chain.leaf_der)),
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

    fn hello(role: i32) -> RelayHello {
        RelayHello {
            role,
            session_credential: "opaque-signed-handoff".to_string(),
            user: None,
            organization_id: String::new(),
            workspace_id: String::new(),
            device: None,
        }
    }

    fn pin(
        chain: &TestCertificateChain,
        subject: &str,
        revoked: bool,
    ) -> BffWorkloadCertificatePin {
        BffWorkloadCertificatePin::new(
            hex_encode(&sha256_digest(&chain.leaf_der)),
            subject,
            revoked,
        )
    }

    fn adapter(
        chain: &TestCertificateChain,
        pins: impl IntoIterator<Item = BffWorkloadCertificatePin>,
    ) -> AcaForwardedBffWorkloadCertificateAdapter {
        AcaForwardedBffWorkloadCertificateAdapter::new(chain.root_pem.as_bytes(), pins).unwrap()
    }

    #[test]
    fn valid_bff_certificate_requires_service_ca_subject_and_fingerprint_pin() {
        let chain = test_certificate_chain(true, false);
        let adapter = adapter(&chain, [pin(&chain, &subject(&chain), false)]);
        let verified = adapter
            .authenticate_request(
                &request_with_xfcc(&xfcc_header(&chain)),
                &hello(cy_proto::workspace_v1::RelayParticipantRole::Frontend as i32),
                TEST_NOW_UNIX_MS,
            )
            .unwrap();
        assert_eq!(
            verified.fingerprint_sha256(),
            hex_encode(&sha256_digest(&chain.leaf_der))
        );
    }

    #[test]
    fn missing_duplicate_spoofed_or_malformed_xfcc_is_rejected() {
        let chain = test_certificate_chain(true, false);
        let adapter = adapter(&chain, [pin(&chain, &subject(&chain), false)]);
        let frontend = hello(cy_proto::workspace_v1::RelayParticipantRole::Frontend as i32);
        assert_eq!(
            adapter.authenticate_request(&Request::new(()), &frontend, TEST_NOW_UNIX_MS),
            Err(BffWorkloadCertificateError::MissingCertificate)
        );

        let header = xfcc_header(&chain);
        let mut duplicate = Request::new(());
        duplicate
            .metadata_mut()
            .append(XFCC_HEADER, header.parse().unwrap());
        duplicate
            .metadata_mut()
            .append(XFCC_HEADER, header.parse().unwrap());
        assert_eq!(
            adapter.authenticate_request(&duplicate, &frontend, TEST_NOW_UNIX_MS),
            Err(BffWorkloadCertificateError::InvalidCertificate)
        );

        for invalid in [
            "Hash=bad;Cert=\"bad\"".to_string(),
            format!("{},{}", header, header),
            header.replace("Hash=", "Unknown=caller;Hash="),
        ] {
            assert_eq!(
                adapter.authenticate_request(
                    &request_with_xfcc(&invalid),
                    &frontend,
                    TEST_NOW_UNIX_MS
                ),
                Err(BffWorkloadCertificateError::InvalidCertificate)
            );
        }
    }

    #[test]
    fn wrong_ca_subject_pin_expiry_and_revocation_are_rejected() {
        let chain = test_certificate_chain(true, false);
        let request = request_with_xfcc(&xfcc_header(&chain));
        let frontend = hello(cy_proto::workspace_v1::RelayParticipantRole::Frontend as i32);

        let unrelated = test_certificate_chain(true, false);
        let wrong_ca = AcaForwardedBffWorkloadCertificateAdapter::new(
            unrelated.root_pem.as_bytes(),
            [pin(&chain, &subject(&chain), false)],
        )
        .unwrap();
        assert_eq!(
            wrong_ca.authenticate_request(&request, &frontend, TEST_NOW_UNIX_MS),
            Err(BffWorkloadCertificateError::InvalidCertificate)
        );

        assert_eq!(
            adapter(&chain, [pin(&chain, "CN=another-service", false)]).authenticate_request(
                &request,
                &frontend,
                TEST_NOW_UNIX_MS
            ),
            Err(BffWorkloadCertificateError::SubjectMismatch)
        );
        assert_eq!(
            adapter(
                &chain,
                [BffWorkloadCertificatePin::new(
                    "aa".repeat(32),
                    subject(&chain),
                    false
                )]
            )
            .authenticate_request(&request, &frontend, TEST_NOW_UNIX_MS),
            Err(BffWorkloadCertificateError::CertificateNotAllowlisted)
        );
        assert_eq!(
            adapter(&chain, [pin(&chain, &subject(&chain), true)]).authenticate_request(
                &request,
                &frontend,
                TEST_NOW_UNIX_MS
            ),
            Err(BffWorkloadCertificateError::RevokedCertificate)
        );

        let expired = test_certificate_chain(true, true);
        assert_eq!(
            adapter(&expired, [pin(&expired, &subject(&expired), false)]).authenticate_request(
                &request_with_xfcc(&xfcc_header(&expired)),
                &frontend,
                TEST_NOW_UNIX_MS
            ),
            Err(BffWorkloadCertificateError::CertificateNotCurrent)
        );
    }

    #[test]
    fn connector_role_cannot_use_the_frontend_workload_trust_path() {
        let chain = test_certificate_chain(true, false);
        let adapter = adapter(&chain, [pin(&chain, &subject(&chain), false)]);
        let connector =
            hello(cy_proto::workspace_v1::RelayParticipantRole::WorkspaceConnector as i32);
        assert_eq!(
            adapter.authenticate_request(
                &request_with_xfcc(&xfcc_header(&chain)),
                &connector,
                TEST_NOW_UNIX_MS,
            ),
            Err(BffWorkloadCertificateError::WrongRole)
        );
    }

    #[test]
    fn startup_rejects_missing_ca_empty_pins_duplicates_and_bad_config() {
        let chain = test_certificate_chain(true, false);
        assert!(matches!(
            AcaForwardedBffWorkloadCertificateAdapter::new(
                b"",
                [pin(&chain, &subject(&chain), false)]
            ),
            Err(BffWorkloadCertificateError::Configuration)
        ));
        assert!(matches!(
            AcaForwardedBffWorkloadCertificateAdapter::new(chain.root_pem.as_bytes(), []),
            Err(BffWorkloadCertificateError::Configuration)
        ));
        assert!(matches!(
            AcaForwardedBffWorkloadCertificateAdapter::new(
                chain.root_pem.as_bytes(),
                [
                    pin(&chain, &subject(&chain), false),
                    pin(&chain, &subject(&chain), false)
                ]
            ),
            Err(BffWorkloadCertificateError::Configuration)
        ));
        assert!(matches!(
            AcaForwardedBffWorkloadCertificateAdapter::new(
                chain.root_pem.as_bytes(),
                [BffWorkloadCertificatePin::new(
                    "nope",
                    subject(&chain),
                    false
                )]
            ),
            Err(BffWorkloadCertificateError::Configuration)
        ));
        assert!(matches!(
            AcaForwardedBffWorkloadCertificateAdapter::new(
                chain.root_pem.as_bytes(),
                [BffWorkloadCertificatePin::new(
                    hex_encode(&sha256_digest(&chain.leaf_der)),
                    " ",
                    false
                )]
            ),
            Err(BffWorkloadCertificateError::Configuration)
        ));
    }

    #[test]
    fn request_role_must_be_frontend_before_the_certificate_is_accepted() {
        let chain = test_certificate_chain(true, false);
        let adapter = adapter(&chain, [pin(&chain, &subject(&chain), false)]);
        assert_eq!(
            adapter.authenticate_request(
                &request_with_xfcc(&xfcc_header(&chain)),
                &hello(0),
                TEST_NOW_UNIX_MS
            ),
            Err(BffWorkloadCertificateError::WrongRole)
        );
    }
}
