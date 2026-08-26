import subprocess
import sys
from pathlib import Path

PREPARE_TOOL = Path(__file__).resolve().parent.parent / "prepare_release.py"

def test_prepare_release_dry_run():
    res = subprocess.run([sys.executable, str(PREPARE_TOOL), "--repo-path", ".", "--version", "0.4.3", "--dry-run"], capture_output=True, text=True)
    assert res.returncode == 0
    assert "Planned Release Actions:" in res.stdout
    assert "Tag Creation" in res.stdout

def test_prepare_release_invalid_version():
    res = subprocess.run([sys.executable, str(PREPARE_TOOL), "--repo-path", ".", "--version", "invalid_ver", "--dry-run"], capture_output=True, text=True)
    assert res.returncode != 0
    assert "[REJECTED]" in res.stdout
