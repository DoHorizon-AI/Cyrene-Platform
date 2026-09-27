//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 device_csr_validator.rs                                         │
//! │  Module: cy_workspace_fabric::device_csr_validator                  │
//! │  Role: Validate PKCS#10 device requests and key possession.         │
//! │                                                                     │
//! │  模块职责：验证设备 PKCS#10 CSR 编码、密钥策略和签名持有证明。          │
//! └─────────────────────────────────────────────────────────────────────┘
//!
//! This validator returns only the digest of the validated CSR SubjectPublicKeyInfo.
//! CSR subject names and requested extensions are untrusted and never supply
//! device identity. The issuer must construct a fresh leaf under the device
//! certificate profile: an empty subject and exactly one critical URI SAN from
//! the trusted Directory binding. CSR subject/SAN values are discarded.

use std::collections::BTreeSet;
use std::convert::TryFrom;

use openssl::ec::EcKey;
use openssl::nid::Nid;
use openssl::pkey::{Id, PKey};
use openssl::x509::X509Req;
use sha2::{Digest, Sha256};
use x509_parser::asn1_rs::ToDer;
use x509_parser::certification_request::X509CertificationRequest;
use x509_parser::cri_attributes::ParsedCriAttribute;
use x509_parser::prelude::FromDer;
use x509_parser::signature_algorithm::RsaSsaPssParams;
use x509_parser::x509::{AlgorithmIdentifier, SubjectPublicKeyInfo, X509Version};

use crate::device_authorization::{DeviceAuthorizationPortError, DeviceCsrValidator};

const MAX_CSR_DER_BYTES: usize = 16 * 1024;
const MIN_RSA_BITS: u32 = 2_048;
const MAX_RSA_BITS: u32 = 8_192;

const OID_RSA_ENCRYPTION: &str = "1.2.840.113549.1.1.1";
const OID_EC_PUBLIC_KEY: &str = "1.2.840.10045.2.1";
const OID_ED25519: &str = "1.3.101.112";
const OID_RSA_WITH_SHA256: &str = "1.2.840.113549.1.1.11";
const OID_RSA_WITH_SHA384: &str = "1.2.840.113549.1.1.12";
const OID_RSA_WITH_SHA512: &str = "1.2.840.113549.1.1.13";
const OID_RSA_PSS: &str = "1.2.840.113549.1.1.10";
const OID_ECDSA_WITH_SHA256: &str = "1.2.840.10045.4.3.2";
const OID_ECDSA_WITH_SHA384: &str = "1.2.840.10045.4.3.3";
const OID_ECDSA_WITH_SHA512: &str = "1.2.840.10045.4.3.4";
const OID_MGF1: &str = "1.2.840.113549.1.1.8";
const OID_SHA256: &str = "2.16.840.1.101.3.4.2.1";
const OID_SHA384: &str = "2.16.840.1.101.3.4.2.2";
const OID_SHA512: &str = "2.16.840.1.101.3.4.2.3";
const OID_SECP256R1: &str = "1.2.840.10045.3.1.7";
const OID_SECP384R1: &str = "1.3.132.0.34";

/// ════════════════════════════════════════════════════════════════════════
/// Production PKCS#10 validator backed by the repository's OpenSSL provider.
///
/// It accepts RSA (2048–8192 bits), P-256/P-384 ECDSA, or Ed25519 keys.
/// RSA/ECDSA request signatures use SHA-256 or stronger; Ed25519 uses its
/// intrinsic signature scheme. Construction needs no CA material; an absent
/// or unconfigured CA remains a separate fail-closed boundary.
///
/// This type proves possession of the CSR private key. The enrollment manager
/// binds the returned SPKI digest and complete CSR digest to the trusted
/// Directory registration, scope, device key, and authorization generation.
/// ════════════════════════════════════════════════════════════════════════
#[derive(Debug, Clone, Copy, Default)]
pub struct ProductionDeviceCsrValidator;

impl ProductionDeviceCsrValidator {
    /// Creates the stateless local CSR validator.
    pub const fn new() -> Self {
        Self
    }
}

impl DeviceCsrValidator for ProductionDeviceCsrValidator {
    fn validate_and_hash_spki(
        &self,
        csr_der: &[u8],
    ) -> Result<[u8; 32], DeviceAuthorizationPortError> {
        validate_pkcs10_csr(csr_der).map_err(|()| DeviceAuthorizationPortError::Rejected)
    }
}

fn validate_pkcs10_csr(csr_der: &[u8]) -> Result<[u8; 32], ()> {
    if csr_der.is_empty() || csr_der.len() > MAX_CSR_DER_BYTES {
        return Err(());
    }

    let (remaining, parsed) = X509CertificationRequest::from_der(csr_der).map_err(|_| ())?;
    if !remaining.is_empty()
        || parsed.as_raw() != csr_der
        || parsed.certification_request_info.version != X509Version::V1
    {
        return Err(());
    }

    validate_request_attributes(&parsed)?;

    // OpenSSL performs the cryptographic proof-of-possession check. The parser
    // round-trip rejects BER, non-canonical DER, and trailing bytes first.
    let request = X509Req::from_der(csr_der).map_err(|_| ())?;
    if request.to_der().map_err(|_| ())?.as_slice() != csr_der {
        return Err(());
    }

    let public_key = request.public_key().map_err(|_| ())?;
    let spki = &parsed.certification_request_info.subject_pki;
    validate_public_key(spki, &public_key)?;
    validate_signature_algorithm(&parsed.signature_algorithm, spki, &public_key)?;

    if public_key.public_key_to_der().map_err(|_| ())?.as_slice() != spki.raw
        || !request.verify(&public_key).map_err(|_| ())?
    {
        return Err(());
    }

    Ok(Sha256::digest(spki.raw).into())
}

fn validate_request_attributes(request: &X509CertificationRequest<'_>) -> Result<(), ()> {
    let info = &request.certification_request_info;
    info.attributes_map().map_err(|_| ())?;
    let mut requested_extension_oids = BTreeSet::new();

    for attribute in info.iter_attributes() {
        match attribute.parsed_attribute() {
            ParsedCriAttribute::ExtensionRequest(requested) => {
                for extension in &requested.extensions {
                    if !requested_extension_oids.insert(extension.oid.to_id_string())
                        || matches!(
                            extension.parsed_extension(),
                            x509_parser::extensions::ParsedExtension::ParseError { .. }
                        )
                    {
                        return Err(());
                    }
                }
            }
            // Unknown attributes and challengePassword have no role in device
            // enrollment and are rejected to avoid ambiguous downstream use.
            ParsedCriAttribute::ChallengePassword(_) | ParsedCriAttribute::UnsupportedAttribute => {
                return Err(())
            }
        }
    }

    // `attributes_map` rejects repeated attribute OIDs, so extensionRequest
    // attributes cannot be silently collapsed.
    Ok(())
}

fn validate_public_key(
    spki: &SubjectPublicKeyInfo<'_>,
    public_key: &PKey<openssl::pkey::Public>,
) -> Result<(), ()> {
    let algorithm_oid = spki.algorithm.algorithm.to_id_string();
    match algorithm_oid.as_str() {
        OID_RSA_ENCRYPTION => {
            if public_key.id() != Id::RSA
                || !(MIN_RSA_BITS..=MAX_RSA_BITS).contains(&public_key.bits())
                || !has_null_parameters(&spki.algorithm)
            {
                return Err(());
            }
        }
        OID_EC_PUBLIC_KEY => {
            if public_key.id() != Id::EC {
                return Err(());
            }
            let curve_oid = oid_parameter(&spki.algorithm).ok_or(())?;
            let ec_key: EcKey<openssl::pkey::Public> = public_key.ec_key().map_err(|_| ())?;
            let expected_curve = match curve_oid.as_str() {
                OID_SECP256R1 => Nid::X9_62_PRIME256V1,
                OID_SECP384R1 => Nid::SECP384R1,
                _ => return Err(()),
            };
            if ec_key.group().curve_name() != Some(expected_curve) {
                return Err(());
            }
        }
        OID_ED25519 => {
            if public_key.id() != Id::ED25519 || spki.algorithm.parameters.is_some() {
                return Err(());
            }
        }
        _ => return Err(()),
    }
    Ok(())
}

fn validate_signature_algorithm(
    signature: &AlgorithmIdentifier<'_>,
    spki: &SubjectPublicKeyInfo<'_>,
    public_key: &PKey<openssl::pkey::Public>,
) -> Result<(), ()> {
    let signature_oid = signature.algorithm.to_id_string();
    let key_oid = spki.algorithm.algorithm.to_id_string();

    match signature_oid.as_str() {
        OID_RSA_WITH_SHA256 | OID_RSA_WITH_SHA384 | OID_RSA_WITH_SHA512 => {
            if key_oid != OID_RSA_ENCRYPTION
                || public_key.id() != Id::RSA
                || !has_null_parameters(signature)
            {
                return Err(());
            }
        }
        OID_RSA_PSS => {
            if key_oid != OID_RSA_ENCRYPTION
                || public_key.id() != Id::RSA
                || !rsa_pss_parameters_are_strong(signature)?
            {
                return Err(());
            }
        }
        OID_ECDSA_WITH_SHA256 | OID_ECDSA_WITH_SHA384 | OID_ECDSA_WITH_SHA512 => {
            if key_oid != OID_EC_PUBLIC_KEY || signature.parameters.is_some() {
                return Err(());
            }
        }
        OID_ED25519 => {
            if key_oid != OID_ED25519 || signature.parameters.is_some() {
                return Err(());
            }
        }
        // SHA-1, MD5, DSA, and unknown signature algorithms are not accepted.
        _ => return Err(()),
    }
    Ok(())
}

fn rsa_pss_parameters_are_strong(signature: &AlgorithmIdentifier<'_>) -> Result<bool, ()> {
    let Some(parameters) = signature.parameters.as_ref() else {
        return Ok(false);
    };
    if !parameters.header.is_universal()
        || !parameters.header.is_constructed()
        || parameters.tag().0 != 16
        || !pss_fields_are_canonical(parameters.data)
    {
        return Ok(false);
    }

    let pss = RsaSsaPssParams::try_from(parameters).map_err(|_| ())?;
    let hash_oid = pss.hash_algorithm_oid().to_id_string();
    let Some(hash_size) = hash_size_for_oid(hash_oid.as_str()) else {
        return Ok(false);
    };
    let Some(hash_algorithm) = pss.hash_algorithm() else {
        return Ok(false);
    };
    if !hash_parameters_are_canonical(hash_algorithm) {
        return Ok(false);
    }

    let Some(mask_generation) = pss.mask_gen_algorithm_raw() else {
        return Ok(false);
    };
    let mask_hash_oid = mask_generation_hash_oid(mask_generation);
    Ok(mask_generation.algorithm.to_id_string() == OID_MGF1
        && mask_hash_oid.as_deref() == Some(hash_oid.as_str())
        && pss.salt_length() == hash_size
        && pss.trailer_field() == 1)
}

fn mask_generation_hash_oid(algorithm: &AlgorithmIdentifier<'_>) -> Option<String> {
    if algorithm.algorithm.to_id_string() != OID_MGF1 {
        return None;
    }
    let parameters = algorithm.parameters.as_ref()?;
    if !parameters.header.is_universal()
        || !parameters.header.is_constructed()
        || parameters.tag().0 != 16
    {
        return None;
    }

    // id-mgf1 parameters encode a nested HashAlgorithm AlgorithmIdentifier SEQUENCE.
    let encoded_parameters = parameters.to_der_vec().ok()?;
    let (remaining, hash_algorithm) = AlgorithmIdentifier::from_der(&encoded_parameters).ok()?;
    if !remaining.is_empty() || !hash_parameters_are_canonical(&hash_algorithm) {
        return None;
    }
    Some(hash_algorithm.algorithm.to_id_string())
}

fn pss_fields_are_canonical(mut fields: &[u8]) -> bool {
    let mut previous_tag = 0_u8;
    let mut seen = 0_u8;

    while !fields.is_empty() {
        let Some((tag, value, remaining)) = read_der_element(fields) else {
            return false;
        };
        if !(0xa0..=0xa3).contains(&tag) || tag <= previous_tag {
            return false;
        }
        let Some((_, _, trailing)) = read_der_element(value) else {
            return false;
        };
        if !trailing.is_empty() {
            return false;
        }
        seen |= 1 << (tag - 0xa0);
        previous_tag = tag;
        fields = remaining;
    }

    // SHA-1 defaults are forbidden, so SHA-2 hash, MGF, and salt fields must be explicit.
    seen & 0b0000_0111 == 0b0000_0111
}

fn read_der_element(input: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, after_tag) = input.split_first()?;
    if tag & 0x1f == 0x1f {
        return None;
    }
    let (&first_length, after_length) = after_tag.split_first()?;
    let (length, value_start) = if first_length & 0x80 == 0 {
        (usize::from(first_length), after_length)
    } else {
        let length_octets = usize::from(first_length & 0x7f);
        if length_octets == 0 || length_octets > std::mem::size_of::<usize>() {
            return None;
        }
        let (encoded_length, remaining) = after_length.split_at_checked(length_octets)?;
        if encoded_length[0] == 0 {
            return None;
        }
        let mut length = 0_usize;
        for byte in encoded_length {
            length = length.checked_mul(256)?.checked_add(usize::from(*byte))?;
        }
        if length < 128 {
            return None;
        }
        (length, remaining)
    };
    let (value, remaining) = value_start.split_at_checked(length)?;
    Some((tag, value, remaining))
}

fn hash_size_for_oid(oid: &str) -> Option<u32> {
    match oid {
        OID_SHA256 => Some(32),
        OID_SHA384 => Some(48),
        OID_SHA512 => Some(64),
        _ => None,
    }
}

fn hash_parameters_are_canonical(algorithm: &AlgorithmIdentifier<'_>) -> bool {
    match algorithm.parameters.as_ref() {
        None => true,
        Some(parameters) => {
            parameters.header.is_universal()
                && !parameters.header.is_constructed()
                && parameters.tag().0 == 5
                && parameters.data.is_empty()
        }
    }
}

fn oid_parameter(algorithm: &AlgorithmIdentifier<'_>) -> Option<String> {
    let parameters = algorithm.parameters.as_ref()?;
    if !parameters.header.is_universal()
        || parameters.header.is_constructed()
        || parameters.tag().0 != 6
    {
        return None;
    }
    parameters.as_oid().ok().map(|oid| oid.to_id_string())
}

fn has_null_parameters(algorithm: &AlgorithmIdentifier<'_>) -> bool {
    algorithm.parameters.as_ref().is_some_and(|parameters| {
        parameters.header.is_universal()
            && !parameters.header.is_constructed()
            && parameters.tag().0 == 5
            && parameters.data.is_empty()
    })
}
