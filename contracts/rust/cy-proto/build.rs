use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?);
    let proto_dir = manifest_dir.join("../../proto");
    let ai_proto = proto_dir.join("ai_service.proto");
    let agent_proto = proto_dir.join("agent_service.proto");

    println!("cargo:rerun-if-changed={}", ai_proto.display());
    println!("cargo:rerun-if-changed={}", agent_proto.display());
    std::env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path()?);

    tonic_build::configure()
        .build_client(true)
        .build_server(true)
        .compile(&[ai_proto, agent_proto], &[proto_dir])?;

    Ok(())
}
