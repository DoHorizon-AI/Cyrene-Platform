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
import re
from collections.abc import Mapping
from dataclasses import dataclass, field
from enum import Enum
from pathlib import Path
from types import MappingProxyType
from typing import Any, Optional, Protocol, Sequence, Tuple


SHA256_PREFIX = "sha256:"
ARTIFACT_URI_PREFIX = "artifact://sha256/"
ARTIFACT_MANIFEST_VERSION = 1
ARTIFACT_SCHEMA_VERSION = "1"
PORTABLE_DIRECTORY_MANIFEST_VERSION = 2
JCS_SAFE_INTEGER_MAX = 9_007_199_254_740_991
MODEL_VERSION_SCHEMA_VERSION = "1"
MODEL_VERSION_URI_PREFIX = "model-version://sha256/"


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


_MODEL_VERSION_ID_PATTERN = re.compile(r"^model-version://sha256/[0-9a-f]{64}$")
_RESOURCE_URI_PATTERN = re.compile(r"^[A-Za-z][A-Za-z0-9+.-]*:[^\s]+$")
_REPOSITORY_PATTERN = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$")
_REVISION_PATTERN = re.compile(r"^[0-9a-f]{40}$")


def _validate_digest(value: str) -> str:
    digest = str(value)
    if not digest.startswith(SHA256_PREFIX) or len(digest) != len(SHA256_PREFIX) + 64:
        raise ValueError(f"invalid SHA-256 digest: {digest!r}")
    try:
        int(digest[len(SHA256_PREFIX) :], 16)
    except ValueError as exc:
        raise ValueError(f"invalid SHA-256 digest: {digest!r}") from exc
    return digest


def _validate_lowercase_digest(value: str) -> str:
    if (
        not isinstance(value, str)
        or not value.startswith(SHA256_PREFIX)
        or len(value) != len(SHA256_PREFIX) + 64
        or any(character not in "0123456789abcdef" for character in value[len(SHA256_PREFIX) :])
    ):
        raise ValueError(f"digest must be lowercase sha256:<hex>: {value!r}")
    return value


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
    return json.dumps(payload, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")


_canonical_json = canonical_json_bytes


def _validate_portable_path(path: str) -> str:
    """Validate a logical POSIX-relative path without normalizing it."""

    if (
        not path
        or path.startswith("/")
        or path.endswith("/")
        or "\\" in path
        or "\x00" in path
        or "//" in path
        or path.startswith("./")
    ):
        raise ValueError(f"portable directory path is not canonical POSIX-relative: {path!r}")
    parts = path.split("/")
    if any(part in {"", ".", ".."} for part in parts):
        raise ValueError(f"portable directory path contains a non-canonical component: {path!r}")
    first = parts[0]
    if len(first) >= 2 and first[0].isascii() and first[0].isalpha() and first[1] == ":":
        raise ValueError(f"portable directory path contains a Windows drive prefix: {path!r}")
    if any(ord(character) < 32 or 0x7F <= ord(character) <= 0x9F for character in path):
        raise ValueError(f"portable directory path contains a control character: {path!r}")
    return path


def _ascii_casefold_path(path: str) -> str:
    """Return the stable V2 case key used for target-filesystem aliases.

    V2 deliberately folds ASCII letters only.  Unicode case mapping and
    normalization are runtime-version dependent and therefore stay outside
    the Linux-first identity and collision rules.
    """

    return "/".join(
        "".join(chr(ord(character) + 32) if "A" <= character <= "Z" else character for character in part)
        for part in path.split("/")
    )


def _reject_duplicate_json_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"duplicate JSON object key: {key!r}")
        result[key] = value
    return result


def _safe_integer(value: Any, field: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int):
        raise ValueError(f"{field} must be an integer")
    if value < 0 or value > JCS_SAFE_INTEGER_MAX:
        raise ValueError(f"{field} must fit the JCS safe integer range")
    return value


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


def _as_json_object(value: Any, field_name: str) -> dict[str, Any]:
    if not isinstance(value, Mapping):
        raise ValueError(f"{field_name} must be a JSON object")
    result = dict(value)
    if any(not isinstance(key, str) for key in result):
        raise ValueError(f"{field_name} object keys must be strings")
    return result


def _expect_json_keys(
    value: Mapping[str, Any],
    *,
    required: set[str],
    optional: set[str],
    field_name: str,
) -> None:
    keys = set(value)
    unknown = keys - required - optional
    missing = required - keys
    if unknown:
        raise ValueError(f"{field_name} has unknown fields: {sorted(unknown)!r}")
    if missing:
        raise ValueError(f"{field_name} is missing fields: {sorted(missing)!r}")


def _reject_floats(value: Any, *, path: str = "$") -> None:
    """Reject values that cannot be represented without JCS number drift.

    ModelVersion V1 intentionally carries only strings, booleans, integers,
    arrays, objects, and null.  This keeps the Python canonical serializer
    byte-compatible with the JCS value subset used by the platform manifests.
    """

    if isinstance(value, float):
        raise ValueError(f"{path} must not contain floating-point numbers")
    if isinstance(value, Mapping):
        for key, child in value.items():
            _reject_floats(child, path=f"{path}.{key}")
    elif isinstance(value, list):
        for index, child in enumerate(value):
            _reject_floats(child, path=f"{path}[{index}]")


def _validate_resource_uri(value: Any, field_name: str) -> str:
    if not isinstance(value, str) or not value or not _RESOURCE_URI_PATTERN.fullmatch(value):
        raise ValueError(f"{field_name} must be a non-empty opaque URI")
    if any(ord(character) < 32 or 0x7F <= ord(character) <= 0x9F for character in value):
        raise ValueError(f"{field_name} must not contain control characters")
    return value


def _normalize_artifact_ref(
    value: Any,
    *,
    field_name: str,
    require_portable_model: bool = False,
) -> dict[str, Any]:
    data = _as_json_object(value, field_name)
    _expect_json_keys(
        data,
        required={"uri", "digest", "size_bytes", "kind"},
        optional={"manifest_digest"},
        field_name=field_name,
    )

    uri = data["uri"]
    digest = data["digest"]
    if not isinstance(uri, str) or not isinstance(digest, str):
        raise ValueError(f"{field_name}.uri and .digest must be strings")
    _validate_lowercase_digest(digest)
    if uri != artifact_uri_for_digest(digest):
        raise ValueError(f"{field_name}.uri must match its digest")

    size_bytes = _safe_integer(data["size_bytes"], f"{field_name}.size_bytes")
    kind_value = data["kind"]
    if not isinstance(kind_value, str):
        raise ValueError(f"{field_name}.kind must be a string")
    try:
        kind = ArtifactKind(kind_value)
    except ValueError as exc:
        raise ValueError(f"{field_name}.kind is not a supported ArtifactKind") from exc

    has_manifest_digest = "manifest_digest" in data
    manifest_digest = data.get("manifest_digest")
    if has_manifest_digest:
        if not isinstance(manifest_digest, str) or not manifest_digest:
            raise ValueError(f"{field_name}.manifest_digest must be a non-empty string")
        _validate_lowercase_digest(manifest_digest)

    if require_portable_model:
        if kind is not ArtifactKind.MODEL:
            raise ValueError(f"{field_name} must use kind 'model'")
        if manifest_digest != digest:
            raise ValueError(f"{field_name} must reference a portable directory with manifest_digest equal to digest")

    normalized: dict[str, Any] = {
        "uri": uri,
        "digest": digest,
        "size_bytes": size_bytes,
        "kind": kind.value,
    }
    if has_manifest_digest:
        normalized["manifest_digest"] = manifest_digest
    return normalized


def _normalize_override(value: Any, *, field_name: str) -> dict[str, Any]:
    data = _as_json_object(value, field_name)
    _expect_json_keys(
        data,
        required={"mode"},
        optional={"artifact"},
        field_name=field_name,
    )
    mode = data["mode"]
    if mode == "INHERIT":
        if "artifact" in data:
            raise ValueError(f"{field_name}.artifact is forbidden when mode is INHERIT")
        return {"mode": mode}
    if mode == "OVERRIDE":
        if "artifact" not in data:
            raise ValueError(f"{field_name}.artifact is required when mode is OVERRIDE")
        return {
            "mode": mode,
            "artifact": _normalize_artifact_ref(data["artifact"], field_name=f"{field_name}.artifact"),
        }
    raise ValueError(f"{field_name}.mode must be INHERIT or OVERRIDE")


def _normalize_lineage(value: Any) -> dict[str, Any]:
    data = _as_json_object(value, "lineage")
    _expect_json_keys(
        data,
        required=set(),
        optional={"trainingRun", "datasetVersion", "inputArtifacts", "derivedFromModelVersion"},
        field_name="lineage",
    )
    normalized: dict[str, Any] = {}
    if "trainingRun" in data:
        normalized["trainingRun"] = _validate_resource_uri(data["trainingRun"], "lineage.trainingRun")
    if "datasetVersion" in data:
        normalized["datasetVersion"] = _validate_resource_uri(data["datasetVersion"], "lineage.datasetVersion")
    if "inputArtifacts" in data:
        input_artifacts = data["inputArtifacts"]
        if not isinstance(input_artifacts, list):
            raise ValueError("lineage.inputArtifacts must be an array")
        normalized["inputArtifacts"] = [
            _normalize_artifact_ref(item, field_name=f"lineage.inputArtifacts[{index}]")
            for index, item in enumerate(input_artifacts)
        ]
    if "derivedFromModelVersion" in data:
        derived = data["derivedFromModelVersion"]
        if not isinstance(derived, str) or not _MODEL_VERSION_ID_PATTERN.fullmatch(derived):
            raise ValueError("lineage.derivedFromModelVersion must be a model-version://sha256/<hex> URI")
        normalized["derivedFromModelVersion"] = derived
    return normalized


def _normalize_base_model(value: Any) -> dict[str, Any]:
    data = _as_json_object(value, "baseModel")
    _expect_json_keys(
        data,
        required={"artifact", "source"},
        optional=set(),
        field_name="baseModel",
    )
    source = _as_json_object(data["source"], "baseModel.source")
    _expect_json_keys(
        source,
        required={"repository", "revision"},
        optional=set(),
        field_name="baseModel.source",
    )
    repository = source["repository"]
    revision = source["revision"]
    if not isinstance(repository, str) or not _REPOSITORY_PATTERN.fullmatch(repository):
        raise ValueError("baseModel.source.repository must use the owner/repo form")
    if not isinstance(revision, str) or not _REVISION_PATTERN.fullmatch(revision):
        raise ValueError("baseModel.source.revision must be a lowercase 40-hex revision")
    return {
        "artifact": _normalize_artifact_ref(
            data["artifact"], field_name="baseModel.artifact", require_portable_model=True
        ),
        "source": {"repository": repository, "revision": revision},
    }


def _normalize_model_version_payload(
    payload: Any,
    *,
    require_id: bool,
) -> dict[str, Any]:
    _reject_floats(payload)
    data = _as_json_object(payload, "model version")
    common_required = {"schemaVersion", "composition", "tokenizer", "chatTemplate", "lineage"}
    common_optional = {"id"}
    composition = data.get("composition")
    if composition == "FULL_MODEL":
        _expect_json_keys(
            data,
            required=common_required | {"fullModelArtifact"} | ({"id"} if require_id else set()),
            optional=common_optional if not require_id else set(),
            field_name="model version",
        )
    elif composition == "BASE_PLUS_LORA":
        _expect_json_keys(
            data,
            required=common_required | {"baseModel", "adapterArtifact"} | ({"id"} if require_id else set()),
            optional=common_optional if not require_id else set(),
            field_name="model version",
        )
    else:
        raise ValueError("model version composition must be FULL_MODEL or BASE_PLUS_LORA")

    schema_version = data["schemaVersion"]
    if schema_version != MODEL_VERSION_SCHEMA_VERSION:
        raise ValueError(f"model version schemaVersion must be {MODEL_VERSION_SCHEMA_VERSION!r}")
    if not isinstance(composition, str):
        raise ValueError("model version composition must be a string")

    normalized: dict[str, Any] = {
        "schemaVersion": schema_version,
        "composition": composition,
    }
    if require_id:
        model_id = data["id"]
        if not isinstance(model_id, str) or not _MODEL_VERSION_ID_PATTERN.fullmatch(model_id):
            raise ValueError("model version id must be a model-version://sha256/<hex> URI")
        normalized["id"] = model_id

    if composition == "FULL_MODEL":
        normalized["fullModelArtifact"] = _normalize_artifact_ref(
            data["fullModelArtifact"],
            field_name="fullModelArtifact",
            require_portable_model=True,
        )
    else:
        normalized["baseModel"] = _normalize_base_model(data["baseModel"])
        normalized["adapterArtifact"] = _normalize_artifact_ref(
            data["adapterArtifact"],
            field_name="adapterArtifact",
            require_portable_model=True,
        )
        if normalized["baseModel"]["artifact"]["digest"] == normalized["adapterArtifact"]["digest"]:
            raise ValueError("baseModel.artifact and adapterArtifact must be different artifacts")

    normalized["tokenizer"] = _normalize_override(data["tokenizer"], field_name="tokenizer")
    normalized["chatTemplate"] = _normalize_override(data["chatTemplate"], field_name="chatTemplate")
    normalized["lineage"] = _normalize_lineage(data["lineage"])
    return normalized


def _freeze_json(value: Any) -> Any:
    if isinstance(value, dict):
        return MappingProxyType({key: _freeze_json(child) for key, child in value.items()})
    if isinstance(value, list):
        return tuple(_freeze_json(child) for child in value)
    return value


def _thaw_json(value: Any) -> Any:
    if isinstance(value, Mapping):
        return {key: _thaw_json(child) for key, child in value.items()}
    if isinstance(value, tuple):
        return [_thaw_json(child) for child in value]
    return value


def _model_version_id(identity_payload: Mapping[str, Any]) -> str:
    digest = hashlib.sha256(canonical_json_bytes(identity_payload)).hexdigest()
    return MODEL_VERSION_URI_PREFIX + digest


@dataclass(frozen=True, slots=True, init=False)
class ModelVersion:
    """Immutable canonical model composition consumed by Product contracts.

    This helper owns only the content-addressed descriptor.  TrainingRun is
    owned by Yield and DatasetVersion is owned by Catalyst; both appear here
    only as opaque lineage references.  LoRA rank, alpha, and target modules
    remain in the adapter payload and are intentionally absent from this
    descriptor.
    """

    _payload: Mapping[str, Any] = field(repr=False, compare=False)
    _id: str = field(repr=False)

    def __new__(cls, *_args: Any, **_kwargs: Any) -> "ModelVersion":
        raise TypeError("ModelVersion instances must be created with create() or from_dict()")

    @classmethod
    def _from_normalized(cls, payload: Mapping[str, Any]) -> "ModelVersion":
        """Build an instance after create/from_dict have completed validation."""

        model_id = payload["id"]
        instance = object.__new__(cls)
        object.__setattr__(instance, "_payload", _freeze_json(dict(payload)))
        object.__setattr__(instance, "_id", model_id)
        return instance

    @classmethod
    def create(cls, payload_without_id: Mapping[str, Any]) -> "ModelVersion":
        data = _as_json_object(payload_without_id, "model version")
        if "id" in data:
            raise ValueError("ModelVersion.create expects the id field to be omitted")
        normalized = _normalize_model_version_payload(data, require_id=False)
        model_id = _model_version_id(normalized)
        complete = dict(normalized)
        complete["id"] = model_id
        return cls._from_normalized(complete)

    @classmethod
    def from_dict(cls, payload: Mapping[str, Any]) -> "ModelVersion":
        normalized = _normalize_model_version_payload(payload, require_id=True)
        model_id = normalized["id"]
        identity_payload = {key: value for key, value in normalized.items() if key != "id"}
        expected_id = _model_version_id(identity_payload)
        if model_id != expected_id:
            raise ValueError(f"model version id does not match its immutable content: expected {expected_id}")
        return cls._from_normalized(normalized)

    @property
    def id(self) -> str:
        return self._id

    @property
    def composition(self) -> str:
        return self._payload["composition"]

    def identity_payload(self) -> dict[str, Any]:
        """Return the canonical hash preimage without the computed id."""

        return {key: _thaw_json(value) for key, value in self._payload.items() if key != "id"}

    def canonical_bytes(self) -> bytes:
        return canonical_json_bytes(self.identity_payload())

    def to_dict(self) -> dict[str, Any]:
        """Return a detached JSON-compatible copy of the immutable value."""

        return _thaw_json(self._payload)


@dataclass(frozen=True)
class ArtifactDirectoryEntry:
    """One raw CAS file in a portable directory artifact."""

    path: str
    digest: str
    size_bytes: int

    def __post_init__(self) -> None:
        if not isinstance(self.path, str):
            raise TypeError("portable directory file path must be a string")
        if not isinstance(self.digest, str):
            raise TypeError("portable directory file digest must be a string")
        _validate_portable_path(self.path)
        _validate_lowercase_digest(self.digest)
        size_bytes = _safe_integer(self.size_bytes, "portable directory file size")
        object.__setattr__(self, "size_bytes", size_bytes)

    def validate(self) -> None:
        """Validate this logical entry without reading the CAS blob."""

        _validate_portable_path(self.path)
        _validate_lowercase_digest(self.digest)

    def to_dict(self) -> dict[str, Any]:
        return {"path": self.path, "digest": self.digest, "size_bytes": self.size_bytes}

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "ArtifactDirectoryEntry":
        if set(data) != {"path", "digest", "size_bytes"}:
            raise ValueError("portable directory file entry has unknown or missing fields")
        if not isinstance(data["path"], str) or not isinstance(data["digest"], str):
            raise ValueError("portable directory file path and digest must be strings")
        return cls(
            path=data["path"],
            digest=data["digest"],
            size_bytes=_safe_integer(data["size_bytes"], "portable directory file size"),
        )


@dataclass(frozen=True)
class PortableDirectoryManifest:
    """Cross-language content-addressed directory index, version 2.

    The wire identity is exactly the JCS encoding of ``version``, sorted
    ``files`` and logical ``size_bytes``.  URI, timestamps, producer metadata,
    and the manifest byte length are deliberately outside this record.
    """

    version: int
    files: Tuple[ArtifactDirectoryEntry, ...]
    size_bytes: int

    def __post_init__(self) -> None:
        version = _safe_integer(self.version, "portable directory manifest version")
        size_bytes = _safe_integer(self.size_bytes, "portable directory logical size")
        object.__setattr__(self, "version", version)
        object.__setattr__(self, "size_bytes", size_bytes)
        object.__setattr__(self, "files", tuple(self.files))
        self.validate()

    def validate(self) -> None:
        if self.version != PORTABLE_DIRECTORY_MANIFEST_VERSION:
            raise ValueError(
                f"portable directory manifest version must be {PORTABLE_DIRECTORY_MANIFEST_VERSION}, got {self.version}"
            )
        paths = [entry.path for entry in self.files]
        if paths != sorted(paths) or len(paths) != len(set(paths)):
            raise ValueError("portable directory files must be strictly sorted with unique paths")

        folded_prefixes: dict[str, str] = {}
        logical_size = 0
        for entry in self.files:
            if not isinstance(entry, ArtifactDirectoryEntry):
                raise TypeError("portable directory files must be ArtifactDirectoryEntry values")
            entry.validate()
            prefix: list[str] = []
            for component in entry.path.split("/"):
                prefix.append(component)
                prefix_path = "/".join(prefix)
                folded_path = _ascii_casefold_path(prefix_path)
                existing = folded_prefixes.setdefault(folded_path, prefix_path)
                if existing != prefix_path:
                    raise ValueError(
                        "portable directory contains an ASCII case-insensitive path collision: "
                        f"{existing!r} and {prefix_path!r}"
                    )
            logical_size += entry.size_bytes

        if logical_size != self.size_bytes:
            raise ValueError(
                f"portable directory logical size mismatch: declared {self.size_bytes}, computed {logical_size}"
            )

        exact_paths = set(paths)
        for path in paths:
            components = path.split("/")
            prefix = []
            for component in components[:-1]:
                prefix.append(component)
                candidate = "/".join(prefix)
                if candidate in exact_paths:
                    raise ValueError(f"portable directory has a file/directory path conflict at {candidate!r}")

    def to_dict(self) -> dict[str, Any]:
        return {
            "version": self.version,
            "files": [entry.to_dict() for entry in self.files],
            "size_bytes": self.size_bytes,
        }

    def canonical_bytes(self) -> bytes:
        return _canonical_json(self.to_dict())

    def computed_digest(self) -> str:
        return sha256_bytes(self.canonical_bytes())

    def to_artifact_ref(self, kind: ArtifactKind = ArtifactKind.GENERIC) -> ArtifactRef:
        digest = self.computed_digest()
        return ArtifactRef(
            uri=artifact_uri_for_digest(digest),
            digest=digest,
            size_bytes=self.size_bytes,
            kind=kind,
            manifest_digest=digest,
        )

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "PortableDirectoryManifest":
        if set(data) != {"version", "files", "size_bytes"}:
            raise ValueError("portable directory manifest has unknown or missing fields")
        if not isinstance(data["files"], list):
            raise ValueError("portable directory manifest files must be an array")
        return cls(
            version=_safe_integer(data["version"], "portable directory manifest version"),
            files=tuple(ArtifactDirectoryEntry.from_dict(item) for item in data["files"]),
            size_bytes=_safe_integer(data["size_bytes"], "portable directory logical size"),
        )

    @classmethod
    def from_bytes(cls, payload: bytes) -> "PortableDirectoryManifest":
        value = json.loads(
            payload.decode("utf-8"),
            object_pairs_hook=_reject_duplicate_json_keys,
        )
        if not isinstance(value, dict):
            raise ValueError("portable directory manifest must be a JSON object")
        return cls.from_dict(value)


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
    ) -> ArtifactRef: ...

    def publish_bytes(
        self,
        payload: bytes,
        *,
        kind: ArtifactKind = ArtifactKind.GENERIC,
        producer: Optional[str] = None,
        format: Optional[str] = None,
        schema: Optional[str] = None,
        inputs: Sequence[ArtifactRef] = (),
    ) -> ArtifactRef: ...

    def publish_portable_directory(
        self,
        source: str | Path,
        *,
        kind: ArtifactKind = ArtifactKind.GENERIC,
    ) -> ArtifactRef:
        """Publish a directory through the public V2 directory contract."""
        ...

    def resolve(self, artifact_ref: ArtifactRef) -> ResolvedArtifact: ...

    def verify(self, artifact_ref: ArtifactRef) -> bool: ...


class ArtifactStager(Protocol):
    def stage(self, artifact_ref: ArtifactRef, destination: str | Path) -> StagedArtifact: ...


__all__ = [
    "ARTIFACT_MANIFEST_VERSION",
    "ARTIFACT_SCHEMA_VERSION",
    "ARTIFACT_URI_PREFIX",
    "JCS_SAFE_INTEGER_MAX",
    "PORTABLE_DIRECTORY_MANIFEST_VERSION",
    "ArtifactDirectoryEntry",
    "ArtifactLineage",
    "ArtifactKind",
    "ArtifactManifest",
    "ArtifactProvider",
    "ArtifactRef",
    "ArtifactStager",
    "PortableDirectoryManifest",
    "ResolvedArtifact",
    "SHA256_PREFIX",
    "StagedArtifact",
    "artifact_uri_for_digest",
    "canonical_json_bytes",
    "sha256_bytes",
    "sha256_file",
]
