import subprocess
import sys
import importlib.util
from pathlib import Path

VERIFY_TOOL = Path(__file__).resolve().parent.parent / "verify.py"

SPEC = importlib.util.spec_from_file_location("cyrene_verify", VERIFY_TOOL)
assert SPEC is not None and SPEC.loader is not None
cyrene_verify = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(cyrene_verify)


def test_verify_docs_scope():
    res = subprocess.run([sys.executable, str(VERIFY_TOOL), "--scope", "docs"], capture_output=True, text=True)
    assert res.returncode == 0
    assert "docs             : PASSED" in res.stdout


def test_verify_governance_scope():
    res = subprocess.run([sys.executable, str(VERIFY_TOOL), "--scope", "governance"], capture_output=True, text=True)
    assert res.returncode == 0
    assert "governance       : PASSED" in res.stdout


def test_verify_rust_fails_when_cargo_is_unavailable(monkeypatch, capsys):
    monkeypatch.setattr(cyrene_verify.shutil, "which", lambda _: None)

    assert cyrene_verify.verify_rust() is False
    assert "Rust verification is required" in capsys.readouterr().out
