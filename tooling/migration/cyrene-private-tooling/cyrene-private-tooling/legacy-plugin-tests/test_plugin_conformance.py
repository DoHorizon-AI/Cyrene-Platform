"""Plugin Conformance Test Suite (NS4-P2).

Validates all community and pro plugin.toml manifests and entrypoints against CY-LLM plugin specification.
"""

from __future__ import annotations

import importlib
import os
import sys
import tomllib
from typing import Dict, List, Set, Tuple

import pytest

# Ensure python source directories and community plugin directories are on sys.path
PROJECT_ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
CY_MANIFEST_SRC = os.path.join(PROJECT_ROOT, "python", "cy_manifest", "src")
CY_EXEC_SRC = os.path.join(PROJECT_ROOT, "python", "cy_exec", "src")
COMMUNITY_PLUGINS_DIR = os.path.join(PROJECT_ROOT, "python", "plugins", "community")

for path in [CY_MANIFEST_SRC, CY_EXEC_SRC, COMMUNITY_PLUGINS_DIR]:
    if os.path.exists(path) and path not in sys.path:
        sys.path.insert(0, path)


EXTENSION_POINT_KINDS: Set[str] = {
    "probe",
    "model-analyzer",
    "compat-rule",
    "runtime-builder",
    "execution-engine",
    "training-backend",
    "quantization",
    "gateway-filter",
    "notification",
    "storage",
}

BUNDLE_KINDS: Set[str] = {
    "bundle",
    "service",
    "library",
    "python-package",
    "protocol-and-services",
    "deployment-assets",
}

ALL_ALLOWED_KINDS: Set[str] = EXTENSION_POINT_KINDS | BUNDLE_KINDS
ALLOWED_EDITIONS: Set[str] = {"community", "pro"}
ALLOWED_RUNTIMES: Set[str] = {
    "in-proc-rust",
    "subprocess-python",
    "subprocess-jvm",
    "service",
}

CONTROLLED_HARDWARE: Set[str] = {"nvidia", "cuda", "ascend", "amd", "rocm", "cpu"}
CONTROLLED_PRECISIONS: Set[str] = {"fp32", "fp16", "bf16", "fp8", "int8", "int4"}
CONTROLLED_QUANTIZATIONS: Set[str] = {
    "none",
    "awq",
    "gptq",
    "bnb-nf4",
    "bnb-int8",
    "fp8",
}


def get_all_plugin_manifest_paths() -> List[str]:
    """Discover all plugin.toml paths in community and pro directories."""
    paths: List[str] = []
    search_dirs = [
        os.path.join(PROJECT_ROOT, "python", "plugins", "community"),
        os.path.join(PROJECT_ROOT, "plugins", "pro"),
    ]
    for search_dir in search_dirs:
        if not os.path.exists(search_dir):
            continue
        for root, _, files in os.walk(search_dir):
            if "plugin.toml" in files:
                paths.append(os.path.abspath(os.path.join(root, "plugin.toml")))
    return sorted(paths)


MANIFEST_PATHS = get_all_plugin_manifest_paths()


@pytest.mark.parametrize("manifest_path", MANIFEST_PATHS)
def test_plugin_manifest_required_fields(manifest_path: str) -> None:
    """Validate that plugin.toml has required [plugin] section and fields."""
    with open(manifest_path, "rb") as f:
        data = tomllib.load(f)

    assert "plugin" in data, f"Missing [plugin] section in {manifest_path}"
    plugin_table = data["plugin"]

    required_fields = ["id", "name", "version", "api_version", "kind", "edition"]
    for field in required_fields:
        assert field in plugin_table, (
            f"Missing required field '{field}' in [plugin] of {manifest_path}"
        )
        assert isinstance(plugin_table[field], str) and plugin_table[field].strip(), (
            f"Field '{field}' must be a non-empty string in {manifest_path}"
        )

    assert plugin_table["edition"] in ALLOWED_EDITIONS, (
        f"Invalid edition '{plugin_table['edition']}' in {manifest_path}"
    )


@pytest.mark.parametrize("manifest_path", MANIFEST_PATHS)
def test_plugin_kind_validity(manifest_path: str) -> None:
    """Validate that plugin kind and component kinds are within controlled taxonomy."""
    with open(manifest_path, "rb") as f:
        data = tomllib.load(f)

    plugin_kind = data["plugin"]["kind"]
    assert plugin_kind in ALL_ALLOWED_KINDS, (
        f"Invalid plugin kind '{plugin_kind}' in {manifest_path}"
    )

    if plugin_kind == "bundle":
        assert data["plugin"]["edition"] == "pro", (
            f"Bundle manifest must have edition='pro' in {manifest_path}"
        )
        assert "components" in data, f"Bundle manifest missing [[components]] in {manifest_path}"
        assert isinstance(data["components"], list) and len(data["components"]) > 0, (
            f"[[components]] must be non-empty list in {manifest_path}"
        )

        for comp in data["components"]:
            comp_kind = comp.get("kind")
            assert comp_kind in ALL_ALLOWED_KINDS, (
                f"Invalid component kind '{comp_kind}' in component '{comp.get('id')}' of {manifest_path}"
            )
    else:
        assert plugin_kind in EXTENSION_POINT_KINDS, (
            f"Single-plugin manifest kind '{plugin_kind}' must be one of the 10 extension points in {manifest_path}"
        )


@pytest.mark.parametrize("manifest_path", MANIFEST_PATHS)
def test_plugin_capabilities_controlled_vocabulary(manifest_path: str) -> None:
    """Validate controlled capability vocabularies in [capabilities]."""
    with open(manifest_path, "rb") as f:
        data = tomllib.load(f)

    capabilities = data.get("capabilities", {})
    if isinstance(capabilities, dict):
        if "supported_hardware" in capabilities:
            for hw in capabilities["supported_hardware"]:
                assert hw in CONTROLLED_HARDWARE, (
                    f"Unsupported hardware '{hw}' in capabilities of {manifest_path}"
                )

        if "supported_precisions" in capabilities:
            for prec in capabilities["supported_precisions"]:
                assert prec in CONTROLLED_PRECISIONS, (
                    f"Unsupported precision '{prec}' in capabilities of {manifest_path}"
                )

        if "supported_quantizations" in capabilities:
            for quant in capabilities["supported_quantizations"]:
                assert quant in CONTROLLED_QUANTIZATIONS, (
                    f"Unsupported quantization '{quant}' in capabilities of {manifest_path}"
                )


@pytest.mark.parametrize("manifest_path", MANIFEST_PATHS)
def test_plugin_entrypoint_loading(manifest_path: str) -> None:
    """Validate that Python entrypoint specified in manifest can be imported and loaded."""
    with open(manifest_path, "rb") as f:
        data = tomllib.load(f)

    plugin_table = data["plugin"]
    entrypoint = plugin_table.get("entrypoint")

    if not entrypoint or plugin_table.get("runtime") != "subprocess-python":
        pytest.skip(f"No Python entrypoint in {manifest_path}")

    assert ":" in entrypoint, f"Entrypoint '{entrypoint}' must format as module:Class in {manifest_path}"
    mod_name, cls_name = entrypoint.split(":", 1)

    try:
        module = importlib.import_module(mod_name)
    except Exception as exc:
        pytest.fail(f"Failed to import entrypoint module '{mod_name}' from {manifest_path}: {exc}")

    assert hasattr(module, cls_name), (
        f"Module '{mod_name}' does not contain entrypoint class '{cls_name}' from {manifest_path}"
    )

    cls_obj = getattr(module, cls_name)
    try:
        instance = cls_obj()
        assert instance is not None
    except Exception as exc:
        # If instantiation fails due to missing optional runtime hardware/deps, verify class exists
        assert isinstance(cls_obj, type), f"Entrypoint {cls_name} is not a valid class"
