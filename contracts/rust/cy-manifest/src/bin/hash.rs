// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: contracts/rust/cy-manifest/src/bin/hash.rs
// ║ Module: CYRENE Platform
// ║ Role: Portable directory manifest identity utility.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：计算可移植目录清单的内容身份。
// ╚══════════════════════════════════════════════════════════════════════╝
//! Compute the content identity of a provider-neutral portable directory.

use std::path::Path;
use std::process::ExitCode;

use cy_manifest::{portable_directory_id, PortableDirectoryManifest};

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("hash") {
        args.remove(0);
    }
    if args.first().map(String::as_str) == Some("--type") {
        if args.get(1).map(String::as_str) != Some("portable-directory") {
            eprintln!("error: only --type portable-directory is supported");
            return ExitCode::from(2);
        }
        args.drain(0..2);
    }
    let Some(path) = args.first() else {
        eprintln!("usage: cy-manifest hash [--type portable-directory] <input.json|input.yaml>");
        return ExitCode::from(2);
    };
    if args.len() != 1 {
        eprintln!("error: expected exactly one manifest path");
        return ExitCode::from(2);
    }

    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) => {
            eprintln!("error: cannot read {path}: {error}");
            return ExitCode::FAILURE;
        }
    };
    let result = parse::<PortableDirectoryManifest>(path, &contents).and_then(|manifest| {
        manifest.validate()?;
        Ok(portable_directory_id(&manifest))
    });
    match result {
        Ok(id) => {
            println!("{id}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: cannot parse {path} as a portable directory manifest: {error}");
            ExitCode::FAILURE
        }
    }
}

fn parse<T: serde::de::DeserializeOwned>(path: &str, contents: &str) -> Result<T, String> {
    match Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("json") => serde_json::from_str(contents).map_err(|error| error.to_string()),
        Some("yaml" | "yml") => serde_yaml::from_str(contents).map_err(|error| error.to_string()),
        _ => serde_json::from_str(contents)
            .map_err(|error| error.to_string())
            .or_else(|_| serde_yaml::from_str(contents).map_err(|error| error.to_string())),
    }
}
