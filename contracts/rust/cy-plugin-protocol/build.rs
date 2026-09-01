// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: contracts/rust/cy-plugin-protocol/build.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    std::env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path()?);
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?);
    let proto_dir = manifest_dir.join("../../proto");
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

    let mut config = prost_build::Config::new();
    config.compile_protos(&proto_files, &[&proto_dir])?;
    Ok(())
}
