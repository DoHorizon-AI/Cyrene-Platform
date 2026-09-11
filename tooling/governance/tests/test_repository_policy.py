"""Tests for the Platform-local repository policy validator."""

import subprocess
import sys
from pathlib import Path

VALIDATOR = Path(__file__).resolve().parent.parent / "validate_repository_policy.py"


def test_repository_policy_validates_cleanly() -> None:
    result = subprocess.run([sys.executable, str(VALIDATOR)], capture_output=True, text=True)
    assert result.returncode == 0, result.stdout + result.stderr
    assert "Repository policy and lifecycle document" in result.stdout
