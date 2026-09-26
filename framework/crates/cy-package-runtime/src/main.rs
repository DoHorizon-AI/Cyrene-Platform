use std::{
    env,
    ffi::OsString,
    io::{self, BufReader},
    path::PathBuf,
    process::ExitCode,
    sync::Arc,
};

use cy_package_runtime::{
    CommandDependencyPreparer, FilesystemPackageRuntime, PackageRuntimeControlServer,
    ProcessPluginServiceSupervisor, ServiceActivationOptions,
};

fn main() -> ExitCode {
    let log_format = std::env::var("CYRENE_LOG_FORMAT")
        .ok()
        .and_then(|f| f.parse().ok())
        .unwrap_or(cy_observability::LogFormat::Json);
    let log_level = std::env::var("CYRENE_LOG_LEVEL")
        .or_else(|_| std::env::var("RUST_LOG"))
        .unwrap_or_else(|_| "info".to_string());
    let obs_config = cy_observability::ObservabilityConfig::managed("cy-package-runtime")
        .with_format(log_format)
        .with_log_level(log_level);
    let _guard = cy_observability::init_observability(obs_config).ok();

    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            tracing::error!(
                event.name = "platform.package.runtime_failed",
                error.code = cy_observability::PlatformErrorCode::PackageActivationFailed.as_str(),
                message = %message,
            );
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let configuration = Configuration::parse(env::args().skip(1))?;
    let dependency_preparer = CommandDependencyPreparer::new(configuration.dependency_preparer)
        .with_args(configuration.dependency_preparer_args);
    let runtime = FilesystemPackageRuntime::open(
        configuration.root,
        Arc::new(dependency_preparer),
        Box::new(ProcessPluginServiceSupervisor::default()),
        ServiceActivationOptions::default(),
    )
    .map_err(|error| error.to_string())?;
    PackageRuntimeControlServer::new(runtime)
        .run(BufReader::new(io::stdin().lock()), io::stdout().lock())
        .map_err(|error| error.to_string())
}

struct Configuration {
    root: PathBuf,
    dependency_preparer: PathBuf,
    dependency_preparer_args: Vec<OsString>,
}

impl Configuration {
    fn parse(arguments: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut root = None;
        let mut dependency_preparer = None;
        let mut dependency_preparer_args = Vec::new();
        let mut arguments = arguments.peekable();
        while let Some(argument) = arguments.next() {
            let value = |arguments: &mut std::iter::Peekable<_>| {
                arguments
                    .next()
                    .ok_or_else(|| format!("missing value after {argument}"))
            };
            match argument.as_str() {
                "--root" => root = Some(PathBuf::from(value(&mut arguments)?)),
                "--dependency-preparer" => {
                    dependency_preparer = Some(PathBuf::from(value(&mut arguments)?));
                }
                "--dependency-preparer-arg" => {
                    dependency_preparer_args.push(OsString::from(value(&mut arguments)?));
                }
                "--help" | "-h" => {
                    return Err(
                        "usage: cy-package-runtime --root PATH --dependency-preparer PATH [--dependency-preparer-arg VALUE]".to_string(),
                    );
                }
                _ => return Err(format!("unknown argument: {argument}")),
            }
        }
        Ok(Self {
            root: root.ok_or_else(|| "--root is required".to_string())?,
            dependency_preparer: dependency_preparer
                .ok_or_else(|| "--dependency-preparer is required".to_string())?,
            dependency_preparer_args,
        })
    }
}
