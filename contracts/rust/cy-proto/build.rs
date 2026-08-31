// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: contracts/rust/cy-proto/build.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
use std::path::PathBuf;

#[cfg(windows)]
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    use_ascii_temp_dir();
    let proto_dir = PathBuf::from("../../proto");
    let core_proto = proto_dir.join("cyrene/core/v1/cyrene_core.proto");
    let authority_proto = proto_dir.join("cyrene/core/v1/kernel_authority.proto");
    let authority_v2_proto = proto_dir.join("cyrene/core/v2/kernel_authority.proto");
    let hardware_adapter_proto = proto_dir.join("cyrene/hardware/v1/hardware_adapter.proto");
    let sandbox_adapter_proto = proto_dir.join("cyrene/sandbox/v1/sandbox_adapter.proto");
    let semantic_contract_proto = proto_dir.join("cyrene/semantic/v1/kernel_contract.proto");
    let provider_proto = proto_dir.join("cyrene/provider/v1/kernel_provider.proto");
    let service_supervision_proto = proto_dir.join("cyrene/core/v1/service_supervision.proto");
    let capability_execution_proto =
        proto_dir.join("cyrene/capability/v1/capability_execution.proto");
    let message_connector_proto =
        proto_dir.join("cyrene/message/connector/v1/message_connector.proto");

    println!("cargo:rerun-if-changed={}", core_proto.display());
    println!("cargo:rerun-if-changed={}", authority_proto.display());
    println!("cargo:rerun-if-changed={}", authority_v2_proto.display());
    println!(
        "cargo:rerun-if-changed={}",
        service_supervision_proto.display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        capability_execution_proto.display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        message_connector_proto.display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        proto_dir.join("cyrene/core/v1").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        hardware_adapter_proto.display()
    );
    println!("cargo:rerun-if-changed={}", sandbox_adapter_proto.display());
    println!(
        "cargo:rerun-if-changed={}",
        semantic_contract_proto.display()
    );
    println!("cargo:rerun-if-changed={}", provider_proto.display());
    let protoc_include = protoc_include_path()?;
    std::env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path()?);

    tonic_build::configure()
        .build_client(true)
        .build_server(true)
        .build_transport(false)
        .compile(
            &[
                core_proto,
                authority_proto,
                authority_v2_proto,
                service_supervision_proto,
                capability_execution_proto,
                message_connector_proto,
                hardware_adapter_proto,
                sandbox_adapter_proto,
                semantic_contract_proto,
                provider_proto,
            ],
            &[proto_dir, protoc_include],
        )?;

    Ok(())
}

/// Keep prost/tonic's temporary descriptor output on an ASCII-only path on
/// Windows.  The vendored protoc binary cannot consume the default temp path
/// when the Windows user profile contains non-ASCII characters.
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
