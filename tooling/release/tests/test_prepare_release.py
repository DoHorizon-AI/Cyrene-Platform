import subprocess
import sys
from pathlib import Path

PREPARE_TOOL = Path(__file__).resolve().parent.parent / "prepare_release.py"

def test_prepare_release_validate_mode():
    res = subprocess.run([sys.executable, str(PREPARE_TOOL), "--repo-path", ".", "--version", "0.4.3", "--validate"], capture_output=True, text=True)
    assert res.returncode == 0
    assert "Planned Release Execution Sequence:" in res.stdout
    assert "Immutable Tag Creation (LAST STEP)" in res.stdout
    assert "Pre-release validation passed" in res.stdout

def test_prepare_release_invalid_version():
    res = subprocess.run([sys.executable, str(PREPARE_TOOL), "--repo-path", ".", "--version", "invalid_ver", "--validate"], capture_output=True, text=True)
    assert res.returncode != 0
    assert "[REJECTED]" in res.stdout
