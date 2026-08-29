#[cfg(windows)]
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    use_ascii_temp_dir();
    for proto in [
        "../../proto/plugin/v1/plugin_protocol.proto",
        "../../proto/plugin/v1/probe.proto",
        "../../proto/plugin/v1/model_analyzer.proto",
        "../../proto/plugin/v1/compat_rule.proto",
        "../../proto/plugin/v1/runtime_builder.proto",
        "../../proto/plugin/v1/execution_engine.proto",
        "../../proto/plugin/v1/training_backend.proto",
        "../../proto/plugin/v1/quantization.proto",
        "../../proto/plugin/v1/gateway_filter.proto",
        "../../proto/plugin/v1/notification.proto",
        "../../proto/plugin/v1/storage.proto",
    ] {
        println!("cargo:rerun-if-changed={proto}");
    }
    std::env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path()?);
    let mut config = prost_build::Config::new();
    config.compile_protos(
        &[
            "../../proto/plugin/v1/plugin_protocol.proto",
            "../../proto/plugin/v1/probe.proto",
            "../../proto/plugin/v1/model_analyzer.proto",
            "../../proto/plugin/v1/compat_rule.proto",
            "../../proto/plugin/v1/runtime_builder.proto",
            "../../proto/plugin/v1/execution_engine.proto",
            "../../proto/plugin/v1/training_backend.proto",
            "../../proto/plugin/v1/quantization.proto",
            "../../proto/plugin/v1/gateway_filter.proto",
            "../../proto/plugin/v1/notification.proto",
            "../../proto/plugin/v1/storage.proto",
        ],
        &["../../"],
    )?;
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
