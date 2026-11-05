//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 connector_host_manifest.rs                                      │
//! │  Module: cy_workspace_authority_host::tests                         │
//! │  Role: Verify the real connector host grants from its manifest.     │
//! │                                                                     │
//! │  模块职责：验证真实 Connector host 按批准的清单授权。                 │
//! └─────────────────────────────────────────────────────────────────────┘

use std::{
    collections::BTreeSet,
    env,
    ffi::OsStr,
    fs,
    io::Write,
    net::TcpListener,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use cy_proto::cyrene::workspace::authority::v2::{
    workspace_connector_authorization_service_client::WorkspaceConnectorAuthorizationServiceClient,
    AuthorizeConnectorOperationRequest,
};
use cy_proto::workspace_v1::UserIdentityRef;
use cy_workspace_control_plane::authority::{
    NAVIGATOR_AUTOMATION_VIEWER_ROLE, NAVIGATOR_CONNECTOR_OPERATOR_ROLE,
    NAVIGATOR_OFFICE_TASK_WRITER_ROLE,
};
use cy_workspace_postgres_storage::DirectoryOperatorProvisioner;
use serde_json::json;
use sha2::{Digest, Sha256};
use tonic::{
    transport::{Certificate, Channel, ClientTlsConfig, Identity},
    Code,
};
use uuid::Uuid;

const HOST_BINARY: &str = env!("CARGO_BIN_EXE_cy-workspace-connector-authority-host");
const TLS_DIRECTORY_ENV: &str = "CYRENE_CONNECTOR_HOST_TEST_TLS_DIR";
const READER_DATABASE_URL_ENV: &str = "CYRENE_WORKSPACE_DIRECTORY_TEST_DATABASE_URL";
const MANIFEST_FILE_ENV: &str = "CYRENE_CONNECTOR_OPERATIONS_MANIFEST_FILE";
const MANIFEST_DIGEST_ENV: &str = "CYRENE_CONNECTOR_OPERATIONS_MANIFEST_SHA256";
const TENANT_ID: &str = "11111111-1111-4111-8111-111111111111";
const STARTUP_LIMIT: Duration = Duration::from_secs(60);

const PLAN_GET: &str = "microsoft_graph.planner.plan.get";
const TASK_GET: &str = "microsoft_graph.planner.task.get";
const TEAMS_REPLY: &str = "msteams.message.reply";
const REPOSITORY_LIST: &str = "github.repository.list";
const DEMO_LIST: &str = "demo.items.list";

type Client = WorkspaceConnectorAuthorizationServiceClient<Channel>;

/// A private directory that goes away with the test.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = env::temp_dir().join(format!("cy-connector-host-test-{}", Uuid::new_v4()));
        fs::create_dir(&path).expect("scratch directory is created");
        Self(path)
    }

    fn secret_file(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.0.join(name);
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .expect("secret file is created")
            .write_all(bytes)
            .expect("secret file is written");
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct RunningHost {
    child: Child,
    rpc_port: u16,
}

impl Drop for RunningHost {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Deployment {
    scratch: Scratch,
    tls_directory: PathBuf,
    organization_id: String,
    workspace_id: String,
    issuer: String,
    database_url_file: PathBuf,
    signing_key_file: PathBuf,
    exchange_fingerprint: String,
}

fn free_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .expect("a loopback port is free")
        .local_addr()
        .expect("the listener has an address")
        .port()
}

impl Deployment {
    fn new() -> Self {
        let scratch = Scratch::new();
        let tls_directory = PathBuf::from(env::var(TLS_DIRECTORY_ENV).expect(TLS_DIRECTORY_ENV));
        let database_url = env::var(READER_DATABASE_URL_ENV).expect(READER_DATABASE_URL_ENV);
        let database_url_file = scratch.secret_file("database-url", database_url.as_bytes());
        let signing_key_file = scratch.secret_file("signing-key", &[9; 32]);
        let exchange_fingerprint =
            fs::read_to_string(tls_directory.join("connector-exchange.sha256"))
                .expect("the Exchange certificate fingerprint is present")
                .trim()
                .to_owned();
        Self {
            scratch,
            tls_directory,
            organization_id: format!("connector-host-org-{}", Uuid::new_v4()),
            workspace_id: format!("connector-host-workspace-{}", Uuid::new_v4()),
            issuer: format!("https://login.microsoftonline.com/{TENANT_ID}/v2.0"),
            database_url_file,
            signing_key_file,
            exchange_fingerprint,
        }
    }

    fn tls_file(&self, name: &str) -> PathBuf {
        self.tls_directory.join(format!("connector-{name}"))
    }

    fn host_command(&self, rpc_port: u16, settings: &[(&str, &OsStr)]) -> Command {
        let mut command = Command::new(HOST_BINARY);
        command
            .env_clear()
            .env("CYRENE_ORGANIZATION_ID", &self.organization_id)
            .env("CYRENE_WORKSPACE_ID", &self.workspace_id)
            .env("CYRENE_AUTHORITY_AAD_TENANT_ID", TENANT_ID)
            .env(
                "CYRENE_WORKSPACE_DIRECTORY_DATABASE_URL_FILE",
                &self.database_url_file,
            )
            .env(
                "CYRENE_AUTHORITY_EXCHANGE_WORKLOAD_ID",
                "connector-host-test",
            )
            .env(
                "CYRENE_AUTHORITY_EXCHANGE_MTLS_SHA256",
                &self.exchange_fingerprint,
            )
            .env("CYRENE_AUTHORITY_SIGNING_KEY_FILE", &self.signing_key_file)
            .env("CYRENE_AUTHORITY_SIGNING_KEY_ID", "connector-host-test-key")
            .env(
                "CYRENE_AUTHORITY_TLS_CERT_FILE",
                self.tls_file("authority.crt"),
            )
            .env(
                "CYRENE_AUTHORITY_TLS_KEY_FILE",
                self.tls_file("authority.key"),
            )
            .env(
                "CYRENE_AUTHORITY_TLS_CLIENT_CA_FILE",
                self.tls_file("ca.crt"),
            )
            .env(
                "CYRENE_WORKSPACE_CONNECTOR_AUTHORITY_LISTEN",
                format!("127.0.0.1:{rpc_port}"),
            )
            .env(
                "CYRENE_WORKSPACE_CONNECTOR_AUTHORITY_HEALTH_ADDR",
                format!("127.0.0.1:{}", free_port()),
            )
            .envs(settings.iter().copied());
        command
    }

    fn start(&self, settings: &[(&str, &OsStr)]) -> RunningHost {
        let rpc_port = free_port();
        let child = self
            .host_command(rpc_port, settings)
            .stdout(Stdio::null())
            .spawn()
            .expect("the host process starts");
        RunningHost { child, rpc_port }
    }

    async fn connect(&self, host: &mut RunningHost) -> Client {
        let read = |name: &str| fs::read(self.tls_file(name)).expect("test certificate is present");
        let tls = ClientTlsConfig::new()
            .domain_name("localhost")
            .ca_certificate(Certificate::from_pem(read("ca.crt")))
            .identity(Identity::from_pem(
                read("exchange.crt"),
                read("exchange.key"),
            ));
        let endpoint = Channel::from_shared(format!("https://localhost:{}", host.rpc_port))
            .expect("the endpoint is valid")
            .tls_config(tls)
            .expect("the client TLS configuration is valid");
        let deadline = Instant::now() + STARTUP_LIMIT;
        loop {
            assert!(
                host.child
                    .try_wait()
                    .expect("the host can be polled")
                    .is_none(),
                "the host exited instead of serving"
            );
            match endpoint.connect().await {
                Ok(channel) => return WorkspaceConnectorAuthorizationServiceClient::new(channel),
                Err(error) if Instant::now() >= deadline => {
                    panic!("the host never accepted a connection: {error}")
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(200)).await,
            }
        }
    }

    /// Run the host expecting it to refuse to start, and return what it logged.
    fn refused_with(&self, settings: &[(&str, &OsStr)]) -> String {
        let mut child = self
            .host_command(free_port(), settings)
            .stdout(Stdio::piped())
            .spawn()
            .expect("the host process starts");
        let deadline = Instant::now() + STARTUP_LIMIT;
        while child.try_wait().expect("the host can be polled").is_none() {
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("the host kept running with settings it should refuse");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let output = child.wait_with_output().expect("the host output is read");
        assert_eq!(output.status.code(), Some(1));
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    async fn outcome(
        &self,
        client: &mut Client,
        subject: &str,
        provider_id: &str,
        operation_id: &str,
    ) -> Result<(), Code> {
        client
            .authorize_connector_operation(AuthorizeConnectorOperationRequest {
                workspace_id: self.workspace_id.clone(),
                principal_issuer: self.issuer.clone(),
                principal_subject: subject.to_owned(),
                connection_ref: format!("{provider_id}-connection"),
                provider_id: provider_id.to_owned(),
                profile_ref: format!("{provider_id}-profile"),
                operation_id: operation_id.to_owned(),
                resource_ref: "source:target-1".to_owned(),
                arguments_digest_sha256: vec![0x27; 32],
                request_id: format!("connector-host-test-{}", Uuid::new_v4()),
            })
            .await
            .map(|response| {
                let grant = response.into_inner().grant.expect("a grant is returned");
                assert_eq!(grant.provider_id, provider_id);
                assert_eq!(grant.operation_id, operation_id);
            })
            .map_err(|status| status.code())
    }

    async fn add_member(&self, provisioner: &DirectoryOperatorProvisioner, role: &str) -> String {
        let subject = Uuid::new_v4().to_string();
        provisioner
            .grant_membership(
                &UserIdentityRef {
                    issuer: self.issuer.clone(),
                    subject: subject.clone(),
                },
                &self.organization_id,
                &self.workspace_id,
                &BTreeSet::from([role.to_owned()]),
                "connector host manifest test",
            )
            .await
            .expect("the Directory membership is granted");
        subject
    }
}

fn row(
    operation_id: &str,
    provider_id: &str,
    roles: &[&str],
    transport: bool,
) -> serde_json::Value {
    json!({
        "operationId": operation_id,
        "providerIds": [provider_id],
        "roles": roles,
        "automationTransport": transport,
        "mutating": if transport { serde_json::Value::Null } else { json!(false) },
    })
}

/// Differs from the built-in lists in every way a manifest can: it adds `demo.items.list` for the
/// operator, leaves out `github.repository.list`, takes `planner.plan.get` from the viewer, and
/// takes the Teams reply from the task writer.
fn test_manifest() -> Vec<u8> {
    serde_json::to_vec(&json!({
        "schemaVersion": "cyrene.connector-operations.v1",
        "roles": {
            "operator": NAVIGATOR_CONNECTOR_OPERATOR_ROLE,
            "automationViewer": NAVIGATOR_AUTOMATION_VIEWER_ROLE,
            "officeTaskWriter": NAVIGATOR_OFFICE_TASK_WRITER_ROLE,
        },
        "operations": [
            row(DEMO_LIST, "demo", &["operator"], false),
            row(PLAN_GET, "microsoft_graph", &["operator", "officeTaskWriter"], false),
            row(TASK_GET, "microsoft_graph", &["operator", "automationViewer", "officeTaskWriter"], false),
            row(TEAMS_REPLY, "msteams", &["operator", "automationViewer"], true),
        ],
    }))
    .expect("the manifest serializes")
}

fn digest_of(bytes: &[u8]) -> String {
    let hex: String = Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("sha256:{hex}")
}

#[tokio::test]
#[ignore = "requires an isolated PostgreSQL database, Directory credentials and test certificates"]
async fn connector_host_grants_from_the_approved_manifest() {
    let deployment = Deployment::new();
    let provisioner = DirectoryOperatorProvisioner::connect_from_environment()
        .await
        .expect("the Directory operator connects");
    let operator = deployment
        .add_member(&provisioner, NAVIGATOR_CONNECTOR_OPERATOR_ROLE)
        .await;
    let viewer = deployment
        .add_member(&provisioner, NAVIGATOR_AUTOMATION_VIEWER_ROLE)
        .await;
    let writer = deployment
        .add_member(&provisioner, NAVIGATOR_OFFICE_TASK_WRITER_ROLE)
        .await;

    let denied = Err(Code::PermissionDenied);
    let not_in_catalog = Err(Code::InvalidArgument);
    let graph = "microsoft_graph";

    // Without the settings, the built-in lists apply.
    let mut host = deployment.start(&[]);
    let mut client = deployment.connect(&mut host).await;
    for (subject, provider, operation, expected) in [
        (&operator, "github", REPOSITORY_LIST, Ok(())),
        (&operator, "demo", DEMO_LIST, not_in_catalog),
        (&viewer, graph, PLAN_GET, Ok(())),
        (&viewer, graph, TASK_GET, Ok(())),
        (&viewer, "github", REPOSITORY_LIST, denied),
        (&writer, "msteams", TEAMS_REPLY, Ok(())),
    ] {
        assert_eq!(
            deployment
                .outcome(&mut client, subject, provider, operation)
                .await,
            expected,
            "built-in lists: {provider} {operation}"
        );
    }
    drop(client);
    drop(host);

    // With them, the manifest decides, in each direction.
    let manifest = test_manifest();
    let digest = digest_of(&manifest);
    let manifest_file = deployment.scratch.secret_file("manifest.json", &manifest);
    let approved: [(&str, &OsStr); 2] = [
        (MANIFEST_FILE_ENV, manifest_file.as_os_str()),
        (MANIFEST_DIGEST_ENV, OsStr::new(&digest)),
    ];
    let mut host = deployment.start(&approved);
    let mut client = deployment.connect(&mut host).await;
    for (subject, provider, operation, expected) in [
        (&operator, "demo", DEMO_LIST, Ok(())),
        (&operator, "github", REPOSITORY_LIST, not_in_catalog),
        (&operator, graph, PLAN_GET, Ok(())),
        (&viewer, "demo", DEMO_LIST, denied),
        (&viewer, graph, PLAN_GET, denied),
        (&viewer, graph, TASK_GET, Ok(())),
        (&viewer, "msteams", TEAMS_REPLY, Ok(())),
        (&writer, graph, PLAN_GET, Ok(())),
        (&writer, "msteams", TEAMS_REPLY, denied),
    ] {
        assert_eq!(
            deployment
                .outcome(&mut client, subject, provider, operation)
                .await,
            expected,
            "approved manifest: {provider} {operation}"
        );
    }
    drop(client);
    drop(host);

    // A manifest other than the approved one, or half of the settings, stops the host.
    let other_digest = digest_of(b"another version");
    let log = deployment.refused_with(&[
        (MANIFEST_FILE_ENV, manifest_file.as_os_str()),
        (MANIFEST_DIGEST_ENV, OsStr::new(&other_digest)),
    ]);
    assert!(
        log.contains("digest differs from the approved digest"),
        "{log}"
    );
    let log = deployment.refused_with(&[(MANIFEST_FILE_ENV, manifest_file.as_os_str())]);
    assert!(log.contains("configuration is invalid"), "{log}");
    let log = deployment.refused_with(&[(MANIFEST_DIGEST_ENV, OsStr::new(&digest))]);
    assert!(log.contains("configuration is invalid"), "{log}");
    let absent: &Path = Path::new("/nonexistent/connector-operations.json");
    let log = deployment.refused_with(&[
        (MANIFEST_FILE_ENV, absent.as_os_str()),
        (MANIFEST_DIGEST_ENV, OsStr::new(&digest)),
    ]);
    assert!(log.contains("manifest could not be read"), "{log}");
}
