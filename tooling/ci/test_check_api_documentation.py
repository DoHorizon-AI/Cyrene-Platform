import importlib.util
from pathlib import Path

# Load check_api_documentation module dynamically
_tooling_path = Path(__file__).resolve().parent / "check_api_documentation.py"
_spec = importlib.util.spec_from_file_location("check_api_documentation", _tooling_path)
_module = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_module)

find_workspace_root = _module.find_workspace_root
verify_workspace_api_docs = _module.verify_workspace_api_docs
verify_single_repo = _module.verify_single_repo
verify_capability_index = _module.verify_capability_index
verify_plugin_docs = _module.verify_plugin_docs


def test_workspace_api_documentation_guard():
    root = find_workspace_root()
    api_errors = verify_workspace_api_docs(root)
    assert not api_errors, f"Workspace API documentation errors: {api_errors}"


def test_standalone_repo_guard():
    root = find_workspace_root()
    platform_dir = root / "Cyrene-Platform"
    errors = verify_single_repo(platform_dir, "Cyrene-Platform", "ACTIVE_PLATFORM")
    assert not errors, f"Standalone platform guard errors: {errors}"


def test_capability_index_stability():
    root = find_workspace_root()
    cap_errors = verify_capability_index(root)
    assert not cap_errors, f"Capability index stability errors: {cap_errors}"


def test_plugin_documentation_coverage():
    root = find_workspace_root()
    plugin_errors = verify_plugin_docs(root)
    assert not plugin_errors, f"Plugin documentation errors: {plugin_errors}"
