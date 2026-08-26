import subprocess
import sys
from pathlib import Path

VALIDATOR = Path(__file__).resolve().parent.parent / "validate_repository_policy.py"

def test_repository_policies_validate_cleanly():
    res = subprocess.run([sys.executable, str(VALIDATOR)], capture_output=True, text=True)
    assert res.returncode == 0
    assert "All repository-policy.yaml files" in res.stdout
