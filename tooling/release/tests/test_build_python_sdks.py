import io
import subprocess
import sys
import tarfile
import tomllib
import zipfile
from pathlib import Path

import pytest

RELEASE_DIR = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(RELEASE_DIR))

import build_python_sdks as sdk_builder


PLATFORM_ROOT = RELEASE_DIR.parent.parent


def _write_manifest(path: Path, name: str, version: str = "1.2.3") -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        "\n".join(
            (
                "[build-system]",
                'requires = ["setuptools>=68"]',
                'build-backend = "setuptools.build_meta"',
                "",
                "[project]",
                f'name = "{name}"',
                f'version = "{version}"',
                'readme = "README.md"',
            )
        ),
        encoding="utf-8",
    )
    (path.parent / "README.md").write_text(f"# {name}\n", encoding="utf-8")


def _write_policy(path: Path, packages: list[str]) -> None:
    path.write_text(
        "artifacts:\n  package_units:\n" + "".join(f"  - {package}\n" for package in packages),
        encoding="utf-8",
    )


def _fake_build(command: list[str], **_: object) -> subprocess.CompletedProcess[str]:
    output_dir = Path(command[command.index("--outdir") + 1])
    source = Path(command[-1])
    project = tomllib.loads((source / "pyproject.toml").read_text(encoding="utf-8"))["project"]
    name = project["name"]
    version = project["version"]
    normalized = name.replace("-", "_")
    metadata = f"Metadata-Version: 2.1\nName: {name}\nVersion: {version}\n"

    with zipfile.ZipFile(output_dir / f"{normalized}-{version}-py3-none-any.whl", "w") as archive:
        archive.writestr(f"{normalized}-{version}.dist-info/METADATA", metadata)
    payload = metadata.encode("utf-8")
    with tarfile.open(output_dir / f"{normalized}-{version}.tar.gz", "w:gz") as archive:
        member = tarfile.TarInfo(f"{normalized}-{version}/PKG-INFO")
        member.size = len(payload)
        archive.addfile(member, io.BytesIO(payload))
    return subprocess.CompletedProcess(command, 0)


def test_repository_policy_matches_every_direct_python_sdk() -> None:
    packages = sdk_builder.load_packages(PLATFORM_ROOT)

    assert {package.name for package in packages} == {
        "cyrene-artifacts",
        "cyrene-control-plane",
        "cyrene-environment",
        "cyrene-preflight",
        "cyrene-worker-shim",
    }


def test_load_packages_rejects_manifest_missing_from_policy(tmp_path: Path) -> None:
    _write_policy(tmp_path / "repository-policy.yaml", ["cyrene-first"])
    _write_manifest(tmp_path / "sdk/python/first/pyproject.toml", "cyrene-first")
    _write_manifest(tmp_path / "sdk/python/second/pyproject.toml", "cyrene-second")

    with pytest.raises(ValueError, match="SDK manifests missing from policy: cyrene-second"):
        sdk_builder.load_packages(tmp_path)


def test_load_packages_ignores_nested_migration_manifests(tmp_path: Path) -> None:
    _write_policy(tmp_path / "repository-policy.yaml", ["cyrene-public"])
    _write_manifest(tmp_path / "sdk/python/public/pyproject.toml", "cyrene-public")
    _write_manifest(tmp_path / "sdk/python/migration/private/pyproject.toml", "cyrene-private")

    packages = sdk_builder.load_packages(tmp_path)

    assert [package.name for package in packages] == ["cyrene-public"]


def test_build_packages_verifies_wheel_and_sdist_metadata(tmp_path: Path) -> None:
    source = tmp_path / "sdk/python/example"
    _write_manifest(source / "pyproject.toml", "cyrene-example")
    package = sdk_builder.PythonPackage("cyrene-example", "1.2.3", source)

    artifacts = sdk_builder.build_packages((package,), tmp_path / "release", runner=_fake_build)

    assert len(artifacts) == 2
    assert {sdk_builder._distribution_metadata(path) for path in artifacts} == {("cyrene-example", "1.2.3")}


def test_release_workflow_preserves_candidates_without_publishing_registry() -> None:
    workflow = (PLATFORM_ROOT / ".github/workflows/prepare-release.yml").read_text(encoding="utf-8")

    assert "tooling/release/build_python_sdks.py" in workflow
    assert "python -m twine check --strict release-dist/*" in workflow
    assert "actions/upload-artifact@v4" in workflow
    assert "if-no-files-found: error" in workflow
    assert "tooling/ci/verify.py --scope rust" in workflow
    assert "twine upload" not in workflow
    assert "pypa/gh-action-pypi-publish" not in workflow
