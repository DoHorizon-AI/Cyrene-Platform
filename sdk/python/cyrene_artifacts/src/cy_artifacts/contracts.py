# ╔══════════════════════════════════════════════════════════════════════╗
# ║ 📄 File: sdk/python/cyrene_artifacts/src/cy_artifacts/contracts.py
# ║ Module: CYRENE Platform
# ║ Role: Python SDK, TCK, or test module for this repository boundary.
# ║
# ║ 模块：CYRENE Platform
# ║ 职责：Python SDK、TCK 或测试模块。
# ╚══════════════════════════════════════════════════════════════════════╝
"""Provider-neutral Artifact Plane contracts.

The contract deliberately describes content identity and verified staging only.
Provider locations are returned by a provider and never become artifact
identity.  No Training, Dataset, Kernel, or cloud-provider types belong here.
"""

from __future__ import annotations

import hashlib
import json
from dataclasses import dataclass
from enum import Enum
from pathlib import Path
from typing import Any, Optional, Protocol, Sequence, Tuple


SHA256_PREFIX = "sha256:"
ARTIFACT_URI_PREFIX = "artifact://sha256/"
ARTIFACT_MANIFEST_VERSION = 1
ARTIFACT_SCHEMA_VERSION = "1"


class ArtifactKind(str, Enum):
    GENERIC = "generic"
    TRAINING_SPEC = "training_spec"
    DATASET = "dataset"
    MODEL = "model"
    CHECKPOINT = "checkpoint"
    METRICS = "metrics"
    REPORT = "report"
    MERGED = "merged"
    QUANTIZED = "quantized"


def _validate_digest(value: str) -> str:
    digest = str(value)
    if not digest.startswith(SHA256_PREFIX) or len(digest) != len(SHA256_PREFIX) + 64:
        raise ValueError(f"invalid SHA-256 digest: {digest!r}")
    try:
        int(digest[len(SHA256_PREFIX) :], 16)
    except ValueError as exc:
        raise ValueError(f"invalid SHA-256 digest: {digest!r}") from exc
    return digest


def artifact_uri_for_digest(digest: str) -> str:
    normalized = _validate_digest(digest)
    return ARTIFACT_URI_PREFIX + normalized[len(SHA256_PREFIX) :]


def sha256_bytes(payload: bytes) -> str:
    return SHA256_PREFIX + hashlib.sha256(payload).hexdigest()


def sha256_file(path: str | Path, *, chunk_size: int = 1024 * 1024) -> Tuple[str, int]:
    hasher = hashlib.sha256()
    size_bytes = 0
    with Path(path).open("rb") as handle:
        for chunk in iter(lambda: handle.read(chunk_size), b""):
            hasher.update(chunk)
            size_bytes += len(chunk)
    return SHA256_PREFIX + hasher.hexdigest(), size_bytes


def canonical_json_bytes(payload: Any) -> bytes:
    return json.dumps(payload, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode(
        "utf-8"
    )


_canonical_json = canonical_json_bytes


@dataclass(frozen=True)
class ArtifactRef:
    """Stable content identity shared by all artifact providers."""

    uri: str
    digest: str
    size_bytes: int
    kind: ArtifactKind
    manifest_digest: Optional[str] = None
    name: Optional[str] = None

    def __post_init__(self) -> None:
        digest = _validate_digest(self.digest)
        object.__setattr__(self, "digest", digest)
        object.__setattr__(self, "kind", ArtifactKind(self.kind))
        if self.size_bytes < 0:
            raise ValueError("artifact size must be non-negative")
        expected_uri = artifact_uri_for_digest(digest)
        if not self.uri:
            object.__setattr__(self, "uri", expected_uri)
        elif self.uri != expected_uri:
            raise ValueError("artifact URI must be the provider-neutral content URI")
        if self.manifest_digest is not None:
            _validate_digest(self.manifest_digest)

    def to_dict(self) -> dict[str, Any]:
        payload: dict[str, Any] = {
            "uri": self.uri,
            "digest": self.digest,
            "size_bytes": self.size_bytes,
            "kind": self.kind.value,
        }
        if self.manifest_digest is not None:
            payload["manifest_digest"] = self.manifest_digest
        return payload

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "ArtifactRef":
        return cls(
            uri=str(data.get("uri") or ""),
            digest=str(data["digest"]),
            size_bytes=int(data["size_bytes"]),
            kind=ArtifactKind(data.get("kind") or ArtifactKind.GENERIC.value),
            manifest_digest=data.get("manifest_digest"),
            name=data.get("name"),
        )


@dataclass(frozen=True)
class ArtifactLineage:
    """Canonical lineage projection of cy-manifest::Lineage."""

    base_model_revision: Optional[str] = None
    dataset_digest: Optional[str] = None
    runtime_id: Optional[str] = None
    revision_chain: Tuple[str, ...] = ()
    checkpoint_id: Optional[str] = None

    def to_dict(self) -> dict[str, Any]:
        payload: dict[str, Any] = {"revision_chain": list(self.revision_chain)}
        if self.base_model_revision is not None:
            payload["base_model_revision"] = self.base_model_revision
        if self.dataset_digest is not None:
            payload["dataset_digest"] = self.dataset_digest
        if self.runtime_id is not None:
            payload["runtime_id"] = self.runtime_id
        if self.checkpoint_id is not None:
            payload["checkpoint_id"] = self.checkpoint_id
        return payload

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "ArtifactLineage":
        return cls(
            base_model_revision=data.get("base_model_revision"),
            dataset_digest=data.get("dataset_digest"),
            runtime_id=data.get("runtime_id"),
            revision_chain=tuple(str(item) for item in data.get("revision_chain") or []),
            checkpoint_id=data.get("checkpoint_id"),
        )


@dataclass(frozen=True)
class ArtifactManifest:
    """Canonical public manifest projection of cy-manifest::ArtifactManifest."""

    kind: ArtifactKind
    integrity: str
    lineage: ArtifactLineage
    schema_version: Optional[str] = None
    artifact_id: Optional[str] = None
    source: Optional[str] = None
    size_bytes: Optional[int] = None
    ref_count: Optional[int] = None

    def __post_init__(self) -> None:
        object.__setattr__(self, "kind", ArtifactKind(self.kind))
        _validate_digest(self.integrity)
        if self.artifact_id is not None:
            _validate_digest(self.artifact_id)
        if self.size_bytes is not None and self.size_bytes < 0:
            raise ValueError("artifact manifest size must be non-negative")
        if self.ref_count is not None and self.ref_count < 0:
            raise ValueError("artifact manifest ref_count must be non-negative")

    def identity_payload(self) -> dict[str, Any]:
        """Return the cy-manifest record without its computed artifact_id."""

        return self.to_dict(include_artifact_id=False)

    def computed_artifact_id(self) -> str:
        return sha256_bytes(_canonical_json(self.identity_payload()))

    def verify_artifact_id(self) -> bool:
        return bool(self.artifact_id) and self.artifact_id == self.computed_artifact_id()

    def to_dict(self, *, include_artifact_id: bool = True) -> dict[str, Any]:
        payload: dict[str, Any] = {
            "kind": self.kind.value,
            "integrity": self.integrity,
            "lineage": self.lineage.to_dict(),
        }
        if self.schema_version is not None:
            payload["schema_version"] = self.schema_version
        if include_artifact_id and self.artifact_id is not None:
            payload["artifact_id"] = self.artifact_id
        if self.source is not None:
            payload["source"] = self.source
        if self.size_bytes is not None:
            payload["size_bytes"] = self.size_bytes
        if self.ref_count is not None:
            payload["ref_count"] = self.ref_count
        return payload

    def to_bytes(self) -> bytes:
        return _canonical_json(self.to_dict())

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "ArtifactManifest":
        return cls(
            kind=ArtifactKind(data.get("kind") or ArtifactKind.GENERIC.value),
            integrity=str(data["integrity"]),
            lineage=ArtifactLineage.from_dict(data.get("lineage") or {}),
            schema_version=data.get("schema_version"),
            artifact_id=data.get("artifact_id"),
            source=data.get("source"),
            size_bytes=int(data["size_bytes"]) if data.get("size_bytes") is not None else None,
            ref_count=int(data["ref_count"]) if data.get("ref_count") is not None else None,
        )

    @classmethod
    def from_bytes(cls, payload: bytes) -> "ArtifactManifest":
        return cls.from_dict(json.loads(payload.decode("utf-8")))


@dataclass(frozen=True)
class ResolvedArtifact:
    """Provider location returned only after existence and digest verification."""

    artifact_ref: ArtifactRef
    location: Path
    manifest: Optional[Any] = None


@dataclass(frozen=True)
class StagedArtifact:
    artifact_ref: ArtifactRef
    local_path: str
    worker_visible_path: str
    verified_digest: str


class ArtifactProvider(Protocol):
    def publish(
        self,
        source: str | Path,
        *,
        kind: ArtifactKind = ArtifactKind.GENERIC,
        producer: Optional[str] = None,
        format: Optional[str] = None,
        schema: Optional[str] = None,
        inputs: Sequence[ArtifactRef] = (),
    ) -> ArtifactRef:
        ...

    def publish_bytes(
        self,
        payload: bytes,
        *,
        kind: ArtifactKind = ArtifactKind.GENERIC,
        producer: Optional[str] = None,
        format: Optional[str] = None,
        schema: Optional[str] = None,
        inputs: Sequence[ArtifactRef] = (),
    ) -> ArtifactRef:
        ...

    def resolve(self, artifact_ref: ArtifactRef) -> ResolvedArtifact:
        ...

    def verify(self, artifact_ref: ArtifactRef) -> bool:
        ...


class ArtifactStager(Protocol):
    def stage(self, artifact_ref: ArtifactRef, destination: str | Path) -> StagedArtifact:
        ...


__all__ = [
    "ARTIFACT_MANIFEST_VERSION",
    "ARTIFACT_SCHEMA_VERSION",
    "ARTIFACT_URI_PREFIX",
    "ArtifactLineage",
    "ArtifactKind",
    "ArtifactManifest",
    "ArtifactProvider",
    "ArtifactRef",
    "ArtifactStager",
    "ResolvedArtifact",
    "SHA256_PREFIX",
    "StagedArtifact",
    "artifact_uri_for_digest",
    "canonical_json_bytes",
    "sha256_bytes",
    "sha256_file",
]
