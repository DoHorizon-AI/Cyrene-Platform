# ╔══════════════════════════════════════════════════════════════════════╗
# ║ 📄 File: sdk/python/cyrene_artifacts/src/cy_artifacts/contracts.py
# ║ Module: CYRENE Platform
# ║ Role: Python SDK, TCK, or test module for this repository boundary.
# ║ 中文:Python SDK、TCK 或用于此仓库边界的测试模块。
# ║
# ║ 模块：CYRENE Platform
# ║ 职责：Python SDK、TCK 或测试模块。
# ╚══════════════════════════════════════════════════════════════════════╝
"""Provider-neutral Artifact Plane contracts.

The contract deliberately describes content identity and verified staging only.
Provider locations are returned by a provider and never become artifact
identity.  No Training, Dataset, Kernel, or cloud-provider types belong here.

中文:本模块定义与 Provider 无关的 Artifact Plane 契约。契约仅描述内容身份和已验证的暂存位置。Provider 返回的位置不会成为 Artifact 身份的一部分。这里不应包含 Training、Dataset、Kernel 或云 Provider 类型。
"""

from __future__ import annotations

import hashlib
import json
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Optional, Protocol, Sequence, Tuple


SHA256_PREFIX = "sha256:"
ARTIFACT_URI_PREFIX = "artifact://sha256/"
PORTABLE_DIRECTORY_MANIFEST_VERSION = 2
JCS_SAFE_INTEGER_MAX = 9_007_199_254_740_991


class ArtifactKind(str):
    """Opaque category owned by the Artifact producer.

    Platform validates the identifier shape and does not maintain a Product
    category taxonomy. New Product categories therefore need no Platform
    release.

    中文:Artifact 类别由 Artifact 生产者不透明地拥有。Platform 只验证标识符格式,不维护 Product 类别目录;因此新增 Product 类别无需发布新的 Platform 版本。
    """

    GENERIC: "ArtifactKind"

    def __new__(cls, value: object) -> "ArtifactKind":
        text = str(value)
        if not text.strip() or len(text) > 128 or any(ord(character) < 32 for character in text):
            raise ValueError("artifact kind must be a bounded non-empty identifier")
        return str.__new__(cls, text)

    @property
    def value(self) -> str:
        return str(self)


ArtifactKind.GENERIC = ArtifactKind("generic")


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
    """Validate a logical POSIX-relative path without normalizing it.

    中文:验证逻辑 POSIX 相对路径,但不对路径进行规范化。
    """

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

    中文:返回供目标文件系统别名检查使用的稳定 V2 大小写键。V2 只折叠 ASCII 字母。Unicode 大小写映射和规范化依赖 runtime 版本,因此不纳入以 Linux 为先的身份与冲突规则。
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
    """Stable content identity shared by all artifact providers.

    中文:所有 Artifact Provider 共用的稳定内容身份。
    """

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
class ArtifactDirectoryEntry:
    """One raw CAS file in a portable directory artifact.

    中文:portable 目录 Artifact 中的一个原始 CAS 文件。
    """

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
        """Validate this logical entry without reading the CAS blob.

        中文:验证此逻辑条目,不读取 CAS blob。
        """

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

    中文:跨语言的内容寻址目录索引,版本为 2。线上身份严格等于对 version、排序后的 files 和逻辑 size_bytes 进行 JCS 编码的结果。URI、时间戳、生产者元数据以及清单字节长度均有意排除在该记录之外。
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
    """Provider location returned only after existence and digest verification.

    中文:仅在确认内容存在且摘要验证通过后返回的 Provider 位置。
    """

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
        """Publish a directory through the public V2 directory contract.

        中文:依据公开的 V2 目录契约发布目录。
        """
        ...

    def resolve(self, artifact_ref: ArtifactRef) -> ResolvedArtifact: ...

    def verify(self, artifact_ref: ArtifactRef) -> bool: ...


class ArtifactStager(Protocol):
    def stage(self, artifact_ref: ArtifactRef, destination: str | Path) -> StagedArtifact: ...


__all__ = [
    "ARTIFACT_URI_PREFIX",
    "JCS_SAFE_INTEGER_MAX",
    "PORTABLE_DIRECTORY_MANIFEST_VERSION",
    "ArtifactDirectoryEntry",
    "ArtifactKind",
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
