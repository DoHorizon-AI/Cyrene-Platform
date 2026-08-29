use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    use_ascii_temp_dir();
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?);
    let proto_dir = PathBuf::from("../../proto");
    let ai_proto = proto_dir.join("ai_service.proto");
    let agent_proto = proto_dir.join("agent_service.proto");
    let absolute_ai_proto = manifest_dir.join(&ai_proto);
    let absolute_agent_proto = manifest_dir.join(&agent_proto);

    println!("cargo:rerun-if-changed={}", absolute_ai_proto.display());
    println!("cargo:rerun-if-changed={}", absolute_agent_proto.display());
    std::env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path()?);

    tonic_build::configure()
        .build_client(true)
        .build_server(true)
        .compile(&[ai_proto, agent_proto], &[proto_dir])?;

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
