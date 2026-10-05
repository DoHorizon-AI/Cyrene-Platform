"""Component packaging, canonical digest, and provenance helper tests.

组件打包、规范摘要和来源校验辅助函数测试。
"""

from __future__ import annotations

import json
import hashlib
import stat
from pathlib import Path
from unittest.mock import patch

import pytest

from tooling.release import component_artifacts as artifacts

FIXTURES = Path(__file__).parent / "fixtures"
SOURCE_SHA = "1" * 40


def _source() -> dict[str, str]:
    return {
        "repository": "https://github.com/DoHorizon-AI/Cyrene-Platform",
        "ref": "refs/heads/develop",
        "commit": SOURCE_SHA,
    }


def _attestation(subject_name: str) -> dict[str, object]:
    return {
        "kind": "github-artifact-attestation",
        "uri": "https://github.com/DoHorizon-AI/Cyrene-Platform/attestations/123456789",
        "subjectName": subject_name,
        "repository": "DoHorizon-AI/Cyrene-Platform",
        "workflow": "DoHorizon-AI/Cyrene-Platform/.github/workflows/component-release.yml",
        "predicateType": artifacts.ATTESTATION_PREDICATE,
        "run": {
            "id": "123456789",
            "attempt": 1,
            "url": "https://github.com/DoHorizon-AI/Cyrene-Platform/actions/runs/123456789/attempts/1",
        },
    }


def _descriptor(release_id: str, kind: str = "native-binary") -> dict[str, object]:
    artifact: dict[str, object] = {
        "kind": kind,
        "format": "tar.gz",
        "entrypoint": "cyrene-sample",
    }
    subject_name = f"cyrene-sample-linux-ubuntu-24-04-x86-64.tar.gz"
    return {
        "schemaVersion": 1,
        "releaseId": release_id,
        "componentId": "cyrene-sample",
        "version": SOURCE_SHA,
        "channel": "preview",
        "target": {
            "os": "linux",
            "osVersion": "24.04",
            "distribution": "ubuntu",
            "distributionVersion": "24.04",
            "architecture": "x86_64",
            "abi": "gnu",
        },
        "artifact": artifact,
        "dependencies": [],
        "restart": {"group": "single-service", "unit": "cyrene-sample.service"},
        "source": _source(),
        "provenance": {"attestation": _attestation(subject_name)},
        "compatibility": {
            "groupId": "workspace-product-v2",
            "groupVersion": "2",
            "contractApiVersion": "0.1.0",
            "wireApiVersion": "cyrene.workspace.product.v2",
            "contractLock": {
                "repository": "DoHorizon-AI/Cyrene-Workspace",
                "commit": "c7dea28958a97ccab3a9cc3199faf0ac5819a2a5",
                "path": "governance/workspace-connection-protocols-v2.lock.json",
                "sha256": "sha256:00fd59fb76d7144b6e1b554feadf035328b216231e0a323178836abf2b8bdd5a",
            },
        },
    }


def _make_fixture_manifest(root: Path) -> tuple[Path, Path]:
    payload = root / "payload"
    payload.mkdir()
    binary = payload / "cyrene-sample"
    binary.write_bytes(b"immutable component payload\n")
    binary.chmod(stat.S_IRUSR | stat.S_IWUSR | stat.S_IXUSR)
    archive = root / "cyrene-sample-linux-ubuntu-24-04-x86-64.tar.gz"
    artifacts._write_deterministic_tar_gz(payload, archive)
    descriptor = root / "descriptor.json"
    descriptor.write_text(
        json.dumps(_descriptor(f"preview-{SOURCE_SHA}"), indent=2) + "\n",
        encoding="utf-8",
    )
    manifest_path = root / "cyrene-sample-linux-ubuntu-24-04-x86-64.manifest.json"
    artifacts.create_manifest(descriptor, manifest_path, archive, payload)
    return archive, manifest_path


def _make_executable_files_manifest(root: Path) -> tuple[Path, Path, list[str]]:
    """Create one signed-source descriptor with explicit executable attachments."""
    payload = root / "executable-payload"
    (payload / "bin").mkdir(parents=True)
    (payload / "systemd").mkdir()
    executable_files = [
        "bin/cy-workspace-authority-host",
        "bin/cy-workspace-authority-admin",
        "bin/cy-workspace-storage-migrator",
        "bin/cy-workspace-device-ca-admin",
        "bin/cy-workspace-directory-admin",
    ]
    for path in executable_files:
        binary = payload / path
        binary.write_bytes(path.encode("ascii") + b"\n")
        binary.chmod(stat.S_IRUSR | stat.S_IWUSR | stat.S_IXUSR)
    (payload / "systemd/cyrene-workspace-authority.service").write_bytes(b"[Service]\n")
    archive = root / "cyrene-sample-linux-ubuntu-24-04-x86-64.tar.gz"
    artifacts._write_deterministic_tar_gz(payload, archive)
    descriptor_value = _descriptor(f"preview-{SOURCE_SHA}")
    artifact = descriptor_value["artifact"]
    artifact["entrypoint"] = executable_files[0]
    artifact["executableFiles"] = executable_files
    descriptor = root / "executable-descriptor.json"
    descriptor.write_text(json.dumps(descriptor_value, indent=2) + "\n", encoding="utf-8")
    manifest_path = root / "executable-manifest.json"
    artifacts.create_manifest(descriptor, manifest_path, archive, payload)
    return archive, manifest_path, executable_files


def test_jcs_golden_manifest_and_index_bytes() -> None:
    """Match the shared Rust/Python/TypeScript RFC 8785 golden bytes."""
    manifest = json.loads((FIXTURES / "component-release-manifest-v1.json").read_text())
    index = json.loads((FIXTURES / "component-release-index-v1.json").read_text())
    manifest_jcs = artifacts._canonical_json({key: value for key, value in manifest.items() if key != "manifestDigest"})
    index_jcs = artifacts._canonical_json({key: value for key, value in index.items() if key != "indexDigest"})
    assert manifest_jcs == (FIXTURES / "component-release-manifest-v1.jcs.json").read_bytes()
    assert index_jcs == (FIXTURES / "component-release-index-v1.jcs.json").read_bytes()
    assert "sha256:" + hashlib.sha256(manifest_jcs).hexdigest() == manifest["manifestDigest"]
    assert "sha256:" + hashlib.sha256(index_jcs).hexdigest() == index["indexDigest"]


def test_jcs_rejects_unsafe_numbers() -> None:
    """Reject values that JavaScript cannot represent as an exact safe integer."""
    with pytest.raises(artifacts.ComponentArtifactError):
        artifacts._canonical_json({"value": 9_007_199_254_740_992})
    with pytest.raises(artifacts.ComponentArtifactError):
        artifacts._canonical_json({"value": 0.25})


def test_archive_and_manifest_bind_exact_payload_bytes(tmp_path: Path) -> None:
    """Bind file and entrypoint digests and reject later archive modification."""
    archive, manifest_path = _make_fixture_manifest(tmp_path)
    manifest = artifacts.verify_manifest(
        manifest_path,
        archive,
        verify_run=False,
        verify_attestation=False,
    )
    assert manifest["artifact"]["files"]["cyrene-sample"].startswith("sha256:")
    assert "executableFiles" not in manifest["artifact"]
    archive.write_bytes(archive.read_bytes() + b"tampered")
    with pytest.raises(artifacts.ComponentArtifactError, match="artifact size"):
        artifacts.verify_manifest(
            manifest_path,
            archive,
            verify_run=False,
            verify_attestation=False,
        )


def test_executable_files_are_source_and_archive_bound(tmp_path: Path) -> None:
    """Explicit executable modes are covered by the same signed payload map."""
    archive, manifest_path, executable_files = _make_executable_files_manifest(tmp_path)
    manifest = artifacts.verify_manifest(
        manifest_path,
        archive,
        verify_run=False,
        verify_attestation=False,
    )

    assert manifest["source"] == _source()
    assert manifest["artifact"]["executableFiles"] == executable_files
    assert manifest["artifact"]["entrypoint"] == executable_files[0]
    assert set(manifest["artifact"]["files"]) == {
        *executable_files,
        "systemd/cyrene-workspace-authority.service",
    }
    assert artifacts._archive_files(archive) == manifest["artifact"]["files"]


@pytest.mark.parametrize(
    "executable_files,expected_error",
    [
        ("bin/cy-workspace-authority-host", "non-empty array"),
        ([], "non-empty array"),
        (["bin/cy-workspace-authority-host", "bin/cy-workspace-authority-host"], "unique"),
        (["bin/cy-workspace-authority-host", "../outside"], "unsafe payload path"),
        (["bin/cy-workspace-authority-host", "bin/not-in-files"], "appear in artifact.files"),
        (["bin/cy-workspace-authority-admin"], "must include artifact.entrypoint"),
        (["bin/cy-workspace-authority-host", 42], "safe relative paths"),
    ],
)
def test_manifest_rejects_invalid_executable_file_lists(
    tmp_path: Path, executable_files: object, expected_error: str
) -> None:
    """Reject unsafe, duplicate, unbound, or entrypoint-omitting declarations."""
    _archive, manifest_path, _valid_executable_files = _make_executable_files_manifest(tmp_path)
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    manifest["artifact"]["executableFiles"] = executable_files
    manifest["manifestDigest"] = artifacts._manifest_digest(manifest)

    assert expected_error in "; ".join(artifacts._validate_manifest(manifest))


def test_index_uses_exact_asset_name_and_source_pins(tmp_path: Path) -> None:
    """Create a repository index that points to a versioned manifest asset."""
    _, manifest_path = _make_fixture_manifest(tmp_path)
    index_path = tmp_path / "component-release-index-v1.json"
    index = artifacts.create_index(
        [manifest_path],
        index_path,
        channel="preview",
        source_ref="refs/heads/develop",
        source_commit=SOURCE_SHA,
        run_id="123456789",
        run_attempt=1,
        workflow="DoHorizon-AI/Cyrene-Platform/.github/workflows/component-release.yml",
        attestation_uri=None,
        generated_at="2026-10-01T12:00:00Z",
    )
    release = index["releases"][0]
    assert release["manifestUri"].endswith(
        "/preview-" + SOURCE_SHA + "/cyrene-sample-linux-ubuntu-24-04-x86-64-gnu.manifest.json"
    )
    assert index["compatibilityGroups"][0]["groupId"] == "workspace-product-v2"
    assert artifacts.verify_index(index_path, verify_run=False, verify_attestation=False) == index


def test_github_run_verification_checks_actual_api_identity() -> None:
    """Reject a successful run response from a different source identity."""
    payload = {
        "repository": {"full_name": "DoHorizon-AI/Cyrene-Platform"},
        "path": ".github/workflows/component-release.yml@refs/heads/develop",
        "head_branch": "develop",
        "head_sha": SOURCE_SHA,
        "run_attempt": 1,
        "status": "completed",
        "conclusion": "success",
    }

    class _Response:
        def __enter__(self) -> _Response:
            return self

        def __exit__(self, *_args: object) -> None:
            return None

        def read(self) -> bytes:
            return json.dumps(payload).encode()

    with patch.object(artifacts.urllib.request, "urlopen", return_value=_Response()):
        artifacts._verify_run(_source(), _attestation("sample"))

    payload["head_sha"] = "2" * 40
    with patch.object(artifacts.urllib.request, "urlopen", return_value=_Response()):
        with pytest.raises(artifacts.ComponentArtifactError, match="commit"):
            artifacts._verify_run(_source(), _attestation("sample"))


def test_oci_attestation_subject_uses_repository_and_digest() -> None:
    """Verify OCI attestations by immutable repository digest, not tag text."""
    digest = "sha256:" + "a" * 64
    manifest = {
        "artifact": {
            "kind": "oci-image",
            "repository": "ghcr.io/dohorizon-ai/cyrene-runtime-maintenance",
            "digest": digest,
            "platform": {"os": "linux", "architecture": "amd64"},
        },
        "provenance": {"attestation": _attestation("ghcr.io/dohorizon-ai/cyrene-runtime-maintenance")},
        "source": _source(),
    }
    with patch.object(artifacts.subprocess, "run") as run:
        run.return_value.stdout = "[]"
        artifacts._verify_attestation(
            f"oci://{manifest['artifact']['repository']}@{digest}",
            manifest["source"],
            manifest["provenance"]["attestation"],
            subject_name=None,
        )
    command = run.call_args.args[0]
    assert command[3] == f"oci://ghcr.io/dohorizon-ai/cyrene-runtime-maintenance@{digest}"
    assert "--signer-workflow" in command
    assert "--source-ref" in command
    assert "--source-digest" in command
