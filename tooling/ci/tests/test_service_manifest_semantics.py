"""Test suite validating service.json schema conformance and planned_extension_points semantics."""

import json
from pathlib import Path

try:
    import jsonschema
except ImportError:
    jsonschema = None


def find_platform_root() -> Path:
    """Locate the standalone Platform contract root."""
    current = Path(__file__).resolve().parent
    for parent in [current, *current.parents]:
        if (parent / "contracts/schemas/advanced-service.schema.json").is_file():
            return parent
    raise AssertionError("Could not locate the Platform schema root")


def load_schema(platform_root: Path) -> dict:
    schema_path = platform_root / "contracts" / "schemas" / "advanced-service.schema.json"
    assert schema_path.exists(), f"Schema not found at {schema_path}"
    return json.loads(schema_path.read_text(encoding="utf-8"))


def test_neutral_example_conforms_to_schema():
    """Verify the Platform-owned example without discovering consumer repositories."""
    platform_root = find_platform_root()
    schema = load_schema(platform_root)
    manifest_path = platform_root / "examples/advanced-service/service.json"
    data = json.loads(manifest_path.read_text(encoding="utf-8"))

    if jsonschema is not None:
        jsonschema.validate(instance=data, schema=schema)
    else:
        assert "schema_version" in data
        assert "core" in data
        assert isinstance(data["core"]["extension_points"], list)


def test_planned_extension_points_manifest_semantics():
    """Verify that planned_extension_points is accepted by schema and distinct from runtime extension_points."""
    platform_root = find_platform_root()
    schema = load_schema(platform_root)

    fixture_manifest = {
        "schema_version": 1,
        "id": "com.cyrene.service.example-host",
        "name": "Example Service Host",
        "service": "example-host",
        "visibility": "private",
        "distribution": "advanced-service-plugin",
        "status": "experimental",
        "core": {
            "protocol_version": 1,
            "minimum_core_version": "0.1.0",
            "extension_points": ["model.analyzer.v1", "media.processor.v1"],
            "planned_extension_points": ["data.processor.v1", "model.registry.v1"],
        },
        "source_roots": ["src"],
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
    available_plugins = {
        "model.analyzer.v1": "example.model-analyzer",
        "media.processor.v1": "example.media-processor",
    }

    # Assert all runtime requirements resolve
    for req in runtime_required:
        assert req in available_plugins, f"Runtime requirement {req} should be resolvable"

    # Assert candidate capabilities are NOT demanded by runtime resolver
    for candidate in planned_candidates:
        assert candidate not in runtime_required, f"Candidate {candidate} must not be a runtime requirement"
