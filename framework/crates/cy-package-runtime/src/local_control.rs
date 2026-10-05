//! Authenticated node-local Unix-socket transport for the package runtime.
//!
//! The transport authenticates a local peer and scopes package/binding access
//! before forwarding a request to the one long-lived control server. Secret
//! tokens, environments, and connection refs are never logged here.
//! 中文：通过本地 peer 与精确 binding scope 验证保护持久化包运行时控制面。

use std::{
    collections::{BTreeSet, HashSet},
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    os::unix::{
        fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

use nix::sys::socket::{getsockopt, sockopt::PeerCredentials};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{
    BindingId, ControlCommand, ControlRequest, ControlResponse, InstallationId, PackageId,
    PackageRuntimeControlServer, PackageRuntimeError, control::failure_response,
};

const MAX_CONTROL_LINE_BYTES: usize = 1024 * 1024;
const MAX_POLICY_BYTES: usize = 1024 * 1024;
const MAX_REQUEST_ID_BYTES: usize = 256;
const MAX_TOKEN_BYTES: usize = 4096;
const MAX_ENVIRONMENT_ENTRIES: usize = 256;
const MAX_ENVIRONMENT_VALUE_BYTES: usize = 64 * 1024;
const MAX_ENVIRONMENT_BYTES: usize = 256 * 1024;
const MAX_CONNECTIONS: usize = 32;
const READ_TIMEOUT: Duration = Duration::from_secs(5);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_MAINTENANCE_SOCKET: &str = "/run/cyrene/runtime-maintenance.sock";
const BROKER_PROTOCOL_VERSION: &str = "cyrene.runtime-maintenance.broker.v1";
const BROKER_STATE_V2_CAPABILITY: &str = "cyrene.runtime-maintenance.state.v2";
const BINDING_OPERATIONS_PROTOCOL_VERSION: &str =
    "cyrene.runtime-maintenance.binding-operations.v1";
const PACKAGE_ADMISSION_CAPABILITY: &str = "cy-package-runtime.binding-operation-admission.v1";

/// Serves the authenticated production control protocol over a private UDS.
///
/// `control` owns the sole runtime instance; all accepted connections dispatch
/// through it instead of constructing per-connection package supervisors.
/// 中文：所有 UDS 客户端共享唯一 lifecycle/runtime supervisor。
pub struct PackageRuntimeSocketServer {
    control: Arc<PackageRuntimeControlServer>,
    policy: SourcePolicyStore,
    socket_path: PathBuf,
    broker_socket_path: PathBuf,
    active_connections: Arc<AtomicUsize>,
    binding_operation_lock: Mutex<()>,
    #[cfg(test)]
    _test_directory: Option<tempfile::TempDir>,
}

impl PackageRuntimeSocketServer {
    /// Creates a UDS server and rejects missing or invalid startup policy.
    pub fn new(
        control: PackageRuntimeControlServer,
        socket_path: impl Into<PathBuf>,
        source_policy_path: impl Into<PathBuf>,
    ) -> Result<Self, PackageRuntimeError> {
        let socket_path = socket_path.into();
        if !socket_path.is_absolute() {
            return Err(PackageRuntimeError::new(
                "CONTROL_SOCKET_PATH_INVALID",
                "control socket path must be absolute",
            ));
        }
        let policy = SourcePolicyStore::file(source_policy_path.into());
        policy.load()?;
        Ok(Self {
            control: Arc::new(control),
            policy,
            socket_path,
            broker_socket_path: PathBuf::from(DEFAULT_MAINTENANCE_SOCKET),
            active_connections: Arc::new(AtomicUsize::new(0)),
            binding_operation_lock: Mutex::new(()),
            #[cfg(test)]
            _test_directory: None,
        })
    }

    /// Binds the configured socket and serves requests until the process stops.
    pub fn serve(self) -> Result<(), PackageRuntimeError> {
        prepare_socket_parent(&self.socket_path)?;
        remove_stale_socket(&self.socket_path)?;
        let listener = UnixListener::bind(&self.socket_path).map_err(|error| {
            PackageRuntimeError::new(
                "CONTROL_SOCKET_BIND_FAILED",
                format!("could not bind the package runtime control socket: {error}"),
            )
        })?;
        fs::set_permissions(&self.socket_path, fs::Permissions::from_mode(0o660)).map_err(
            |error| {
                PackageRuntimeError::new(
                    "CONTROL_SOCKET_PERMISSIONS_FAILED",
                    format!("could not set package runtime socket permissions: {error}"),
                )
            },
        )?;

        let server = Arc::new(self);
        for accepted in listener.incoming() {
            match accepted {
                Ok(stream) => {
                    if !server.try_acquire_connection() {
                        continue;
                    }
                    let server = Arc::clone(&server);
                    thread::spawn(move || {
                        let _permit = ConnectionPermit(server.active_connections.clone());
                        if let Err(error) = server.handle_connection(stream) {
                            tracing::warn!(
                                event.name = "platform.package.control_connection_failed",
                                error.code = %error.code,
                                message = "package runtime control connection failed",
                            );
                        }
                    });
                }
                Err(error) => {
                    tracing::warn!(
                        event.name = "platform.package.control_accept_failed",
                        message = %error,
                    );
                }
            }
        }
        Ok(())
    }

    fn try_acquire_connection(&self) -> bool {
        self.active_connections
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < MAX_CONNECTIONS).then_some(active + 1)
            })
            .is_ok()
    }

    fn handle_connection(&self, mut stream: UnixStream) -> Result<(), PackageRuntimeError> {
        stream
            .set_read_timeout(Some(READ_TIMEOUT))
            .and_then(|()| stream.set_write_timeout(Some(WRITE_TIMEOUT)))
            .map_err(|error| {
                PackageRuntimeError::new(
                    "CONTROL_SOCKET_IO_FAILED",
                    format!("could not configure control socket timeouts: {error}"),
                )
            })?;
        let peer = peer_identity(&stream)?;
        let mut line = Vec::new();
        if let Err(error) = read_bounded_line(&mut stream, &mut line) {
            let response = error_response(
                request_id_from_line(&line),
                "CONTROL_REQUEST_INVALID",
                "control request could not be read within protocol bounds",
            );
            write_response(&mut stream, &response)?;
            if error.kind() == io::ErrorKind::TimedOut || error.kind() == io::ErrorKind::WouldBlock
            {
                return Ok(());
            }
            return Ok(());
        }
        let request = match parse_authenticated_request(&line) {
            Ok(request) => request,
            Err((request_id, code, message)) => {
                write_response(&mut stream, &error_response(request_id, code, message))?;
                return Ok(());
            }
        };
        let response = self.process_authenticated_request(request, peer);
        write_response(&mut stream, &response)
    }

    /// Applies the same authentication and authorization path to socket clients
    /// and deterministic in-memory tests.
    fn process_authenticated_request(
        &self,
        request: AuthenticatedRequest,
        peer: PeerIdentity,
    ) -> ControlResponse {
        let request_id = request.control.request_id.clone();
        let policy = match self.policy.load() {
            Ok(policy) => policy,
            Err(_) => {
                return error_response(
                    request_id,
                    "CONTROL_SOURCE_POLICY_UNAVAILABLE",
                    "package runtime source policy is unavailable or invalid",
                );
            }
        };
        let principal = match authenticate(
            &policy,
            peer,
            &request.auth,
            request.generation,
            &request.control.command,
        ) {
            Ok(principal) => principal,
            Err((code, message)) => return error_response(request_id, code, message),
        };
        if let Err((code, message)) =
            authorize_command(&self.control, &principal, &request.control.command)
        {
            return error_response(request_id, code, message);
        }
        if let Err((code, message)) = validate_request_limits(&request.control.command) {
            return error_response(request_id, code, message);
        }

        if request.control.command.is_state_mutation() {
            let response = if request.control.command.is_binding_mutation() {
                match self.handle_binding_mutation(request, &policy, principal) {
                    Ok(response) => response,
                    Err(error) => failure_response(request_id, error),
                }
            } else {
                error_response(
                    request_id,
                    "MAINTENANCE_ADMISSION_REQUIRED",
                    "administrative mutation has no scoped maintenance admission",
                )
            };
            return response;
        }
        if matches!(request.control.command, ControlCommand::Shutdown) {
            return error_response(
                request_id,
                "CONTROL_SHUTDOWN_UNSUPPORTED",
                "stop the managed service through its systemd owner",
            );
        }

        let mut response = self
            .control
            .handle_command(request.control.request_id, request.control.command);
        if response.ok
            && let Some(result) = response.result.as_mut()
            && result.get("authority").and_then(Value::as_str) == Some("platform_package_runtime")
        {
            result["catalog_generation"] = json!(policy.generation);
            result["capabilities"] = if self
                .broker_advertises_binding_admission(&response.request_id, policy.generation)
            {
                json!([PACKAGE_ADMISSION_CAPABILITY])
            } else {
                json!([])
            };
        }
        response
    }

    /// Reserves one Product-scoped gate operation before changing runtime state.
    ///
    /// A successful result carries the broker receipt so the owner can persist
    /// its own binding record and complete the lease afterward.
    /// 中文：daemon 不知道 Product 的提交点，因此绝不自动 Complete。
    fn handle_binding_mutation(
        &self,
        request: AuthenticatedRequest,
        policy: &PolicySnapshot,
        principal: AuthenticatedPrincipal<'_>,
    ) -> Result<ControlResponse, PackageRuntimeError> {
        let _serialized = self.binding_operation_lock.lock().map_err(|_| {
            PackageRuntimeError::new(
                "MAINTENANCE_ADMISSION_REQUIRED",
                "binding operation serialization is unavailable",
            )
        })?;
        let AuthenticatedPrincipal::Source(source) = principal else {
            return Ok(error_response(
                request.control.request_id,
                "MAINTENANCE_ADMISSION_REQUIRED",
                "binding mutation requires an authenticated Product source",
            ));
        };
        let Some(scope) = self.binding_operation_scope(&request.control.command)? else {
            return Ok(error_response(
                request.control.request_id,
                "MAINTENANCE_ADMISSION_REQUIRED",
                "this binding mutation has no admitted maintenance operation",
            ));
        };
        let source_id = request.auth.source_id.as_deref().ok_or_else(|| {
            PackageRuntimeError::new(
                "CONTROL_AUTH_INVALID",
                "authenticated Product source is required for a binding mutation",
            )
        })?;
        let source_token = request.auth.source_token.as_deref().ok_or_else(|| {
            PackageRuntimeError::new(
                "CONTROL_AUTH_INVALID",
                "authenticated Product source is required for a binding mutation",
            )
        })?;
        if source.source_id != source_id {
            return Ok(error_response(
                request.control.request_id,
                "CONTROL_AUTH_INVALID",
                "authenticated Product source is inconsistent",
            ));
        }
        let generation = policy.generation;
        let admission = match self.admit_binding_operation(
            &request.control.request_id,
            source_id,
            source_token,
            generation,
            &scope,
        ) {
            Ok(admission) => admission,
            Err(error) => return Ok(failure_response(request.control.request_id, error)),
        };
        Ok(self.respond_to_admitted_mutation(request, admission))
    }

    /// Executes a fresh reservation or returns a typed no-replay outcome.
    fn respond_to_admitted_mutation(
        &self,
        request: AuthenticatedRequest,
        admission: BrokerBindingOperationAdmission,
    ) -> ControlResponse {
        let receipt = serde_json::to_value(BindingOperationReceipt {
            request_id: &admission.request_id,
            source_id: &admission.source_id,
            protocol_version: BINDING_OPERATIONS_PROTOCOL_VERSION,
            scope: &admission.scope,
            catalog_generation: admission.catalog_generation,
            gate_generation: admission.gate_generation,
            operation_token: &admission.operation_token,
            already_in_flight: admission.already_in_flight,
            already_completed: admission.already_completed,
        })
        .map_err(|_| {
            PackageRuntimeError::new(
                "MAINTENANCE_ADMISSION_INVALID",
                "maintenance broker returned an invalid binding admission",
            )
        });
        let receipt = match receipt {
            Ok(receipt) => receipt,
            Err(error) => return failure_response(request.control.request_id, error),
        };

        if admission.already_in_flight {
            let mut response = error_response(
                request.control.request_id,
                "BINDING_OPERATION_PENDING",
                "the admitted binding operation is still pending owner reconciliation",
            );
            if let Some(error) = response.error.as_mut() {
                error.binding_operation = Some(receipt);
            }
            return response;
        }

        if admission.already_completed {
            let status = self.control.handle_command(
                request.control.request_id.clone(),
                ControlCommand::RuntimeStatus {
                    binding_id: admission.scope.binding_id.clone(),
                },
            );
            let mut response = status;
            if response.ok
                && let Some(result) = response.result.as_mut()
            {
                result["binding_operation"] = receipt;
            } else if let Some(error) = response.error.as_mut() {
                error.binding_operation = Some(receipt);
            }
            return response;
        }

        let mut response = self
            .control
            .handle_command(request.control.request_id, request.control.command);
        if response.ok
            && let Some(result) = response.result.as_mut()
        {
            result["binding_operation"] = receipt;
        } else if let Some(error) = response.error.as_mut() {
            error.binding_operation = Some(receipt);
        }
        response
    }

    /// Resolves exact package/install identities from durable runtime state.
    ///
    /// `recover` and `deactivate` do not accept installation IDs from callers;
    /// they inherit the identity from the activation record.
    fn binding_operation_scope(
        &self,
        command: &ControlCommand,
    ) -> Result<Option<BindingOperationScope>, PackageRuntimeError> {
        let (binding_id, installation_id, operation) = match command {
            ControlCommand::Activate {
                binding_id,
                installation_id,
                ..
            } => (
                binding_id.as_str(),
                Some(installation_id.as_str()),
                "activate",
            ),
            ControlCommand::RecoverBinding { binding_id, .. } => {
                (binding_id.as_str(), None, "recover")
            }
            ControlCommand::Deactivate { binding_id } => (binding_id.as_str(), None, "deactivate"),
            _ => return Ok(None),
        };
        let (package_id, installation_id) = match installation_id {
            Some(installation_id) => (
                self.control.installation_package_id(installation_id)?,
                installation_id.to_string(),
            ),
            None => {
                let (package_id, installation_id) =
                    self.control.binding_package_scope(binding_id)?;
                (package_id, installation_id)
            }
        };
        Ok(Some(BindingOperationScope {
            binding_id: binding_id.to_string(),
            package_id,
            installation_id,
            operation: operation.to_string(),
        }))
    }

    /// Performs a version-checked broker handshake and exact-scope reservation.
    fn admit_binding_operation(
        &self,
        request_id: &str,
        source_id: &str,
        source_token: &str,
        generation: u64,
        scope: &BindingOperationScope,
    ) -> Result<BrokerBindingOperationAdmission, PackageRuntimeError> {
        self.require_broker_capability(request_id, generation)?;
        let request = json!({
            "request_id": request_id,
            "method": "AdmitBindingOperation",
            "protocol_version": BINDING_OPERATIONS_PROTOCOL_VERSION,
            "auth": { "source_id": source_id, "source_token": source_token },
            "params": {
                "source_id": source_id,
                "expected_catalog_generation": generation,
                "binding_id": scope.binding_id,
                "package_id": scope.package_id,
                "installation_id": scope.installation_id,
                "operation": scope.operation,
            },
        });
        let response = broker_exchange(&self.broker_socket_path, &request)?;
        let result = broker_result(&response, request_id)?;
        let admission: BrokerBindingOperationAdmission = serde_json::from_value(result.clone())
            .map_err(|_| {
                PackageRuntimeError::new(
                    "MAINTENANCE_ADMISSION_INVALID",
                    "maintenance broker returned an invalid binding admission",
                )
            })?;
        if admission.request_id != request_id
            || admission.source_id != source_id
            || &admission.scope != scope
            || admission.operation_token.is_empty()
            || admission.operation_token.len() > 256
            || admission.catalog_generation != generation
            || admission.gate_generation == 0
            || (admission.already_in_flight && admission.already_completed)
        {
            return Err(PackageRuntimeError::new(
                "MAINTENANCE_ADMISSION_INVALID",
                "maintenance broker returned an inconsistent binding admission",
            ));
        }
        Ok(admission)
    }

    /// Rejects mutations when the installed broker lacks the exact gate API.
    fn require_broker_capability(
        &self,
        request_id: &str,
        generation: u64,
    ) -> Result<(), PackageRuntimeError> {
        if self.broker_advertises_binding_admission(request_id, generation) {
            Ok(())
        } else {
            Err(PackageRuntimeError::new(
                "MAINTENANCE_ADMISSION_UNAVAILABLE",
                "maintenance broker does not advertise the required binding admission protocol",
            ))
        }
    }

    /// Returns true only for the exact broker protocol/capability/generation.
    fn broker_advertises_binding_admission(&self, request_id: &str, generation: u64) -> bool {
        let request = json!({
            "request_id": request_id,
            "method": "Health",
            "params": {},
        });
        let Ok(response) = broker_exchange(&self.broker_socket_path, &request) else {
            return false;
        };
        let Ok(result) = broker_result(&response, request_id) else {
            return false;
        };
        result.get("status").and_then(Value::as_str) == Some("SERVING")
            && result.get("protocol_version").and_then(Value::as_str)
                == Some(BROKER_PROTOCOL_VERSION)
            && result
                .get("capabilities")
                .and_then(Value::as_array)
                .is_some_and(|capabilities| {
                    let unique_capabilities = capabilities
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<BTreeSet<_>>();
                    unique_capabilities.len() == capabilities.len()
                        && capabilities.iter().all(|capability| {
                            capability.as_str().is_some_and(|value| !value.is_empty())
                        })
                        && unique_capabilities.contains(BROKER_STATE_V2_CAPABILITY)
                        && unique_capabilities.contains(BINDING_OPERATIONS_PROTOCOL_VERSION)
                })
            && result.get("catalog_generation").and_then(Value::as_u64) == Some(generation)
    }
}

#[derive(Debug, Clone, Copy)]
struct PeerIdentity {
    uid: u32,
    gid: u32,
}

struct ConnectionPermit(Arc<AtomicUsize>);

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Debug)]
struct AuthenticatedRequest {
    control: ControlRequest,
    auth: RequestAuth,
    generation: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestAuth {
    #[serde(default)]
    source_id: Option<String>,
    #[serde(default)]
    source_token: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SourcePolicyFile {
    schema_version: u32,
    generation: u64,
    sources: Vec<SourcePolicy>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SourcePolicy {
    source_id: String,
    uid: u32,
    gid: u32,
    source_token_sha256: String,
    bindings: Vec<BindingPolicy>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BindingPolicy {
    binding_id: String,
    package_id: String,
    installation_ids: Vec<String>,
    operations: Vec<String>,
}

struct ValidatedSourcePolicy {
    source_id: String,
    uid: u32,
    gid: u32,
    token_digest: [u8; 32],
    bindings: Vec<ValidatedBindingPolicy>,
}

struct ValidatedBindingPolicy {
    binding_id: String,
    package_id: String,
    installation_ids: BTreeSet<String>,
    operations: BTreeSet<String>,
}

struct PolicySnapshot {
    generation: u64,
    sources: Vec<ValidatedSourcePolicy>,
}

enum PolicySource {
    File(PathBuf),
    #[cfg(test)]
    Memory(Arc<PolicySnapshot>),
}

struct SourcePolicyStore(PolicySource);

impl SourcePolicyStore {
    fn file(path: PathBuf) -> Self {
        Self(PolicySource::File(path))
    }

    fn load(&self) -> Result<PolicySnapshot, PackageRuntimeError> {
        match &self.0 {
            PolicySource::File(path) => load_root_owned_policy(path),
            #[cfg(test)]
            PolicySource::Memory(policy) => Ok(clone_policy(policy)),
        }
    }

    #[cfg(test)]
    fn memory(policy: PolicySnapshot) -> Self {
        Self(PolicySource::Memory(Arc::new(policy)))
    }
}

#[derive(Clone, Copy)]
enum AuthenticatedPrincipal<'a> {
    Operator,
    Source(&'a ValidatedSourcePolicy),
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BindingOperationScope {
    binding_id: String,
    package_id: String,
    installation_id: String,
    operation: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BrokerBindingOperationAdmission {
    request_id: String,
    source_id: String,
    scope: BindingOperationScope,
    operation_token: String,
    catalog_generation: u64,
    gate_generation: u64,
    already_in_flight: bool,
    already_completed: bool,
}

#[derive(Debug, serde::Serialize)]
struct BindingOperationReceipt<'a> {
    request_id: &'a str,
    source_id: &'a str,
    protocol_version: &'static str,
    scope: &'a BindingOperationScope,
    catalog_generation: u64,
    gate_generation: u64,
    operation_token: &'a str,
    already_in_flight: bool,
    already_completed: bool,
}

fn parse_authenticated_request(
    line: &[u8],
) -> Result<AuthenticatedRequest, (String, &'static str, &'static str)> {
    let mut value: Value = serde_json::from_slice(line).map_err(|_| {
        (
            request_id_from_line(line),
            "CONTROL_REQUEST_INVALID",
            "control request is not valid JSON",
        )
    })?;
    let object = value.as_object_mut().ok_or_else(|| {
        (
            String::new(),
            "CONTROL_REQUEST_INVALID",
            "control request must be a JSON object",
        )
    })?;
    let request_id = object
        .get("request_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let auth_value = object.remove("auth").ok_or_else(|| {
        (
            request_id.clone(),
            "CONTROL_AUTH_REQUIRED",
            "authenticated control envelope is required",
        )
    })?;
    let auth = serde_json::from_value::<RequestAuth>(auth_value).map_err(|_| {
        (
            request_id.clone(),
            "CONTROL_AUTH_INVALID",
            "control authentication envelope is invalid",
        )
    })?;
    let generation = match object.remove("catalog_generation") {
        None => None,
        Some(Value::Number(number)) => Some(number.as_u64().filter(|value| *value > 0).ok_or((
            request_id.clone(),
            "CONTROL_GENERATION_INVALID",
            "catalog_generation must be a positive integer",
        ))?),
        Some(_) => {
            return Err((
                request_id,
                "CONTROL_GENERATION_INVALID",
                "catalog_generation must be a positive integer",
            ));
        }
    };
    validate_control_fields(&value)
        .map_err(|message| (request_id.clone(), "CONTROL_REQUEST_INVALID", message))?;
    let control: ControlRequest = serde_json::from_value(value).map_err(|_| {
        (
            request_id.clone(),
            "CONTROL_REQUEST_INVALID",
            "control operation or fields are invalid",
        )
    })?;
    if control.request_id.is_empty()
        || control.request_id.len() > MAX_REQUEST_ID_BYTES
        || control
            .request_id
            .bytes()
            .any(|byte| byte == 0 || byte.is_ascii_control())
    {
        return Err((
            control.request_id,
            "CONTROL_REQUEST_INVALID",
            "request_id is empty or exceeds protocol limits",
        ));
    }
    Ok(AuthenticatedRequest {
        control,
        auth,
        generation,
    })
}

fn validate_control_fields(value: &Value) -> Result<(), &'static str> {
    let object = value
        .as_object()
        .ok_or("control request must be a JSON object")?;
    let operation = object
        .get("operation")
        .and_then(Value::as_str)
        .ok_or("control operation is required")?;
    let fields: &[&str] = match operation {
        "authority" | "list_installations" | "cleanup" | "orphan_runtime_count" | "shutdown" => {
            &["request_id", "operation"]
        }
        "inspect" | "verify" | "install" => {
            &["request_id", "operation", "descriptor_path", "archive_path"]
        }
        "install_offline" => &["request_id", "operation", "package_id", "package_version"],
        "get_installation" | "uninstall" => &["request_id", "operation", "installation_id"],
        "activate" | "upgrade" => &[
            "request_id",
            "operation",
            "binding_id",
            "installation_id",
            "environment",
        ],
        "recover_binding" | "rollback" => &["request_id", "operation", "binding_id", "environment"],
        "deactivate" | "runtime_status" | "remove_binding_reference" => {
            &["request_id", "operation", "binding_id"]
        }
        _ => return Err("control operation is invalid"),
    };
    if object.keys().any(|key| !fields.contains(&key.as_str())) {
        return Err("control request contains an unknown field");
    }
    Ok(())
}

fn authenticate<'a>(
    policy: &'a PolicySnapshot,
    peer: PeerIdentity,
    auth: &RequestAuth,
    generation: Option<u64>,
    command: &ControlCommand,
) -> Result<AuthenticatedPrincipal<'a>, (&'static str, &'static str)> {
    let is_authority = matches!(command, ControlCommand::Authority);
    match (&auth.source_id, &auth.source_token) {
        (None, None) if peer.uid == 0 => {
            require_generation(policy, generation, is_authority)?;
            Ok(AuthenticatedPrincipal::Operator)
        }
        (Some(source_id), Some(source_token)) => {
            if source_id.is_empty()
                || source_token.is_empty()
                || source_token.len() > MAX_TOKEN_BYTES
            {
                return Err(("CONTROL_AUTH_INVALID", "source credentials are invalid"));
            }
            let source = policy
                .sources
                .iter()
                .find(|source| source.source_id == *source_id)
                .ok_or((
                    "CONTROL_SOURCE_UNTRUSTED",
                    "control source is not installed",
                ))?;
            if source.uid != peer.uid || source.gid != peer.gid {
                return Err((
                    "CONTROL_PEER_MISMATCH",
                    "socket peer does not match the installed source",
                ));
            }
            if !constant_time_eq(
                &source.token_digest,
                Sha256::digest(source_token.as_bytes()).as_slice(),
            ) {
                return Err(("CONTROL_AUTH_INVALID", "source credentials are invalid"));
            }
            require_generation(policy, generation, is_authority)?;
            Ok(AuthenticatedPrincipal::Source(source))
        }
        _ => Err((
            "CONTROL_AUTH_INVALID",
            "control authentication envelope is invalid",
        )),
    }
}

fn require_generation(
    policy: &PolicySnapshot,
    generation: Option<u64>,
    is_authority: bool,
) -> Result<(), (&'static str, &'static str)> {
    match generation {
        Some(generation) if generation == policy.generation => Ok(()),
        None if is_authority => Ok(()),
        _ => Err((
            "CONTROL_GENERATION_STALE",
            "control request does not match the current source policy generation",
        )),
    }
}

fn authorize_command(
    control: &PackageRuntimeControlServer,
    principal: &AuthenticatedPrincipal<'_>,
    command: &ControlCommand,
) -> Result<(), (&'static str, &'static str)> {
    if matches!(command, ControlCommand::Shutdown) {
        return Err((
            "CONTROL_SHUTDOWN_UNSUPPORTED",
            "stop the managed service through its systemd owner",
        ));
    }
    let AuthenticatedPrincipal::Source(source) = principal else {
        return Ok(());
    };
    let operation = operation_name(command);
    match command {
        ControlCommand::Authority => Ok(()),
        ControlCommand::GetInstallation { installation_id } => {
            let scoped = source.bindings.iter().any(|binding| {
                binding.operations.contains(operation)
                    && binding.installation_ids.contains(installation_id)
                    && control
                        .installation_package_id(installation_id)
                        .is_ok_and(|package_id| package_id == binding.package_id)
            });
            scoped.then_some(()).ok_or((
                "CONTROL_SCOPE_DENIED",
                "operation is outside the installed source scope",
            ))
        }
        ControlCommand::Activate {
            binding_id,
            installation_id,
            ..
        }
        | ControlCommand::Upgrade {
            binding_id,
            installation_id,
            ..
        } => {
            let binding = find_binding_scope(source, binding_id, operation, Some(installation_id))?;
            if !control
                .installation_package_id(installation_id)
                .is_ok_and(|package_id| package_id == binding.package_id)
            {
                return Err((
                    "CONTROL_SCOPE_DENIED",
                    "operation is outside the installed source scope",
                ));
            }
            Ok(())
        }
        ControlCommand::RecoverBinding { binding_id, .. }
        | ControlCommand::Deactivate { binding_id }
        | ControlCommand::RuntimeStatus { binding_id }
        | ControlCommand::Rollback { binding_id, .. } => {
            let binding = find_binding_scope(source, binding_id, operation, None)?;
            let in_scope = control.binding_package_scope(binding_id).is_ok_and(
                |(package_id, installation_id)| {
                    package_id == binding.package_id
                        && binding.installation_ids.contains(&installation_id)
                },
            );
            if !in_scope {
                return Err((
                    "CONTROL_SCOPE_DENIED",
                    "operation is outside the installed source scope",
                ));
            }
            Ok(())
        }
        _ => Err((
            "CONTROL_SCOPE_DENIED",
            "operation is outside the installed source scope",
        )),
    }
}

fn find_binding_scope<'a>(
    source: &'a ValidatedSourcePolicy,
    binding_id: &str,
    operation: &str,
    installation_id: Option<&str>,
) -> Result<&'a ValidatedBindingPolicy, (&'static str, &'static str)> {
    source
        .bindings
        .iter()
        .find(|binding| {
            binding.binding_id == binding_id
                && binding.operations.contains(operation)
                && installation_id.is_none_or(|id| binding.installation_ids.contains(id))
        })
        .ok_or((
            "CONTROL_SCOPE_DENIED",
            "operation is outside the installed source scope",
        ))
}

fn operation_name(command: &ControlCommand) -> &'static str {
    match command {
        ControlCommand::Authority => "authority",
        ControlCommand::Inspect { .. } => "inspect",
        ControlCommand::Verify { .. } => "verify",
        ControlCommand::Install { .. } => "install",
        ControlCommand::InstallOffline { .. } => "install_offline",
        ControlCommand::GetInstallation { .. } => "get_installation",
        ControlCommand::ListInstallations => "list_installations",
        ControlCommand::Activate { .. } => "activate",
        ControlCommand::RecoverBinding { .. } => "recover_binding",
        ControlCommand::Deactivate { .. } => "deactivate",
        ControlCommand::RuntimeStatus { .. } => "runtime_status",
        ControlCommand::Upgrade { .. } => "upgrade",
        ControlCommand::Rollback { .. } => "rollback",
        ControlCommand::RemoveBindingReference { .. } => "remove_binding_reference",
        ControlCommand::Uninstall { .. } => "uninstall",
        ControlCommand::Cleanup => "cleanup",
        ControlCommand::OrphanRuntimeCount => "orphan_runtime_count",
        ControlCommand::Shutdown => "shutdown",
    }
}

fn validate_request_limits(command: &ControlCommand) -> Result<(), (&'static str, &'static str)> {
    let environment = match command {
        ControlCommand::Activate { environment, .. }
        | ControlCommand::RecoverBinding { environment, .. }
        | ControlCommand::Upgrade { environment, .. }
        | ControlCommand::Rollback { environment, .. } => environment,
        _ => return Ok(()),
    };
    if environment.len() > MAX_ENVIRONMENT_ENTRIES {
        return Err((
            "CONTROL_REQUEST_TOO_LARGE",
            "environment exceeds protocol limits",
        ));
    }
    let mut total = 0usize;
    for (key, value) in environment {
        if key.is_empty()
            || key.len() > 256
            || key.contains('=')
            || key.as_bytes().contains(&0)
            || value.len() > MAX_ENVIRONMENT_VALUE_BYTES
            || value.as_bytes().contains(&0)
        {
            return Err((
                "CONTROL_REQUEST_INVALID",
                "environment exceeds protocol limits",
            ));
        }
        total = total.saturating_add(key.len()).saturating_add(value.len());
    }
    if total > MAX_ENVIRONMENT_BYTES {
        return Err((
            "CONTROL_REQUEST_TOO_LARGE",
            "environment exceeds protocol limits",
        ));
    }
    Ok(())
}

fn load_root_owned_policy(path: &Path) -> Result<PolicySnapshot, PackageRuntimeError> {
    if !path.is_absolute() {
        return Err(policy_error());
    }
    let parent = path.parent().ok_or_else(policy_error)?;
    validate_root_owned_directories(parent)?;
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    let file = options.open(path).map_err(|_| policy_error())?;
    let metadata = file.metadata().map_err(|_| policy_error())?;
    let path_metadata = fs::symlink_metadata(path).map_err(|_| policy_error())?;
    if !metadata.is_file()
        || metadata.uid() != 0
        || metadata.nlink() != 1
        || metadata.permissions().mode() & 0o022 != 0
        || path_metadata.file_type().is_symlink()
        || metadata.dev() != path_metadata.dev()
        || metadata.ino() != path_metadata.ino()
    {
        return Err(policy_error());
    }
    let mut bytes = Vec::new();
    file.take((MAX_POLICY_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| policy_error())?;
    if bytes.len() > MAX_POLICY_BYTES {
        return Err(policy_error());
    }
    parse_policy(&bytes)
}

fn validate_root_owned_directories(parent: &Path) -> Result<(), PackageRuntimeError> {
    for directory in parent.ancestors() {
        let metadata = fs::symlink_metadata(directory).map_err(|_| policy_error())?;
        if metadata.file_type().is_symlink()
            || !metadata.is_dir()
            || metadata.uid() != 0
            || metadata.permissions().mode() & 0o022 != 0
        {
            return Err(policy_error());
        }
    }
    Ok(())
}

fn parse_policy(bytes: &[u8]) -> Result<PolicySnapshot, PackageRuntimeError> {
    let parsed: SourcePolicyFile = serde_json::from_slice(bytes).map_err(|_| policy_error())?;
    if parsed.schema_version != 1 || parsed.generation == 0 {
        return Err(policy_error());
    }
    let mut source_ids = HashSet::new();
    let mut binding_owners = HashSet::new();
    let mut sources = Vec::with_capacity(parsed.sources.len());
    for source in parsed.sources {
        if !valid_source_id(&source.source_id)
            || source.uid == 0
            || !source_ids.insert(source.source_id.clone())
        {
            return Err(policy_error());
        }
        let token_digest =
            decode_sha256_hex(&source.source_token_sha256).ok_or_else(policy_error)?;
        let mut binding_ids = HashSet::new();
        let mut bindings = Vec::with_capacity(source.bindings.len());
        for binding in source.bindings {
            BindingId::new(binding.binding_id.clone()).map_err(|_| policy_error())?;
            PackageId::new(binding.package_id.clone()).map_err(|_| policy_error())?;
            if !binding_ids.insert(binding.binding_id.clone())
                || !binding_owners.insert(binding.binding_id.clone())
            {
                return Err(policy_error());
            }
            let mut installation_ids = BTreeSet::new();
            for installation_id in binding.installation_ids {
                InstallationId::new(installation_id.clone()).map_err(|_| policy_error())?;
                if !installation_ids.insert(installation_id) {
                    return Err(policy_error());
                }
            }
            let mut operations = BTreeSet::new();
            for operation in binding.operations {
                if !matches!(
                    operation.as_str(),
                    "activate"
                        | "recover_binding"
                        | "deactivate"
                        | "runtime_status"
                        | "get_installation"
                ) || !operations.insert(operation)
                {
                    return Err(policy_error());
                }
            }
            if (operations.contains("activate") || operations.contains("upgrade"))
                && installation_ids.is_empty()
            {
                return Err(policy_error());
            }
            bindings.push(ValidatedBindingPolicy {
                binding_id: binding.binding_id,
                package_id: binding.package_id,
                installation_ids,
                operations,
            });
        }
        sources.push(ValidatedSourcePolicy {
            source_id: source.source_id,
            uid: source.uid,
            gid: source.gid,
            token_digest,
            bindings,
        });
    }
    Ok(PolicySnapshot {
        generation: parsed.generation,
        sources,
    })
}

fn valid_source_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
}

fn decode_sha256_hex(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    let mut decoded = [0u8; 32];
    for (index, byte) in decoded.iter_mut().enumerate() {
        let offset = index * 2;
        *byte = u8::from_str_radix(&value[offset..offset + 2], 16).ok()?;
    }
    Some(decoded)
}

fn constant_time_eq(left: &[u8; 32], right: &[u8]) -> bool {
    left.iter()
        .zip(right.iter())
        .fold(0u8, |difference, (left, right)| difference | (left ^ right))
        == 0
}

/// Sends one bounded JSON-lines request to the node-local maintenance broker.
///
/// Broker errors are reduced to structured codes; credential and broker detail
/// never enters transport logs or returned messages.
fn broker_exchange(socket_path: &Path, request: &Value) -> Result<Value, PackageRuntimeError> {
    let request_id = request
        .get("request_id")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            PackageRuntimeError::new(
                "MAINTENANCE_ADMISSION_INVALID",
                "maintenance request identity is unavailable",
            )
        })?;
    let mut stream = UnixStream::connect(socket_path).map_err(|_| {
        PackageRuntimeError::new(
            "MAINTENANCE_ADMISSION_UNAVAILABLE",
            "maintenance broker is unavailable",
        )
    })?;
    stream
        .set_read_timeout(Some(READ_TIMEOUT))
        .and_then(|()| stream.set_write_timeout(Some(WRITE_TIMEOUT)))
        .map_err(|_| {
            PackageRuntimeError::new(
                "MAINTENANCE_ADMISSION_UNAVAILABLE",
                "maintenance broker connection could not be configured",
            )
        })?;
    serde_json::to_writer(&mut stream, request).map_err(|_| {
        PackageRuntimeError::new(
            "MAINTENANCE_ADMISSION_INVALID",
            "maintenance request could not be encoded",
        )
    })?;
    stream.write_all(b"\n").map_err(|_| {
        PackageRuntimeError::new(
            "MAINTENANCE_ADMISSION_UNAVAILABLE",
            "maintenance broker connection failed",
        )
    })?;
    stream.flush().map_err(|_| {
        PackageRuntimeError::new(
            "MAINTENANCE_ADMISSION_UNAVAILABLE",
            "maintenance broker connection failed",
        )
    })?;
    let mut line = Vec::new();
    read_bounded_line(&mut stream, &mut line).map_err(|_| {
        PackageRuntimeError::new(
            "MAINTENANCE_ADMISSION_UNAVAILABLE",
            "maintenance broker response was unavailable or exceeded protocol limits",
        )
    })?;
    let response: Value = serde_json::from_slice(&line).map_err(|_| {
        PackageRuntimeError::new(
            "MAINTENANCE_ADMISSION_INVALID",
            "maintenance broker returned invalid JSON",
        )
    })?;
    if response.get("request_id").and_then(Value::as_str) != Some(request_id) {
        return Err(PackageRuntimeError::new(
            "MAINTENANCE_ADMISSION_INVALID",
            "maintenance broker response did not match the request",
        ));
    }
    Ok(response)
}

fn broker_result<'a>(
    response: &'a Value,
    request_id: &str,
) -> Result<&'a Value, PackageRuntimeError> {
    if response.get("request_id").and_then(Value::as_str) != Some(request_id) {
        return Err(PackageRuntimeError::new(
            "MAINTENANCE_ADMISSION_INVALID",
            "maintenance broker response did not match the request",
        ));
    }
    if let Some(error) = response.get("error") {
        let code = error
            .get("code")
            .and_then(Value::as_str)
            .filter(|code| {
                !code.is_empty()
                    && code.len() <= 128
                    && code.bytes().all(|byte| {
                        byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_'
                    })
            })
            .unwrap_or("MAINTENANCE_ADMISSION_DENIED");
        let code = match code {
            "METHOD_NOT_FOUND" | "PROTOCOL_VERSION_UNSUPPORTED" => {
                "MAINTENANCE_ADMISSION_UNAVAILABLE"
            }
            code => code,
        };
        return Err(PackageRuntimeError::new(
            code,
            "maintenance broker rejected the binding operation",
        ));
    }
    response.get("result").ok_or_else(|| {
        PackageRuntimeError::new(
            "MAINTENANCE_ADMISSION_INVALID",
            "maintenance broker response omitted its result",
        )
    })
}

fn peer_identity(stream: &UnixStream) -> Result<PeerIdentity, PackageRuntimeError> {
    let credentials = getsockopt(stream, PeerCredentials).map_err(|_| {
        PackageRuntimeError::new(
            "CONTROL_PEER_UNKNOWN",
            "could not authenticate Unix socket peer credentials",
        )
    })?;
    Ok(PeerIdentity {
        uid: credentials.uid(),
        gid: credentials.gid(),
    })
}

fn read_bounded_line(stream: &mut UnixStream, line: &mut Vec<u8>) -> io::Result<()> {
    let mut chunk = [0u8; 4096];
    loop {
        let count = stream.read(&mut chunk)?;
        if count == 0 {
            return if line.is_empty() {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "empty request",
                ))
            } else {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "missing newline",
                ))
            };
        }
        let newline = chunk[..count].iter().position(|byte| *byte == b'\n');
        let accepted = newline.map_or(count, |position| position + 1);
        if line.len().saturating_add(accepted) > MAX_CONTROL_LINE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "request too large",
            ));
        }
        line.extend_from_slice(&chunk[..accepted]);
        if newline.is_some() {
            if line.last() != Some(&b'\n') {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid request line",
                ));
            }
            line.pop();
            return Ok(());
        }
    }
}

fn request_id_from_line(line: &[u8]) -> String {
    serde_json::from_slice::<Value>(line)
        .ok()
        .and_then(|value| {
            value
                .get("request_id")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .filter(|request_id| request_id.len() <= MAX_REQUEST_ID_BYTES)
        .unwrap_or_default()
}

fn write_response(
    stream: &mut UnixStream,
    response: &ControlResponse,
) -> Result<(), PackageRuntimeError> {
    serde_json::to_writer(&mut *stream, response).map_err(|_| {
        PackageRuntimeError::new(
            "CONTROL_RESPONSE_FAILED",
            "could not encode control response",
        )
    })?;
    stream.write_all(b"\n").map_err(|_| {
        PackageRuntimeError::new(
            "CONTROL_RESPONSE_FAILED",
            "could not write control response",
        )
    })?;
    stream.flush().map_err(|_| {
        PackageRuntimeError::new(
            "CONTROL_RESPONSE_FAILED",
            "could not write control response",
        )
    })
}

fn error_response(request_id: String, code: &str, message: &str) -> ControlResponse {
    failure_response(request_id, PackageRuntimeError::new(code, message))
}

fn policy_error() -> PackageRuntimeError {
    PackageRuntimeError::new(
        "CONTROL_SOURCE_POLICY_INVALID",
        "package runtime source policy is missing, unsafe, or invalid",
    )
}

fn prepare_socket_parent(socket_path: &Path) -> Result<(), PackageRuntimeError> {
    let parent = socket_path.parent().ok_or_else(|| {
        PackageRuntimeError::new(
            "CONTROL_SOCKET_PATH_INVALID",
            "control socket path has no parent directory",
        )
    })?;
    let metadata = fs::symlink_metadata(parent).map_err(|_| {
        PackageRuntimeError::new(
            "CONTROL_SOCKET_DIRECTORY_INVALID",
            "control socket parent directory is unavailable",
        )
    })?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || (metadata.uid() != 0 && metadata.uid() != nix::unistd::geteuid().as_raw())
        || metadata.permissions().mode() & 0o022 != 0
    {
        return Err(PackageRuntimeError::new(
            "CONTROL_SOCKET_DIRECTORY_INVALID",
            "control socket parent directory has unsafe ownership or permissions",
        ));
    }
    Ok(())
}

fn remove_stale_socket(socket_path: &Path) -> Result<(), PackageRuntimeError> {
    let metadata = match fs::symlink_metadata(socket_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(_) => {
            return Err(PackageRuntimeError::new(
                "CONTROL_SOCKET_PATH_INVALID",
                "existing control socket path could not be inspected",
            ));
        }
    };
    if metadata.file_type().is_symlink()
        || !metadata.file_type().is_socket()
        || (metadata.uid() != 0 && metadata.uid() != nix::unistd::geteuid().as_raw())
    {
        return Err(PackageRuntimeError::new(
            "CONTROL_SOCKET_PATH_INVALID",
            "existing control socket path is not a trusted socket",
        ));
    }
    if UnixStream::connect(socket_path).is_ok() {
        return Err(PackageRuntimeError::new(
            "CONTROL_SOCKET_IN_USE",
            "another package runtime control server is already listening",
        ));
    }
    fs::remove_file(socket_path).map_err(|_| {
        PackageRuntimeError::new(
            "CONTROL_SOCKET_PATH_INVALID",
            "stale control socket could not be removed safely",
        )
    })
}

#[cfg(test)]
fn clone_policy(policy: &PolicySnapshot) -> PolicySnapshot {
    PolicySnapshot {
        generation: policy.generation,
        sources: policy
            .sources
            .iter()
            .map(|source| ValidatedSourcePolicy {
                source_id: source.source_id.clone(),
                uid: source.uid,
                gid: source.gid,
                token_digest: source.token_digest,
                bindings: source
                    .bindings
                    .iter()
                    .map(|binding| ValidatedBindingPolicy {
                        binding_id: binding.binding_id.clone(),
                        package_id: binding.package_id.clone(),
                        installation_ids: binding.installation_ids.clone(),
                        operations: binding.operations.clone(),
                    })
                    .collect(),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{BufRead, BufReader, Cursor, Read},
        os::unix::net::{UnixListener, UnixStream},
        sync::{Arc, Barrier},
        thread,
    };

    use serde_json::json;
    use sha2::{Digest, Sha256};
    use tempfile::tempdir;

    use crate::{
        ArtifactDigest, DependencyPreparationEvidence, DependencyPreparer,
        FilesystemPackageRuntime, PackageRuntimeError, ProcessPluginServiceSupervisor,
        ServiceActivationOptions,
    };

    use super::*;

    const SOURCE_ID: &str = "test-product";
    const SOURCE_TOKEN: &str = "test-source-token";
    const BINDING_ID: &str = "binding-main";
    const INSTALLATION_ID: &str = "installation-main";
    const TEST_SOURCE_UID: u32 = 42;
    const TEST_SOURCE_GID: u32 = 43;

    struct TestDependencyPreparer;

    impl DependencyPreparer for TestDependencyPreparer {
        fn prepare(
            &self,
            _package_root: &Path,
            _runtime_root: &Path,
            _lock_digest: &ArtifactDigest,
        ) -> Result<DependencyPreparationEvidence, PackageRuntimeError> {
            Err(PackageRuntimeError::new(
                "TEST_PREPARER_NOT_USED",
                "the socket transport test does not install packages",
            ))
        }
    }

    fn test_policy(uid: u32, gid: u32) -> PolicySnapshot {
        let token_digest = Sha256::digest(SOURCE_TOKEN.as_bytes());
        let token_digest = token_digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        parse_policy(
            &serde_json::to_vec(&json!({
                "schema_version": 1,
                "generation": 7,
                "sources": [{
                    "source_id": SOURCE_ID,
                    "uid": uid,
                    "gid": gid,
                    "source_token_sha256": token_digest,
                    "bindings": [{
                        "binding_id": BINDING_ID,
                        "package_id": "com.cyrene.test",
                        "installation_ids": [INSTALLATION_ID],
                        "operations": ["activate", "deactivate", "runtime_status", "get_installation"]
                    }]
                }]
            }))
            .unwrap(),
        )
        .unwrap()
    }

    fn test_peer() -> PeerIdentity {
        PeerIdentity {
            uid: TEST_SOURCE_UID,
            gid: TEST_SOURCE_GID,
        }
    }

    fn server(policy: PolicySnapshot) -> PackageRuntimeSocketServer {
        let directory = tempdir().unwrap();
        let runtime = FilesystemPackageRuntime::open(
            directory.path().join("runtime"),
            Arc::new(TestDependencyPreparer),
            Box::new(ProcessPluginServiceSupervisor::default()),
            ServiceActivationOptions::default(),
        )
        .unwrap();
        PackageRuntimeSocketServer {
            control: Arc::new(PackageRuntimeControlServer::new(runtime)),
            policy: SourcePolicyStore::memory(policy),
            socket_path: directory.path().join("control.sock"),
            broker_socket_path: directory.path().join("broker.sock"),
            active_connections: Arc::new(AtomicUsize::new(0)),
            binding_operation_lock: Mutex::new(()),
            _test_directory: Some(directory),
        }
    }

    fn invoke(server: &PackageRuntimeSocketServer, request: Value, peer: PeerIdentity) -> Value {
        let (mut client, mut accepted) = UnixStream::pair().unwrap();
        let handler = thread::scope(|scope| {
            let task = scope.spawn(|| server.handle_stream_for_test(&mut accepted, peer));
            serde_json::to_writer(&mut client, &request).unwrap();
            client.write_all(b"\n").unwrap();
            let mut reader = BufReader::new(client);
            let mut response = String::new();
            reader.read_line(&mut response).unwrap();
            let handler = task.join().unwrap();
            (handler, response)
        });
        handler.0.unwrap();
        serde_json::from_str(&handler.1).unwrap()
    }

    impl PackageRuntimeSocketServer {
        fn handle_stream_for_test(
            &self,
            stream: &mut UnixStream,
            peer: PeerIdentity,
        ) -> Result<(), PackageRuntimeError> {
            stream.set_read_timeout(Some(READ_TIMEOUT)).unwrap();
            stream.set_write_timeout(Some(WRITE_TIMEOUT)).unwrap();
            let mut line = Vec::new();
            read_bounded_line(stream, &mut line).unwrap();
            let request = match parse_authenticated_request(&line) {
                Ok(request) => request,
                Err((request_id, code, message)) => {
                    return write_response(stream, &error_response(request_id, code, message));
                }
            };
            let response = self.process_authenticated_request(request, peer);
            write_response(stream, &response)
        }
    }

    fn request(operation: &str, generation: Value, auth: Value) -> Value {
        let mut request = json!({
            "request_id": "req-1",
            "operation": operation,
            "auth": auth,
        });
        if !generation.is_null() {
            request["catalog_generation"] = generation;
        }
        request
    }

    fn scoped_request(operation: &str, generation: Value, auth: Value) -> Value {
        let mut request = request(operation, generation, auth);
        request["binding_id"] = json!(BINDING_ID);
        request
    }

    fn source_auth() -> Value {
        json!({"source_id": SOURCE_ID, "source_token": SOURCE_TOKEN})
    }

    fn spawn_fake_broker(path: PathBuf, responses: Vec<Value>) -> thread::JoinHandle<Vec<Value>> {
        let listener = UnixListener::bind(path).unwrap();
        listener.set_nonblocking(true).unwrap();
        thread::spawn(move || {
            let mut requests = Vec::new();
            for response in responses {
                let deadline = std::time::Instant::now() + Duration::from_secs(10);
                let (mut stream, _) = loop {
                    match listener.accept() {
                        Ok(connection) => break connection,
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            assert!(
                                std::time::Instant::now() < deadline,
                                "test broker did not receive the expected request"
                            );
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("test broker accept failed: {error}"),
                    }
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                let mut line = Vec::new();
                let mut byte = [0u8; 1];
                loop {
                    assert_eq!(stream.read(&mut byte).unwrap(), 1);
                    if byte[0] == b'\n' {
                        break;
                    }
                    line.push(byte[0]);
                    assert!(line.len() <= MAX_CONTROL_LINE_BYTES);
                }
                requests.push(serde_json::from_slice::<Value>(&line).unwrap());
                serde_json::to_writer(&mut stream, &response).unwrap();
                stream.write_all(b"\n").unwrap();
                stream.flush().unwrap();
            }
            requests
        })
    }

    fn broker_health_response(request_id: &str, generation: u64) -> Value {
        json!({
            "request_id": request_id,
            "result": {
                "status": "SERVING",
                "protocol_version": BROKER_PROTOCOL_VERSION,
                "capabilities": [
                    BROKER_STATE_V2_CAPABILITY,
                    BINDING_OPERATIONS_PROTOCOL_VERSION,
                ],
                "catalog_generation": generation,
                "gate_generation": 4,
            }
        })
    }

    #[test]
    fn authority_returns_the_current_generation_and_existing_protocol() {
        let server = server(test_policy(TEST_SOURCE_UID, TEST_SOURCE_GID));
        let response = invoke(
            &server,
            request("authority", Value::Null, source_auth()),
            test_peer(),
        );
        assert_eq!(response["request_id"], "req-1");
        assert_eq!(response["ok"], true);
        assert_eq!(response["result"]["authority"], "platform_package_runtime");
        assert_eq!(
            response["result"]["protocol_version"],
            "cy-package-runtime.control.v1"
        );
        assert_eq!(response["result"]["catalog_generation"], 7);
        assert_eq!(response["result"]["capabilities"], json!([]));
    }

    #[test]
    fn source_and_generation_mismatches_are_denied_without_echoing_credentials() {
        let peer = test_peer();
        let server = server(test_policy(peer.uid, peer.gid));
        let mut bad_token = source_auth();
        bad_token["source_token"] = json!("wrong-token-secret");
        for (request, expected_code) in [
            (
                request("authority", Value::Null, bad_token),
                "CONTROL_AUTH_INVALID",
            ),
            (
                scoped_request("runtime_status", json!(6), source_auth()),
                "CONTROL_GENERATION_STALE",
            ),
            (
                scoped_request("runtime_status", Value::Null, source_auth()),
                "CONTROL_GENERATION_STALE",
            ),
        ] {
            let response = invoke(&server, request, peer);
            assert_eq!(response["ok"], false);
            assert_eq!(response["error"]["code"], expected_code);
            let encoded = response.to_string();
            assert!(!encoded.contains("test-source-token"));
            assert!(!encoded.contains("wrong-token-secret"));
        }
    }

    #[test]
    fn wrong_peer_and_cross_binding_scope_are_denied() {
        let peer = test_peer();
        let server = server(test_policy(peer.uid, peer.gid));
        let wrong_peer = PeerIdentity {
            uid: peer.uid.saturating_add(1),
            gid: peer.gid,
        };
        let response = invoke(
            &server,
            request("authority", Value::Null, source_auth()),
            wrong_peer,
        );
        assert_eq!(response["error"]["code"], "CONTROL_PEER_MISMATCH");

        let mut cross_binding = request("runtime_status", json!(7), source_auth());
        cross_binding["binding_id"] = json!("another-product-binding");
        let response = invoke(&server, cross_binding, peer);
        assert_eq!(response["error"]["code"], "CONTROL_SCOPE_DENIED");
    }

    #[test]
    fn uninstalled_product_scope_and_root_mutation_fail_closed() {
        let peer = test_peer();
        let server = server(test_policy(peer.uid, peer.gid));
        let mut activate = request("activate", json!(7), source_auth());
        activate["binding_id"] = json!(BINDING_ID);
        activate["installation_id"] = json!(INSTALLATION_ID);
        let response = invoke(&server, activate, peer);
        // The policy entry alone is insufficient: package and activation state
        // must exist before the operation can reach the maintenance gate.
        assert_eq!(response["error"]["code"], "CONTROL_SCOPE_DENIED");

        let mut root_deactivate = request("deactivate", json!(7), json!({}));
        root_deactivate["binding_id"] = json!(BINDING_ID);
        let response = invoke(&server, root_deactivate, PeerIdentity { uid: 0, gid: 0 });
        assert_eq!(response["error"]["code"], "MAINTENANCE_ADMISSION_REQUIRED");

        let mut root_install = request("install_offline", json!(7), json!({}));
        root_install["package_id"] = json!("com.cyrene.test");
        root_install["package_version"] = json!("1.0.0");
        let response = invoke(&server, root_install, PeerIdentity { uid: 0, gid: 0 });
        assert_eq!(response["error"]["code"], "MAINTENANCE_ADMISSION_REQUIRED");

        let response = invoke(
            &server,
            request("cleanup", json!(7), json!({})),
            PeerIdentity { uid: 0, gid: 0 },
        );
        assert_eq!(response["error"]["code"], "MAINTENANCE_ADMISSION_REQUIRED");
        assert_eq!(server.active_connections.load(Ordering::Acquire), 0);
    }

    #[test]
    fn malformed_policy_scopes_and_request_limits_fail_closed() {
        let duplicate_source = json!({
            "schema_version": 1,
            "generation": 7,
            "sources": []
        });
        assert!(parse_policy(&serde_json::to_vec(&duplicate_source).unwrap()).is_ok());
        let duplicated_binding = json!({
            "schema_version": 1,
            "generation": 7,
            "sources": [
                {
                    "source_id": "product-one",
                    "uid": 1001,
                    "gid": 1001,
                    "source_token_sha256": "0".repeat(64),
                    "bindings": [{
                        "binding_id": BINDING_ID,
                        "package_id": "com.cyrene.test",
                        "installation_ids": [INSTALLATION_ID],
                        "operations": ["runtime_status"]
                    }]
                },
                {
                    "source_id": "product-two",
                    "uid": 1002,
                    "gid": 1002,
                    "source_token_sha256": "1".repeat(64),
                    "bindings": [{
                        "binding_id": BINDING_ID,
                        "package_id": "com.cyrene.test",
                        "installation_ids": [INSTALLATION_ID],
                        "operations": ["runtime_status"]
                    }]
                }
            ]
        });
        assert!(parse_policy(&serde_json::to_vec(&duplicated_binding).unwrap()).is_err());
        let invalid_operation = json!({
            "schema_version": 1,
            "generation": 7,
            "sources": [{
                "source_id": SOURCE_ID,
                "uid": 1000,
                "gid": 1000,
                "source_token_sha256": "0".repeat(64),
                "bindings": [{
                    "binding_id": BINDING_ID,
                    "package_id": "com.cyrene.test",
                    "installation_ids": [INSTALLATION_ID],
                    "operations": ["activate", "install"]
                }]
            }]
        });
        assert!(parse_policy(&serde_json::to_vec(&invalid_operation).unwrap()).is_err());

        let mut extra_field_request = request("authority", json!(7), source_auth());
        extra_field_request["unexpected"] = json!("ignored? no");
        assert_eq!(
            parse_authenticated_request(&serde_json::to_vec(&extra_field_request).unwrap())
                .unwrap_err()
                .1,
            "CONTROL_REQUEST_INVALID"
        );
        for generation in [json!(0), json!(1.5), json!("7")] {
            let malformed = request("authority", generation, source_auth());
            assert_eq!(
                parse_authenticated_request(&serde_json::to_vec(&malformed).unwrap())
                    .unwrap_err()
                    .1,
                "CONTROL_GENERATION_INVALID"
            );
        }

        let environment = (0..MAX_ENVIRONMENT_ENTRIES + 1)
            .map(|index| (format!("KEY_{index}"), "value".to_string()))
            .collect();
        assert_eq!(
            validate_request_limits(&ControlCommand::Activate {
                binding_id: BINDING_ID.to_string(),
                installation_id: INSTALLATION_ID.to_string(),
                environment,
            })
            .unwrap_err()
            .0,
            "CONTROL_REQUEST_TOO_LARGE"
        );
    }

    #[test]
    fn concurrent_mutation_attempts_are_denied_without_runtime_state_change() {
        let peer = test_peer();
        let server = Arc::new(server(test_policy(peer.uid, peer.gid)));
        let barrier = Arc::new(Barrier::new(8));
        let mut workers = Vec::new();
        for index in 0..8 {
            let server = Arc::clone(&server);
            let barrier = Arc::clone(&barrier);
            workers.push(thread::spawn(move || {
                let mut request = request("deactivate", json!(7), source_auth());
                request["request_id"] = json!(format!("req-{index}"));
                request["binding_id"] = json!(BINDING_ID);
                barrier.wait();
                invoke(&server, request, peer)
            }));
        }
        for worker in workers {
            let response = worker.join().unwrap();
            assert_eq!(response["error"]["code"], "CONTROL_SCOPE_DENIED");
        }
        assert_eq!(server.active_connections.load(Ordering::Acquire), 0);
    }

    #[test]
    fn unix_peer_credentials_are_read_from_the_kernel() {
        let (_client, accepted) = UnixStream::pair().unwrap();
        let peer = peer_identity(&accepted).unwrap();
        assert_eq!(peer.uid, nix::unistd::geteuid().as_raw());
        assert_eq!(peer.gid, nix::unistd::getegid().as_raw());
    }

    #[test]
    fn request_frames_enforce_size_and_read_time_limits() {
        let (mut reader, mut writer) = UnixStream::pair().unwrap();
        let writer_thread = thread::spawn(move || {
            let oversized = vec![b'x'; MAX_CONTROL_LINE_BYTES + 1];
            let _ = writer.write_all(&oversized);
        });
        let mut line = Vec::new();
        let error = read_bounded_line(&mut reader, &mut line).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        writer_thread.join().unwrap();

        let (mut reader, _writer) = UnixStream::pair().unwrap();
        reader
            .set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        let mut line = Vec::new();
        let error = read_bounded_line(&mut reader, &mut line).unwrap_err();
        assert!(matches!(
            error.kind(),
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
        ));
    }

    #[test]
    fn broker_health_requires_exact_protocol_capability_and_generation() {
        let peer = test_peer();
        let server = server(test_policy(peer.uid, peer.gid));
        let directory = tempdir().unwrap();
        let socket = directory.path().join("broker.sock");
        let responses = vec![
            broker_health_response("health-1", 7),
            json!({
                "request_id": "health-2",
                "result": {
                    "status": "SERVING",
                    "protocol_version": "cyrene.runtime-maintenance.broker.v0",
                    "capabilities": [
                        BROKER_STATE_V2_CAPABILITY,
                        BINDING_OPERATIONS_PROTOCOL_VERSION,
                    ],
                    "catalog_generation": 7,
                }
            }),
            json!({
                "request_id": "health-3",
                "result": {
                    "status": "SERVING",
                    "protocol_version": BROKER_PROTOCOL_VERSION,
                    "capabilities": [BINDING_OPERATIONS_PROTOCOL_VERSION],
                    "catalog_generation": 7,
                }
            }),
            broker_health_response("health-4", 8),
            json!({
                "request_id": "health-5",
                "result": {
                    "status": "SERVING",
                    "protocol_version": BROKER_PROTOCOL_VERSION,
                    "capabilities": [
                        BROKER_STATE_V2_CAPABILITY,
                        BINDING_OPERATIONS_PROTOCOL_VERSION,
                        "cyrene.runtime-maintenance.future.v1",
                    ],
                    "catalog_generation": 7,
                }
            }),
            json!({
                "request_id": "health-6",
                "result": {
                    "status": "SERVING",
                    "protocol_version": BROKER_PROTOCOL_VERSION,
                    "capabilities": [
                        BROKER_STATE_V2_CAPABILITY,
                        BINDING_OPERATIONS_PROTOCOL_VERSION,
                        BINDING_OPERATIONS_PROTOCOL_VERSION,
                    ],
                    "catalog_generation": 7,
                }
            }),
        ];
        let broker = spawn_fake_broker(socket.clone(), responses);
        let mut server = server;
        server.broker_socket_path = socket;
        assert!(server.broker_advertises_binding_admission("health-1", 7));
        assert!(!server.broker_advertises_binding_admission("health-2", 7));
        assert!(!server.broker_advertises_binding_admission("health-3", 7));
        assert!(!server.broker_advertises_binding_admission("health-4", 7));
        assert!(server.broker_advertises_binding_admission("health-5", 7));
        assert!(!server.broker_advertises_binding_admission("health-6", 7));
        let requests = broker.join().unwrap();
        assert!(requests.iter().all(|request| {
            request["method"] == "Health"
                && request["params"] == json!({})
                && request.get("auth").is_none()
                && request.get("protocol_version").is_none()
        }));
    }

    #[test]
    fn authority_advertises_admission_only_after_exact_broker_health() {
        let peer = test_peer();
        let mut server = server(test_policy(peer.uid, peer.gid));
        let directory = tempdir().unwrap();
        let socket = directory.path().join("broker.sock");
        let broker = spawn_fake_broker(socket.clone(), vec![broker_health_response("req-1", 7)]);
        server.broker_socket_path = socket;
        let response = invoke(
            &server,
            request("authority", Value::Null, source_auth()),
            peer,
        );
        assert_eq!(response["ok"], true);
        assert_eq!(
            response["result"]["capabilities"],
            json!([PACKAGE_ADMISSION_CAPABILITY])
        );
        assert_eq!(broker.join().unwrap()[0]["method"], "Health");
    }

    #[test]
    fn broker_admission_forwards_exact_source_scope_and_preserves_replay_flags() {
        let peer = test_peer();
        let mut server = server(test_policy(peer.uid, peer.gid));
        let directory = tempdir().unwrap();
        let socket = directory.path().join("broker.sock");
        let scope = BindingOperationScope {
            binding_id: BINDING_ID.to_string(),
            package_id: "com.cyrene.test".to_string(),
            installation_id: INSTALLATION_ID.to_string(),
            operation: "recover".to_string(),
        };
        let admission = json!({
            "request_id": "admit-1",
            "source_id": SOURCE_ID,
            "scope": scope,
            "operation_token": "opaque-operation-token",
            "catalog_generation": 7,
            "gate_generation": 4,
            "already_in_flight": true,
            "already_completed": false,
        });
        let responses = vec![
            broker_health_response("admit-1", 7),
            json!({"request_id": "admit-1", "result": admission.clone()}),
        ];
        let broker = spawn_fake_broker(socket.clone(), responses);
        server.broker_socket_path = socket;
        let returned = server
            .admit_binding_operation("admit-1", SOURCE_ID, SOURCE_TOKEN, 7, &scope)
            .unwrap();
        assert!(returned.already_in_flight);
        assert!(!returned.already_completed);
        let requests = broker.join().unwrap();
        assert_eq!(requests[0]["method"], "Health");
        assert_eq!(requests[1]["method"], "AdmitBindingOperation");
        assert_eq!(
            requests[1]["protocol_version"],
            BINDING_OPERATIONS_PROTOCOL_VERSION
        );
        assert_eq!(requests[1]["auth"]["source_id"], SOURCE_ID);
        assert_eq!(requests[1]["auth"]["source_token"], SOURCE_TOKEN);
        assert_eq!(requests[1]["params"]["expected_catalog_generation"], 7);
        assert_eq!(requests[1]["params"]["operation"], "recover");
        assert_eq!(requests[1]["params"]["package_id"], "com.cyrene.test");
        assert_eq!(requests[1]["params"]["installation_id"], INSTALLATION_ID);
    }

    #[test]
    fn in_flight_admission_returns_lease_without_dispatching_again() {
        let peer = test_peer();
        let server = server(test_policy(peer.uid, peer.gid));
        let scope = BindingOperationScope {
            binding_id: BINDING_ID.to_string(),
            package_id: "com.cyrene.test".to_string(),
            installation_id: INSTALLATION_ID.to_string(),
            operation: "deactivate".to_string(),
        };
        let response = server.respond_to_admitted_mutation(
            AuthenticatedRequest {
                control: ControlRequest {
                    request_id: "pending-1".to_string(),
                    command: ControlCommand::Deactivate {
                        binding_id: BINDING_ID.to_string(),
                    },
                },
                auth: RequestAuth {
                    source_id: Some(SOURCE_ID.to_string()),
                    source_token: Some(SOURCE_TOKEN.to_string()),
                },
                generation: Some(7),
            },
            BrokerBindingOperationAdmission {
                request_id: "pending-1".to_string(),
                source_id: SOURCE_ID.to_string(),
                scope,
                operation_token: "pending-operation-token".to_string(),
                catalog_generation: 7,
                gate_generation: 8,
                already_in_flight: true,
                already_completed: false,
            },
        );
        assert!(!response.ok);
        let error = response.error.unwrap();
        assert_eq!(error.code, "BINDING_OPERATION_PENDING");
        assert!(
            error.binding_operation.as_ref().unwrap()["already_in_flight"]
                .as_bool()
                .unwrap()
        );
        assert_eq!(
            error.binding_operation.as_ref().unwrap()["operation_token"],
            "pending-operation-token"
        );
        assert!(server.control.binding_package_scope(BINDING_ID).is_err());
    }

    #[test]
    fn broker_generation_and_returned_scope_mismatches_fail_closed() {
        let peer = test_peer();
        let mut generation_server = server(test_policy(peer.uid, peer.gid));
        let directory = tempdir().unwrap();
        let socket = directory.path().join("broker.sock");
        let broker = spawn_fake_broker(
            socket.clone(),
            vec![broker_health_response("generation-1", 8)],
        );
        generation_server.broker_socket_path = socket;
        let scope = BindingOperationScope {
            binding_id: BINDING_ID.to_string(),
            package_id: "com.cyrene.test".to_string(),
            installation_id: INSTALLATION_ID.to_string(),
            operation: "activate".to_string(),
        };
        let error = generation_server
            .admit_binding_operation("generation-1", SOURCE_ID, SOURCE_TOKEN, 7, &scope)
            .unwrap_err();
        assert_eq!(error.code, "MAINTENANCE_ADMISSION_UNAVAILABLE");
        assert_eq!(broker.join().unwrap().len(), 1);

        let mut scope_server = server(test_policy(peer.uid, peer.gid));
        let socket = directory.path().join("broker-mismatch.sock");
        let mismatch = json!({
            "request_id": "scope-1",
            "source_id": SOURCE_ID,
            "scope": {
                "binding_id": "another-binding",
                "package_id": "com.cyrene.test",
                "installation_id": INSTALLATION_ID,
                "operation": "activate",
            },
            "operation_token": "opaque-operation-token",
            "catalog_generation": 7,
            "gate_generation": 4,
            "already_in_flight": false,
            "already_completed": false,
        });
        let broker = spawn_fake_broker(
            socket.clone(),
            vec![
                broker_health_response("scope-1", 7),
                json!({"request_id": "scope-1", "result": mismatch}),
            ],
        );
        scope_server.broker_socket_path = socket;
        let error = scope_server
            .admit_binding_operation("scope-1", SOURCE_ID, SOURCE_TOKEN, 7, &scope)
            .unwrap_err();
        assert_eq!(error.code, "MAINTENANCE_ADMISSION_INVALID");
        assert_eq!(broker.join().unwrap().len(), 2);
    }

    #[test]
    fn legacy_stdio_authority_wire_remains_unchanged() {
        let peer = test_peer();
        let server = server(test_policy(peer.uid, peer.gid));
        let mut output = Vec::new();
        server
            .control
            .run(
                Cursor::new(b"{\"request_id\":\"stdio-1\",\"operation\":\"authority\"}\n"),
                &mut output,
            )
            .unwrap();
        let response: Value = serde_json::from_slice(output.strip_suffix(b"\n").unwrap()).unwrap();
        assert_eq!(response["request_id"], "stdio-1");
        assert_eq!(response["ok"], true);
        assert_eq!(response["result"]["authority"], "platform_package_runtime");
        assert_eq!(
            response["result"]["protocol_version"],
            "cy-package-runtime.control.v1"
        );
        assert!(response["result"].get("capabilities").is_none());
    }
}
