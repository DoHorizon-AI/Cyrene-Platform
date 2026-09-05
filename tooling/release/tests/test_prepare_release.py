import subprocess
import sys
from pathlib import Path

PREPARE_TOOL = Path(__file__).resolve().parent.parent / "prepare_release.py"
sys.path.insert(0, str(PREPARE_TOOL.parent))

import prepare_release


def test_prepare_release_validate_mode():
    res = subprocess.run(
        [sys.executable, str(PREPARE_TOOL), "--repo-path", ".", "--version", "0.4.3", "--validate"],
        capture_output=True,
        text=True,
    )
    assert res.returncode == 0
    assert "Planned Release Execution Sequence:" in res.stdout
    assert "Immutable Tag Creation (LAST STEP)" in res.stdout
    assert "Pre-release validation passed" in res.stdout


def test_prepare_release_invalid_version():
    res = subprocess.run(
        [sys.executable, str(PREPARE_TOOL), "--repo-path", ".", "--version", "invalid_ver", "--validate"],
        capture_output=True,
        text=True,
    )
    assert res.returncode != 0
    assert "[REJECTED]" in res.stdout


def test_release_target_is_checked_out_head_not_main(tmp_path: Path):
    subprocess.run(["git", "init", "-b", "main", str(tmp_path)], check=True, capture_output=True)
    subprocess.run(["git", "-C", str(tmp_path), "config", "user.name", "Release Test"], check=True)
    subprocess.run(["git", "-C", str(tmp_path), "config", "user.email", "release@example.invalid"], check=True)
    (tmp_path / "value.txt").write_text("main\n", encoding="utf-8")
    subprocess.run(["git", "-C", str(tmp_path), "add", "value.txt"], check=True)
    subprocess.run(["git", "-C", str(tmp_path), "commit", "-m", "main"], check=True, capture_output=True)
    main_sha = prepare_release.resolve_head(tmp_path)
    subprocess.run(["git", "-C", str(tmp_path), "switch", "-c", "release-candidate"], check=True, capture_output=True)
    (tmp_path / "value.txt").write_text("candidate\n", encoding="utf-8")
    subprocess.run(["git", "-C", str(tmp_path), "commit", "-am", "candidate"], check=True, capture_output=True)

    assert prepare_release.resolve_head(tmp_path) != main_sha
