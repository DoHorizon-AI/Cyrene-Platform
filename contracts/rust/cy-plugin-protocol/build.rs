use std::path::PathBuf;

#[cfg(windows)]
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    use_ascii_temp_dir();
    let proto_dir = PathBuf::from("../../proto");
    let plugin_dir = proto_dir.join("plugin/v1");
    let proto_files = [
        "plugin_protocol.proto",
        "probe.proto",
        "model_analyzer.proto",
        "compat_rule.proto",
        "runtime_builder.proto",
        "execution_engine.proto",
        "training_backend.proto",
        "quantization.proto",
        "gateway_filter.proto",
        "notification.proto",
        "storage.proto",
    ]
    .map(|file| plugin_dir.join(file));

    for path in &proto_files {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    let protoc_include = protoc_include_path()?;
    std::env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path()?);

    let mut config = prost_build::Config::new();
    config.compile_protos(&proto_files, &[&proto_dir, &protoc_include])?;
    Ok(())
}

/// The vendored protoc used by prost-build still consumes its descriptor output
/// path through a Windows ANSI code path.  Keep build-only temporary files out
/// of a non-ASCII user profile (for example `C:\Users\禹航`) so the protocol
/// crates can be built in-place on Windows.
#[cfg(windows)]
fn use_ascii_temp_dir() {
    let current_temp = std::env::temp_dir();
    if current_temp.to_string_lossy().is_ascii() {
        return;
    }

    let Some(system_root) = std::env::var_os("SystemRoot") else {
        return;
    };
    let system_temp = PathBuf::from(system_root).join("Temp");
    if !system_temp.to_string_lossy().is_ascii() {
        return;
    }

    if std::fs::create_dir_all(&system_temp).is_ok() {
        let temp = system_temp.as_os_str();
        std::env::set_var("TEMP", temp);
        std::env::set_var("TMP", temp);
        std::env::set_var("TMPDIR", temp);
    }
}

#[cfg(not(windows))]
fn use_ascii_temp_dir() {}

#[cfg(windows)]
fn protoc_include_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let include_path = protoc_bin_vendored::include_path()?;
    let temp_root = std::env::temp_dir();
    if include_path.to_string_lossy().is_ascii() || !temp_root.to_string_lossy().is_ascii() {
        return Ok(include_path);
    }

    let ascii_include = temp_root.join("cyrene-protoc-vendored-include");
    copy_directory(&include_path, &ascii_include)?;
    Ok(ascii_include)
}

#[cfg(not(windows))]
fn protoc_include_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
    Ok(protoc_bin_vendored::include_path()?)
}

#[cfg(windows)]
fn copy_directory(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_directory(&source_path, &destination_path)?;
        } else {
            std::fs::copy(source_path, destination_path)?;
        }
    }
    Ok(())
}
