import subprocess
import sys
from pathlib import Path

WORKSPACE_TOOL = Path(__file__).resolve().parent.parent / "workspace.py"

def test_workspace_status_runs():
    res = subprocess.run([sys.executable, str(WORKSPACE_TOOL), "status"], capture_output=True, text=True)
    assert res.returncode == 0
    assert "Cyrene Workspace Status" in res.stdout
    assert "Cyrene-Platform" in res.stdout
    assert "Cyrene-Plugins" in res.stdout

def test_workspace_doctor_runs():
    res = subprocess.run([sys.executable, str(WORKSPACE_TOOL), "doctor"], capture_output=True, text=True)
    assert res.returncode == 0
    assert "Cyrene Developer Environment Doctor" in res.stdout
    assert "Git" in res.stdout
    assert "Python" in res.stdout