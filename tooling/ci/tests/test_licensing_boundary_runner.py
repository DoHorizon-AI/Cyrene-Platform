import os
import subprocess
from pathlib import Path


RUNNER = Path(__file__).resolve().parents[2] / "acceptance" / "licensing-boundary" / "run.sh"


def write_executable(path: Path, contents: str) -> None:
    path.write_text(contents, encoding="utf-8")
    path.chmod(0o755)


def run_with_python(tmp_path: Path, python_body: str) -> subprocess.CompletedProcess[str]:
    write_executable(
        tmp_path / "cargo",
        '#!/usr/bin/env bash\nif [[ "${1:-}" == "tree" ]]; then\n  echo "cyrene-external-worker-consumer v0.1.0"\nfi\n',
    )
    write_executable(tmp_path / "python3", f"#!/usr/bin/env bash\n{python_body}\n")
    environment = os.environ.copy()
    environment["PATH"] = f"{tmp_path}{os.pathsep}{environment['PATH']}"
    return subprocess.run(
        ["bash", str(RUNNER)],
        capture_output=True,
        text=True,
        env=environment,
    )


def test_dependency_boundary_fails_when_configuration_reader_fails(tmp_path: Path) -> None:
    result = run_with_python(tmp_path, "exit 23")

    assert result.returncode != 0
    assert "failed to read the configured Core crate boundary" in result.stderr
    assert "dependency closure: PASS" not in result.stdout


def test_dependency_boundary_fails_when_core_boundary_is_empty(tmp_path: Path) -> None:
    result = run_with_python(tmp_path, "exit 0")

    assert result.returncode != 0
    assert "configured Core crate boundary is empty" in result.stderr
    assert "dependency closure: PASS" not in result.stdout
