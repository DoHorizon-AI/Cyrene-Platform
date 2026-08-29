"""Deterministic canonical JSON serialization (RFC 8785 / JCS).

This implements the scheme specified in ``schemas/CANONICALIZATION.md``, which is
now RFC 8785 (JSON Canonicalization Scheme) including ES6/ECMA-262 number
formatting. It delegates to the ``jcs`` library (the RFC 8785 reference
implementation) so that this stays byte-for-byte identical to the Rust
implementation in ``contracts/rust/cy-manifest/src/canonical.rs`` (which uses the
``serde_jcs`` crate).
"""

from __future__ import annotations

import hashlib
from typing import Any

import jcs

__all__ = ["canonicalize", "canonical_sha256_hex"]


def canonicalize(value: Any) -> bytes:
    """Serialize a JSON-compatible value into RFC 8785 canonical UTF-8 bytes."""
    return jcs.canonicalize(value)


def canonical_sha256_hex(value: Any) -> str:
    """Lowercase hex SHA-256 of :func:`canonicalize`."""
    return hashlib.sha256(canonicalize(value)).hexdigest()
