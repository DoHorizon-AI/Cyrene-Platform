"""Component packaging, canonical digest, and provenance helper tests.

组件打包、规范摘要和来源校验辅助函数测试。
"""

from __future__ import annotations

import base64
import hashlib
import json
import stat
from pathlib import Path
from unittest.mock import patch

import pytest
import yaml

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


def test_trusted_catalog_compatibility_accepts_frozen_package_runtime_lock() -> None:
    """Validate the C12 Package Runtime group against its own pinned Workspace lock."""
    group = dict(artifacts.PACKAGE_RUNTIME_COMPATIBILITY)
    component = {
        "componentId": "cyrene-runtime-maintenance",
        "compatibilityGroup": artifacts.PACKAGE_RUNTIME_COMPATIBILITY_GROUP_ID,
    }
    catalog = {"compatibilityGroups": [group]}

    with patch.object(artifacts, "_verify_contract_lock") as verify_lock:
        result = artifacts.trusted_catalog_compatibility(Path("catalog.json"), catalog, component)

    assert result == group
    verify_lock.assert_called_once_with({"contractLock": group["contractLock"]})


@pytest.mark.parametrize(
    ("field", "value"),
    [
        ("contractLock", {"repository": "DoHorizon-AI/Cyrene-Platform"}),
        ("wireApiVersion", "cyrene.workspace.product.v2"),
    ],
)
def test_trusted_catalog_compatibility_rejects_unpinned_package_runtime_contract(field: str, value: object) -> None:
    """Reject a mutated Package Runtime lock or wire identity before release creation."""
    group = dict(artifacts.PACKAGE_RUNTIME_COMPATIBILITY)
    group[field] = value
    component = {
        "componentId": "cyrene-runtime-maintenance",
        "compatibilityGroup": artifacts.PACKAGE_RUNTIME_COMPATIBILITY_GROUP_ID,
    }

    with patch.object(artifacts, "_verify_contract_lock") as verify_lock:
        with pytest.raises(artifacts.ComponentArtifactError, match="frozen protocol lock"):
            artifacts.trusted_catalog_compatibility(Path("catalog.json"), {"compatibilityGroups": [group]}, component)

    verify_lock.assert_not_called()


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


def _make_raw_native_manifests(root: Path) -> list[Path]:
    """Create valid v2 raw manifests for both native components and Ubuntu targets."""
    manifest_dir = root / "manifests"
    manifest_dir.mkdir()
    paths: list[Path] = []
    for component_id in sorted(artifacts.RAW_NATIVE_MANIFEST_COMPONENTS):
        for target_version, target in sorted(artifacts.RAW_NATIVE_MANIFEST_TARGETS.items()):
            name = f"{component_id}-{target_version}"
            payload = root / f"payload-{name}"
            entrypoint = payload / "bin" / component_id
            entrypoint.parent.mkdir(parents=True)
            entrypoint.write_bytes(f"{name}\n".encode())
            entrypoint.chmod(stat.S_IRUSR | stat.S_IWUSR | stat.S_IXUSR)
            archive = root / f"{name}.tar.gz"
            artifacts._write_deterministic_tar_gz(payload, archive)
            descriptor = root / f"{name}-descriptor.json"
            descriptor.write_text(
                json.dumps(
                    {
                        "schemaVersion": 2,
                        "protocolVersion": "cyrene.test.v1",
                        "releaseId": f"preview-{SOURCE_SHA}",
                        "componentId": component_id,
                        "version": "0.1.0",
                        "channel": "preview",
                        "target": target,
                        "artifact": {
                            "kind": "native-binary",
                            "format": "tar.gz",
                            "entrypoint": f"bin/{component_id}",
                        },
                        "dependencies": [],
                        "restart": {},
                        "source": _source(),
                        "provenance": {
                            "attestation": _attestation(archive.name),
                        },
                    },
                    indent=2,
                )
                + "\n",
                encoding="utf-8",
            )
            manifest_path = manifest_dir / artifacts.manifest_asset_name(
                {"componentId": component_id, "target": target}
            )
            artifacts.create_manifest(descriptor, manifest_path, archive, payload)
            paths.append(manifest_path)
    return paths


def test_raw_native_manifest_checksum_subjects_bind_exact_four_raw_files(tmp_path: Path) -> None:
    """Create one exact subject row for each required native component/Ubuntu tuple."""
    manifests = _make_raw_native_manifests(tmp_path)
    output = tmp_path / "raw-native-checksums.txt"

    subjects = artifacts.write_native_manifest_subject_checksums(
        tmp_path / "manifests",
        output,
        repository="DoHorizon-AI/Cyrene-Platform",
        channel="preview",
        source_ref="refs/heads/develop",
        source_commit=SOURCE_SHA,
    )

    assert subjects == sorted(manifests, key=lambda path: path.name)
    assert output.read_text(encoding="utf-8").splitlines() == [
        f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.name}" for path in subjects
    ]
    assert len(subjects) == 4


def test_raw_native_manifest_subjects_reject_missing_or_foreign_source(tmp_path: Path) -> None:
    """Require the closed four-target set and exact current release source identity."""
    manifest_dir = tmp_path / "manifests"
    manifests = _make_raw_native_manifests(tmp_path)
    manifests[0].unlink()

    with pytest.raises(artifacts.ComponentArtifactError, match="missing required raw native manifest"):
        artifacts.write_native_manifest_subject_checksums(
            manifest_dir,
            tmp_path / "missing.txt",
            repository="DoHorizon-AI/Cyrene-Platform",
            channel="preview",
            source_ref="refs/heads/develop",
            source_commit=SOURCE_SHA,
        )

    foreign_path = next(
        path
        for path in manifest_dir.glob("*.manifest.json")
        if json.loads(path.read_text(encoding="utf-8"))["componentId"] == "cy-package-runtime"
    )
    document = json.loads(foreign_path.read_text(encoding="utf-8"))
    document["source"]["commit"] = "2" * 40
    document["releaseId"] = f"preview-{'2' * 40}"
    document["manifestDigest"] = artifacts._manifest_digest(document)
    foreign_path.write_text(json.dumps(document) + "\n", encoding="utf-8")

    with pytest.raises(artifacts.ComponentArtifactError, match="source identity differs"):
        artifacts.write_native_manifest_subject_checksums(
            manifest_dir,
            tmp_path / "foreign.txt",
            repository="DoHorizon-AI/Cyrene-Platform",
            channel="preview",
            source_ref="refs/heads/develop",
            source_commit=SOURCE_SHA,
        )


def test_raw_native_manifest_bundle_verification_creates_detached_assets(tmp_path: Path) -> None:
    """Bind four exact raw manifests, then request gh's signed identity verification."""
    manifests = _make_raw_native_manifests(tmp_path)
    manifest_dir = tmp_path / "manifests"
    checksum_path = tmp_path / "checksums.txt"
    subjects = artifacts.write_native_manifest_subject_checksums(
        manifest_dir,
        checksum_path,
        repository="DoHorizon-AI/Cyrene-Platform",
        channel="preview",
        source_ref="refs/heads/develop",
        source_commit=SOURCE_SHA,
    )
    statement = {
        "_type": "https://in-toto.io/Statement/v1",
        "subject": [
            {
                "name": path.name,
                "digest": {"sha256": hashlib.sha256(path.read_bytes()).hexdigest()},
            }
            for path in subjects
        ],
        "predicateType": artifacts.ATTESTATION_PREDICATE,
        "predicate": {},
    }
    bundle_document = {
        "dsseEnvelope": {
            "payloadType": "application/vnd.in-toto+json",
            "payload": base64.b64encode(json.dumps(statement).encode()).decode("ascii"),
            "signatures": [{"sig": "test-only-placeholder"}],
        }
    }
    bundle_path = tmp_path / "actions-attestation-bundle.json"
    bundle_path.write_text(json.dumps(bundle_document), encoding="utf-8")
    commands: list[list[str]] = []

    def fake_runner(command: list[str], **_kwargs: object) -> None:
        commands.append(command)

    sidecars = artifacts.write_native_manifest_attestation_assets(
        manifest_dir,
        checksum_path,
        bundle_path,
        repository="DoHorizon-AI/Cyrene-Platform",
        channel="preview",
        source_ref="refs/heads/develop",
        source_commit=SOURCE_SHA,
        gh_executable="gh-test-double",
        runner=fake_runner,
    )

    assert len(commands) == len(manifests) == len(sidecars) == 4
    for command, manifest in zip(commands, subjects, strict=True):
        assert command[0:3] == ["gh-test-double", "attestation", "verify"]
        assert command[3] == str(manifest)
        for flag, value in (
            ("--repo", "DoHorizon-AI/Cyrene-Platform"),
            ("--signer-workflow", "DoHorizon-AI/Cyrene-Platform/.github/workflows/component-release.yml"),
            ("--source-ref", "refs/heads/develop"),
            ("--source-digest", SOURCE_SHA),
            ("--predicate-type", artifacts.ATTESTATION_PREDICATE),
        ):
            assert command[command.index(flag) + 1] == value
        assert command[command.index("--bundle") + 1] == str(bundle_path)
    assert [path.name for path in sidecars] == [path.name + ".attestation.jsonl" for path in subjects]
    expected_bundle_bytes = json.dumps(bundle_document, separators=(",", ":")).encode() + b"\n"
    assert all(path.read_bytes() == expected_bundle_bytes for path in sidecars)


def test_raw_native_manifest_bundle_rejects_subject_drift_before_gh(tmp_path: Path) -> None:
    """Do not invoke the signature verifier when a bundle names other raw bytes."""
    _make_raw_native_manifests(tmp_path)
    manifest_dir = tmp_path / "manifests"
    checksum_path = tmp_path / "checksums.txt"
    artifacts.write_native_manifest_subject_checksums(
        manifest_dir,
        checksum_path,
        repository="DoHorizon-AI/Cyrene-Platform",
        channel="preview",
        source_ref="refs/heads/develop",
        source_commit=SOURCE_SHA,
    )
    bundle_path = tmp_path / "wrong-bundle.json"
    statement = {
        "subject": [{"name": "different.manifest.json", "digest": {"sha256": "0" * 64}}],
        "predicateType": artifacts.ATTESTATION_PREDICATE,
    }
    bundle_path.write_text(
        json.dumps(
            {
                "dsseEnvelope": {
                    "payloadType": "application/vnd.in-toto+json",
                    "payload": base64.b64encode(json.dumps(statement).encode()).decode("ascii"),
                    "signatures": [{"sig": "test-only-placeholder"}],
                }
            }
        ),
        encoding="utf-8",
    )
    runner_calls: list[list[str]] = []

    with pytest.raises(artifacts.ComponentArtifactError, match="subject set differs"):
        artifacts.write_native_manifest_attestation_assets(
            manifest_dir,
            checksum_path,
            bundle_path,
            repository="DoHorizon-AI/Cyrene-Platform",
            channel="preview",
            source_ref="refs/heads/develop",
            source_commit=SOURCE_SHA,
            runner=lambda command, **_kwargs: runner_calls.append(command),
        )

    assert runner_calls == []


def test_component_release_workflow_publishes_detached_raw_manifest_proofs() -> None:
    """Keep archive and index proofs intact while adding release assets for four raw manifests."""
    workflow_path = Path(__file__).resolve().parents[3] / ".github/workflows/component-release.yml"
    workflow = yaml.safe_load(workflow_path.read_text(encoding="utf-8"))
    steps = workflow["jobs"]["component-release"]["steps"]
    by_name = {step.get("name"): step for step in steps if isinstance(step, dict)}

    archive_attestation = by_name["Attest native and SDK bundle payloads"]
    assert archive_attestation["with"]["subject-checksums"] == "${{ runner.temp }}/component-payload-checksums.txt"
    assert by_name["Attest the exact component index bytes"]["with"]["subject-checksums"] == (
        "${{ runner.temp }}/component-index-checksum.txt"
    )
    raw_attestation = by_name["Attest exact raw native manifest bytes"]
    assert raw_attestation["uses"] == "actions/attest@v4"
    assert raw_attestation["with"]["subject-checksums"] == "${{ runner.temp }}/native-manifest-checksums.txt"
    assert "outputs.bundle-path" in by_name["Verify and save detached raw native manifest proof assets"]["run"]
    asset_creation = by_name["Create the draft release with all durable assets"]["run"]
    assert '"$OUTPUT_ROOT/native/manifests"/*.manifest.json.attestation.jsonl' in asset_creation
