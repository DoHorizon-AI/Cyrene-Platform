import subprocess
import sys
from pathlib import Path

INTEGRATION_TOOL = Path(__file__).resolve().parent.parent / "integration.py"

def test_integration_planner_dry_run():
    res = subprocess.run([sys.executable, str(INTEGRATION_TOOL), "--profile", "training", "--dry-run"], capture_output=True, text=True)
    assert res.returncode == 0
    assert "Cyrene-Yield" in res.stdout
    assert "Cyrene-Platform" in res.stdout
