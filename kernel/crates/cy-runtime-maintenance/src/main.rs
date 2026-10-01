// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: kernel/crates/cy-runtime-maintenance/src/main.rs             ║
// ║ Module: cyrene_runtime_maintenance                                   ║
// ║ Role: Authenticated maintenance broker and installer CLI.             ║
// ║                                                                      ║
// ║ 模块职责：提供鉴权的维护 broker 与安装配置命令。                       ║
// ╚══════════════════════════════════════════════════════════════════════╝
//! Unix-socket broker for the shared runtime admission and update gate.

use std::{
    collections::BTreeMap,
    env,
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::{
        fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    process,
    sync::Arc,
    thread,
};

use cy_proto::core_v2::{
    kernel_authority_service_client::KernelAuthorityServiceClient, BeginMaintenanceRequest,
    EndMaintenanceRequest, MaintenanceOutcome as ProtoOutcome, MaintenanceTargetKind,
    UpdateReadinessRequest, UpdateReadinessStatus, WorkspaceTaskActivityState,
};
use cy_runtime_maintenance::{
    MaintenanceError, MaintenanceOutcome, MaintenancePlan, ReadinessRequest, RuntimeMaintenance,
    RuntimeUsage, TaskActivityRecord, TaskActivityState, TrustedActivitySource,
    TrustedActivitySourceCatalog, UpdateTargetKind,
};
use nix::sys::socket::{getsockopt, sockopt::PeerCredentials};
use nix::unistd::{chown, getegid, Gid, Uid};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::net::UnixStream as TokioUnixStream;
use tonic::transport::{Channel, Endpoint};
use tower::service_fn;
use uuid::Uuid;

const DEFAULT_STATE_DIR: &str = "/var/lib/cyrene/runtime";
const DEFAULT_OPERATOR_TOKEN: &str = "/var/lib/cyrene/runtime-maintenance-private/operator.token";
const DEFAULT_CATALOG: &str = "/etc/cyrene/runtime-activity-sources.json";
const DEFAULT_TOKEN_DIR: &str = "/etc/cyrene/runtime-activity-source-tokens";
const DEFAULT_SOCKET: &str = "/run/cyrene/runtime-maintenance.sock";
const DEFAULT_KERNEL_SOCKET: &str = "/run/cyrene/kernel.sock";
const MAX_REQUEST_BYTES: u64 = 1024 * 1024;

#[derive(Clone)]
struct Broker {
    gate: RuntimeMaintenance,
    kernel_socket: PathBuf,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestAuth {
    #[serde(default)]
    source_id: Option<String>,
    #[serde(default)]
    source_token: Option<String>,
    #[serde(default)]
    operator_token: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestEnvelope {
    request_id: String,
    method: String,
    #[serde(default)]
    auth: RequestAuth,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogInput {
    schema_version: u32,
    generation: u64,
    sources: Vec<TrustedActivitySource>,
}

#[derive(Debug, Clone, Copy)]
struct PeerIdentity {
    uid: u32,
    gid: u32,
}

#[derive(Debug)]
struct ApiError {
    code: String,
    message: String,
}

impl ApiError {
    fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    fn from_maintenance(error: MaintenanceError) -> Self {
        Self::new(error.code(), error.to_string())
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("cyrene-runtime-maintenance: {error}");
        process::exit(2);
    }
}

fn run() -> Result<(), String> {
    let mut args = env::args().skip(1).collect::<Vec<_>>();
    if args.is_empty() || args[0] == "--help" || args[0] == "help" {
        print_usage();
        return Ok(());
    }
    match args.remove(0).as_str() {
        "serve" => run_serve(parse_options(args, false)?),
        "request" => run_request(parse_options(args, false)?),
        "init-catalog" => run_init_catalog(parse_options(args, true)?),
        "health" => run_health(parse_options(args, false)?),
        command => Err(format!("unknown command: {command}")),
    }
}

fn print_usage() {
    println!(
        "Usage:\n  cyrene-runtime-maintenance serve [--state-dir PATH] [--catalog PATH] [--socket PATH] [--private-token-file PATH] [--kernel-socket PATH]\n  cyrene-runtime-maintenance request --socket PATH [--operator] [--operator-token-file PATH]\n  cyrene-runtime-maintenance init-catalog --catalog PATH --token-dir PATH [--catalog-gid GID] --source ID=UID[:GID] [--source ...]"
    );
}

fn parse_options(
    args: Vec<String>,
    repeat_source: bool,
) -> Result<BTreeMap<String, Vec<String>>, String> {
    let mut options = BTreeMap::<String, Vec<String>>::new();
    let mut index = 0;
    while index < args.len() {
        let key = args[index]
            .strip_prefix("--")
            .ok_or_else(|| format!("expected an option, got {}", args[index]))?
            .to_string();
        if key == "operator" {
            options.entry(key).or_default().push("true".to_string());
            index += 1;
            continue;
        }
        let value = args
            .get(index + 1)
            .ok_or_else(|| format!("missing value for --{key}"))?
            .clone();
        if !(repeat_source && key == "source") && options.contains_key(&key) {
            return Err(format!("--{key} may be provided only once"));
        }
        options.entry(key).or_default().push(value);
        index += 2;
    }
    Ok(options)
}

fn option(options: &BTreeMap<String, Vec<String>>, key: &str, default: &str) -> String {
    options
        .get(key)
        .and_then(|values| values.first())
        .cloned()
        .unwrap_or_else(|| default.to_string())
}

fn run_serve(options: BTreeMap<String, Vec<String>>) -> Result<(), String> {
    let state_dir = PathBuf::from(option(&options, "state-dir", DEFAULT_STATE_DIR));
    let catalog_path = PathBuf::from(option(&options, "catalog", DEFAULT_CATALOG));
    let socket_path = PathBuf::from(option(&options, "socket", DEFAULT_SOCKET));
    let private_token_path = PathBuf::from(option(
        &options,
        "private-token-file",
        DEFAULT_OPERATOR_TOKEN,
    ));
    let kernel_socket = PathBuf::from(option(&options, "kernel-socket", DEFAULT_KERNEL_SOCKET));
    let gate = RuntimeMaintenance::open_with_catalog_file(state_dir, &catalog_path)
        .map_err(|error| error.to_string())?;
    gate.initialize_operator_capability(&private_token_path)
        .map_err(|error| error.to_string())?;
    let broker = Arc::new(Broker {
        gate,
        kernel_socket,
    });
    serve_socket(&socket_path, broker)
}

fn serve_socket(socket_path: &Path, broker: Arc<Broker>) -> Result<(), String> {
    if let Some(parent) = socket_path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    if let Ok(metadata) = fs::symlink_metadata(socket_path) {
        if !metadata.file_type().is_socket() {
            return Err(format!(
                "socket path is not a Unix socket: {}",
                socket_path.display()
            ));
        }
        fs::remove_file(socket_path).map_err(|error| error.to_string())?;
    }
    let listener = UnixListener::bind(socket_path).map_err(|error| error.to_string())?;
    fs::set_permissions(socket_path, fs::Permissions::from_mode(0o660))
        .map_err(|error| error.to_string())?;
    for accepted in listener.incoming() {
        match accepted {
            Ok(stream) => {
                let broker = Arc::clone(&broker);
                thread::spawn(move || handle_connection(stream, broker));
            }
            Err(error) => eprintln!("maintenance broker accept failed: {error}"),
        }
    }
    Ok(())
}

fn handle_connection(mut stream: UnixStream, broker: Arc<Broker>) {
    let peer = match getsockopt(&stream, PeerCredentials) {
        Ok(credentials) => PeerIdentity {
            uid: credentials.uid(),
            gid: credentials.gid(),
        },
        Err(error) => {
            write_response(
                &mut stream,
                "",
                Err(ApiError::new("CALLER_IDENTITY_UNKNOWN", error.to_string())),
            );
            return;
        }
    };
    let mut line = Vec::new();
    if let Err(error) = read_bounded_line(&mut stream, &mut line) {
        write_response(
            &mut stream,
            "",
            Err(ApiError::new(
                "MAINTENANCE_PROTOCOL_INVALID",
                error.to_string(),
            )),
        );
        return;
    }
    if line.len() as u64 > MAX_REQUEST_BYTES || !line.ends_with(b"\n") {
        write_response(
            &mut stream,
            "",
            Err(ApiError::new(
                "MAINTENANCE_PROTOCOL_INVALID",
                "request is too large or missing newline",
            )),
        );
        return;
    }
    let request = match serde_json::from_slice::<RequestEnvelope>(&line) {
        Ok(request) => request,
        Err(error) => {
            write_response(
                &mut stream,
                "",
                Err(ApiError::new(
                    "MAINTENANCE_PROTOCOL_INVALID",
                    error.to_string(),
                )),
            );
            return;
        }
    };
    if request.request_id.is_empty() || request.request_id.len() > 256 {
        write_response(
            &mut stream,
            &request.request_id,
            Err(ApiError::new(
                "MAINTENANCE_PROTOCOL_INVALID",
                "request_id is required and limited to 256 bytes",
            )),
        );
        return;
    }
    let request_id = request.request_id.clone();
    let result = dispatch(&broker, peer, request);
    write_response(&mut stream, &request_id, result);
}

fn read_bounded_line(stream: &mut UnixStream, line: &mut Vec<u8>) -> io::Result<()> {
    let mut byte = [0_u8; 1];
    loop {
        match stream.read(&mut byte)? {
            0 => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "request ended before newline",
                ))
            }
            _ => {
                line.push(byte[0]);
                if line.len() as u64 > MAX_REQUEST_BYTES {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "request exceeds size limit",
                    ));
                }
                if byte[0] == b'\n' {
                    return Ok(());
                }
            }
        }
    }
}

fn write_response(stream: &mut UnixStream, request_id: &str, result: Result<Value, ApiError>) {
    let response = match result {
        Ok(result) => json!({"request_id": request_id, "result": result}),
        Err(error) => json!({
            "request_id": request_id,
            "error": {"code": error.code, "message": error.message}
        }),
    };
    if serde_json::to_writer(&mut *stream, &response).is_ok() {
        let _ = stream.write_all(b"\n");
        let _ = stream.flush();
    }
}

fn dispatch(
    broker: &Broker,
    peer: PeerIdentity,
    request: RequestEnvelope,
) -> Result<Value, ApiError> {
    // Reload the root-owned install catalog before authentication or readiness
    // decisions. A newly installed Product can join without restarting Kernel
    // or invalidating unchanged sources' long-lived tokens.
    broker
        .gate
        .refresh_catalog()
        .map_err(ApiError::from_maintenance)?;
    match request.method.as_str() {
        "Health" => Ok(json!({
            "status": "SERVING",
            "catalog_generation": broker.gate.catalog_generation(),
            "gate_generation": broker.gate.current_gate_generation().map_err(ApiError::from_maintenance)?,
        })),
        "GetUpdateReadiness" => {
            authorize_operator(&broker.gate, peer, &request.auth)?;
            let readiness_request = readiness_request(&request.params)?;
            if readiness_request.target_kind == UpdateTargetKind::CoreRuntime {
                kernel_readiness(&broker.kernel_socket, &readiness_request)
            } else {
                let snapshot = broker
                    .gate
                    .get_update_readiness(
                        &readiness_request,
                        RuntimeUsage {
                            known: true,
                            active_worker_count: 0,
                            active_allocation_count: 0,
                        },
                    )
                    .map_err(ApiError::from_maintenance)?;
                serde_json::to_value(snapshot).map_err(serialization_error)
            }
        }
        "BeginMaintenance" => {
            authorize_operator(&broker.gate, peer, &request.auth)?;
            let readiness_request = readiness_request(&request.params)?;
            let plan = maintenance_plan(&request.params)?;
            let transaction_id = required_string(&request.params, "request_id")?;
            let expected_gate_generation =
                required_u64(&request.params, "expected_gate_generation")?;
            let user_confirmed_restart =
                optional_bool(&request.params, "user_confirmed_restart", false)?;
            if readiness_request.target_kind == UpdateTargetKind::CoreRuntime {
                kernel_begin(
                    &broker.kernel_socket,
                    &transaction_id,
                    &plan,
                    &readiness_request,
                    expected_gate_generation,
                    user_confirmed_restart,
                    request.auth.operator_token.as_deref().unwrap_or_default(),
                )
            } else {
                let result = broker
                    .gate
                    .begin_maintenance(
                        &transaction_id,
                        &plan,
                        &readiness_request,
                        expected_gate_generation,
                        user_confirmed_restart,
                        RuntimeUsage {
                            known: true,
                            active_worker_count: 0,
                            active_allocation_count: 0,
                        },
                    )
                    .map_err(ApiError::from_maintenance)?;
                serde_json::to_value(result).map_err(serialization_error)
            }
        }
        "EndMaintenance" => {
            authorize_operator(&broker.gate, peer, &request.auth)?;
            let transaction_id = required_string(&request.params, "request_id")?;
            let token = required_string(&request.params, "maintenance_token")?;
            let outcome = parse_outcome(&required_string(&request.params, "outcome")?)?;
            let healthy = optional_bool(&request.params, "healthy", false)?;
            let target = optional_string(&request.params, "target_kind")?
                .as_deref()
                .map(parse_target_kind)
                .transpose()?;
            if target == Some(UpdateTargetKind::CoreRuntime) {
                kernel_end(
                    &broker.kernel_socket,
                    &transaction_id,
                    &token,
                    outcome,
                    healthy,
                    request.auth.operator_token.as_deref().unwrap_or_default(),
                )
            } else {
                let result = broker
                    .gate
                    .end_maintenance(&transaction_id, &token, outcome, healthy)
                    .map_err(ApiError::from_maintenance)?;
                serde_json::to_value(result).map_err(serialization_error)
            }
        }
        "HeartbeatActivitySource" => {
            let source_id = authorize_source(&broker.gate, peer, &request.auth)?;
            require_param_source_id(&request.params, &source_id)?;
            require_activity_catalog_generation(&broker.gate, &request.params)?;
            broker
                .gate
                .heartbeat_activity_source(&source_id)
                .map_err(ApiError::from_maintenance)?;
            Ok(
                json!({"source_id": source_id, "catalog_generation": broker.gate.catalog_generation(), "gate_generation": broker.gate.current_gate_generation().map_err(ApiError::from_maintenance)?}),
            )
        }
        "AdmitTask" => {
            let source_id = authorize_source(&broker.gate, peer, &request.auth)?;
            require_param_source_id(&request.params, &source_id)?;
            let task_id = required_string(&request.params, "task_id")?;
            let state = TaskActivityState::parse(&required_string(&request.params, "state")?)
                .map_err(ApiError::from_maintenance)?;
            let admission = broker
                .gate
                .admit_task(&source_id, &task_id, state)
                .map_err(ApiError::from_maintenance)?;
            Ok(
                json!({"admitted": true, "status": "READY", "activity_token": admission.token, "gate_generation": admission.gate_generation, "blocker_codes": []}),
            )
        }
        "UpdateTaskActivity" => {
            let source_id = authorize_source(&broker.gate, peer, &request.auth)?;
            require_param_source_id(&request.params, &source_id)?;
            let task_id = required_string(&request.params, "task_id")?;
            let state = TaskActivityState::parse(&required_string(&request.params, "state")?)
                .map_err(ApiError::from_maintenance)?;
            let admission = broker
                .gate
                .update_task_activity(&source_id, &task_id, state)
                .map_err(ApiError::from_maintenance)?;
            Ok(
                json!({"accepted": true, "status": "READY", "gate_generation": admission.gate_generation, "blocker_codes": []}),
            )
        }
        "CompleteTask" => {
            let source_id = authorize_source(&broker.gate, peer, &request.auth)?;
            require_param_source_id(&request.params, &source_id)?;
            let task_id = required_string(&request.params, "task_id")?;
            broker
                .gate
                .complete_task(&source_id, &task_id)
                .map_err(ApiError::from_maintenance)?;
            Ok(
                json!({"completed": true, "gate_generation": broker.gate.current_gate_generation().map_err(ApiError::from_maintenance)?, "blocker_codes": []}),
            )
        }
        "ListActiveTasks" => {
            let source_id = authorize_source(&broker.gate, peer, &request.auth)?;
            require_param_source_id(&request.params, &source_id)?;
            let tasks = broker
                .gate
                .list_active_tasks(&source_id)
                .map_err(ApiError::from_maintenance)?;
            Ok(json!({"active_tasks": tasks}))
        }
        "ReconcileActivitySource" => {
            let source_id = authorize_source(&broker.gate, peer, &request.auth)?;
            require_param_source_id(&request.params, &source_id)?;
            require_activity_catalog_generation(&broker.gate, &request.params)?;
            let entries = request
                .params
                .get("active_tasks")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    ApiError::new("INVALID_ARGUMENT", "active_tasks must be an array")
                })?;
            let tasks = entries
                .iter()
                .map(|task| {
                    Ok(TaskActivityRecord {
                        source_id: source_id.clone(),
                        task_id: required_string(task, "task_id")?,
                        state: TaskActivityState::parse(&required_string(task, "state")?)
                            .map_err(ApiError::from_maintenance)?,
                    })
                })
                .collect::<Result<Vec<_>, ApiError>>()?;
            broker
                .gate
                .reconcile_activity_source(&source_id, tasks)
                .map_err(ApiError::from_maintenance)?;
            Ok(
                json!({"reconciled": true, "catalog_generation": broker.gate.catalog_generation(), "gate_generation": broker.gate.current_gate_generation().map_err(ApiError::from_maintenance)?}),
            )
        }
        method => Err(ApiError::new(
            "METHOD_NOT_FOUND",
            format!("unsupported broker method: {method}"),
        )),
    }
}

fn authorize_operator(
    gate: &RuntimeMaintenance,
    peer: PeerIdentity,
    auth: &RequestAuth,
) -> Result<(), ApiError> {
    if peer.uid != 0 {
        return Err(ApiError::new(
            "OPERATOR_AUTH_REQUIRED",
            "operator broker calls require UID 0",
        ));
    }
    let token = auth.operator_token.as_deref().ok_or_else(|| {
        ApiError::new("OPERATOR_AUTH_REQUIRED", "operator capability is required")
    })?;
    if gate
        .verify_operator_token(token)
        .map_err(ApiError::from_maintenance)?
    {
        Ok(())
    } else {
        Err(ApiError::new(
            "OPERATOR_AUTH_INVALID",
            "operator capability did not match",
        ))
    }
}

fn authorize_source(
    gate: &RuntimeMaintenance,
    peer: PeerIdentity,
    auth: &RequestAuth,
) -> Result<String, ApiError> {
    let source_id = auth
        .source_id
        .as_deref()
        .ok_or_else(|| ApiError::new("ACTIVITY_SOURCE_AUTH_REQUIRED", "source_id is required"))?;
    let token = auth.source_token.as_deref().ok_or_else(|| {
        ApiError::new("ACTIVITY_SOURCE_AUTH_REQUIRED", "source_token is required")
    })?;
    let source = gate
        .trusted_source(source_id)
        .ok_or_else(|| ApiError::new("ACTIVITY_SOURCE_UNTRUSTED", "source is not installed"))?;
    if source.uid != peer.uid || source.gid.is_some_and(|gid| gid != peer.gid) {
        return Err(ApiError::new(
            "ACTIVITY_SOURCE_CALLER_MISMATCH",
            "socket peer identity does not match the installed source",
        ));
    }
    if !gate.verify_source_token(source_id, token) {
        return Err(ApiError::new(
            "ACTIVITY_SOURCE_AUTH_INVALID",
            "source token did not match",
        ));
    }
    Ok(source_id.to_string())
}

fn require_param_source_id(params: &Value, source_id: &str) -> Result<(), ApiError> {
    if required_string(params, "source_id")? == source_id {
        Ok(())
    } else {
        Err(ApiError::new(
            "ACTIVITY_SOURCE_CALLER_MISMATCH",
            "request source_id does not match authenticated source",
        ))
    }
}

fn require_activity_catalog_generation(
    gate: &RuntimeMaintenance,
    params: &Value,
) -> Result<(), ApiError> {
    // Per-source authentication is already bound to the currently installed
    // catalog entry. Accept an older cached generation when that source token
    // remains current, then return the new generation so the SDK can refresh.
    // Future generations still fail closed.
    if required_u64(params, "expected_catalog_generation")? <= gate.catalog_generation() {
        Ok(())
    } else {
        Err(ApiError::new(
            "UPDATE_READINESS_UNKNOWN",
            "source client catalog generation is ahead of the installed catalog",
        ))
    }
}

fn readiness_request(params: &Value) -> Result<ReadinessRequest, ApiError> {
    let target_kind = parse_target_kind(&required_string(params, "target_kind")?)?;
    let expected_catalog_generation = required_u64(params, "expected_catalog_generation")?;
    let expected_activity_sources = params
        .get("expected_activity_sources")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            ApiError::new(
                "INVALID_ARGUMENT",
                "expected_activity_sources must be an array",
            )
        })?
        .iter()
        .map(|value| {
            value.as_str().map(str::to_string).ok_or_else(|| {
                ApiError::new(
                    "INVALID_ARGUMENT",
                    "expected_activity_sources entries must be strings",
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ReadinessRequest {
        target_kind,
        requires_restart: optional_bool(params, "requires_restart", true)?,
        expected_catalog_generation,
        expected_activity_sources,
    })
}

fn maintenance_plan(params: &Value) -> Result<MaintenancePlan, ApiError> {
    let plan_id = required_string(params, "plan_id")?;
    let plan_digest = required_string(params, "plan_digest")?;
    let digests = params
        .get("component_artifact_digests")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            ApiError::new(
                "INVALID_ARGUMENT",
                "component_artifact_digests must be an object",
            )
        })?;
    let component_artifact_digests = digests
        .iter()
        .map(|(key, value)| {
            value
                .as_str()
                .map(|digest| (key.clone(), digest.to_string()))
                .ok_or_else(|| {
                    ApiError::new("INVALID_ARGUMENT", "artifact digest values must be strings")
                })
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    Ok(MaintenancePlan {
        plan_id,
        plan_digest,
        component_artifact_digests,
    })
}

fn parse_target_kind(value: &str) -> Result<UpdateTargetKind, ApiError> {
    match value {
        "PACKAGE_ONLY" => Ok(UpdateTargetKind::PackageOnly),
        "CORE_RUNTIME" => Ok(UpdateTargetKind::CoreRuntime),
        _ => Err(ApiError::new(
            "INVALID_ARGUMENT",
            "target_kind must be PACKAGE_ONLY or CORE_RUNTIME",
        )),
    }
}

fn parse_outcome(value: &str) -> Result<MaintenanceOutcome, ApiError> {
    match value {
        "SUCCESS" => Ok(MaintenanceOutcome::Success),
        "ROLLED_BACK" => Ok(MaintenanceOutcome::RolledBack),
        "FAILED" => Ok(MaintenanceOutcome::Failed),
        _ => Err(ApiError::new(
            "INVALID_ARGUMENT",
            "outcome must be SUCCESS, ROLLED_BACK, or FAILED",
        )),
    }
}

fn required_string(value: &Value, field: &str) -> Result<String, ApiError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| ApiError::new("INVALID_ARGUMENT", format!("{field} is required")))
}

fn optional_string(value: &Value, field: &str) -> Result<Option<String>, ApiError> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .map(|value| Some(value.to_string()))
            .ok_or_else(|| ApiError::new("INVALID_ARGUMENT", format!("{field} must be a string"))),
    }
}

fn required_u64(value: &Value, field: &str) -> Result<u64, ApiError> {
    value.get(field).and_then(Value::as_u64).ok_or_else(|| {
        ApiError::new(
            "INVALID_ARGUMENT",
            format!("{field} must be an unsigned integer"),
        )
    })
}

fn optional_bool(value: &Value, field: &str, default: bool) -> Result<bool, ApiError> {
    match value.get(field) {
        None => Ok(default),
        Some(value) => value
            .as_bool()
            .ok_or_else(|| ApiError::new("INVALID_ARGUMENT", format!("{field} must be a boolean"))),
    }
}

fn kernel_readiness(socket: &Path, request: &ReadinessRequest) -> Result<Value, ApiError> {
    let target_kind = proto_target_kind(request.target_kind);
    let request = UpdateReadinessRequest {
        target_kind: target_kind as i32,
        expected_activity_sources: request.expected_activity_sources.clone(),
        expected_catalog_generation: request.expected_catalog_generation,
        requires_restart: request.requires_restart,
    };
    let response = run_kernel(socket, async move |mut client| {
        client
            .get_update_readiness(request)
            .await
            .map(|response| response.into_inner())
    })?;
    let status = parse_proto_status(response.status);
    let active_tasks = response
        .active_tasks
        .into_iter()
        .map(|task| json!({"source_id": task.source_id, "task_id": task.task_id, "state": format_task_state(task.state)}))
        .collect::<Vec<_>>();
    Ok(json!({
        "status": status,
        "gate_generation": response.gate_generation,
        "install_catalog_generation": response.install_catalog_generation,
        "active_task_count": response.active_task_count,
        "active_tasks": active_tasks,
        "inflight_runtime_admission_count": response.inflight_runtime_admission_count,
        "unknown_activity_sources": response.unknown_activity_sources,
        "active_worker_count": response.active_worker_count,
        "active_allocation_count": response.active_lease_or_allocation_count,
        "blocker_codes": response.blocker_codes,
        "requires_restart_confirmation": response.requires_restart_confirmation,
    }))
}

#[allow(clippy::too_many_arguments)]
fn kernel_begin(
    socket: &Path,
    request_id: &str,
    plan: &MaintenancePlan,
    readiness: &ReadinessRequest,
    expected_gate_generation: u64,
    user_confirmed_restart: bool,
    operator_token: &str,
) -> Result<Value, ApiError> {
    let request = BeginMaintenanceRequest {
        request_id: request_id.to_string(),
        target_kind: proto_target_kind(readiness.target_kind) as i32,
        expected_gate_generation,
        user_confirmed_restart,
        expected_activity_sources: readiness.expected_activity_sources.clone(),
        expected_catalog_generation: readiness.expected_catalog_generation,
        operator_token: operator_token.to_string(),
        plan_id: plan.plan_id.clone(),
        plan_digest: plan.plan_digest.clone(),
        component_artifact_digests: plan
            .component_artifact_digests
            .clone()
            .into_iter()
            .collect(),
    };
    let response = run_kernel(socket, async move |mut client| {
        client
            .begin_maintenance(request)
            .await
            .map(|response| response.into_inner())
    })?;
    Ok(json!({
        "status": parse_proto_status(response.status),
        "maintenance_token": if response.maintenance_token.is_empty() { Value::Null } else { json!(response.maintenance_token) },
        "gate_generation": response.gate_generation,
        "blocker_codes": response.blocker_codes,
    }))
}

#[allow(clippy::too_many_arguments)]
fn kernel_end(
    socket: &Path,
    request_id: &str,
    token: &str,
    outcome: MaintenanceOutcome,
    healthy: bool,
    operator_token: &str,
) -> Result<Value, ApiError> {
    let request = EndMaintenanceRequest {
        request_id: request_id.to_string(),
        maintenance_token: token.to_string(),
        outcome: match outcome {
            MaintenanceOutcome::Success => ProtoOutcome::Success as i32,
            MaintenanceOutcome::RolledBack => ProtoOutcome::RolledBack as i32,
            MaintenanceOutcome::Failed => ProtoOutcome::Failed as i32,
        },
        healthy,
        operator_token: operator_token.to_string(),
    };
    let response = run_kernel(socket, async move |mut client| {
        client
            .end_maintenance(request)
            .await
            .map(|response| response.into_inner())
    })?;
    Ok(json!({
        "unlocked": response.unlocked,
        "status": parse_proto_status(response.status),
        "gate_generation": response.gate_generation,
        "blocker_codes": response.blocker_codes,
    }))
}

fn run_kernel<T, F, Fut>(socket: &Path, call: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce(KernelAuthorityServiceClient<Channel>) -> Fut,
    Fut: std::future::Future<Output = Result<T, tonic::Status>> + Send + 'static,
{
    let socket = socket.to_path_buf();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| ApiError::new("UPDATE_READINESS_UNKNOWN", error.to_string()))?;
    runtime.block_on(async move {
        let connector_path = socket.clone();
        let endpoint = Endpoint::from_static("http://[::]:50051");
        let channel = endpoint
            .connect_with_connector(service_fn(move |_| {
                let path = connector_path.clone();
                async move {
                    TokioUnixStream::connect(path)
                        .await
                        .map(hyper_util::rt::TokioIo::new)
                }
            }))
            .await
            .map_err(|error| ApiError::new("UPDATE_READINESS_UNKNOWN", error.to_string()))?;
        let client = KernelAuthorityServiceClient::new(channel);
        call(client)
            .await
            .map_err(|error| ApiError::new("UPDATE_READINESS_UNKNOWN", error.to_string()))
    })
}

fn proto_target_kind(value: UpdateTargetKind) -> MaintenanceTargetKind {
    match value {
        UpdateTargetKind::PackageOnly => MaintenanceTargetKind::PackageOnly,
        UpdateTargetKind::CoreRuntime => MaintenanceTargetKind::CoreRuntime,
    }
}

fn parse_proto_status(value: i32) -> &'static str {
    match UpdateReadinessStatus::try_from(value).unwrap_or(UpdateReadinessStatus::Unknown) {
        UpdateReadinessStatus::Unknown => "UNKNOWN",
        UpdateReadinessStatus::Ready => "READY",
        UpdateReadinessStatus::ActiveTasks => "ACTIVE_TASKS",
        UpdateReadinessStatus::IdleRuntimeRequiresUnload => "IDLE_RUNTIME_REQUIRES_UNLOAD",
        UpdateReadinessStatus::MaintenanceActive => "MAINTENANCE_ACTIVE",
        UpdateReadinessStatus::StaleReadiness => "STALE_READINESS",
        UpdateReadinessStatus::UserConfirmationRequired => "USER_CONFIRMATION_REQUIRED",
    }
}

fn format_task_state(value: i32) -> &'static str {
    match WorkspaceTaskActivityState::try_from(value)
        .unwrap_or(WorkspaceTaskActivityState::Unspecified)
    {
        WorkspaceTaskActivityState::Accepted => "ACCEPTED",
        WorkspaceTaskActivityState::Queued => "QUEUED",
        WorkspaceTaskActivityState::Dispatching => "DISPATCHING",
        WorkspaceTaskActivityState::Running => "RUNNING",
        WorkspaceTaskActivityState::Canceling => "CANCELING",
        WorkspaceTaskActivityState::Inflight => "INFLIGHT",
        WorkspaceTaskActivityState::Unspecified => "UNKNOWN",
    }
}

fn serialization_error(error: serde_json::Error) -> ApiError {
    ApiError::new("MAINTENANCE_PROTOCOL_INVALID", error.to_string())
}

fn run_request(options: BTreeMap<String, Vec<String>>) -> Result<(), String> {
    let socket_path = PathBuf::from(option(&options, "socket", DEFAULT_SOCKET));
    let is_operator = options.contains_key("operator");
    let token_path = PathBuf::from(option(
        &options,
        "operator-token-file",
        DEFAULT_OPERATOR_TOKEN,
    ));
    let mut input = Vec::new();
    io::stdin()
        .take(MAX_REQUEST_BYTES + 1)
        .read_to_end(&mut input)
        .map_err(|error| error.to_string())?;
    if input.len() as u64 > MAX_REQUEST_BYTES {
        return Err("request is too large".to_string());
    }
    let mut request: Value = serde_json::from_slice(&input).map_err(|error| error.to_string())?;
    if request.get("request_id").and_then(Value::as_str).is_none()
        || request.get("method").and_then(Value::as_str).is_none()
    {
        return Err("request_id and method are required".to_string());
    }
    if is_operator {
        let token = read_private_token(&token_path)?;
        if !matches!(
            request.get("method").and_then(Value::as_str),
            Some("GetUpdateReadiness" | "BeginMaintenance" | "EndMaintenance")
        ) {
            return Err("--operator is valid only for readiness and maintenance calls".to_string());
        }
        let root = request.as_object_mut().ok_or("request must be an object")?;
        let auth = root.entry("auth").or_insert_with(|| json!({}));
        let auth = auth.as_object_mut().ok_or("auth must be an object")?;
        auth.insert("operator_token".to_string(), Value::String(token));
    }
    let encoded = serde_json::to_vec(&request).map_err(|error| error.to_string())?;
    let mut stream = UnixStream::connect(&socket_path).map_err(|error| error.to_string())?;
    stream
        .write_all(&encoded)
        .map_err(|error| error.to_string())?;
    stream.write_all(b"\n").map_err(|error| error.to_string())?;
    stream.flush().map_err(|error| error.to_string())?;
    let mut response = Vec::new();
    BufReader::new(stream)
        .take(MAX_REQUEST_BYTES + 1)
        .read_until(b'\n', &mut response)
        .map_err(|error| error.to_string())?;
    if response.len() as u64 > MAX_REQUEST_BYTES || !response.ends_with(b"\n") {
        return Err("broker returned an invalid or oversized response".to_string());
    }
    io::stdout()
        .write_all(&response)
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn run_health(options: BTreeMap<String, Vec<String>>) -> Result<(), String> {
    let socket_path = PathBuf::from(option(&options, "socket", DEFAULT_SOCKET));
    let request = json!({
        "request_id": Uuid::new_v4().to_string(),
        "method": "Health",
        "params": {},
    });
    let response = send_local_request(&socket_path, &request)?;
    if response
        .get("result")
        .and_then(|result| result.get("status"))
        .and_then(Value::as_str)
        == Some("SERVING")
    {
        Ok(())
    } else {
        Err(response
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("broker health response was not SERVING")
            .to_string())
    }
}

fn send_local_request(socket_path: &Path, request: &Value) -> Result<Value, String> {
    let encoded = serde_json::to_vec(request).map_err(|error| error.to_string())?;
    let mut stream = UnixStream::connect(socket_path).map_err(|error| error.to_string())?;
    stream
        .write_all(&encoded)
        .map_err(|error| error.to_string())?;
    stream.write_all(b"\n").map_err(|error| error.to_string())?;
    stream.flush().map_err(|error| error.to_string())?;
    let mut response = Vec::new();
    BufReader::new(stream)
        .take(MAX_REQUEST_BYTES + 1)
        .read_until(b'\n', &mut response)
        .map_err(|error| error.to_string())?;
    if response.len() as u64 > MAX_REQUEST_BYTES || !response.ends_with(b"\n") {
        return Err("broker returned an invalid or oversized response".to_string());
    }
    serde_json::from_slice(&response).map_err(|error| error.to_string())
}

fn read_private_token(path: &Path) -> Result<String, String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("operator token path must be a regular file".to_string());
    }
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err("operator token file permissions must deny group and other access".to_string());
    }
    let token = fs::read_to_string(path)
        .map_err(|error| error.to_string())?
        .trim()
        .to_string();
    if token.is_empty() {
        return Err("operator token file is empty".to_string());
    }
    Ok(token)
}

fn run_init_catalog(options: BTreeMap<String, Vec<String>>) -> Result<(), String> {
    let catalog_path = PathBuf::from(option(&options, "catalog", DEFAULT_CATALOG));
    let token_dir = PathBuf::from(option(&options, "token-dir", DEFAULT_TOKEN_DIR));
    let entries = options
        .get("source")
        .ok_or("at least one --source ID=UID[:GID] is required")?;
    let mut identities = BTreeMap::<String, (u32, Option<u32>)>::new();
    for entry in entries {
        let (source_id, identity) = entry
            .split_once('=')
            .ok_or_else(|| format!("invalid --source entry: {entry}"))?;
        let (uid, gid) = match identity.split_once(':') {
            Some((uid, gid)) => (
                uid.parse().map_err(|_| format!("invalid UID in {entry}"))?,
                Some(gid.parse().map_err(|_| format!("invalid GID in {entry}"))?),
            ),
            None => (
                identity
                    .parse()
                    .map_err(|_| format!("invalid UID in {entry}"))?,
                None,
            ),
        };
        if source_id.is_empty()
            || identities
                .insert(source_id.to_string(), (uid, gid))
                .is_some()
        {
            return Err(format!("empty or duplicate source id: {source_id}"));
        }
    }
    fs::create_dir_all(&token_dir).map_err(|error| error.to_string())?;
    let token_dir_metadata = fs::symlink_metadata(&token_dir).map_err(|error| error.to_string())?;
    if token_dir_metadata.file_type().is_symlink()
        || !token_dir_metadata.is_dir()
        || token_dir_metadata.uid() != 0
    {
        return Err(
            "source token directory must be a root-owned, non-writable real directory".to_string(),
        );
    }
    fs::set_permissions(&token_dir, fs::Permissions::from_mode(0o711))
        .map_err(|error| error.to_string())?;
    let token_dir_metadata = fs::symlink_metadata(&token_dir).map_err(|error| error.to_string())?;
    if token_dir_metadata.file_type().is_symlink()
        || !token_dir_metadata.is_dir()
        || token_dir_metadata.uid() != 0
        || token_dir_metadata.permissions().mode() & 0o022 != 0
    {
        return Err(
            "source token directory must be a root-owned, non-writable real directory".to_string(),
        );
    }
    if let Some(parent) = catalog_path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let existing = match fs::read(&catalog_path) {
        Ok(bytes) => {
            let metadata =
                fs::symlink_metadata(&catalog_path).map_err(|error| error.to_string())?;
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || metadata.uid() != 0
                || metadata.permissions().mode() & 0o022 != 0
            {
                return Err(
                    "existing activity catalog must be a root-owned, non-writable regular file"
                        .to_string(),
                );
            }
            let catalog = serde_json::from_slice::<CatalogInput>(&bytes)
                .map_err(|error| format!("existing catalog is invalid: {error}"))?;
            TrustedActivitySourceCatalog {
                schema_version: catalog.schema_version,
                generation: catalog.generation,
                sources: catalog.sources.clone(),
            }
            .validate()
            .map_err(|error| format!("existing catalog is invalid: {error}"))?;
            Some(catalog)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.to_string()),
    };
    let old_sources = existing
        .as_ref()
        .map(|catalog| {
            catalog
                .sources
                .iter()
                .map(|source| (source.source_id.clone(), source))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    let catalog_gid = match options.get("catalog-gid").and_then(|values| values.first()) {
        Some(value) => Some(
            value
                .parse::<u32>()
                .map_err(|_| "--catalog-gid must be an unsigned integer".to_string())?,
        ),
        None => fs::metadata(&catalog_path)
            .ok()
            .map(|metadata| metadata.gid())
            .or_else(|| Some(getegid().as_raw())),
    };
    let mut sources = Vec::new();
    let mut output = Vec::new();
    for (source_id, (uid, gid)) in &identities {
        validate_source_id(source_id)?;
        let token_path = token_dir.join(format!("{source_id}.token"));
        let token = match fs::symlink_metadata(&token_path) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    return Err(format!(
                        "source token path is not a regular file: {}",
                        token_path.display()
                    ));
                }
                if metadata.permissions().mode() & 0o777 != 0o400 {
                    return Err(format!(
                        "source token mode must be 0400: {}",
                        token_path.display()
                    ));
                }
                if metadata.uid() != 0 || metadata.gid() != 0 {
                    if nix::unistd::geteuid().as_raw() != 0 {
                        return Err(format!(
                            "source token must be root-owned; fixing its owner requires root: {}",
                            token_path.display()
                        ));
                    }
                    chown(&token_path, Some(Uid::from_raw(0)), Some(Gid::from_raw(0)))
                        .map_err(|error| error.to_string())?;
                }
                fs::read_to_string(&token_path)
                    .map_err(|error| error.to_string())?
                    .trim()
                    .to_string()
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if old_sources.contains_key(source_id) {
                    return Err(format!(
                        "source token is missing for installed source {source_id}; refusing implicit token rotation"
                    ));
                }
                let token = Uuid::new_v4().to_string();
                write_source_token(&token_path, &token)?;
                token
            }
            Err(error) => return Err(error.to_string()),
        };
        if token.is_empty() {
            return Err(format!("source token is empty: {}", token_path.display()));
        }
        let token_hash = format!("{:x}", Sha256::digest(token.as_bytes()));
        if let Some(old) = old_sources.get(source_id) {
            if old.source_token_sha256 != token_hash {
                return Err(format!(
                    "source token hash changed unexpectedly for {source_id}"
                ));
            }
        }
        sources.push(TrustedActivitySource {
            source_id: source_id.clone(),
            uid: *uid,
            gid: *gid,
            source_token_sha256: token_hash,
        });
        output
            .push(json!({"source_id": source_id, "token_file": token_path.display().to_string()}));
    }
    let mut new_sources = sources.clone();
    new_sources.sort_by(|left, right| left.source_id.cmp(&right.source_id));
    let mut old_sorted = existing
        .as_ref()
        .map(|catalog| catalog.sources.clone())
        .unwrap_or_default();
    old_sorted.sort_by(|left, right| left.source_id.cmp(&right.source_id));
    let changed = old_sorted != new_sources;
    let generation = match existing {
        Some(catalog) if changed => catalog
            .generation
            .checked_add(1)
            .ok_or_else(|| "activity catalog generation exhausted".to_string())?,
        Some(catalog) => catalog.generation,
        None => 1,
    };
    let catalog = TrustedActivitySourceCatalog {
        schema_version: 1,
        generation,
        sources: new_sources,
    };
    catalog.validate().map_err(|error| error.to_string())?;
    write_catalog_atomic(&catalog_path, &catalog, catalog_gid)?;
    println!(
        "{}",
        json!({"schema_version": 1, "generation": generation, "sources": output})
    );
    Ok(())
}

/// Creates a broker-managed secret that service managers project to one unit.
/// Host copies stay root-only so Products sharing a Unix UID cannot read each
/// other's activity-source tokens.
/// 中文：宿主token仅由systemd或Compose投影给对应服务，避免共享UID横向读取。
fn write_source_token(path: &Path, token: &str) -> Result<(), String> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true).mode(0o400);
    let mut file = options.open(path).map_err(|error| error.to_string())?;
    file.write_all(token.as_bytes())
        .map_err(|error| error.to_string())?;
    file.write_all(b"\n").map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())?;
    chown(path, Some(Uid::from_raw(0)), Some(Gid::from_raw(0)))
        .map_err(|error| error.to_string())?;
    File::open(path.parent().unwrap_or_else(|| Path::new(".")))
        .and_then(|directory| directory.sync_all())
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn write_catalog_atomic(
    path: &Path,
    catalog: &TrustedActivitySourceCatalog,
    catalog_gid: Option<u32>,
) -> Result<(), String> {
    let temp_path = path.with_extension(format!("json.tmp-{}", Uuid::new_v4()));
    let bytes = serde_json::to_vec_pretty(catalog).map_err(|error| error.to_string())?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true).mode(0o640);
    let mut file = options
        .open(&temp_path)
        .map_err(|error| error.to_string())?;
    file.set_permissions(fs::Permissions::from_mode(0o640))
        .map_err(|error| error.to_string())?;
    file.write_all(&bytes).map_err(|error| error.to_string())?;
    file.write_all(b"\n").map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())?;
    fs::rename(&temp_path, path).map_err(|error| error.to_string())?;
    chown(path, Some(Uid::from_raw(0)), catalog_gid.map(Gid::from_raw))
        .map_err(|error| error.to_string())?;
    File::open(path)
        .and_then(|catalog| catalog.sync_all())
        .map_err(|error| error.to_string())?;
    File::open(path.parent().unwrap_or_else(|| Path::new(".")))
        .and_then(|directory| directory.sync_all())
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn validate_source_id(source_id: &str) -> Result<(), String> {
    if !source_id.is_empty()
        && source_id.len() <= 256
        && source_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
    {
        Ok(())
    } else {
        Err(format!("invalid source_id: {source_id}"))
    }
}
