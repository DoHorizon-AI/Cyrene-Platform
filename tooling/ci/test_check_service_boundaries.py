"""Unit test for Service Boundary & Repository Governance Guard."""

from pathlib import Path
import sys

ci_dir = Path(__file__).resolve().parent
sys.path.insert(0, str(ci_dir))

from check_service_boundaries import check_service_boundaries, find_workspace_root


def test_service_boundaries_on_workspace() -> None:
    workspace_root = find_workspace_root()
    violations = check_service_boundaries(workspace_root)
    assert not violations, f"Expected 0 service boundary violations, got: {violations}"
