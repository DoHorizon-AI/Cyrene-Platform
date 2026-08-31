# ╔══════════════════════════════════════════════════════════════════════╗
# ║ 📄 File: sdk/python/cyrene_artifacts/src/cy_artifacts/local.py
# ║ Module: CYRENE Platform
# ║ Role: Python SDK, TCK, or test module for this repository boundary.
# ║
# ║ 模块：CYRENE Platform
# ║ 职责：Python SDK、TCK 或测试模块。
# ╚══════════════════════════════════════════════════════════════════════╝
"""Local filesystem Artifact Provider and Stager.

All content is copied with bounded chunks.  Files are committed to a
content-addressed store through a temporary file and a single atomic directory
entry operation.  Directories are represented by manifests whose entries point
at individual immutable blobs.
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
    ARTIFACT_MANIFEST_VERSION,
    ArtifactKind,
    ArtifactRef,
    ResolvedArtifact,
    StagedArtifact,
    _canonical_json,
    _validate_digest,
    artifact_uri_for_digest,
    sha256_file,
    sha256_bytes,
)


class ArtifactError(RuntimeError):
    """Base class for fail-closed artifact errors."""


class ArtifactNotFoundError(ArtifactError):
    pass


class ArtifactIntegrityError(ArtifactError):
    pass


@dataclass(frozen=True)
class LocalDirectoryFile:
    """Provider-private file entry; never part of the public ArtifactManifest."""

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
    """Provider-private CAS index for a multi-file artifact."""

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
    """Reference provider backed by a local immutable CAS."""

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
                manifest = LocalDirectoryManifest.from_bytes(manifest_path.read_bytes())
            except Exception as exc:
                raise ArtifactIntegrityError(f"invalid artifact manifest: {manifest_path}") from exc
            if manifest.digest != artifact_ref.manifest_digest or not manifest.verify_digest():
                raise ArtifactIntegrityError(f"artifact manifest digest mismatch: {artifact_ref.uri}")
            if manifest.uri != artifact_ref.uri or manifest.size_bytes != artifact_ref.size_bytes:
                raise ArtifactIntegrityError(f"artifact reference metadata mismatch: {artifact_ref.uri}")
            for item in manifest.files:
                self._verify_blob(item.digest, item.size_bytes)
            return ResolvedArtifact(artifact_ref=artifact_ref, location=manifest_path, manifest=manifest)

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
            version=ARTIFACT_MANIFEST_VERSION,
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
    """Stage verified provider content into a worker-visible local path."""

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
        temp = target.parent / f".{target.name}.stage-{next(tempfile._get_candidate_names())}"
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

    def _stage_directory(self, manifest: LocalDirectoryManifest, target: Path) -> None:
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

    def _verify_staged_directory(self, manifest: LocalDirectoryManifest, target: Path) -> None:
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
