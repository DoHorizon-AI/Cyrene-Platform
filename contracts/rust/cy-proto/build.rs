use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?);
    let proto_dir = manifest_dir.join("../../proto");
    let core_proto = proto_dir.join("cyrene/core/v1/cyrene_core.proto");
    let hardware_adapter_proto = proto_dir.join("cyrene/hardware/v1/hardware_adapter.proto");
    let sandbox_adapter_proto = proto_dir.join("cyrene/sandbox/v1/sandbox_adapter.proto");

    println!("cargo:rerun-if-changed={}", core_proto.display());
    println!(
        "cargo:rerun-if-changed={}",
        proto_dir.join("cyrene/core/v1").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        hardware_adapter_proto.display()
    );
    println!("cargo:rerun-if-changed={}", sandbox_adapter_proto.display());
    std::env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path()?);

    tonic_build::configure()
        .build_client(true)
        .build_server(true)
        .build_transport(false)
        .compile(
            &[core_proto, hardware_adapter_proto, sandbox_adapter_proto],
            &[proto_dir],
        )?;

    Ok(())
}
