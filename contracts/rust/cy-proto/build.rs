use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?);
    let proto_dir = manifest_dir.join("../../proto");
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
    let model_provider_proto = proto_dir.join("cyrene/model/provider/v1/model_provider.proto");

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
    println!("cargo:rerun-if-changed={}", model_provider_proto.display());
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
                model_provider_proto,
                hardware_adapter_proto,
                sandbox_adapter_proto,
                semantic_contract_proto,
                provider_proto,
            ],
            &[proto_dir],
        )?;

    Ok(())
}
