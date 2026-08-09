fn main() -> Result<(), Box<dyn std::error::Error>> {
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
