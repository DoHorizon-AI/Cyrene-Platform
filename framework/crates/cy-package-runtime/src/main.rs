use std::{
    env,
    io::{self, BufReader},
    path::PathBuf,
    process::ExitCode,
    sync::Arc,
};

use cy_package_runtime::{
    FilesystemPackageRuntime, PackageRuntimeControlServer, PythonPluginServiceSupervisor,
    PythonVenvDependencyPreparer, ServiceActivationOptions,
};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("cy-package-runtime: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let configuration = Configuration::parse(env::args().skip(1))?;
    let mut dependency_preparer =
        PythonVenvDependencyPreparer::new(&configuration.python_executable)
            .with_uv_executable(&configuration.uv_executable);
    if let Some(wheelhouse) = configuration.offline_wheelhouse {
        dependency_preparer = dependency_preparer.offline(wheelhouse);
    }
    let service_options = ServiceActivationOptions {
        python_executable: Some(configuration.python_executable),
        python_path: configuration.plugin_python_paths,
        ..ServiceActivationOptions::default()
    };
    let runtime = FilesystemPackageRuntime::open(
        configuration.root,
        Arc::new(dependency_preparer),
        Box::new(PythonPluginServiceSupervisor::default()),
        service_options,
    )
    .map_err(|error| error.to_string())?;
    PackageRuntimeControlServer::new(runtime)
        .run(BufReader::new(io::stdin().lock()), io::stdout().lock())
        .map_err(|error| error.to_string())
}

struct Configuration {
    root: PathBuf,
    python_executable: String,
    uv_executable: String,
    offline_wheelhouse: Option<PathBuf>,
    plugin_python_paths: Vec<PathBuf>,
}

impl Configuration {
    fn parse(arguments: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut root = None;
        let mut python_executable = "python3".to_string();
        let mut uv_executable = "uv".to_string();
        let mut offline_wheelhouse = None;
        let mut plugin_python_paths = Vec::new();
        let mut arguments = arguments.peekable();
        while let Some(argument) = arguments.next() {
            let value = |arguments: &mut std::iter::Peekable<_>| {
                arguments
                    .next()
                    .ok_or_else(|| format!("missing value after {argument}"))
            };
            match argument.as_str() {
                "--root" => root = Some(PathBuf::from(value(&mut arguments)?)),
                "--python" => python_executable = value(&mut arguments)?,
                "--uv" => uv_executable = value(&mut arguments)?,
                "--offline-wheelhouse" => {
                    offline_wheelhouse = Some(PathBuf::from(value(&mut arguments)?));
                }
                "--plugin-python-path" => {
                    plugin_python_paths.push(PathBuf::from(value(&mut arguments)?));
                }
                "--help" | "-h" => {
                    return Err(
                        "usage: cy-package-runtime --root PATH [--python PATH] [--offline-wheelhouse PATH] [--plugin-python-path PATH]".to_string(),
                    );
                }
                _ => return Err(format!("unknown argument: {argument}")),
            }
        }
        Ok(Self {
            root: root.ok_or_else(|| "--root is required".to_string())?,
            python_executable,
            uv_executable,
            offline_wheelhouse,
            plugin_python_paths,
        })
    }
}
