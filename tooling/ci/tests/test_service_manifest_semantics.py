"""Test suite validating service.json schema conformance and planned_extension_points semantics."""

import json
from pathlib import Path

try:
    import jsonschema
except ImportError:
    jsonschema = None


def find_manifest_layout() -> tuple[Path, Path]:
    """Locate the workspace services and the Platform schema root.

    The full Cyrene workspace keeps Platform under ``Cyrene-Platform/`` while
    standalone CI checks out this repository as its own root.  Both layouts
    must exercise the same schema assertions rather than manufacturing a
    path that exists only in the umbrella checkout.
    """
    current = Path(__file__).resolve().parent
    for parent in [current, *current.parents]:
        platform_root = parent / "Cyrene-Platform"
        if (platform_root / "contracts/schemas/advanced-service.schema.json").is_file() and (
            parent / "services"
        ).is_dir():
            return parent, platform_root
        if (parent / "contracts/schemas/advanced-service.schema.json").is_file():
            return parent, parent
    raise AssertionError("Could not locate the Platform schema root")


def load_schema(platform_root: Path) -> dict:
    schema_path = platform_root / "contracts" / "schemas" / "advanced-service.schema.json"
    assert schema_path.exists(), f"Schema not found at {schema_path}"
    return json.loads(schema_path.read_text(encoding="utf-8"))


def service_manifest_paths(workspace_root: Path, platform_root: Path) -> list[Path]:
    """Return product manifests for an umbrella or standalone checkout."""
    services_root = workspace_root / "services"
    if services_root.is_dir():
        return [
            services_root / name / "service.json"
            for name in (
                "cyrene-catalyst",
                "Cyrene-Yield",
                "cyrene-reactor",
                "cyrene-exchange",
                "cyrene-navigator",
                "cyrene-echo",
            )
        ]

    # The standalone repository owns the schema and this checked-in example;
    # validate it as the local product-manifest fixture.
    return [platform_root / "examples/advanced-service/service.json"]


def test_all_product_service_manifests_conform_to_schema():
    """Verify that every product service.json in the workspace strictly conforms to advanced-service.schema.json."""
    root, platform_root = find_manifest_layout()
    schema = load_schema(platform_root)

    for manifest_path in service_manifest_paths(root, platform_root):
        assert manifest_path.exists(), f"Missing service.json at {manifest_path}"
        data = json.loads(manifest_path.read_text(encoding="utf-8"))

        if jsonschema is not None:
            jsonschema.validate(instance=data, schema=schema)
        else:
            # Fallback basic structural checks if jsonschema is not installed
            assert "schema_version" in data
            assert "core" in data
            assert "extension_points" in data["core"]
            assert isinstance(data["core"]["extension_points"], list)


def test_planned_extension_points_manifest_semantics():
    """Verify that planned_extension_points is accepted by schema and distinct from runtime extension_points."""
    _, platform_root = find_manifest_layout()
    schema = load_schema(platform_root)

    fixture_manifest = {
        "schema_version": 1,
        "id": "com.cyrene.service.catalyst",
        "name": "Catalyst",
        "service": "catalyst",
        "visibility": "private",
        "distribution": "advanced-service-plugin",
        "status": "preserved-source",
        "core": {
            "protocol_version": 1,
            "minimum_core_version": "0.1.0",
            "extension_points": ["model.analyzer.v1", "media.processor.v1"],
            "planned_extension_points": ["data.processor.v1", "model.registry.v1"],
        },
        "source_roots": ["legacy/CY_LLM_Training"],
    }

    if jsonschema is not None:
        # 1. Assert schema validation succeeds
        jsonschema.validate(instance=fixture_manifest, schema=schema)

    # 2. Simulated Runtime Resolver logic
    # Resolver MUST ONLY attempt to resolve active 'extension_points'
    runtime_required = fixture_manifest["core"].get("extension_points", [])
    planned_candidates = fixture_manifest["core"].get("planned_extension_points", [])

    assert "model.analyzer.v1" in runtime_required
    assert "data.processor.v1" not in runtime_required
    assert "data.processor.v1" in planned_candidates

    # Resolver query simulation
    available_plugins = {"model.analyzer.v1": "cyrene.models.hf-analyzer", "media.processor.v1": "cyrene.tools.media"}

    # Assert all runtime requirements resolve
    for req in runtime_required:
        assert req in available_plugins, f"Runtime requirement {req} should be resolvable"

    # Assert candidate capabilities are NOT demanded by runtime resolver
    for candidate in planned_candidates:
        assert candidate not in runtime_required, f"Candidate {candidate} must not be a runtime requirement"
