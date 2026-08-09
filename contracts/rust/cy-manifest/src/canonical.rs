//! Deterministic canonical JSON serialization (RFC 8785 / JCS).
//!
//! This implements the scheme specified in `schemas/CANONICALIZATION.md`, which
//! is now RFC 8785 (JSON Canonicalization Scheme) including ES6/ECMA-262 number
//! formatting. It uses the `serde_jcs` crate so that this stays byte-for-byte
//! identical to the Python implementation in
//! `framework/sdk/python/cy-manifest/src/cy_manifest/canonical.py`, which uses the RFC 8785
//! `rfc8785` library.

use serde_json::Value;
use sha2::{Digest, Sha256};

/// Serialize a JSON value into canonical bytes per RFC 8785 (JCS).
///
/// See `schemas/CANONICALIZATION.md`.
pub fn canonicalize(value: &Value) -> Vec<u8> {
    serde_jcs::to_vec(value).expect("JSON value is serializable to RFC 8785 canonical form")
}

/// SHA-256 of the canonical bytes, formatted as lowercase hex.
pub fn canonical_sha256_hex(value: &Value) -> String {
    let bytes = canonicalize(value);
    let digest = Sha256::digest(&bytes);
    to_hex(&digest)
}

fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(char::from_digit((b >> 4) as u32, 16).unwrap());
        s.push(char::from_digit((b & 0x0f) as u32, 16).unwrap());
    }
    s
}
