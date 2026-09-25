# ╔══════════════════════════════════════════════════════════════════════╗
# ║ 📄 File: sdk/python/cyrene_artifacts/src/cy_artifacts/local.py
# ║ Module: CYRENE Platform
# ║ Role: Python SDK, TCK, or test module for this repository boundary.
# ║ 中文：Python SDK、TCK 或用于此仓库边界的测试模块。
# ║
# ║ 模块：CYRENE Platform
# ║ 职责：Python SDK、TCK 或测试模块。
# ╚══════════════════════════════════════════════════════════════════════╝
"""Local filesystem Artifact Provider and Stager.

All content is copied with bounded chunks.  Files are committed to a
content-addressed store through a temporary file and a single atomic directory
entry operation.  Directories are represented by manifests whose entries point
at individual immutable blobs.

中文：本模块实现本地文件系统 Artifact Provider 和 Stager。所有内容都按有界块复制。文件先写入临时文件，再通过一次原子目录项操作提交到内容寻址存储。目录使用清单表示，清单条目指向各个不可变 blob。
"""

from __future__ import annotations

import os
import hashlib
import json
import shutil
import tempfile
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path, PurePosixPath
from typing import Any, Optional, Sequence, Tuple

from .contracts import (
    ArtifactDirectoryEntry,
    ArtifactKind,
    ArtifactRef,
    PORTABLE_DIRECTORY_MANIFEST_VERSION,
    PortableDirectoryManifest,
    ResolvedArtifact,
    StagedArtifact,
    _canonical_json,
    _validate_digest,
    artifact_uri_for_digest,
    sha256_file,
    sha256_bytes,
    _ascii_casefold_path,
)


LOCAL_DIRECTORY_MANIFEST_VERSION = 1


class ArtifactError(RuntimeError):
    """Base class for fail-closed artifact errors.

    中文：按 fail-closed 方式处理的 Artifact 错误基类。
    """


class ArtifactNotFoundError(ArtifactError):
    pass


class ArtifactIntegrityError(ArtifactError):
    pass


@dataclass(frozen=True)
class LocalDirectoryFile:
    """Provider-private file entry; never part of public Artifact identity.

    中文：Provider 私有文件条目，绝不属于公开 Artifact 身份的一部分。
    """

    path: str
    digest: str
    size_bytes: int

    def __post_init__(self) -> None:
        normalized = str(self.path).replace("\\", "/")
        parsed = PurePosixPath(normalized)
        from pathlib import PureWindowsPath

        windows = PureWindowsPath(normalized)
        if (
            not normalized
            or parsed.as_posix() == "."
            or parsed.is_absolute()
            or windows.is_absolute()
            or windows.drive
            or ".." in parsed.parts
        ):
            raise ValueError(f"local directory manifest path must be relative: {self.path!r}")
        object.__setattr__(self, "path", parsed.as_posix())
        _validate_digest(self.digest)
        if self.size_bytes < 0:
            raise ValueError("local directory file size must be non-negative")

    def to_dict(self) -> dict[str, Any]:
        return {"path": self.path, "digest": self.digest, "size_bytes": self.size_bytes}

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "LocalDirectoryFile":
        return cls(path=str(data["path"]), digest=str(data["digest"]), size_bytes=int(data["size_bytes"]))


@dataclass(frozen=True)
class LocalDirectoryManifest:
    """Provider-private CAS index for a multi-file artifact.

    中文：多文件 Artifact 的 Provider 私有 CAS 索引。
    """

    version: int
    uri: str
    files: Tuple[LocalDirectoryFile, ...]
    digest: str
    size_bytes: int
    kind: ArtifactKind
    format: Optional[str] = None
    schema: Optional[str] = None
    created_at: str = ""
    producer: Optional[str] = None
    inputs: Tuple[ArtifactRef, ...] = ()

    def __post_init__(self) -> None:
        files = tuple(self.files)
        if len({item.path for item in files}) != len(files):
            raise ValueError("local directory manifest contains duplicate file paths")
        object.__setattr__(self, "files", tuple(sorted(files, key=lambda item: item.path)))
        object.__setattr__(self, "inputs", tuple(self.inputs))
        object.__setattr__(self, "kind", ArtifactKind(self.kind))
        if self.version <= 0:
            raise ValueError("local directory manifest version must be positive")
        if self.size_bytes < 0:
            raise ValueError("local directory manifest size must be non-negative")
        if not self.created_at:
            object.__setattr__(self, "created_at", datetime.now(timezone.utc).isoformat())
        if self.digest:
            _validate_digest(self.digest)
            expected_uri = artifact_uri_for_digest(self.digest)
            if not self.uri:
                object.__setattr__(self, "uri", expected_uri)
            elif self.uri != expected_uri:
                raise ValueError("local directory manifest URI does not match digest")

    def identity_payload(self) -> dict[str, Any]:
        return {
            "version": self.version,
            "files": [item.to_dict() for item in self.files],
            "size_bytes": self.size_bytes,
        }

    def computed_digest(self) -> str:
        return sha256_bytes(_canonical_json(self.identity_payload()))

    def verify_digest(self) -> bool:
        return bool(self.digest) and self.digest == self.computed_digest()

    def to_dict(self) -> dict[str, Any]:
        return {
            "version": self.version,
            "uri": self.uri,
            "files": [item.to_dict() for item in self.files],
            "size_bytes": self.size_bytes,
            "kind": self.kind.value,
            "format": self.format,
            "schema": self.schema,
            "created_at": self.created_at,
            "producer": self.producer,
            "inputs": [item.to_dict() for item in self.inputs],
            "digest": self.digest,
        }

    def to_bytes(self) -> bytes:
        return _canonical_json(self.to_dict())

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "LocalDirectoryManifest":
        return cls(
            version=int(data["version"]),
            uri=str(data.get("uri") or ""),
            files=tuple(LocalDirectoryFile.from_dict(item) for item in data.get("files") or []),
            digest=str(data["digest"]),
            size_bytes=int(data["size_bytes"]),
            kind=ArtifactKind(data.get("kind") or ArtifactKind.GENERIC.value),
            format=data.get("format"),
            schema=data.get("schema"),
            created_at=str(data.get("created_at") or ""),
            producer=data.get("producer"),
            inputs=tuple(ArtifactRef.from_dict(item) for item in data.get("inputs") or []),
        )

    @classmethod
    def from_bytes(cls, payload: bytes) -> "LocalDirectoryManifest":
        return cls.from_dict(json.loads(payload.decode("utf-8")))


class LocalArtifactProvider:
    """Reference provider backed by a local immutable CAS.

    中文：由本地不可变 CAS 支持的参考 Provider。
    """

    def __init__(self, root: str | Path, *, chunk_size: int = 1024 * 1024) -> None:
        self.root = Path(root)
        self.chunk_size = chunk_size
        self._tmp_dir = self.root / "tmp"
        self._tmp_dir.mkdir(parents=True, exist_ok=True)

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
        source_path = Path(source)
        if source_path.is_symlink():
            raise ArtifactError("symbolic links are not valid artifact sources")
        if source_path.is_file():
            digest, size_bytes = self._store_file(source_path)
            return ArtifactRef(
                uri=artifact_uri_for_digest(digest),
                digest=digest,
                size_bytes=size_bytes,
                kind=kind,
                name=source_path.name,
            )
        if source_path.is_dir():
            return self._publish_directory(
                source_path,
                kind=kind,
                producer=producer,
                format=format,
                schema=schema,
                inputs=inputs,
            )
        raise ArtifactNotFoundError(f"artifact source does not exist: {source_path}")

    def publish_portable_directory(
        self,
        source: str | Path,
        *,
        kind: ArtifactKind = ArtifactKind.GENERIC,
    ) -> ArtifactRef:
        """Publish a directory using the public portable V2 contract.

        The complete tree is checked before any blob is committed.  The
        resulting manifest contains only logical paths, raw blob digests and
        sizes; producer metadata remains provider-private.

        中文：按照公开的 portable V2 契约发布目录。提交任何 blob 前都会检查完整目录树。生成的清单只包含逻辑路径、原始 blob 摘要和大小；生产者元数据仍由 Provider 私有持有。
        """

        source_path = Path(source)
        if source_path.is_symlink() or not source_path.is_dir():
            raise ArtifactError(f"portable directory source must be a real directory: {source_path}")

        source_files = self._collect_portable_source_files(source_path)
        entries: list[ArtifactDirectoryEntry] = []
        for relative, path in source_files:
            if path.is_symlink() or not path.is_file():
                raise ArtifactError(f"portable directory member changed during publish: {path}")
            digest, size_bytes = self._store_file(path)
            entries.append(
                ArtifactDirectoryEntry(
                    path=relative,
                    digest=digest,
                    size_bytes=size_bytes,
                )
            )

        manifest = PortableDirectoryManifest(
            version=PORTABLE_DIRECTORY_MANIFEST_VERSION,
            files=tuple(entries),
            size_bytes=sum(entry.size_bytes for entry in entries),
        )
        digest = manifest.computed_digest()
        self._commit_portable_manifest(manifest, digest)
        return manifest.to_artifact_ref(kind)

    @staticmethod
    def _collect_portable_source_files(source: Path) -> list[tuple[str, Path]]:
        """Preflight a directory tree before copying any member into the CAS.

        中文：在将任何成员复制到 CAS 之前，预检整个目录树。
        """

        files: list[tuple[str, Path]] = []
        for root, directories, names in os.walk(
            source,
            topdown=True,
            onerror=LocalArtifactProvider._raise_walk_error,
            followlinks=False,
        ):
            root_path = Path(root)
            for directory in directories:
                candidate = root_path / directory
                if candidate.is_symlink():
                    raise ArtifactError(f"symbolic links are not valid portable members: {candidate}")
                if not candidate.is_dir():
                    raise ArtifactError(f"portable directory member is not a directory: {candidate}")
            for name in names:
                candidate = root_path / name
                if candidate.is_symlink():
                    raise ArtifactError(f"symbolic links are not valid portable members: {candidate}")
                if not candidate.is_file():
                    raise ArtifactError(f"portable directory member is not a regular file: {candidate}")
                relative = candidate.relative_to(source).as_posix()
                ArtifactDirectoryEntry(
                    path=relative,
                    digest="sha256:" + "0" * 64,
                    size_bytes=0,
                )
                files.append((relative, candidate))

        files.sort(key=lambda item: item[0])
        paths = [relative for relative, _ in files]
        folded_prefixes: dict[str, str] = {}
        exact = set(paths)
        if len(paths) != len(exact):
            raise ArtifactError("portable directory contains duplicate paths")
        for path in paths:
            prefix: list[str] = []
            for component in path.split("/"):
                prefix.append(component)
                prefix_path = "/".join(prefix)
                folded_path = _ascii_casefold_path(prefix_path)
                previous = folded_prefixes.setdefault(folded_path, prefix_path)
                if previous != prefix_path:
                    raise ArtifactError(
                        "portable directory contains an ASCII case-insensitive path collision: "
                        f"{previous!r} and {prefix_path!r}"
                    )
                if prefix_path != path and prefix_path in exact:
                    raise ArtifactError(
                        f"portable directory has a file/directory path conflict at {prefix_path!r}"
                    )
        return files

    @staticmethod
    def _raise_walk_error(error: OSError) -> None:
        """Turn an unreadable source directory into a failed publication.

        中文：将无法读取源目录的情况转换为发布失败。
        """

        raise ArtifactError(f"cannot read portable directory: {error}") from error

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
        temp = self._new_temp_path("bytes-")
        try:
            temp.write_bytes(payload)
            return self.publish(
                temp,
                kind=kind,
                producer=producer,
                format=format,
                schema=schema,
                inputs=inputs,
            )
        finally:
            temp.unlink(missing_ok=True)

    def resolve(self, artifact_ref: ArtifactRef) -> ResolvedArtifact:
        if artifact_ref.manifest_digest:
            manifest_path = self._manifest_path(artifact_ref.manifest_digest)
            if not manifest_path.is_file():
                raise ArtifactNotFoundError(f"artifact manifest is missing: {artifact_ref.uri}")
            try:
                payload = manifest_path.read_bytes()
                wire = json.loads(payload.decode("utf-8"))
                if wire.get("version") == PORTABLE_DIRECTORY_MANIFEST_VERSION:
                    portable_manifest = PortableDirectoryManifest.from_bytes(payload)
                    if payload != portable_manifest.canonical_bytes():
                        raise ArtifactIntegrityError(
                            f"portable artifact manifest is not canonical: {artifact_ref.uri}"
                        )
                    expected_digest = portable_manifest.computed_digest()
                    if expected_digest != artifact_ref.manifest_digest:
                        raise ArtifactIntegrityError(
                            f"portable artifact manifest digest mismatch: {artifact_ref.uri}"
                        )
                    if (
                        artifact_ref.digest != expected_digest
                        or artifact_ref.size_bytes != portable_manifest.size_bytes
                    ):
                        raise ArtifactIntegrityError(f"artifact reference metadata mismatch: {artifact_ref.uri}")
                    for portable_item in portable_manifest.files:
                        self._verify_blob(portable_item.digest, portable_item.size_bytes)
                    return ResolvedArtifact(
                        artifact_ref=artifact_ref,
                        location=manifest_path,
                        manifest=portable_manifest,
                    )
                legacy_manifest = LocalDirectoryManifest.from_bytes(payload)
            except Exception as exc:
                if isinstance(exc, ArtifactIntegrityError):
                    raise
                raise ArtifactIntegrityError(f"invalid artifact manifest: {manifest_path}") from exc
            if legacy_manifest.digest != artifact_ref.manifest_digest or not legacy_manifest.verify_digest():
                raise ArtifactIntegrityError(f"artifact manifest digest mismatch: {artifact_ref.uri}")
            if legacy_manifest.uri != artifact_ref.uri or legacy_manifest.size_bytes != artifact_ref.size_bytes:
                raise ArtifactIntegrityError(f"artifact reference metadata mismatch: {artifact_ref.uri}")
            for legacy_item in legacy_manifest.files:
                self._verify_blob(legacy_item.digest, legacy_item.size_bytes)
            return ResolvedArtifact(artifact_ref=artifact_ref, location=manifest_path, manifest=legacy_manifest)

        blob_path = self._blob_path(artifact_ref.digest)
        self._verify_blob(artifact_ref.digest, artifact_ref.size_bytes)
        return ResolvedArtifact(artifact_ref=artifact_ref, location=blob_path)

    def verify(self, artifact_ref: ArtifactRef) -> bool:
        self.resolve(artifact_ref)
        return True

    def stage(self, artifact_ref: ArtifactRef, destination: str | Path) -> StagedArtifact:
        return LocalArtifactStager(self).stage(artifact_ref, destination)

    def _publish_directory(
        self,
        source: Path,
        *,
        kind: ArtifactKind,
        producer: Optional[str],
        format: Optional[str],
        schema: Optional[str],
        inputs: Sequence[ArtifactRef],
    ) -> ArtifactRef:
        entries: list[LocalDirectoryFile] = []
        for path in sorted(source.rglob("*"), key=lambda item: item.relative_to(source).as_posix()):
            if path.is_symlink():
                raise ArtifactError(f"symbolic links are not valid artifact members: {path}")
            if not path.is_file():
                continue
            digest, size_bytes = self._store_file(path)
            entries.append(
                LocalDirectoryFile(
                    path=path.relative_to(source).as_posix(),
                    digest=digest,
                    size_bytes=size_bytes,
                )
            )

        manifest = LocalDirectoryManifest(
            version=LOCAL_DIRECTORY_MANIFEST_VERSION,
            uri="",
            files=tuple(entries),
            digest="",
            size_bytes=sum(item.size_bytes for item in entries),
            kind=kind,
            format=format,
            schema=schema,
            producer=producer,
            inputs=tuple(inputs),
        )
        digest = manifest.computed_digest()
        manifest = LocalDirectoryManifest(
            version=manifest.version,
            uri=artifact_uri_for_digest(digest),
            files=manifest.files,
            digest=digest,
            size_bytes=manifest.size_bytes,
            kind=manifest.kind,
            format=manifest.format,
            schema=manifest.schema,
            created_at=manifest.created_at,
            producer=manifest.producer,
            inputs=manifest.inputs,
        )
        self._commit_manifest(manifest)
        return ArtifactRef(
            uri=manifest.uri,
            digest=manifest.digest,
            size_bytes=manifest.size_bytes,
            kind=kind,
            manifest_digest=manifest.digest,
            name=source.name,
        )

    def _store_file(self, source: Path) -> tuple[str, int]:
        temp = self._new_temp_path("blob-")
        try:
            hasher = hashlib.sha256()
            size_bytes = 0
            with source.open("rb") as source_handle, temp.open("wb") as temp_handle:
                for chunk in iter(lambda: source_handle.read(self.chunk_size), b""):
                    hasher.update(chunk)
                    temp_handle.write(chunk)
                    size_bytes += len(chunk)
                temp_handle.flush()
                os.fsync(temp_handle.fileno())
            digest = "sha256:" + hasher.hexdigest()
            final = self._blob_path(digest)
            self._atomic_commit(
                temp,
                final,
                lambda path: self._verify_blob_at_path(path, digest, size_bytes),
            )
            return digest, size_bytes
        finally:
            temp.unlink(missing_ok=True)

    def _commit_manifest(self, manifest: LocalDirectoryManifest) -> None:
        temp = self._new_temp_path("manifest-")
        try:
            with temp.open("wb") as handle:
                handle.write(manifest.to_bytes())
                handle.flush()
                os.fsync(handle.fileno())
            final = self._manifest_path(manifest.digest)
            self._atomic_commit(
                temp,
                final,
                lambda path: self._verify_manifest_at_path(path, manifest.digest),
            )
        finally:
            temp.unlink(missing_ok=True)

    def _commit_portable_manifest(self, manifest: PortableDirectoryManifest, digest: str) -> None:
        temp = self._new_temp_path("portable-manifest-")
        try:
            payload = manifest.canonical_bytes()
            if sha256_bytes(payload) != digest:
                raise ArtifactIntegrityError("portable directory manifest digest changed before commit")
            with temp.open("wb") as handle:
                handle.write(payload)
                handle.flush()
                os.fsync(handle.fileno())
            final = self._manifest_path(digest)
            self._atomic_commit(
                temp,
                final,
                lambda path: self._verify_portable_manifest_at_path(path, digest),
            )
        finally:
            temp.unlink(missing_ok=True)

    def _atomic_commit(self, temp: Path, final: Path, verify_existing) -> None:
        final.parent.mkdir(parents=True, exist_ok=True)
        if final.exists():
            verify_existing(final)
            return
        try:
            os.link(temp, final)
            temp.unlink(missing_ok=True)
        except FileExistsError:
            verify_existing(final)
        except OSError:
            if final.exists():
                verify_existing(final)
            else:
                os.replace(temp, final)

    def _verify_blob(self, digest: str, size_bytes: int) -> None:
        self._verify_blob_at_path(self._blob_path(digest), digest, size_bytes)

    def _verify_blob_at_path(self, path: Path, digest: str, size_bytes: int) -> None:
        if path.is_symlink() or not path.is_file():
            raise ArtifactNotFoundError(f"artifact blob is missing: {digest}")
        actual_digest, actual_size = sha256_file(path, chunk_size=self.chunk_size)
        if actual_digest != digest or actual_size != size_bytes:
            raise ArtifactIntegrityError(f"artifact blob digest mismatch: {digest}")

    @staticmethod
    def _verify_manifest_at_path(path: Path, expected_digest: str) -> None:
        if path.is_symlink() or not path.is_file():
            raise ArtifactNotFoundError(f"artifact manifest is missing: {path}")
        manifest = LocalDirectoryManifest.from_bytes(path.read_bytes())
        if manifest.digest != expected_digest or not manifest.verify_digest():
            raise ArtifactIntegrityError(f"artifact manifest digest mismatch: {path}")

    @staticmethod
    def _verify_portable_manifest_at_path(path: Path, expected_digest: str) -> None:
        if path.is_symlink() or not path.is_file():
            raise ArtifactNotFoundError(f"portable artifact manifest is missing: {path}")
        payload = path.read_bytes()
        manifest = PortableDirectoryManifest.from_bytes(payload)
        if payload != manifest.canonical_bytes():
            raise ArtifactIntegrityError(f"portable artifact manifest is not canonical: {path}")
        if manifest.computed_digest() != expected_digest:
            raise ArtifactIntegrityError(f"portable artifact manifest digest mismatch: {path}")

    def _blob_path(self, digest: str) -> Path:
        normalized = digest.split(":", 1)[-1]
        return self.root / "blobs" / "sha256" / normalized[:2] / normalized

    def _manifest_path(self, digest: str) -> Path:
        normalized = digest.split(":", 1)[-1]
        return self.root / "manifests" / "sha256" / normalized[:2] / normalized

    def _new_temp_path(self, prefix: str) -> Path:
        handle, name = tempfile.mkstemp(prefix=prefix, dir=self._tmp_dir)
        os.close(handle)
        return Path(name)


class LocalArtifactStager:
    """Stage verified provider content into a worker-visible local path.

    中文：将已经验证的 Provider 内容暂存到 Worker 可见的本地路径。
    """

    def __init__(self, provider: LocalArtifactProvider) -> None:
        self.provider = provider

    def stage(self, artifact_ref: ArtifactRef, destination: str | Path) -> StagedArtifact:
        resolved = self.provider.resolve(artifact_ref)
        target = Path(destination)
        if resolved.manifest is None:
            if target.exists() and target.is_dir():
                target = target / (artifact_ref.name or artifact_ref.digest.split(":", 1)[-1])
            self._stage_blob(resolved.location, target, artifact_ref.digest, artifact_ref.size_bytes)
        else:
            self._stage_directory(resolved.manifest, target)
        return StagedArtifact(
            artifact_ref=artifact_ref,
            local_path=str(target),
            worker_visible_path=str(target),
            verified_digest=artifact_ref.digest,
        )

    def _stage_blob(self, source: Path, target: Path, digest: str, size_bytes: int) -> None:
        target.parent.mkdir(parents=True, exist_ok=True)
        if target.is_symlink():
            raise ArtifactIntegrityError(f"staged target must not be a symbolic link: {target}")
        if target.exists():
            if target.is_file() and sha256_file(target, chunk_size=self.provider.chunk_size) == (digest, size_bytes):
                return
            raise ArtifactIntegrityError(f"staged target does not match artifact: {target}")
        temp_fd, temp_name = tempfile.mkstemp(prefix=f".{target.name}.stage-", dir=str(target.parent))
        os.close(temp_fd)
        temp = Path(temp_name)
        try:
            hasher = hashlib.sha256()
            copied = 0
            with source.open("rb") as source_handle, temp.open("wb") as target_handle:
                for chunk in iter(lambda: source_handle.read(self.provider.chunk_size), b""):
                    hasher.update(chunk)
                    target_handle.write(chunk)
                    copied += len(chunk)
                target_handle.flush()
                os.fsync(target_handle.fileno())
            actual = "sha256:" + hasher.hexdigest()
            if actual != digest or copied != size_bytes:
                raise ArtifactIntegrityError(f"staged artifact digest mismatch: {target}")
            os.replace(temp, target)
        finally:
            temp.unlink(missing_ok=True)

    def _stage_directory(
        self,
        manifest: LocalDirectoryManifest | PortableDirectoryManifest,
        target: Path,
    ) -> None:
        if target.is_symlink():
            raise ArtifactIntegrityError(f"staged target must not be a symbolic link: {target}")
        if target.exists():
            self._verify_staged_directory(manifest, target)
            return
        target.parent.mkdir(parents=True, exist_ok=True)
        temp = Path(tempfile.mkdtemp(prefix=f".{target.name}.stage-", dir=str(target.parent)))
        try:
            for item in manifest.files:
                source = self.provider._blob_path(item.digest)
                self._stage_blob(source, temp / PurePosixPath(item.path), item.digest, item.size_bytes)
            os.replace(temp, target)
        finally:
            if temp.exists():
                shutil.rmtree(temp)

    def _verify_staged_directory(
        self,
        manifest: LocalDirectoryManifest | PortableDirectoryManifest,
        target: Path,
    ) -> None:
        if not target.is_dir():
            raise ArtifactIntegrityError(f"staged target is not a directory: {target}")
        items = list(target.rglob("*"))
        if any(item.is_symlink() for item in items):
            raise ArtifactIntegrityError(f"staged directory contains a symbolic link: {target}")
        expected = {PurePosixPath(item.path).as_posix(): item for item in manifest.files}
        actual = {
            item.relative_to(target).as_posix()
            for item in items
            if item.is_file()
        }
        if actual != set(expected):
            raise ArtifactIntegrityError(f"staged directory contents mismatch: {target}")
        for relative, item in expected.items():
            path = target / PurePosixPath(relative)
            if sha256_file(path, chunk_size=self.provider.chunk_size) != (item.digest, item.size_bytes):
                raise ArtifactIntegrityError(f"staged file digest mismatch: {path}")


__all__ = [
    "ArtifactError",
    "ArtifactIntegrityError",
    "ArtifactNotFoundError",
    "LocalArtifactProvider",
    "LocalArtifactStager",
]
