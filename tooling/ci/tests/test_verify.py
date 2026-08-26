import subprocess
import sys
from pathlib import Path

VERIFY_TOOL = Path(__file__).resolve().parent.parent / "verify.py"

def test_verify_docs_scope():
    res = subprocess.run([sys.executable, str(VERIFY_TOOL), "--scope", "docs"], capture_output=True, text=True)
    assert res.returncode == 0
    assert "docs             : PASSED" in res.stdout

def test_verify_governance_scope():
    res = subprocess.run([sys.executable, str(VERIFY_TOOL), "--scope", "governance"], capture_output=True, text=True)
    assert res.returncode == 0
    assert "governance       : PASSED" in res.stdout
