use std::fs;
use std::path::{Path, PathBuf};

use jsonschema::JSONSchema;
use serde_json::Value;

fn contracts_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../")
}

fn read_json(relative_path: impl AsRef<Path>) -> Value {
    let path = contracts_dir().join(relative_path);
    let contents = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    serde_json::from_str(&contents)
        .unwrap_or_else(|error| panic!("invalid JSON in {}: {error}", path.display()))
}

fn assert_valid(relative_schema: impl AsRef<Path>, instance: &Value) {
    let schema_path = relative_schema.as_ref();
    let schema = read_json(schema_path);
    let compiled = JSONSchema::compile(&schema).unwrap_or_else(|error| {
        panic!("invalid JSON Schema in {}: {error}", schema_path.display())
    });

    if let Err(errors) = compiled.validate(instance) {
        let details = errors
            .map(|error| error.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        panic!(
            "contract instance does not validate against {}:\n{details}",
            schema_path.display()
        );
    };
}

#[test]
fn every_checked_in_schema_compiles() {
    let schema_root = contracts_dir().join("schemas");
    let mut schema_paths = Vec::new();

    for entry in fs::read_dir(&schema_root).expect("failed to read contracts/schemas") {
        let path = entry.expect("failed to read schema directory entry").path();
        if path.extension().and_then(|extension| extension.to_str()) == Some("json") {
            schema_paths.push(path);
        }
    }

    let manifests_root = schema_root.join("manifests");
    for entry in fs::read_dir(&manifests_root).expect("failed to read manifest schemas") {
        let path = entry
            .expect("failed to read manifest schema directory entry")
            .path();
        if path.extension().and_then(|extension| extension.to_str()) == Some("json") {
            schema_paths.push(path);
        }
    }

    assert!(!schema_paths.is_empty(), "no JSON Schemas were discovered");
    for path in schema_paths {
        let relative_path = path
            .strip_prefix(contracts_dir())
            .expect("schema path must be inside contracts/");
        let schema = read_json(relative_path);
        JSONSchema::compile(&schema).unwrap_or_else(|error| {
            panic!(
                "invalid JSON Schema in {}: {error}",
                relative_path.display()
            )
        });
    }
}

#[test]
fn checked_in_json_examples_validate_against_contracts() {
    let examples = [
        (
            "schemas/manifests/artifact_manifest.schema.json",
            "schemas/examples/artifact_manifest.example.json",
        ),
        (
            "schemas/manifests/runtime_manifest.schema.json",
            "schemas/examples/runtime_manifest.example.json",
        ),
        (
            "schemas/manifests/training_revision.schema.json",
            "schemas/examples/training_revision.example.json",
        ),
        (
            "schemas/advanced-service.schema.json",
            "../examples/advanced-service/service.json",
        ),
    ];

    for (schema, instance) in examples {
        let instance_value = read_json(instance);
        assert_valid(schema, &instance_value);
    }
}

#[test]
fn checked_in_yaml_example_is_valid_and_matches_json_contract() {
    let yaml_path = contracts_dir().join("schemas/examples/runtime_manifest.example.yaml");
    let yaml_contents = fs::read_to_string(&yaml_path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", yaml_path.display()));
    let instance: Value = serde_yaml::from_str(&yaml_contents)
        .unwrap_or_else(|error| panic!("invalid YAML in {}: {error}", yaml_path.display()));
    assert_valid("schemas/manifests/runtime_manifest.schema.json", &instance);
}

#[test]
fn checked_in_plugin_manifest_is_toml_and_schema_valid() {
    let plugin_path = contracts_dir().join("../examples/plugins/jvm/poc/plugin.toml");
    let plugin_contents = fs::read_to_string(&plugin_path)
        .unwrap_or_else(|error| panic!("invalid TOML in {}: {error}", plugin_path.display()));
    let plugin_toml: toml::Value = toml::from_str(&plugin_contents)
        .unwrap_or_else(|error| panic!("invalid TOML in {}: {error}", plugin_path.display()));
    let plugin_json =
        serde_json::to_value(plugin_toml).expect("TOML value should be representable as JSON");
    assert_valid("schemas/plugin.schema.json", &plugin_json);
}
