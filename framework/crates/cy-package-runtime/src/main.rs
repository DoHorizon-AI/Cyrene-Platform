//! Host executable for the persistent package runtime and guarded first install.
//!
//! The normal mode owns the single long-lived runtime. Offline bootstrap mode
//! validates a root maintenance hold, then runs filesystem installation as the
//! existing cyrene service account.
//! 中文：常驻服务与受维护锁保护的一次性离线安装入口。

use std::{
    env,
    ffi::OsString,
    io::{self, Read, Write},
    os::fd::AsRawFd,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, ExitCode, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};

use cy_package_runtime::{
    BootstrapInstallReceipt, BootstrapWorkerError, BootstrapWorkerInput, BootstrapWorkerOutput,
    CommandDependencyPreparer, FilesystemPackageRuntime, InstallationRecord,
    PackageRuntimeControlServer, PackageRuntimeError, PackageRuntimeSocketServer,
    ProcessPluginServiceSupervisor, RuntimeProcessLock, ServiceActivationOptions,
    cleanup_worker_candidate_handoff, ensure_runtime_daemon_stopped, ensure_runtime_state_root,
    prepare_worker_candidate, read_bootstrap_file, read_bootstrap_stdin, read_operator_token,
    validate_bootstrap_input_file_location, validate_bootstrap_toolchain, validate_candidate_paths,
    validate_maintenance_hold, validate_persistent_candidate_paths, verify_root_worker_peer,
};
use nix::unistd::User;
use serde::Serialize;
use serde_json::json;

const DEFAULT_CONTROL_SOCKET: &str = "/run/cyrene-package-runtime/control.sock";
const DEFAULT_SOURCE_POLICY: &str = "/etc/cyrene/runtime-package-sources.json";
const PERSISTENT_BOOTSTRAP_ROOT: &str = "/var/lib/cyrene-updates/plugin-package-bootstrap";
const RUNTIME_ROOT: &str = "/var/lib/cyrene/package-runtime";
const MAX_WORKER_FRAME_BYTES: usize = 64 * 1024;
const MAX_WORKER_STDOUT_BYTES: usize = 64 * 1024;
const MAX_WORKER_STDERR_BYTES: usize = 16 * 1024;
const WORKER_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const WORKER_POLL_INTERVAL: Duration = Duration::from_millis(100);

fn main() -> ExitCode {
    let configuration = match Configuration::parse(env::args().skip(1)) {
        Ok(configuration) => configuration,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    };
    let is_worker = matches!(configuration.mode, ExecutionMode::BootstrapWorker { .. });
    let log_level = if is_worker {
        "off".to_string()
    } else {
        env::var("CYRENE_LOG_LEVEL")
            .or_else(|_| env::var("RUST_LOG"))
            .unwrap_or_else(|_| "info".to_string())
    };
    let log_format = env::var("CYRENE_LOG_FORMAT")
        .ok()
        .and_then(|format| format.parse().ok())
        .unwrap_or(cy_observability::LogFormat::Json);
    let observability = cy_observability::ObservabilityConfig::managed("cy-package-runtime")
        .with_format(log_format)
        .with_log_level(log_level);
    let _guard = cy_observability::init_observability(observability).ok();

    match configuration.mode.clone() {
        ExecutionMode::Service { stdio } => match run_service(configuration, stdio) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                tracing::error!(
                    event.name = "platform.package.runtime_failed",
                    error.code = cy_observability::PlatformErrorCode::PackageActivationFailed.as_str(),
                    message = %error,
                );
                ExitCode::FAILURE
            }
        },
        ExecutionMode::BootstrapInstall { input_file } => {
            run_bootstrap_parent(configuration, input_file)
        }
        ExecutionMode::BootstrapWorker { lock_fd, auth_fd } => {
            run_bootstrap_worker(configuration, lock_fd, auth_fd)
        }
    }
}

fn run_service(configuration: Configuration, stdio: bool) -> Result<(), PackageRuntimeError> {
    let dependency_preparer = CommandDependencyPreparer::new(configuration.dependency_preparer)
        .with_args(configuration.dependency_preparer_args);
    let runtime = FilesystemPackageRuntime::open(
        configuration.root,
        Arc::new(dependency_preparer),
        Box::new(ProcessPluginServiceSupervisor::default()),
        ServiceActivationOptions::default(),
    )?;
    let control = PackageRuntimeControlServer::new(runtime);
    if stdio {
        control
            .run(io::BufReader::new(io::stdin().lock()), io::stdout().lock())
            .map_err(|error| PackageRuntimeError::new("CONTROL_SERVER_FAILED", error.to_string()))
    } else {
        PackageRuntimeSocketServer::new(control, configuration.socket, configuration.source_policy)?
            .serve()
    }
}

fn run_bootstrap_parent(configuration: Configuration, input_file: Option<PathBuf>) -> ExitCode {
    let mut response_request_id = String::new();
    let result = install_offline_as_root(configuration, input_file, &mut response_request_id);
    let (response, exit_code) = match result {
        Ok(receipt) => (
            json!({
                "request_id": response_request_id,
                "ok": true,
                "result": receipt,
            }),
            ExitCode::SUCCESS,
        ),
        Err(error) => {
            let code = safe_error_code(&error.code);
            tracing::error!(
                event.name = "platform.package.bootstrap_install_failed",
                error.code = %code,
                message = "offline install failed; maintenance hold remains active",
            );
            (
                json!({
                    "request_id": response_request_id,
                    "ok": false,
                    "error": {
                        "code": code,
                        "message": "offline install failed; maintenance hold remains active",
                        "remediation": "Keep the hold active and correct the failed precondition before retrying."
                    }
                }),
                ExitCode::FAILURE,
            )
        }
    };
    if write_json_line(&response).is_err() {
        return ExitCode::FAILURE;
    }
    exit_code
}

fn install_offline_as_root(
    configuration: Configuration,
    input_file: Option<PathBuf>,
    response_request_id: &mut String,
) -> Result<BootstrapInstallReceipt, PackageRuntimeError> {
    if nix::unistd::geteuid().as_raw() != 0 {
        return Err(PackageRuntimeError::new(
            "ROOT_REQUIRED",
            "offline package bootstrap requires effective UID 0",
        ));
    }
    if configuration.root != Path::new(RUNTIME_ROOT) {
        return Err(PackageRuntimeError::new(
            "BOOTSTRAP_CONFIGURATION_INVALID",
            "offline bootstrap is bound to the managed package runtime root",
        ));
    }

    let (runtime_uid, runtime_gid) = runtime_account()?;
    validate_bootstrap_toolchain(
        &configuration.dependency_preparer,
        &configuration.dependency_preparer_args,
    )?;
    let root = ensure_runtime_state_root(runtime_uid, runtime_gid)?;
    let process_lock = RuntimeProcessLock::acquire(&root, runtime_uid, runtime_gid, false)?;
    ensure_runtime_daemon_stopped(runtime_uid)?;

    if let Some(path) = &input_file {
        validate_bootstrap_input_file_location(path)?;
    }
    let input = match input_file.as_ref() {
        Some(path) => read_bootstrap_file(path)?,
        None => read_bootstrap_stdin()?,
    };
    *response_request_id = input.request_id.clone();
    let identity = input.validate()?;
    if let Some(path) = input_file
        && path
            != Path::new(PERSISTENT_BOOTSTRAP_ROOT)
                .join(&input.request_id)
                .join("request.json")
    {
        return Err(PackageRuntimeError::new(
            "BOOTSTRAP_PRIVATE_FILE_INVALID",
            "bootstrap request path does not match its request ID",
        ));
    }
    validate_persistent_candidate_paths(&input.request_id, &input.candidate)?;

    let hold_before = {
        let operator_token = read_operator_token()?;
        let validated = validate_maintenance_hold(&input, &operator_token)?;
        drop(operator_token);
        validated
    };
    let worker_candidate =
        prepare_worker_candidate(&input.request_id, &input.candidate, runtime_gid)?;
    let installation_result = run_install_worker(
        &configuration,
        &root,
        runtime_uid,
        runtime_gid,
        &process_lock,
        BootstrapWorkerInput {
            request_id: input.request_id.clone(),
            candidate: worker_candidate,
        },
    );

    // Confirm the exact hold again even when dependency preparation or install
    // failed after the worker started. The root coordinator never ends the hold.
    let hold_after = {
        let operator_token = read_operator_token()?;
        let validated = validate_maintenance_hold(&input, &operator_token)?;
        drop(operator_token);
        validated
    };
    if hold_after != hold_before {
        return Err(PackageRuntimeError::new(
            "MAINTENANCE_ADMISSION_DENIED",
            "maintenance hold changed during offline package installation",
        ));
    }
    let installation = installation_result?;
    if !installation_matches(&installation, &identity) {
        return Err(PackageRuntimeError::new(
            "OFFLINE_INSTALL_RECEIPT_MISMATCH",
            "installed package record did not match the held artifact identity",
        ));
    }

    cleanup_worker_candidate_handoff(&input.request_id, runtime_gid)?;

    Ok(BootstrapInstallReceipt {
        transaction_id: input.maintenance.transaction_id.clone(),
        target_kind: hold_after.target_kind,
        plan_id: input.maintenance.plan_id.clone(),
        plan_digest: input.maintenance.plan_digest.clone(),
        component_artifact_digests: input.maintenance.component_artifact_digests.clone(),
        component_id: input.candidate.component_id.clone(),
        artifact_digest: input.candidate.artifact_digest.clone(),
        expected_gate_generation: input.maintenance.expected_gate_generation,
        expected_catalog_generation: input.maintenance.expected_catalog_generation,
        gate_generation: hold_after.gate_generation,
        catalog_generation: hold_after.catalog_generation,
        installation,
    })
}

fn run_install_worker(
    configuration: &Configuration,
    root: &Path,
    runtime_uid: u32,
    runtime_gid: u32,
    process_lock: &RuntimeProcessLock,
    input: BootstrapWorkerInput,
) -> Result<InstallationRecord, PackageRuntimeError> {
    let expected_request_id = input.request_id.clone();
    let worker_input = serde_json::to_vec(&input).map_err(|_| worker_failed())?;
    if worker_input.len() > MAX_WORKER_FRAME_BYTES {
        return Err(PackageRuntimeError::new(
            "BOOTSTRAP_INPUT_TOO_LARGE",
            "worker request exceeds the 64 KiB limit",
        ));
    }

    let executable = env::current_exe().map_err(|_| worker_failed())?;
    let (root_channel, worker_channel) =
        std::os::unix::net::UnixStream::pair().map_err(|_| worker_failed())?;
    let parent_auth_fd = root_channel.as_raw_fd();
    let worker_auth_fd = worker_channel.as_raw_fd();
    let lock_fd = process_lock.as_raw_fd();
    let mut command = Command::new(executable);
    command
        .arg("--root")
        .arg(root)
        .arg("--dependency-preparer")
        .arg(&configuration.dependency_preparer)
        .args(
            configuration
                .dependency_preparer_args
                .iter()
                .flat_map(|argument| {
                    [
                        OsString::from("--dependency-preparer-arg"),
                        argument.clone(),
                    ]
                }),
        )
        .arg("--bootstrap-install-worker")
        .arg("--bootstrap-worker-lock-fd")
        .arg(lock_fd.to_string())
        .arg("--bootstrap-worker-auth-fd")
        .arg(worker_auth_fd.to_string())
        .env_clear()
        .env("HOME", root)
        .env("PATH", "/opt/cyrene/python/3.12.14/bin:/usr/bin:/bin")
        .env("UV_OFFLINE", "1")
        .env("CYRENE_LOG_LEVEL", "off")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // SAFETY: child setup uses async-signal-safe libc calls before exec.
    unsafe {
        command.pre_exec(move || {
            if libc::setpgid(0, 0) == -1
                || libc::fcntl(lock_fd, libc::F_SETFD, 0) == -1
                || libc::fcntl(worker_auth_fd, libc::F_SETFD, 0) == -1
                || libc::close(parent_auth_fd) == -1
                || libc::setgroups(0, std::ptr::null()) == -1
                || libc::setgid(runtime_gid) == -1
                || libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) == -1
                || libc::setuid(runtime_uid) == -1
                || libc::geteuid() != runtime_uid
                || libc::getegid() != runtime_gid
            {
                return Err(io::Error::last_os_error());
            }
            libc::umask(0o077);
            Ok(())
        });
    }

    let mut child = command.spawn().map_err(|_| worker_failed())?;
    drop(worker_channel);
    let stdout = child.stdout.take().ok_or_else(worker_failed)?;
    let stderr = child.stderr.take().ok_or_else(worker_failed)?;
    let stdout_reader = std::thread::spawn(move || read_capped(stdout, MAX_WORKER_STDOUT_BYTES));
    let stderr_reader = std::thread::spawn(move || read_capped(stderr, MAX_WORKER_STDERR_BYTES));
    let mut stdin = child.stdin.take().ok_or_else(worker_failed)?;
    if stdin.write_all(&worker_input).is_err() || stdin.write_all(b"\n").is_err() {
        drop(stdin);
        terminate_worker_group(&mut child);
        let _ = stdout_reader.join();
        let _ = stderr_reader.join();
        return Err(worker_failed());
    }
    drop(stdin);

    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(_) => {
                terminate_worker_group(&mut child);
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(worker_failed());
            }
        }
        if started.elapsed() >= WORKER_TIMEOUT {
            terminate_worker_group(&mut child);
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            return Err(PackageRuntimeError::new(
                "BOOTSTRAP_WORKER_TIMEOUT",
                "offline package preparation exceeded its time limit",
            ));
        }
        std::thread::sleep(WORKER_POLL_INTERVAL);
    };
    let stdout = stdout_reader.join().map_err(|_| worker_failed())?;
    let _stderr = stderr_reader.join().map_err(|_| worker_failed())?;
    if stdout.overflow || stdout.error || stdout.bytes.len() > MAX_WORKER_STDOUT_BYTES {
        return Err(worker_failed());
    }
    let output: BootstrapWorkerOutput =
        serde_json::from_slice(&stdout.bytes).map_err(|_| worker_failed())?;
    match (status.success(), output.installation, output.error) {
        (true, Some(installation), None) if output.request_id == expected_request_id => {
            Ok(installation)
        }
        (false, None, Some(error)) => Err(PackageRuntimeError::new(
            safe_error_code(&error.code),
            "offline package installation failed; maintenance hold remains active",
        )),
        _ => Err(worker_failed()),
    }
}

fn terminate_worker_group(child: &mut std::process::Child) {
    let process_group = child.id() as libc::pid_t;
    // SAFETY: a positive child PID was made its own process-group ID in pre_exec.
    if process_group <= 0 || unsafe { libc::kill(-process_group, libc::SIGKILL) } != 0 {
        let _ = child.kill();
    }
    let _ = child.wait();
}

fn run_bootstrap_worker(configuration: Configuration, lock_fd: i32, auth_fd: i32) -> ExitCode {
    if nix::unistd::geteuid().as_raw() == 0 || verify_root_worker_peer(auth_fd).is_err() {
        return emit_worker_error(worker_failed());
    }
    let (runtime_uid, runtime_gid) = match runtime_account() {
        Ok(account) => account,
        Err(error) => return emit_worker_error(error),
    };
    if nix::unistd::getuid().as_raw() != runtime_uid
        || nix::unistd::geteuid().as_raw() != runtime_uid
        || nix::unistd::getgid().as_raw() != runtime_gid
        || nix::unistd::getegid().as_raw() != runtime_gid
        || configuration.root != Path::new(RUNTIME_ROOT)
        || validate_bootstrap_toolchain(
            &configuration.dependency_preparer,
            &configuration.dependency_preparer_args,
        )
        .is_err()
    {
        return emit_worker_error(PackageRuntimeError::new(
            "BOOTSTRAP_WORKER_UNAUTHORIZED",
            "offline installer worker configuration is invalid",
        ));
    }
    let mut bytes = Vec::new();
    if io::stdin()
        .take((MAX_WORKER_FRAME_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() > MAX_WORKER_FRAME_BYTES
    {
        return emit_worker_error(PackageRuntimeError::new(
            "BOOTSTRAP_INPUT_INVALID",
            "worker request is invalid",
        ));
    }
    let input: BootstrapWorkerInput = match serde_json::from_slice(&bytes) {
        Ok(input) => input,
        Err(_) => {
            return emit_worker_error(PackageRuntimeError::new(
                "BOOTSTRAP_INPUT_INVALID",
                "worker request is invalid",
            ));
        }
    };
    let expected = match input.validate() {
        Ok(expected) => expected,
        Err(error) => return emit_worker_error(error),
    };
    if validate_candidate_paths(&input.candidate, runtime_uid, runtime_gid).is_err() {
        return emit_worker_error(PackageRuntimeError::new(
            "BOOTSTRAP_CANDIDATE_PATH_INVALID",
            "candidate files are missing or unsafe",
        ));
    }
    // SAFETY: only the root parent passes this descriptor over its private,
    // credential-checked socketpair; this child takes ownership of its copy.
    let process_lock = match unsafe {
        RuntimeProcessLock::from_inherited_fd(
            lock_fd,
            &configuration.root,
            runtime_uid,
            runtime_gid,
        )
    } {
        Ok(lock) => lock,
        Err(error) => return emit_worker_error(error),
    };
    let dependency_preparer = CommandDependencyPreparer::new(configuration.dependency_preparer)
        .with_args(configuration.dependency_preparer_args);
    let runtime = match FilesystemPackageRuntime::open_with_process_lock(
        configuration.root,
        Arc::new(dependency_preparer),
        Box::new(ProcessPluginServiceSupervisor::default()),
        ServiceActivationOptions::default(),
        process_lock,
    ) {
        Ok(runtime) => runtime,
        Err(error) => return emit_worker_error(error),
    };
    if let Err(error) =
        runtime.cache_offline_candidate(&input.candidate.package_source(), &expected)
    {
        return emit_worker_error(error);
    }
    let installation =
        match runtime.install_offline(&expected.package_id, &expected.package_version) {
            Ok(installation) => installation,
            Err(error) => return emit_worker_error(error),
        };
    if !installation_matches(&installation, &expected) {
        return emit_worker_error(PackageRuntimeError::new(
            "OFFLINE_INSTALL_RECEIPT_MISMATCH",
            "installed package record did not match the candidate identity",
        ));
    }
    emit_worker_output(BootstrapWorkerOutput {
        request_id: input.request_id,
        installation: Some(installation),
        error: None,
    })
}

fn emit_worker_error(error: PackageRuntimeError) -> ExitCode {
    emit_worker_output(BootstrapWorkerOutput {
        request_id: String::new(),
        installation: None,
        error: Some(BootstrapWorkerError {
            code: safe_error_code(&error.code),
            message: "offline package installation failed".to_string(),
        }),
    })
}

fn emit_worker_output(output: BootstrapWorkerOutput) -> ExitCode {
    let successful = output.installation.is_some() && output.error.is_none();
    match write_json_line(&output) {
        Ok(()) if successful => ExitCode::SUCCESS,
        Ok(()) | Err(_) => ExitCode::FAILURE,
    }
}

fn runtime_account() -> Result<(u32, u32), PackageRuntimeError> {
    let user = User::from_name("cyrene")
        .map_err(|_| runtime_account_error())?
        .ok_or_else(runtime_account_error)?;
    let uid = user.uid.as_raw();
    let gid = user.gid.as_raw();
    if uid == 0 || gid == 0 {
        return Err(PackageRuntimeError::new(
            "RUNTIME_ACCOUNT_INVALID",
            "cyrene must be a non-root runtime account",
        ));
    }
    Ok((uid, gid))
}

fn installation_matches(
    installation: &InstallationRecord,
    expected: &cy_package_runtime::OfflineInstallCandidateIdentity,
) -> bool {
    installation.package_id == expected.package_id
        && installation.package_version == expected.package_version
        && installation.artifact_digest == expected.artifact_digest
        && installation.archive_digest == expected.archive_digest
        && installation.verification.artifact_digest == expected.artifact_digest
        && installation.verification.archive_digest == expected.archive_digest
        && installation.verification.descriptor_digest == expected.descriptor_digest
        && installation.verification.manifest_digest == expected.manifest_digest
        && installation.verification.dependency_lock_digest == expected.dependency_lock_digest
        && installation.dependencies.lock_digest == expected.dependency_lock_digest
}

#[derive(Debug)]
struct CappedRead {
    bytes: Vec<u8>,
    overflow: bool,
    error: bool,
}

fn read_capped(mut reader: impl Read, limit: usize) -> CappedRead {
    let mut bytes = Vec::new();
    let mut overflow = false;
    let mut error = false;
    let mut buffer = [0_u8; 4096];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                let available = limit.saturating_sub(bytes.len());
                bytes.extend_from_slice(&buffer[..read.min(available)]);
                overflow |= read > available;
            }
            Err(read_error) if read_error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => {
                error = true;
                break;
            }
        }
    }
    CappedRead {
        bytes,
        overflow,
        error,
    }
}

fn write_json_line(value: &impl Serialize) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, value).map_err(io::Error::other)?;
    stdout.write_all(b"\n")?;
    stdout.flush()
}

fn safe_error_code(code: &str) -> String {
    if !code.is_empty()
        && code.len() <= 64
        && code
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
    {
        code.to_string()
    } else {
        "BOOTSTRAP_FAILED".to_string()
    }
}

fn runtime_account_error() -> PackageRuntimeError {
    PackageRuntimeError::new(
        "RUNTIME_ACCOUNT_UNAVAILABLE",
        "cyrene runtime account is unavailable",
    )
}

fn worker_failed() -> PackageRuntimeError {
    PackageRuntimeError::new(
        "BOOTSTRAP_WORKER_FAILED",
        "offline package installation worker failed",
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ExecutionMode {
    Service { stdio: bool },
    BootstrapInstall { input_file: Option<PathBuf> },
    BootstrapWorker { lock_fd: i32, auth_fd: i32 },
}

struct Configuration {
    root: PathBuf,
    dependency_preparer: PathBuf,
    dependency_preparer_args: Vec<OsString>,
    socket: PathBuf,
    source_policy: PathBuf,
    mode: ExecutionMode,
}

impl Configuration {
    fn parse(arguments: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut root = None;
        let mut dependency_preparer = None;
        let mut dependency_preparer_args = Vec::new();
        let mut socket = PathBuf::from(DEFAULT_CONTROL_SOCKET);
        let mut source_policy = PathBuf::from(DEFAULT_SOURCE_POLICY);
        let mut socket_explicit = false;
        let mut policy_explicit = false;
        let mut stdio = false;
        let mut bootstrap_install = false;
        let mut bootstrap_input_file = None;
        let mut worker = false;
        let mut worker_lock_fd = None;
        let mut worker_auth_fd = None;
        let mut arguments = arguments.peekable();
        while let Some(argument) = arguments.next() {
            let value = |arguments: &mut std::iter::Peekable<_>| {
                arguments
                    .next()
                    .ok_or_else(|| format!("missing value after {argument}"))
            };
            match argument.as_str() {
                "--root" => {
                    if root
                        .replace(PathBuf::from(value(&mut arguments)?))
                        .is_some()
                    {
                        return Err("--root may be provided only once".to_string());
                    }
                }
                "--dependency-preparer" => {
                    if dependency_preparer
                        .replace(PathBuf::from(value(&mut arguments)?))
                        .is_some()
                    {
                        return Err("--dependency-preparer may be provided only once".to_string());
                    }
                }
                "--dependency-preparer-arg" => {
                    dependency_preparer_args.push(OsString::from(value(&mut arguments)?));
                }
                "--socket" => {
                    socket = PathBuf::from(value(&mut arguments)?);
                    socket_explicit = true;
                }
                "--source-policy" => {
                    source_policy = PathBuf::from(value(&mut arguments)?);
                    policy_explicit = true;
                }
                "--stdio" => stdio = true,
                "--bootstrap-install-offline" => {
                    if bootstrap_install {
                        return Err(
                            "--bootstrap-install-offline may be provided only once".to_string()
                        );
                    }
                    bootstrap_install = true;
                }
                "--bootstrap-input-file" => {
                    if bootstrap_input_file
                        .replace(PathBuf::from(value(&mut arguments)?))
                        .is_some()
                    {
                        return Err("--bootstrap-input-file may be provided only once".to_string());
                    }
                }
                "--bootstrap-install-worker" => worker = true,
                "--bootstrap-worker-lock-fd" => {
                    if worker_lock_fd
                        .replace(parse_fd(&value(&mut arguments)?)?)
                        .is_some()
                    {
                        return Err("worker lock descriptor may be provided only once".to_string());
                    }
                }
                "--bootstrap-worker-auth-fd" => {
                    if worker_auth_fd
                        .replace(parse_fd(&value(&mut arguments)?)?)
                        .is_some()
                    {
                        return Err("worker auth descriptor may be provided only once".to_string());
                    }
                }
                "--help" | "-h" => {
                    return Err(
                        "usage: cy-package-runtime --root PATH --dependency-preparer PATH [--dependency-preparer-arg VALUE] [--socket ABSOLUTE_PATH] [--source-policy ABSOLUTE_PATH] [--stdio] | --bootstrap-install-offline [--bootstrap-input-file ROOT_PRIVATE_FILE]".to_string(),
                    );
                }
                _ => return Err(format!("unknown argument: {argument}")),
            }
        }
        let root = root.ok_or_else(|| "--root is required".to_string())?;
        let dependency_preparer =
            dependency_preparer.ok_or_else(|| "--dependency-preparer is required".to_string())?;
        let mode = if worker {
            if bootstrap_install
                || bootstrap_input_file.is_some()
                || stdio
                || socket_explicit
                || policy_explicit
            {
                return Err(
                    "bootstrap worker flags cannot be mixed with public server modes".to_string(),
                );
            }
            let lock_fd =
                worker_lock_fd.ok_or_else(|| "worker lock descriptor is required".to_string())?;
            let auth_fd =
                worker_auth_fd.ok_or_else(|| "worker auth descriptor is required".to_string())?;
            if lock_fd == auth_fd {
                return Err("worker descriptors must be distinct".to_string());
            }
            ExecutionMode::BootstrapWorker { lock_fd, auth_fd }
        } else if worker_lock_fd.is_some() || worker_auth_fd.is_some() {
            return Err("worker descriptors require --bootstrap-install-worker".to_string());
        } else if bootstrap_install {
            if stdio || socket_explicit || policy_explicit {
                return Err("offline bootstrap cannot start a control server".to_string());
            }
            ExecutionMode::BootstrapInstall {
                input_file: bootstrap_input_file,
            }
        } else {
            if bootstrap_input_file.is_some() {
                return Err(
                    "--bootstrap-input-file requires --bootstrap-install-offline".to_string(),
                );
            }
            ExecutionMode::Service { stdio }
        };
        Ok(Self {
            root,
            dependency_preparer,
            dependency_preparer_args,
            socket,
            source_policy,
            mode,
        })
    }
}

fn parse_fd(value: &str) -> Result<i32, String> {
    value
        .parse::<i32>()
        .ok()
        .filter(|fd| *fd > 2)
        .ok_or_else(|| "worker descriptor is invalid".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(arguments: &[&str]) -> Result<Configuration, String> {
        Configuration::parse(arguments.iter().map(|value| (*value).to_string()))
    }

    #[test]
    fn offline_bootstrap_is_one_shot_and_cannot_select_a_server_transport() {
        let valid = parse(&[
            "--root",
            RUNTIME_ROOT,
            "--dependency-preparer",
            "/usr/libexec/cyrene-plugin-python-preparer",
            "--bootstrap-install-offline",
        ])
        .unwrap();
        assert_eq!(
            valid.mode,
            ExecutionMode::BootstrapInstall { input_file: None }
        );

        assert!(
            parse(&[
                "--root",
                RUNTIME_ROOT,
                "--dependency-preparer",
                "/usr/libexec/cyrene-plugin-python-preparer",
                "--bootstrap-install-offline",
                "--stdio",
            ])
            .is_err()
        );
        assert!(
            parse(&[
                "--root",
                RUNTIME_ROOT,
                "--dependency-preparer",
                "/usr/libexec/cyrene-plugin-python-preparer",
                "--bootstrap-install-offline",
                "--socket",
                "/tmp/control.sock",
            ])
            .is_err()
        );
    }

    #[test]
    fn worker_mode_requires_both_private_descriptors_and_cannot_use_public_input() {
        assert!(
            parse(&[
                "--root",
                RUNTIME_ROOT,
                "--dependency-preparer",
                "/usr/libexec/cyrene-plugin-python-preparer",
                "--bootstrap-install-worker",
                "--bootstrap-worker-lock-fd",
                "4",
                "--bootstrap-worker-auth-fd",
                "5",
                "--bootstrap-input-file",
                "/tmp/request.json",
            ])
            .is_err()
        );
        assert!(
            parse(&[
                "--root",
                RUNTIME_ROOT,
                "--dependency-preparer",
                "/usr/libexec/cyrene-plugin-python-preparer",
                "--bootstrap-install-worker",
                "--bootstrap-worker-lock-fd",
                "4",
            ])
            .is_err()
        );
    }
}
