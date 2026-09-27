//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 relay_peer_certificate_validation.rs                           │
//! │  Module: cy_workspace_fabric::relay_peer_certificate_validation     │
//! │  Role: Validate inbound WorkspaceDevice TLS peer certificates.     │
//! │                                                                     │
//! │  模块职责：验证 Relay 入站 WorkspaceDevice TLS 对端证书。               │
//! └─────────────────────────────────────────────────────────────────────┘
//!
//! This validator is CA-independent. It requires private trust roots and a
//! separately configured revocation checker; neither has a production
//! implementation in this module. Request metadata is never a peer identity.

use std::collections::BTreeSet;
use std::fmt;
use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use rustls::pki_types::{CertificateDer, UnixTime};
use rustls::RootCertStore;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tonic::Request;
use webpki::{EndEntityCert, KeyUsage};
use x509_parser::certificate::X509Certificate;
use x509_parser::extensions::{GeneralName, ParsedExtension};
use x509_parser::parse_x509_certificate;

use crate::device_registry::{WorkspaceDeviceCertificateIdentity, WorkspaceDeviceKey};

const MAX_TRUST_ROOTS: usize = 32;
const MAX_TRUST_ROOT_BUNDLE_BYTES: usize = 1024 * 1024;
const MAX_CHAIN_CERTIFICATES: usize = 8;
const MAX_CERTIFICATE_DER_BYTES: usize = 64 * 1024;
const MAX_CHAIN_DER_BYTES: usize = 256 * 1024;
const MAX_DEVICE_IDENTITY_URI_BYTES: usize = 8 * 1024;
const MAX_REVOCATION_EVIDENCE_AGE_MS: u64 = 5 * 60 * 1_000;
const CLIENT_AUTH_EKU_DER: &[u8] = &[0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x02];

/// Configuration failure for an absent or invalid private client-CA bundle.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RelayPeerCertificateConfigError {
    #[error("WORKSPACE_RELAY_PEER_CERTIFICATE_TRUST_CONFIG_INVALID")]
    InvalidTrustRoots,
}

/// Stable validation errors that do not include certificate or CA diagnostics.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RelayPeerCertificateError {
    #[error("WORKSPACE_RELAY_PEER_CERTIFICATE_REQUIRED")]
    MissingPeerCertificate,
    #[error("WORKSPACE_RELAY_PEER_CERTIFICATE_INVALID")]
    InvalidCertificate,
    #[error("WORKSPACE_RELAY_PEER_CERTIFICATE_EXPIRED")]
    ExpiredCertificate,
    #[error("WORKSPACE_RELAY_PEER_CERTIFICATE_REVOKED")]
    Revoked,
    #[error("WORKSPACE_RELAY_PEER_CERTIFICATE_REVOCATION_UNKNOWN")]
    RevocationStatusUnknown,
    #[error("WORKSPACE_RELAY_PEER_CERTIFICATE_IDENTITY_MISMATCH")]
    IdentityMismatch,
}

/// Revocation adapter outcomes; unknown, stale, or unavailable status denies.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RelayPeerRevocationCheckError {
    #[error("certificate is revoked")]
    Revoked,
    #[error("current certificate revocation status is unknown")]
    Unknown,
}

/// Exact authenticated certificate material passed to the revocation adapter.
pub(crate) struct RelayPeerRevocationQuery<'a> {
    certificate_der: &'a [u8],
    intermediate_chain_der: &'a [Vec<u8>],
    trusted_roots_der: &'a [Vec<u8>],
    serial_number: &'a [u8],
    certificate_sha256: [u8; 32],
    checked_at_unix_ms: u64,
}

impl RelayPeerRevocationQuery<'_> {
    pub(crate) fn certificate_der(&self) -> &[u8] {
        self.certificate_der
    }

    pub(crate) fn intermediate_chain_der(&self) -> &[Vec<u8>] {
        self.intermediate_chain_der
    }

    pub(crate) fn trusted_roots_der(&self) -> &[Vec<u8>] {
        self.trusted_roots_der
    }

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

/// Freshness metadata returned only after a trusted checker verifies signed good-status evidence.
pub(crate) struct CurrentRelayPeerRevocationEvidence {
    certificate_sha256: [u8; 32],
    this_update_unix_ms: u64,
    next_update_unix_ms: u64,
}

impl CurrentRelayPeerRevocationEvidence {
    /// Build evidence after the adapter authenticates a current CRL/OCSP response.
    ///
    /// This constructor does not validate signatures. Its caller is part of the
    /// trusted revocation adapter and must bind the response to the query's exact
    /// certificate and issuer path before returning it.
    pub(crate) fn from_verified_good_status(
        certificate_sha256: [u8; 32],
        this_update_unix_ms: u64,
        next_update_unix_ms: u64,
    ) -> Self {
        Self {
            certificate_sha256,
            this_update_unix_ms,
            next_update_unix_ms,
        }
    }
}

/// Trusted CRL/OCSP status boundary. There is no default or soft-fail implementation.
pub(crate) trait RelayPeerCertificateRevocationChecker: Send + Sync {
    /// Return evidence only after proving current good status for this exact certificate.
    fn require_current_good_status(
        &self,
        query: &RelayPeerRevocationQuery<'_>,
    ) -> Result<CurrentRelayPeerRevocationEvidence, RelayPeerRevocationCheckError>;
}

/// Identity facts signed by the leaf's project-private `cyrene-device:v1` URI SAN.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct RelayPeerCertificateIdentity {
    key: WorkspaceDeviceKey,
    authorization_generation: u64,
    csr_sha256: [u8; 32],
    spki_sha256: [u8; 32],
}

impl RelayPeerCertificateIdentity {
    pub(crate) fn key(&self) -> &WorkspaceDeviceKey {
        &self.key
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

/// A leaf that passed path, WorkspaceDevice profile, expiry, and revocation checks.
///
/// This is certificate evidence only. It does not grant a session until the
/// current Directory/registry binding is read and every identity field matches.
pub(crate) struct ValidatedRelayPeerCertificate {
    identity: RelayPeerCertificateIdentity,
    certificate_sha256: [u8; 32],
    serial_number: Vec<u8>,
    not_after_unix_ms: u64,
}

impl ValidatedRelayPeerCertificate {
    pub(crate) fn identity(&self) -> &RelayPeerCertificateIdentity {
        &self.identity
    }

    pub(crate) fn certificate_sha256(&self) -> &[u8; 32] {
        &self.certificate_sha256
    }

    pub(crate) fn fingerprint_sha256_hex(&self) -> String {
        lowercase_hex(&self.certificate_sha256)
    }

    pub(crate) fn serial_number(&self) -> &[u8] {
        &self.serial_number
    }

    pub(crate) fn not_after_unix_ms(&self) -> u64 {
        self.not_after_unix_ms
    }

    /// Bind the certificate facts to a current, active Registry result.
    ///
    /// The supplied result must come from a current-generation Registry query
    /// that verifies the delivered authorization and current Directory binding.
    /// A cached result must never be used to fence a later rotation transaction.
    pub(crate) fn match_registry_identity(
        &self,
        record: &WorkspaceDeviceCertificateIdentity,
    ) -> Result<AuthenticatedRelayWorkspaceDevice, RelayPeerCertificateError> {
        if self.identity.key != record.key
            || self.identity.authorization_generation != record.authorization_generation
            || self.identity.csr_sha256 != record.csr_sha256
            || self.identity.spki_sha256 != record.spki_sha256
            || self.fingerprint_sha256_hex() != record.certificate_fingerprint_sha256
            || self.serial_number != record.serial_number
            || self.not_after_unix_ms != record.not_after_unix_ms
            || self.identity.authorization_generation == 0
            || record.registration_binding_id.iter().all(|byte| *byte == 0)
        {
            return Err(RelayPeerCertificateError::IdentityMismatch);
        }

        Ok(AuthenticatedRelayWorkspaceDevice {
            registration_binding_id: record.registration_binding_id,
            key: self.identity.key.clone(),
            authorization_generation: self.identity.authorization_generation,
            csr_sha256: self.identity.csr_sha256,
            spki_sha256: self.identity.spki_sha256,
            certificate_sha256: self.certificate_sha256,
            serial_number: self.serial_number.clone(),
            not_after_unix_ms: self.not_after_unix_ms,
        })
    }
}

/// Certificate facts plus the matched Directory binding needed to fence rotations.
///
/// Never construct this from RelayHello or request data. A store must compare
/// this binding ID and generation with its locked current identity row before
/// reusing a device ID for certificate rotation.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct AuthenticatedRelayWorkspaceDevice {
    registration_binding_id: [u8; 16],
    key: WorkspaceDeviceKey,
    authorization_generation: u64,
    csr_sha256: [u8; 32],
    spki_sha256: [u8; 32],
    certificate_sha256: [u8; 32],
    serial_number: Vec<u8>,
    not_after_unix_ms: u64,
}

impl AuthenticatedRelayWorkspaceDevice {
    pub(crate) fn registration_binding_id(&self) -> &[u8; 16] {
        &self.registration_binding_id
    }

    pub(crate) fn key(&self) -> &WorkspaceDeviceKey {
        &self.key
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

/// Inbound peer certificate validator initialized with private, configured trust roots.
#[derive(Clone)]
pub(crate) struct TonicPeerCertificateChain {
    leaf_der: Vec<u8>,
    intermediate_chain_der: Vec<Vec<u8>>,
}

impl TonicPeerCertificateChain {
    /// Extract peer certificates only from Tonic's server-side TLS extensions.
    pub(crate) fn from_request<T>(request: &Request<T>) -> Result<Self, RelayPeerCertificateError> {
        let certificates = request
            .peer_certs()
            .ok_or(RelayPeerCertificateError::MissingPeerCertificate)?;
        let leaf_der = certificates
            .first()
            .ok_or(RelayPeerCertificateError::MissingPeerCertificate)?
            .as_ref()
            .to_vec();
        let intermediate_chain_der = certificates
            .iter()
            .skip(1)
            .map(|certificate| certificate.as_ref().to_vec())
            .collect();
        Ok(Self {
            leaf_der,
            intermediate_chain_der,
        })
    }
}

pub(crate) struct RelayPeerCertificateValidator {
    roots: RootCertStore,
    trusted_roots_der: Vec<Vec<u8>>,
    root_fingerprints: BTreeSet<[u8; 32]>,
}

impl RelayPeerCertificateValidator {
    /// Build from trusted DER roots supplied by server configuration.
    ///
    /// Missing, malformed, duplicate, oversized, or non-CA trust roots fail
    /// closed. This does not select or generate a production CA.
    pub(crate) fn new(
        trusted_roots_der: Vec<Vec<u8>>,
    ) -> Result<Self, RelayPeerCertificateConfigError> {
        let total_bytes = trusted_roots_der
            .iter()
            .try_fold(0_usize, |total, certificate| {
                total.checked_add(certificate.len())
            })
            .ok_or(RelayPeerCertificateConfigError::InvalidTrustRoots)?;
        if trusted_roots_der.is_empty()
            || trusted_roots_der.len() > MAX_TRUST_ROOTS
            || total_bytes > MAX_TRUST_ROOT_BUNDLE_BYTES
        {
            return Err(RelayPeerCertificateConfigError::InvalidTrustRoots);
        }

        let mut roots = RootCertStore::empty();
        let mut root_fingerprints = BTreeSet::new();
        for der in &trusted_roots_der {
            if der.is_empty() || der.len() > MAX_CERTIFICATE_DER_BYTES {
                return Err(RelayPeerCertificateConfigError::InvalidTrustRoots);
            }
            let certificate = parse_certificate(der)
                .map_err(|_| RelayPeerCertificateConfigError::InvalidTrustRoots)?;
            validate_ca_certificate(&certificate, None)
                .map_err(|_| RelayPeerCertificateConfigError::InvalidTrustRoots)?;
            if !root_fingerprints.insert(sha256(der)) {
                return Err(RelayPeerCertificateConfigError::InvalidTrustRoots);
            }
            roots
                .add(CertificateDer::from(der.clone()))
                .map_err(|_| RelayPeerCertificateConfigError::InvalidTrustRoots)?;
        }
        if roots.is_empty() {
            return Err(RelayPeerCertificateConfigError::InvalidTrustRoots);
        }

        Ok(Self {
            roots,
            trusted_roots_der,
            root_fingerprints,
        })
    }

    /// Validate the TLS peer certificate chain attached by a Tonic server transport.
    ///
    /// This reads only Tonic TLS connection metadata. It never inspects request
    /// headers, RelayHello identity, or enrollment fields.
    pub(crate) fn validate_tonic_peer<T>(
        &self,
        request: &Request<T>,
        now_unix_ms: u64,
        revocation_checker: &dyn RelayPeerCertificateRevocationChecker,
    ) -> Result<ValidatedRelayPeerCertificate, RelayPeerCertificateError> {
        let chain = TonicPeerCertificateChain::from_request(request)?;
        self.validate_tonic_peer_chain(&chain, now_unix_ms, revocation_checker)
    }

    /// Validate a chain extracted from Tonic's trusted TLS peer extensions.
    pub(crate) fn validate_tonic_peer_chain(
        &self,
        chain: &TonicPeerCertificateChain,
        now_unix_ms: u64,
        revocation_checker: &dyn RelayPeerCertificateRevocationChecker,
    ) -> Result<ValidatedRelayPeerCertificate, RelayPeerCertificateError> {
        self.validate_trusted_transport_chain(
            &chain.leaf_der,
            &chain.intermediate_chain_der,
            now_unix_ms,
            revocation_checker,
        )
    }

    /// Validate a chain extracted by a separately configured trusted ingress adapter.
    ///
    /// Callers may use this only after proving that the ingress terminates client
    /// TLS, overwrites caller-supplied forwarding headers, and blocks direct
    /// bypass. Passing arbitrary request/header bytes here is a trust-boundary bug.
    pub(crate) fn validate_trusted_ingress_chain(
        &self,
        leaf_der: &[u8],
        intermediate_chain_der: &[Vec<u8>],
        now_unix_ms: u64,
        revocation_checker: &dyn RelayPeerCertificateRevocationChecker,
    ) -> Result<ValidatedRelayPeerCertificate, RelayPeerCertificateError> {
        self.validate_trusted_transport_chain(
            leaf_der,
            intermediate_chain_der,
            now_unix_ms,
            revocation_checker,
        )
    }

    fn validate_trusted_transport_chain(
        &self,
        leaf_der: &[u8],
        intermediate_chain_der: &[Vec<u8>],
        now_unix_ms: u64,
        revocation_checker: &dyn RelayPeerCertificateRevocationChecker,
    ) -> Result<ValidatedRelayPeerCertificate, RelayPeerCertificateError> {
        if leaf_der.is_empty()
            || leaf_der.len() > MAX_CERTIFICATE_DER_BYTES
            || intermediate_chain_der.len() > MAX_CHAIN_CERTIFICATES
            || intermediate_chain_der
                .iter()
                .any(|der| der.is_empty() || der.len() > MAX_CERTIFICATE_DER_BYTES)
            || intermediate_chain_der.iter().map(Vec::len).sum::<usize>() > MAX_CHAIN_DER_BYTES
        {
            return Err(RelayPeerCertificateError::InvalidCertificate);
        }

        let now_seconds =
            unix_seconds(now_unix_ms).ok_or(RelayPeerCertificateError::InvalidCertificate)?;
        let leaf = parse_certificate(leaf_der)
            .map_err(|_| RelayPeerCertificateError::InvalidCertificate)?;
        if leaf.validity().not_before.timestamp() > now_seconds
            || leaf.validity().not_after.timestamp() <= now_seconds
        {
            return Err(RelayPeerCertificateError::ExpiredCertificate);
        }

        let serial_number = canonical_serial(leaf.tbs_certificate.raw_serial())
            .ok_or(RelayPeerCertificateError::InvalidCertificate)?;
        let not_after_unix_ms = timestamp_millis(leaf.validity().not_after.timestamp())
            .ok_or(RelayPeerCertificateError::InvalidCertificate)?;
        let certificate_sha256 = sha256(leaf_der);
        let identity = parse_device_identity_profile(&leaf)
            .map_err(|_| RelayPeerCertificateError::InvalidCertificate)?;
        let identity = RelayPeerCertificateIdentity {
            spki_sha256: sha256(leaf.tbs_certificate.subject_pki.raw),
            ..identity
        };

        self.verify_rfc5280_path(
            &leaf,
            leaf_der,
            intermediate_chain_der,
            now_unix_ms,
            now_seconds,
        )?;

        // Revocation must be proven before any registry lookup can authorize a session.
        let query = RelayPeerRevocationQuery {
            certificate_der: leaf_der,
            intermediate_chain_der,
            trusted_roots_der: &self.trusted_roots_der,
            serial_number: &serial_number,
            certificate_sha256,
            checked_at_unix_ms: now_unix_ms,
        };
        let evidence = revocation_checker
            .require_current_good_status(&query)
            .map_err(|error| match error {
                RelayPeerRevocationCheckError::Revoked => RelayPeerCertificateError::Revoked,
                RelayPeerRevocationCheckError::Unknown => {
                    RelayPeerCertificateError::RevocationStatusUnknown
                }
            })?;
        if evidence.certificate_sha256 != certificate_sha256
            || evidence.this_update_unix_ms > now_unix_ms
            || evidence.next_update_unix_ms <= now_unix_ms
            || evidence.next_update_unix_ms <= evidence.this_update_unix_ms
            || now_unix_ms.saturating_sub(evidence.this_update_unix_ms)
                > MAX_REVOCATION_EVIDENCE_AGE_MS
        {
            return Err(RelayPeerCertificateError::RevocationStatusUnknown);
        }

        Ok(ValidatedRelayPeerCertificate {
            identity,
            certificate_sha256,
            serial_number,
            not_after_unix_ms,
        })
    }

    fn verify_rfc5280_path(
        &self,
        leaf: &X509Certificate<'_>,
        leaf_der: &[u8],
        intermediate_chain_der: &[Vec<u8>],
        now_unix_ms: u64,
        now_seconds: i64,
    ) -> Result<(), RelayPeerCertificateError> {
        for root_der in &self.trusted_roots_der {
            let root = parse_certificate(root_der)
                .map_err(|_| RelayPeerCertificateError::InvalidCertificate)?;
            validate_ca_certificate(&root, Some(now_seconds))
                .map_err(|_| RelayPeerCertificateError::InvalidCertificate)?;
        }

        let mut seen = BTreeSet::new();
        let mut intermediates = Vec::with_capacity(intermediate_chain_der.len());
        let mut parsed_intermediates = Vec::with_capacity(intermediate_chain_der.len());
        for der in intermediate_chain_der {
            let fingerprint = sha256(der);
            if fingerprint == sha256(leaf_der)
                || self.root_fingerprints.contains(&fingerprint)
                || !seen.insert(fingerprint)
            {
                return Err(RelayPeerCertificateError::InvalidCertificate);
            }
            let certificate = parse_certificate(der)
                .map_err(|_| RelayPeerCertificateError::InvalidCertificate)?;
            validate_ca_certificate(&certificate, Some(now_seconds))
                .map_err(|_| RelayPeerCertificateError::InvalidCertificate)?;
            parsed_intermediates.push(certificate);
            intermediates.push(CertificateDer::from(der.clone()));
        }

        // Reject unused or reordered certificates instead of silently ignoring chain members.
        let mut next_issuer = leaf.issuer().as_raw();
        for intermediate in &parsed_intermediates {
            if next_issuer != intermediate.subject().as_raw() {
                return Err(RelayPeerCertificateError::InvalidCertificate);
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
            return Err(RelayPeerCertificateError::InvalidCertificate);
        }

        let leaf_der = CertificateDer::from(leaf_der.to_vec());
        let leaf_certificate = EndEntityCert::try_from(&leaf_der)
            .map_err(|_| RelayPeerCertificateError::InvalidCertificate)?;
        let validation_time = UnixTime::since_unix_epoch(Duration::from_millis(now_unix_ms));
        let supported_algorithms = rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .all;
        leaf_certificate
            .verify_for_usage(
                supported_algorithms,
                &self.roots.roots,
                &intermediates,
                validation_time,
                KeyUsage::required_if_present(CLIENT_AUTH_EKU_DER),
                None,
                None,
            )
            .map_err(|_| RelayPeerCertificateError::InvalidCertificate)?;
        Ok(())
    }
}

impl fmt::Debug for RelayPeerCertificateValidator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RelayPeerCertificateValidator")
            .field("trusted_root_count", &self.roots.len())
            .field(
                "max_revocation_evidence_age_ms",
                &MAX_REVOCATION_EVIDENCE_AGE_MS,
            )
            .finish()
    }
}

fn parse_device_identity_profile(
    leaf: &X509Certificate<'_>,
) -> Result<RelayPeerCertificateIdentity, ()> {
    if has_duplicate_extensions(leaf)
        || leaf.extensions().iter().any(|extension| {
            matches!(
                extension.parsed_extension(),
                ParsedExtension::ParseError { .. }
            )
        })
        || leaf.subject().iter_attributes().next().is_some()
    {
        return Err(());
    }

    let basic_constraints = leaf.basic_constraints().map_err(|_| ())?.ok_or(())?;
    if basic_constraints.value.ca || !basic_constraints.critical {
        return Err(());
    }
    let key_usage = leaf.key_usage().map_err(|_| ())?.ok_or(())?;
    if !key_usage.critical || key_usage.value.flags != 1 {
        return Err(());
    }
    let extended_key_usage = leaf.extended_key_usage().map_err(|_| ())?.ok_or(())?;
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
        return Err(());
    }

    let subject_alt_name = leaf.subject_alternative_name().map_err(|_| ())?.ok_or(())?;
    if !subject_alt_name.critical || subject_alt_name.value.general_names.len() != 1 {
        return Err(());
    }
    let Some(GeneralName::URI(uri)) = subject_alt_name.value.general_names.first() else {
        return Err(());
    };
    if uri.len() > MAX_DEVICE_IDENTITY_URI_BYTES {
        return Err(());
    }
    parse_device_identity_uri(uri)
}

/// Parse only the canonical private profile emitted by the Workspace issuer.
fn parse_device_identity_uri(uri: &str) -> Result<RelayPeerCertificateIdentity, ()> {
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
    let authorization_generation = parts[3].parse::<u64>().map_err(|_| ())?;
    if authorization_generation == 0 || authorization_generation.to_string() != parts[3] {
        return Err(());
    }
    let csr_sha256 = parse_lowercase_sha256(parts[4])?;
    Ok(RelayPeerCertificateIdentity {
        key: WorkspaceDeviceKey {
            organization_id,
            workspace_id,
            device_id,
        },
        authorization_generation,
        csr_sha256,
        spki_sha256: [0; 32],
    })
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

fn has_duplicate_extensions(certificate: &X509Certificate<'_>) -> bool {
    let mut seen = BTreeSet::new();
    certificate
        .extensions()
        .iter()
        .any(|extension| !seen.insert(extension.oid.to_id_string()))
}

fn parse_certificate(der: &[u8]) -> Result<X509Certificate<'_>, ()> {
    let (remaining, certificate) = parse_x509_certificate(der).map_err(|_| ())?;
    if !remaining.is_empty() {
        return Err(());
    }
    Ok(certificate)
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

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
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

fn unix_seconds(unix_ms: u64) -> Option<i64> {
    i64::try_from(unix_ms / 1_000).ok()
}

fn timestamp_millis(unix_seconds: i64) -> Option<u64> {
    u64::try_from(unix_seconds).ok()?.checked_mul(1_000)
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}
