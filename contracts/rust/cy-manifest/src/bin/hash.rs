//! `cy-manifest` CLI: compute the content id of a manifest.
//!
//! Usage:
//!   cy-manifest hash [--type <kind>] <input.json|input.yaml>
//!   cy-manifest [--type <kind>] <input.json|input.yaml>   # `hash` is the default subcommand
//!
//! `<kind>` is one of: runtime (default), training-revision, checkpoint,
//! artifact. Reads a JSON or YAML manifest of that type (format inferred from
//! the extension, with a JSON-then-YAML fallback) and prints the computed id
//! ("sha256:<hex>") to stdout. This backs the REBUILD_PLAN acceptance
//! `cy manifest hash <input>` and cross-language identity with the Python
//! `python -m cy_manifest.hash` entrypoint.

use std::path::Path;
use std::process::ExitCode;

use cy_manifest::{
    artifact_id, checkpoint_id, revision_id, runtime_id, ArtifactManifest, CheckpointMetadata,
    RuntimeManifest, TrainingRevision,
};

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();

    // Accept an optional leading `hash` subcommand.
    if args.first().map(|s| s.as_str()) == Some("hash") {
        args.remove(0);
    }

    // Parse an optional `--type <kind>` flag (anywhere before the path).
    let mut kind = "runtime".to_string();
    let mut positional: Vec<String> = Vec::new();
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--type" | "-t" => match it.next() {
                Some(v) => kind = v,
                None => {
                    eprintln!("error: --type requires a value");
                    return ExitCode::from(2);
                }
            },
            other if other.starts_with("--type=") => {
                kind = other["--type=".len()..].to_string();
            }
            _ => positional.push(arg),
        }
    }

    let path = match positional.first() {
        Some(p) => p.clone(),
        None => {
            eprintln!(
                "usage: cy-manifest hash [--type runtime|training-revision|checkpoint|artifact] <input.json|input.yaml>"
            );
            return ExitCode::from(2);
        }
    };

    let contents = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: cannot read {path}: {e}");
            return ExitCode::FAILURE;
        }
    };

    let id = match kind.as_str() {
        "runtime" | "runtime-manifest" => {
            parse::<RuntimeManifest>(&path, &contents).map(|m| runtime_id(&m))
        }
        "training-revision" | "revision" => {
            parse::<TrainingRevision>(&path, &contents).map(|m| revision_id(&m))
        }
        "checkpoint" | "checkpoint-metadata" => {
            parse::<CheckpointMetadata>(&path, &contents).map(|m| checkpoint_id(&m))
        }
        "artifact" | "artifact-manifest" => {
            parse::<ArtifactManifest>(&path, &contents).map(|m| artifact_id(&m))
        }
        other => {
            eprintln!(
                "error: unknown --type '{other}' (expected runtime|training-revision|checkpoint|artifact)"
            );
            return ExitCode::from(2);
        }
    };

    match id {
        Ok(id) => {
            println!("{id}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: cannot parse {path} as {kind}: {e}");
            ExitCode::FAILURE
        }
    }
}

fn parse<T: serde::de::DeserializeOwned>(path: &str, contents: &str) -> Result<T, String> {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();

    match ext.as_str() {
        "json" => serde_json::from_str(contents).map_err(|e| e.to_string()),
        "yaml" | "yml" => serde_yaml::from_str(contents).map_err(|e| e.to_string()),
        _ => {
            // Unknown extension: try JSON first, then YAML.
            serde_json::from_str(contents)
                .map_err(|e| e.to_string())
                .or_else(|_| serde_yaml::from_str(contents).map_err(|e| e.to_string()))
        }
    }
}
