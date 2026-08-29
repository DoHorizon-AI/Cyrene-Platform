import subprocess
import sys
from pathlib import Path

VALIDATE_TOOL = Path(__file__).resolve().parent.parent / "validate_docs.py"

def test_validate_docs_runs_cleanly():
    res = subprocess.run([sys.executable, str(VALIDATE_TOOL)], capture_output=True, text=True)
    assert res.returncode == 0
    assert "All documentation links" in res.stdout
