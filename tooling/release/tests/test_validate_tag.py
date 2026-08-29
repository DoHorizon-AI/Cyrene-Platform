import subprocess
import sys
from pathlib import Path

VALIDATE_TAG_TOOL = Path(__file__).resolve().parent.parent / "validate_tag.py"

def test_validate_single_component_tag():
    res = subprocess.run([sys.executable, str(VALIDATE_TAG_TOOL), "--repo-path", ".", "--tag", "v0.4.2"], capture_output=True, text=True)
    assert res.returncode == 0
    assert "is syntactically valid" in res.stdout

def test_validate_multi_component_tag():
    res = subprocess.run([sys.executable, str(VALIDATE_TAG_TOOL), "--repo-path", ".", "--tag", "hf-analyzer/v1.2.0", "--multi-component"], capture_output=True, text=True)
    assert res.returncode == 0
    assert "is syntactically valid" in res.stdout

def test_invalid_tag_format():
    res = subprocess.run([sys.executable, str(VALIDATE_TAG_TOOL), "--repo-path", ".", "--tag", "release-2026"], capture_output=True, text=True)
    assert res.returncode != 0
