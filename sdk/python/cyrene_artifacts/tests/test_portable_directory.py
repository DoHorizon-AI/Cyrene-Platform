# ╔══════════════════════════════════════════════════════════════════════╗
# ║ 📄 File: sdk/python/cyrene_artifacts/tests/test_portable_directory.py ║
# ║ Module: CYRENE Platform                                             ║
# ║ Role: Cross-language portable Artifact directory contract tests.     ║
# ║ 中文:跨语言可移植 Artifact 目录契约测试。
# ║                                                                     ║
# ║ 模块：CYRENE Platform                                                ║
# ║ 职责：跨语言 portable Artifact 目录契约测试。                         ║
# ╚══════════════════════════════════════════════════════════════════════╝
from __future__ import annotations

import json
import os
import subprocess
from pathlib import Path

import pytest

from cy_artifacts import (
    ArtifactError,
    ArtifactDirectoryEntry,
    ArtifactIntegrityError,
    ArtifactKind,
    JCS_SAFE_INTEGER_MAX,
    LocalArtifactProvider,
    PortableDirectoryManifest,
    sha256_bytes,
    sha256_file,
)
from cy_artifacts.local import LocalDirectoryManifest


PLATFORM_ROOT = Path(__file__).resolve().parents[4]
FIXTURE = PLATFORM_ROOT / "contracts" / "schemas" / "examples" / "portable_directory_manifest.example.json"
EXPECTED_DIGEST = "sha256:5d3c4bb7f4b864c409fa3baafb199f33f54c5fdfead4c6ec724f16898b2a828f"
ZERO_DIGEST = "sha256:" + "0" * 64
INVALID_DIGEST_SUFFIXES = ("+" + "0" * 63, "0_" + "0" * 62, " " + "0" * 63)


def test_portable_manifest_fixture_has_byte_stable_jcs_identity() -> None:
    payload = FIXTURE.read_bytes()
    manifest = PortableDirectoryManifest.from_bytes(payload)

    assert manifest.to_dict() == json.loads(payload)
    assert manifest.canonical_bytes() == (
        b'{"files":[{"digest":"sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",'
        b'"path":"weights.bin","size_bytes":3},{"digest":"sha256:06eb7d6a69ee19e5fbdf749018d3d2abfa04bcbd1365db312eb86dc7169389b8",'
        b'"path":"z/\xc3\xa9.txt","size_bytes":2},{"digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",'
        b'"path":"\xe6\xa8\xa1\xe5\x9e\x8b/\xe7\xa9\xba.txt","size_bytes":0}],"size_bytes":5,"version":2}'
    )
    assert manifest.computed_digest() == EXPECTED_DIGEST
    assert manifest.to_artifact_ref(ArtifactKind("producer.bundle")).manifest_digest == EXPECTED_DIGEST


def test_portable_manifest_rejects_paths_layout_and_size_errors() -> None:
    unsafe_paths = ("/absolute", "../parent", "a/../b", "a\\b", "C:/drive", "a//b", "a/./b")
    for path in unsafe_paths:
        with pytest.raises(ValueError):
            ArtifactDirectoryEntry(path=path, digest=ZERO_DIGEST, size_bytes=0)
    with pytest.raises(ValueError, match="lowercase"):
        ArtifactDirectoryEntry(path="uppercase", digest="sha256:" + "A" * 64, size_bytes=0)

    entry_a = ArtifactDirectoryEntry(path="a", digest=ZERO_DIGEST, size_bytes=0)
    entry_b = ArtifactDirectoryEntry(path="a/b", digest=ZERO_DIGEST, size_bytes=0)
    with pytest.raises(ValueError, match="conflict"):
        PortableDirectoryManifest(version=2, files=(entry_a, entry_b), size_bytes=0)

    with pytest.raises(ValueError, match="sorted"):
        PortableDirectoryManifest(version=2, files=(entry_b, entry_a), size_bytes=0)

    upper = ArtifactDirectoryEntry(path="Readme", digest=ZERO_DIGEST, size_bytes=0)
    lower = ArtifactDirectoryEntry(path="readme", digest=ZERO_DIGEST, size_bytes=0)
    with pytest.raises(ValueError, match="case-insensitive"):
        PortableDirectoryManifest(version=2, files=(upper, lower), size_bytes=0)

    with pytest.raises(ValueError, match="case-insensitive"):
        PortableDirectoryManifest(
            version=2,
            files=(
                ArtifactDirectoryEntry(path="A/y", digest=ZERO_DIGEST, size_bytes=0),
                ArtifactDirectoryEntry(path="a/X", digest=ZERO_DIGEST, size_bytes=0),
            ),
            size_bytes=0,
        )

    safe_entry = ArtifactDirectoryEntry(
        path="large.bin",
        digest=ZERO_DIGEST,
        size_bytes=JCS_SAFE_INTEGER_MAX,
    )
    PortableDirectoryManifest(version=2, files=(safe_entry,), size_bytes=JCS_SAFE_INTEGER_MAX)
    with pytest.raises(ValueError, match="safe integer"):
        ArtifactDirectoryEntry(path="too-large.bin", digest=ZERO_DIGEST, size_bytes=JCS_SAFE_INTEGER_MAX + 1)

    with pytest.raises(ValueError, match="mismatch"):
        PortableDirectoryManifest(version=2, files=(entry_a,), size_bytes=1)


def test_portable_manifest_rejects_non_hex_digest_and_non_integer_wire_sizes(tmp_path: Path) -> None:
    for suffix in INVALID_DIGEST_SUFFIXES:
        with pytest.raises(ValueError, match="lowercase"):
            ArtifactDirectoryEntry(path="weights.bin", digest="sha256:" + suffix, size_bytes=0)

    for value in (True, "0", 0.0):
        payload = json.dumps({"version": 2, "files": [], "size_bytes": value}).encode("utf-8")
        with pytest.raises(ValueError, match="integer"):
            PortableDirectoryManifest.from_bytes(payload)

    duplicate_key_payload = b'{"version":2,"files":[],"size_bytes":0,"size_bytes":0}'
    with pytest.raises(ValueError, match="duplicate"):
        PortableDirectoryManifest.from_bytes(duplicate_key_payload)

    for index, suffix in enumerate(INVALID_DIGEST_SUFFIXES):
        manifest_path = tmp_path / f"invalid-digest-{index}.json"
        manifest_path.write_text(
            json.dumps(
                {
                    "version": 2,
                    "files": [{"path": "weights.bin", "digest": "sha256:" + suffix, "size_bytes": 0}],
                    "size_bytes": 0,
                },
                ensure_ascii=False,
                sort_keys=True,
                separators=(",", ":"),
            ),
            encoding="utf-8",
        )
        completed = subprocess.run(
            [
                "cargo",
                "run",
                "--locked",
                "-p",
                "cy-manifest",
                "--bin",
                "cy-manifest",
                "--",
                "--type",
                "portable-directory",
                str(manifest_path),
            ],
            cwd=PLATFORM_ROOT,
            capture_output=True,
            text=True,
            check=False,
            timeout=180,
        )
        assert completed.returncode != 0, completed.stdout

    for index, value in enumerate((True, "0", 0.0)):
        manifest_path = tmp_path / f"invalid-size-{index}.json"
        manifest_path.write_text(
            json.dumps(
                {"version": 2, "files": [], "size_bytes": value},
                sort_keys=True,
                separators=(",", ":"),
            ),
            encoding="utf-8",
        )
        completed = subprocess.run(
            [
                "cargo",
                "run",
                "--locked",
                "-p",
                "cy-manifest",
                "--bin",
                "cy-manifest",
                "--",
                "--type",
                "portable-directory",
                str(manifest_path),
            ],
            cwd=PLATFORM_ROOT,
            capture_output=True,
            text=True,
            check=False,
            timeout=180,
        )
        assert completed.returncode != 0, completed.stdout

def test_python_publish_stage_and_rust_validate_the_same_manifest(tmp_path: Path) -> None:
    source = tmp_path / "model"
    source.mkdir()
    (source / "weights.bin").write_bytes(b"abc")
    (source / "z").mkdir()
    (source / "z" / "é.txt").write_bytes(b"\x00\xff")
    (source / "模型").mkdir()
    (source / "模型" / "空.txt").write_bytes(b"")

    provider = LocalArtifactProvider(tmp_path / "cas")
    artifact = provider.publish_portable_directory(source, kind=ArtifactKind("producer.bundle"))
    assert artifact.digest == EXPECTED_DIGEST
    staged = provider.stage(artifact, tmp_path / "staged")
    assert Path(staged.local_path, "weights.bin").read_bytes() == b"abc"
    assert Path(staged.local_path, "z", "é.txt").read_bytes() == b"\x00\xff"
    assert Path(staged.local_path, "模型", "空.txt").read_bytes() == b""

    manifest_path = provider._manifest_path(artifact.manifest_digest or artifact.digest)
    completed = subprocess.run(
        [
            "cargo",
            "run",
            "--locked",
            "-p",
            "cy-manifest",
            "--bin",
            "cy-manifest",
            "--",
            "--type",
            "portable-directory",
            str(manifest_path),
        ],
        cwd=PLATFORM_ROOT,
        capture_output=True,
        text=True,
        check=False,
        timeout=180,
    )
    assert completed.returncode == 0, completed.stderr
    assert completed.stdout.strip() == EXPECTED_DIGEST


def test_legacy_v1_directory_publish_and_resolution_remain_compatible(tmp_path: Path) -> None:
    source = tmp_path / "legacy"
    source.mkdir()
    (source / "weights.bin").write_bytes(b"legacy")
    provider = LocalArtifactProvider(tmp_path / "cas")

    artifact = provider.publish(source, kind=ArtifactKind("producer.bundle"))
    resolved = provider.resolve(artifact)
    assert isinstance(resolved.manifest, LocalDirectoryManifest)
    assert resolved.manifest.version == 1
    assert resolved.manifest.size_bytes == len(b"legacy")
    staged = provider.stage(artifact, tmp_path / "legacy-staged")
    assert Path(staged.local_path, "weights.bin").read_bytes() == b"legacy"


def test_portable_manifest_does_not_stage_a_corrupt_or_partial_tree(tmp_path: Path) -> None:
    source = tmp_path / "model"
    source.mkdir()
    (source / "weights.bin").write_bytes(b"abc")
    provider = LocalArtifactProvider(tmp_path / "cas")
    artifact = provider.publish_portable_directory(source)
    blob = provider._blob_path("sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
    blob.write_bytes(b"bad")

    destination = tmp_path / "staged"
    with pytest.raises(ArtifactIntegrityError):
        provider.stage(artifact, destination)
    assert not destination.exists()


def test_portable_publish_rejects_symlink_before_manifest_commit(tmp_path: Path) -> None:
    source = tmp_path / "model"
    source.mkdir()
    (source / "good.bin").write_bytes(b"good")
    outside = tmp_path / "outside.bin"
    outside.write_bytes(b"outside")
    (source / "link.bin").symlink_to(outside)

    provider = LocalArtifactProvider(tmp_path / "cas")
    with pytest.raises(ArtifactError, match="symbolic links"):
        provider.publish_portable_directory(source)
    assert not list((tmp_path / "cas" / "manifests").rglob("*"))


def test_portable_disk_manifest_is_canonical_and_metadata_is_not_identity(tmp_path: Path) -> None:
    source = tmp_path / "model"
    source.mkdir()
    (source / "weights.bin").write_bytes(b"weights")
    provider = LocalArtifactProvider(tmp_path / "cas")
    artifact = provider.publish_portable_directory(source)
    manifest_path = provider._manifest_path(artifact.digest)
    payload = manifest_path.read_bytes()
    manifest = PortableDirectoryManifest.from_bytes(payload)

    assert payload == manifest.canonical_bytes()
    assert sha256_bytes(payload) == artifact.digest
    entry = manifest.files[0]
    assert sha256_file(provider._blob_path(entry.digest)) == (entry.digest, entry.size_bytes)

    decorated = manifest.to_dict()
    decorated["producer"] = "display-only"
    manifest_path.write_text(json.dumps(decorated, ensure_ascii=False), encoding="utf-8")
    with pytest.raises(ArtifactIntegrityError):
        provider.resolve(artifact)


def test_portable_publish_fails_closed_on_source_walk_error(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    source = tmp_path / "model"
    source.mkdir()
    (source / "weights.bin").write_bytes(b"weights")
    provider = LocalArtifactProvider(tmp_path / "cas")

    def denied_scandir(_path: str | bytes | os.PathLike[str] | os.PathLike[bytes]):
        raise PermissionError("injected directory read failure")

    monkeypatch.setattr(os, "scandir", denied_scandir)
    with pytest.raises(ArtifactError, match="cannot read portable directory"):
        provider.publish_portable_directory(source)
    assert not list((tmp_path / "cas" / "manifests").rglob("*"))
    assert not list((tmp_path / "cas" / "blobs").rglob("*"))
