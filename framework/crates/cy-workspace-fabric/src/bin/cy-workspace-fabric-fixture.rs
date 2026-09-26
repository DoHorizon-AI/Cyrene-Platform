//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 cy-workspace-fabric-fixture.rs                                  │
//! │  Module: cy_workspace_fabric::fixture                               │
//! │  Role: Real relay, connector, and reference frontend acceptance.    │
//! │                                                                     │
//! │  模块职责：真实 Relay、Workspace Connector 与参考前端验收。              │
//! └─────────────────────────────────────────────────────────────────────┘
//!
//! # STDOUT CONTRACT
//!
//! This fixture writes acceptance results to stdout as `KEY=VALUE` lines
//! (for example `WORKSPACE_DISCOVERED_BY_IDENTITY=PASS`). Acceptance harnesses
//! parse these lines, so anything else printed to stdout corrupts the result.
//!
//! - `println!` is reserved for `KEY=VALUE` result lines only.
//! - All diagnostics and progress output MUST use `eprintln!`.
//! - Failure details already reach stderr correctly: `main` returns `Result`,
//!   so the runtime prints `Error: ...` to stderr on failure.
//! - Never route tracing/log output to stdout here.

use std::collections::BTreeSet;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cy_proto::core_v1::ConnectivityMode;
use cy_proto::google::rpc::Status as RpcStatus;
use cy_proto::semantic_v1::Identity as OperationIdentity;
use cy_proto::workspace_v1::workspace_api_request;
use cy_proto::workspace_v1::workspace_api_response;
use cy_proto::workspace_v1::workspace_direct_service_server::WorkspaceDirectServiceServer;
use cy_proto::workspace_v1::workspace_relay_service_server::WorkspaceRelayServiceServer;
use cy_proto::workspace_v1::{
    DeviceEnrollmentRef, GetWorkspaceOperationRequest, RelayHello, RelayParticipantRole,
    StartWorkspaceOperationRequest, UserIdentityRef, WorkspaceApiRequest, WorkspaceApiResponse,
    WorkspaceConnectionCandidate, WorkspaceConnectionDescriptor, WorkspaceDirectRequest,
    WorkspaceOperationState, WorkspaceOperationView,
};
use cy_workspace_fabric::{
    connect_discovered_workspace, connect_relay_session, DevelopmentSessionVerifier,
    DirectWorkspaceServer, InMemoryWorkspaceDirectory, RelayClientConfig, RelaySessionClaims,
    SessionPrincipal, WorkspaceApi, WorkspaceConnection, WorkspaceMembership, WorkspaceRelay,
};
use tonic::transport::{Certificate, Identity, Server, ServerTlsConfig};
use tonic::Code;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    match env::args().nth(1).as_deref() {
        Some("relay") => run_relay().await,
        Some("connector") => run_connector().await,
        Some("frontend-start") => run_frontend(true).await,
        Some("frontend-observe") => run_frontend(false).await,
        Some("frontend-direct") => run_frontend_direct().await,
        Some("frontend-fallback") => run_frontend_fallback().await,
        _ => Err("expected role: relay | connector | frontend-start | frontend-observe | frontend-direct | frontend-fallback".into()),
    }
}

async fn run_relay() -> Result<(), Box<dyn std::error::Error>> {
    let bind: SocketAddr = required("CYRENE_WORKSPACE_RELAY_BIND")?.parse()?;
    let workspace_id = required("CYRENE_WORKSPACE_ID")?;
    let organization_id = required("CYRENE_ORGANIZATION_ID")?;
    let user = fixture_user();
    let expires_at_unix_ms = now_unix_ms().saturating_add(10 * 60 * 1000);
    let descriptor = WorkspaceConnectionDescriptor {
        descriptor_version: "cyrene.workspace.connection.v1".to_string(),
        workspace_id: workspace_id.clone(),
        organization_id: organization_id.clone(),
        display_name: "Cyrene Fixture Workspace".to_string(),
        candidates: vec![
            WorkspaceConnectionCandidate {
                mode: ConnectivityMode::Local as i32,
                provider_id: "cyrene.local.v1".to_string(),
                connection_uri: "https://workspace.local".to_string(),
                server_name: "workspace.local".to_string(),
                priority: 10,
                routing_hint: Vec::new(),
            },
            WorkspaceConnectionCandidate {
                mode: ConnectivityMode::LanDirect as i32,
                provider_id: "cyrene.direct.fixture.v1".to_string(),
                connection_uri: required("CYRENE_WORKSPACE_DESCRIPTOR_DIRECT_URI")?,
                server_name: required("CYRENE_WORKSPACE_DIRECT_SERVER_NAME")?,
                priority: 15,
                routing_hint: Vec::new(),
            },
            WorkspaceConnectionCandidate {
                mode: ConnectivityMode::Relay as i32,
                provider_id: "cyrene.relay.fixture.v1".to_string(),
                connection_uri: required("CYRENE_WORKSPACE_DESCRIPTOR_RELAY_URI")?,
                server_name: required("CYRENE_WORKSPACE_RELAY_SERVER_NAME")?,
                priority: 20,
                routing_hint: workspace_id.as_bytes().to_vec(),
            },
        ],
        expires_at: Some(timestamp_from_ms(expires_at_unix_ms)),
    };
    let directory = Arc::new(InMemoryWorkspaceDirectory::new(
        vec![WorkspaceMembership {
            user: user.clone(),
            organization_id: organization_id.clone(),
            workspace_id: workspace_id.clone(),
            roles: BTreeSet::from(["workspace.member".to_string()]),
        }],
        vec![descriptor],
    )?);
    let authenticator = Arc::new(DevelopmentSessionVerifier::new([
        (
            required("CYRENE_FRONTEND_SESSION_CREDENTIAL")?,
            RelaySessionClaims {
                principal: SessionPrincipal::User(user),
                organization_id: organization_id.clone(),
                workspace_id: String::new(),
                expires_at_unix_ms,
            },
        ),
        (
            required("CYRENE_WORKSPACE_SESSION_CREDENTIAL")?,
            RelaySessionClaims {
                principal: SessionPrincipal::WorkspaceDevice {
                    workspace_id,
                    device_id: required("CYRENE_WORKSPACE_DEVICE_ID")?,
                },
                organization_id,
                workspace_id: required("CYRENE_WORKSPACE_ID")?,
                expires_at_unix_ms,
            },
        ),
    ]));
    let relay = WorkspaceRelay::new(directory, authenticator);
    trace(
        Path::new(&required("CYRENE_WORKSPACE_RELAY_TRACE")?),
        &format!("RELAY_STARTED bind={bind}"),
    );
    let tls = ServerTlsConfig::new()
        .identity(Identity::from_pem(
            fs::read(required("CYRENE_WORKSPACE_RELAY_SERVER_CERT")?)?,
            fs::read(required("CYRENE_WORKSPACE_RELAY_SERVER_KEY")?)?,
        ))
        .client_ca_root(Certificate::from_pem(fs::read(required(
            "CYRENE_WORKSPACE_RELAY_CLIENT_CA",
        )?)?));
    Server::builder()
        .tls_config(tls)?
        .add_service(WorkspaceRelayServiceServer::new(relay))
        .serve(bind)
        .await?;
    Ok(())
}

async fn run_connector() -> Result<(), Box<dyn std::error::Error>> {
    let workspace_id = required("CYRENE_WORKSPACE_ID")?;
    let organization_id = required("CYRENE_ORGANIZATION_ID")?;
    let trace_path = PathBuf::from(required("CYRENE_WORKSPACE_CONNECTOR_TRACE")?);
    let api = Arc::new(FileWorkspaceApi::new(
        workspace_id.clone(),
        required("CYRENE_WORKSPACE_ARTIFACT_URI")?,
        PathBuf::from(required("CYRENE_WORKSPACE_ASSIGNMENT_TRIGGER")?),
        PathBuf::from(required("CYRENE_RUNTIME_CONTROL_TRACE")?),
        trace_path.clone(),
        required("CYRENE_WORKSPACE_AUTHORITY_INSTANCE_ID")?,
    ));
    let config = relay_client_config()?;
    let hello = RelayHello {
        role: RelayParticipantRole::WorkspaceConnector as i32,
        session_credential: required("CYRENE_WORKSPACE_SESSION_CREDENTIAL")?,
        user: None,
        organization_id,
        workspace_id: workspace_id.clone(),
        device: Some(DeviceEnrollmentRef {
            device_id: required("CYRENE_WORKSPACE_DEVICE_ID")?,
            workspace_id,
            enrollment_state: "approved".to_string(),
        }),
    };
    let direct_server = run_direct_server(api.clone(), trace_path.clone());
    tokio::select! {
        result = direct_server => result,
        _ = async {
            loop {
                match connect_relay_session(&config, hello.clone()).await {
                    Ok(session) => {
                        trace(
                            &trace_path,
                            &format!("WORKSPACE_RELAY_CONNECTED session={}", session.relay_session_id()),
                        );
                        if let Err(error) = session.serve_workspace(api.clone()).await {
                            trace(&trace_path, &format!("WORKSPACE_RELAY_DISCONNECTED reason={error}"));
                        }
                    }
                    Err(error) => trace(
                        &trace_path,
                        &format!("WORKSPACE_RELAY_CONNECT_RETRY reason={error}"),
                    ),
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        } => Ok(()),
    }
}

async fn run_direct_server(
    api: Arc<dyn WorkspaceApi>,
    trace_path: PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    let bind: SocketAddr = required("CYRENE_WORKSPACE_DIRECT_BIND")?.parse()?;
    let workspace_id = required("CYRENE_WORKSPACE_ID")?;
    let organization_id = required("CYRENE_ORGANIZATION_ID")?;
    let user = fixture_user();
    let directory = Arc::new(InMemoryWorkspaceDirectory::new(
        vec![WorkspaceMembership {
            user: user.clone(),
            organization_id: organization_id.clone(),
            workspace_id: workspace_id.clone(),
            roles: BTreeSet::from(["workspace.member".to_string()]),
        }],
        Vec::new(),
    )?);
    let authenticator = Arc::new(DevelopmentSessionVerifier::new([(
        required("CYRENE_FRONTEND_SESSION_CREDENTIAL")?,
        RelaySessionClaims {
            principal: SessionPrincipal::User(user),
            organization_id,
            workspace_id: String::new(),
            expires_at_unix_ms: now_unix_ms().saturating_add(10 * 60 * 1000),
        },
    )]));
    let tls = ServerTlsConfig::new()
        .identity(Identity::from_pem(
            fs::read(required("CYRENE_WORKSPACE_DIRECT_SERVER_CERT")?)?,
            fs::read(required("CYRENE_WORKSPACE_DIRECT_SERVER_KEY")?)?,
        ))
        .client_ca_root(Certificate::from_pem(fs::read(required(
            "CYRENE_WORKSPACE_DIRECT_CLIENT_CA",
        )?)?));
    let direct = DirectWorkspaceServer::new(workspace_id, directory, authenticator, api);
    trace(
        &trace_path,
        &format!("WORKSPACE_DIRECT_STARTED bind={bind}"),
    );
    Server::builder()
        .tls_config(tls)?
        .add_service(WorkspaceDirectServiceServer::new(direct))
        .serve(bind)
        .await?;
    Ok(())
}

async fn run_frontend_direct() -> Result<(), Box<dyn std::error::Error>> {
    let workspace_id = required("CYRENE_WORKSPACE_ID")?;
    let user = fixture_user();
    let hello = RelayHello {
        role: RelayParticipantRole::Frontend as i32,
        session_credential: required("CYRENE_FRONTEND_SESSION_CREDENTIAL")?,
        user: Some(user.clone()),
        organization_id: required("CYRENE_ORGANIZATION_ID")?,
        workspace_id: String::new(),
        device: None,
    };
    let config = relay_client_config()?;
    let mut relay = connect_relay_session(&config, hello.clone()).await?;
    let descriptor = relay
        .discover("discover-direct", user, hello.organization_id.clone())
        .await?
        .into_iter()
        .find(|descriptor| descriptor.workspace_id == workspace_id)
        .ok_or("Workspace was not discovered by identity")?;
    let barrier = PathBuf::from(required("CYRENE_WORKSPACE_DIRECT_BARRIER")?);
    fs::write(&barrier, b"DIRECT_DISCOVERED")?;
    let release = barrier.with_extension("go");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !release.exists() {
        if tokio::time::Instant::now() >= deadline {
            return Err("timed out waiting for Relay shutdown".into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let mut connection = connect_discovered_workspace(&descriptor, hello, &config, relay).await?;
    if connection.mode() != ConnectivityMode::LanDirect {
        return Err("LAN_DIRECT was not selected after Relay shutdown".into());
    }
    let response = connection
        .execute(WorkspaceApiRequest {
            request_id: "direct-get-operation-1".to_string(),
            workspace_id,
            request: Some(workspace_api_request::Request::GetOperation(
                GetWorkspaceOperationRequest {
                    operation: Some(OperationIdentity {
                        id: "operation-1".to_string(),
                        generation: 1,
                    }),
                },
            )),
        })
        .await?;
    let view = operation_view(response)?;
    if view.authority_instance_id != required("CYRENE_WORKSPACE_AUTHORITY_INSTANCE_ID")? {
        return Err("direct route changed Workspace authority".into());
    }
    let WorkspaceConnection::Direct { client, hello, .. } = &mut connection else {
        return Err("direct connection changed mode".into());
    };
    let mut invalid_hello = hello.clone();
    invalid_hello.session_credential = "invalid-session".to_string();
    let denied = client
        .execute(WorkspaceDirectRequest {
            frontend: Some(invalid_hello),
            request: Some(WorkspaceApiRequest {
                request_id: "direct-denied-operation-1".to_string(),
                workspace_id: required("CYRENE_WORKSPACE_ID")?,
                request: Some(workspace_api_request::Request::GetOperation(
                    GetWorkspaceOperationRequest {
                        operation: Some(OperationIdentity {
                            id: "operation-1".to_string(),
                            generation: 1,
                        }),
                    },
                )),
            }),
        })
        .await;
    if !matches!(denied, Err(ref error) if error.code() == Code::Unauthenticated) {
        return Err("direct endpoint accepted an invalid session".into());
    }
    println!("LAN_DIRECT_NO_RELAY=PASS");
    println!("LAN_DIRECT_INVALID_CREDENTIAL_DENIED=PASS");
    println!("WORKSPACE_DIRECT_AUTHORITY_PRESERVED=PASS");
    Ok(())
}

async fn run_frontend_fallback() -> Result<(), Box<dyn std::error::Error>> {
    let workspace_id = required("CYRENE_WORKSPACE_ID")?;
    let user = fixture_user();
    let hello = RelayHello {
        role: RelayParticipantRole::Frontend as i32,
        session_credential: required("CYRENE_FRONTEND_SESSION_CREDENTIAL")?,
        user: Some(user.clone()),
        organization_id: required("CYRENE_ORGANIZATION_ID")?,
        workspace_id: String::new(),
        device: None,
    };
    let config = relay_client_config()?;
    let mut relay = connect_relay_session(&config, hello.clone()).await?;
    let mut descriptor = relay
        .discover("discover-fallback", user, hello.organization_id.clone())
        .await?
        .into_iter()
        .find(|descriptor| descriptor.workspace_id == workspace_id)
        .ok_or("Workspace was not discovered by identity")?;
    let direct = descriptor
        .candidates
        .iter_mut()
        .find(|candidate| candidate.mode == ConnectivityMode::LanDirect as i32)
        .ok_or("LAN_DIRECT candidate is missing")?;
    direct.connection_uri = "https://127.0.0.1:9".to_string();
    let mut connection = connect_discovered_workspace(&descriptor, hello, &config, relay).await?;
    if connection.mode() != ConnectivityMode::Relay {
        return Err("unreachable direct candidate did not fall back to Relay".into());
    }
    let response = connection
        .execute(WorkspaceApiRequest {
            request_id: "fallback-get-operation-1".to_string(),
            workspace_id,
            request: Some(workspace_api_request::Request::GetOperation(
                GetWorkspaceOperationRequest {
                    operation: Some(OperationIdentity {
                        id: "operation-1".to_string(),
                        generation: 1,
                    }),
                },
            )),
        })
        .await?;
    let view = operation_view(response)?;
    if view.authority_instance_id != required("CYRENE_WORKSPACE_AUTHORITY_INSTANCE_ID")? {
        return Err("fallback route changed Workspace authority".into());
    }
    println!("LAN_DIRECT_UNREACHABLE_RELAY_FALLBACK=PASS");
    Ok(())
}

async fn run_frontend(start: bool) -> Result<(), Box<dyn std::error::Error>> {
    let workspace_id = required("CYRENE_WORKSPACE_ID")?;
    let organization_id = required("CYRENE_ORGANIZATION_ID")?;
    let artifact_uri = required("CYRENE_WORKSPACE_ARTIFACT_URI")?;
    let user = fixture_user();
    let hello = RelayHello {
        role: RelayParticipantRole::Frontend as i32,
        session_credential: required("CYRENE_FRONTEND_SESSION_CREDENTIAL")?,
        user: Some(user.clone()),
        organization_id: organization_id.clone(),
        workspace_id: String::new(),
        device: None,
    };
    let mut session = connect_relay_session(&relay_client_config()?, hello).await?;
    let descriptors = session
        .discover("discover-workspaces", user, organization_id)
        .await?;
    let descriptor = descriptors
        .iter()
        .find(|descriptor| descriptor.workspace_id == workspace_id)
        .ok_or("Workspace was not discovered by identity")?;
    let has_local = descriptor.candidates.iter().any(|candidate| {
        candidate.mode == ConnectivityMode::Local as i32
            && candidate.provider_id == "cyrene.local.v1"
    });
    let has_relay = descriptor.candidates.iter().any(|candidate| {
        candidate.mode == ConnectivityMode::Relay as i32
            && candidate.provider_id == "cyrene.relay.fixture.v1"
    });
    if !has_local || !has_relay {
        return Err("Workspace descriptor is missing LOCAL or RELAY candidate".into());
    }
    let operation = OperationIdentity {
        id: "operation-1".to_string(),
        generation: 1,
    };
    if start {
        let response = session
            .execute(WorkspaceApiRequest {
                request_id: "start-operation-1".to_string(),
                workspace_id: workspace_id.clone(),
                request: Some(workspace_api_request::Request::StartOperation(
                    StartWorkspaceOperationRequest {
                        operation: Some(operation.clone()),
                        input_artifact_uris: vec![artifact_uri.clone()],
                    },
                )),
            })
            .await?;
        operation_view(response)?;
    }

    let deadline = tokio::time::Instant::now() + Duration::from_secs(45);
    let observed = loop {
        let response = session
            .execute(WorkspaceApiRequest {
                request_id: format!("get-operation-{}", now_unix_ms()),
                workspace_id: workspace_id.clone(),
                request: Some(workspace_api_request::Request::GetOperation(
                    GetWorkspaceOperationRequest {
                        operation: Some(operation.clone()),
                    },
                )),
            })
            .await?;
        let view = operation_view(response)?;
        if view.state == WorkspaceOperationState::Running as i32 && view.completed_units >= 1 {
            break view;
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("timed out waiting for relayed Workspace operation".into());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    if observed.operation.as_ref() != Some(&operation)
        || !observed.artifact_uris.contains(&artifact_uri)
        || observed.authority_instance_id != required("CYRENE_WORKSPACE_AUTHORITY_INSTANCE_ID")?
    {
        return Err("Workspace authority or Artifact identity changed across relay".into());
    }
    println!("WORKSPACE_DISCOVERED_BY_IDENTITY=PASS");
    println!("WORKSPACE_CONNECTION_DESCRIPTOR=PASS");
    println!("LOCAL_CONNECTIVITY=PASS");
    println!("RELAY_CONNECTIVITY=PASS");
    println!(
        "REMOTE_FRONTEND_OPERATION={}@{}",
        operation.id, operation.generation
    );
    println!(
        "REMOTE_FRONTEND_PROGRESS={}/{} {}",
        observed.completed_units, observed.total_units, observed.unit
    );
    println!("REMOTE_FRONTEND_ARTIFACT={artifact_uri}");
    println!(
        "WORKSPACE_AUTHORITY_INSTANCE={}",
        observed.authority_instance_id
    );
    if !start {
        println!("RELAY_DISCONNECT_RECONNECT=PASS");
        println!("WORKSPACE_AUTHORITY_PRESERVED=PASS");
    }
    Ok(())
}

fn fixture_user() -> UserIdentityRef {
    UserIdentityRef {
        issuer: "https://identity.fixture.cyrene.test".to_string(),
        subject: "user-fixture".to_string(),
    }
}

fn relay_client_config() -> Result<RelayClientConfig, Box<dyn std::error::Error>> {
    let endpoint = required("CYRENE_WORKSPACE_RELAY_ENDPOINT")?;
    let server_name = required("CYRENE_WORKSPACE_RELAY_SERVER_NAME")?;
    Ok(RelayClientConfig {
        control_endpoint: endpoint,
        server_name,
        ca_certificate_pem: fs::read(required("CYRENE_WORKSPACE_RELAY_CA")?)?,
        client_certificate_pem: fs::read(required("CYRENE_WORKSPACE_RELAY_CLIENT_CERT")?)?,
        client_key_pem: fs::read(required("CYRENE_WORKSPACE_RELAY_CLIENT_KEY")?)?,
    })
}

fn operation_view(
    response: WorkspaceApiResponse,
) -> Result<WorkspaceOperationView, Box<dyn std::error::Error>> {
    match response.outcome {
        Some(workspace_api_response::Outcome::Operation(operation)) => Ok(operation),
        Some(workspace_api_response::Outcome::Error(error)) => Err(error.message.into()),
        None => Err("Workspace API response has no outcome".into()),
    }
}

struct FileWorkspaceApi {
    workspace_id: String,
    artifact_uri: String,
    assignment_trigger: PathBuf,
    runtime_trace: PathBuf,
    connector_trace: PathBuf,
    authority_instance_id: String,
    operation: Mutex<Option<WorkspaceOperationView>>,
}

impl FileWorkspaceApi {
    fn new(
        workspace_id: String,
        artifact_uri: String,
        assignment_trigger: PathBuf,
        runtime_trace: PathBuf,
        connector_trace: PathBuf,
        authority_instance_id: String,
    ) -> Self {
        Self {
            workspace_id,
            artifact_uri,
            assignment_trigger,
            runtime_trace,
            connector_trace,
            authority_instance_id,
            operation: Mutex::new(None),
        }
    }

    fn start(
        &self,
        request_id: String,
        request: StartWorkspaceOperationRequest,
    ) -> WorkspaceApiResponse {
        let Some(operation) = request.operation else {
            return workspace_error(request_id, 3, "OPERATION_IDENTITY_REQUIRED");
        };
        if operation.id != "operation-1"
            || operation.generation != 1
            || request.input_artifact_uris != [self.artifact_uri.clone()]
        {
            return workspace_error(request_id, 3, "WORKSPACE_OPERATION_REQUEST_INVALID");
        }
        let mut state = match self.operation.lock() {
            Ok(state) => state,
            Err(_) => return workspace_error(request_id, 13, "WORKSPACE_STATE_UNAVAILABLE"),
        };
        if state
            .as_ref()
            .and_then(|view| view.operation.as_ref())
            .is_some_and(|existing| existing != &operation)
        {
            return workspace_error(request_id, 6, "WORKSPACE_OPERATION_CONFLICT");
        }
        if state.is_none() {
            *state = Some(WorkspaceOperationView {
                operation: Some(operation.clone()),
                state: WorkspaceOperationState::Pending as i32,
                completed_units: 0,
                total_units: 10,
                unit: "steps".to_string(),
                artifact_uris: vec![self.artifact_uri.clone()],
                resource_references: vec![format!(
                    "workspace://{}/operations/{}",
                    self.workspace_id, operation.id
                )],
                status_reason: "assignment_requested".to_string(),
                authority_instance_id: self.authority_instance_id.clone(),
            });
            if let Err(error) = fs::write(&self.assignment_trigger, b"start\n") {
                *state = None;
                return workspace_error(
                    request_id,
                    13,
                    &format!("ASSIGNMENT_TRIGGER_FAILED: {error}"),
                );
            }
            trace(
                &self.connector_trace,
                &format!(
                    "WORKSPACE_OPERATION_ACCEPTED operation={} artifact={}",
                    operation.id, self.artifact_uri
                ),
            );
        }
        let operation = self.refresh_locked(&mut state);
        WorkspaceApiResponse {
            request_id,
            outcome: operation.map(workspace_api_response::Outcome::Operation),
        }
    }

    fn get(
        &self,
        request_id: String,
        request: GetWorkspaceOperationRequest,
    ) -> WorkspaceApiResponse {
        let mut state = match self.operation.lock() {
            Ok(state) => state,
            Err(_) => return workspace_error(request_id, 13, "WORKSPACE_STATE_UNAVAILABLE"),
        };
        let requested = request.operation;
        if requested.is_none()
            || state.as_ref().and_then(|view| view.operation.as_ref()) != requested.as_ref()
        {
            return workspace_error(request_id, 5, "WORKSPACE_OPERATION_NOT_FOUND");
        }
        let operation = self.refresh_locked(&mut state);
        WorkspaceApiResponse {
            request_id,
            outcome: operation.map(workspace_api_response::Outcome::Operation),
        }
    }

    fn refresh_locked(
        &self,
        state: &mut Option<WorkspaceOperationView>,
    ) -> Option<WorkspaceOperationView> {
        let view = state.as_mut()?;
        let Ok(contents) = fs::read_to_string(&self.runtime_trace) else {
            return Some(view.clone());
        };
        for line in contents.lines() {
            if line.contains("ASSIGNMENT_RELEASED generation=1") {
                view.state = WorkspaceOperationState::Scheduled as i32;
                view.status_reason = "execution_fabric_scheduled".to_string();
            }
            if line.contains("OBSERVATION generation=1") && line.contains("reason=WORKLOAD_RUNNING")
            {
                view.state = WorkspaceOperationState::Running as i32;
                view.status_reason = "runtime_agent_running".to_string();
            }
            if line.contains("OBSERVATION generation=1")
                && line.contains("reason=GRACEFUL_TERMINATION")
            {
                view.state = WorkspaceOperationState::Succeeded as i32;
                view.status_reason = "graceful_termination".to_string();
            }
            if line.contains("LEASE_EXPIRED generation=1")
                && line.contains("classification=UNEXPECTED_LOSS")
            {
                view.state = WorkspaceOperationState::Lost as i32;
                view.status_reason = "unexpected_loss".to_string();
            }
            if line.contains("PROGRESS generation=1") {
                if let Some(completed) = field(line, "completed=").and_then(parse_u64) {
                    view.completed_units = completed;
                }
                if let Some(total) = field(line, "total=").and_then(parse_u64) {
                    view.total_units = total;
                }
                if let Some(unit) = field(line, "unit=") {
                    view.unit = unit.to_string();
                }
            }
            if line.contains("LOG_REFERENCE generation=1") {
                if let Some(artifact) = field(line, "artifact=") {
                    let artifact = artifact.to_string();
                    if !view.artifact_uris.contains(&artifact) {
                        view.artifact_uris.push(artifact);
                    }
                }
            }
        }
        Some(view.clone())
    }
}

#[tonic::async_trait]
impl WorkspaceApi for FileWorkspaceApi {
    async fn handle(&self, request: WorkspaceApiRequest) -> WorkspaceApiResponse {
        if request.workspace_id != self.workspace_id {
            return workspace_error(request.request_id, 5, "WORKSPACE_NOT_FOUND");
        }
        match request.request {
            Some(workspace_api_request::Request::StartOperation(start)) => {
                self.start(request.request_id, start)
            }
            Some(workspace_api_request::Request::GetOperation(get)) => {
                self.get(request.request_id, get)
            }
            None => workspace_error(request.request_id, 3, "WORKSPACE_REQUEST_REQUIRED"),
        }
    }
}

fn workspace_error(request_id: String, code: i32, message: &str) -> WorkspaceApiResponse {
    WorkspaceApiResponse {
        request_id,
        outcome: Some(workspace_api_response::Outcome::Error(RpcStatus {
            code,
            message: message.to_string(),
            details: Vec::new(),
        })),
    }
}

fn field<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    line.split_whitespace()
        .find_map(|value| value.strip_prefix(prefix))
}

fn parse_u64(value: &str) -> Option<u64> {
    value.parse().ok()
}

fn required(name: &str) -> Result<String, std::io::Error> {
    env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("{name} is required"),
            )
        })
}

fn trace(path: &Path, line: &str) {
    if let Ok(mut output) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(output, "{line}");
        let _ = output.flush();
    }
}

fn now_unix_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

fn timestamp_from_ms(value: u64) -> prost_types::Timestamp {
    prost_types::Timestamp {
        seconds: i64::try_from(value / 1000).unwrap_or(i64::MAX),
        nanos: i32::try_from((value % 1000) * 1_000_000).unwrap_or_default(),
    }
}
