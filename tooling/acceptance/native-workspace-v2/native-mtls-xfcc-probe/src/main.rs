use std::env;
use std::error::Error;
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cy_proto::workspace_v1::workspace_relay_service_client::WorkspaceRelayServiceClient;
use cy_proto::workspace_v1::{relay_frame, RelayFrame, RelayHello, RelayParticipantRole};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_stream::wrappers::ReceiverStream;
use tonic::metadata::MetadataValue;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint, Identity};
use tonic::{Code, Request};

const XFCC_VALUE: &str =
    "By=spiffe://relay-probe.invalid;Hash=0000000000000000;Subject=\"CN=xfcc-only-negative-probe\"";

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("Native Tonic XFCC probe failed: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn Error>> {
    let host = env::var("CYRENE_NATIVE_RELAY_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    if host != "127.0.0.1" {
        return Err("this local-only Tonic probe requires 127.0.0.1".into());
    }
    let port = env::var("CYRENE_NATIVE_RELAY_PORT")
        .unwrap_or_else(|_| "8080".into())
        .parse::<u16>()?;
    if port == 0 {
        return Err("CYRENE_NATIVE_RELAY_PORT must be a non-zero port".into());
    }
    let server_name = env::var("CYRENE_NATIVE_RELAY_SERVER_NAME").unwrap_or_else(|_| host.clone());
    let server_ca_path = required_path("CYRENE_NATIVE_RELAY_SERVER_CA_FILE")?;
    let tls_report_path = required_path("CYRENE_NATIVE_RELAY_TLS_NEGATIVE_REPORT_FILE")?;
    let bff_certificate_path = required_path("CYRENE_NATIVE_RELAY_CLIENT_CERT_FILE")?;
    let bff_key_path = required_path("CYRENE_NATIVE_RELAY_CLIENT_KEY_FILE")?;
    let acceptance_root = PathBuf::from(
        env::var("CYRENE_NATIVE_ACCEPTANCE_DIR")
            .unwrap_or_else(|_| "/tmp/cyrene-components-v2-acceptance/native-relay".into()),
    );
    fs::create_dir_all(&acceptance_root)?;
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&acceptance_root, fs::Permissions::from_mode(0o700))?;
    let server_ca_bytes = fs::read(&server_ca_path)?;
    let server_ca_hash = hex_sha256(&server_ca_bytes);
    let tls_probe_report =
        read_tls_probe_report(&tls_report_path, &host, port, &server_name, &server_ca_hash)?;
    let tls_report_hash = hex_sha256(&fs::read(&tls_report_path)?);
    let bff_certificate = fs::read(&bff_certificate_path)?;
    let bff_private_key = fs::read(&bff_key_path)?;
    let (untrusted_certificate, untrusted_private_key) =
        generate_untrusted_client_identity(&acceptance_root)?;

    let socket_address: SocketAddr = format!("{host}:{port}").parse()?;
    timeout(Duration::from_secs(3), TcpStream::connect(socket_address)).await??;

    // Complete a real client-authenticated Tonic TLS handshake before probing XFCC rejection.
    let tls = ClientTlsConfig::new()
        .ca_certificate(Certificate::from_pem(server_ca_bytes.clone()))
        .identity(Identity::from_pem(
            bff_certificate.clone(),
            bff_private_key.clone(),
        ))
        .domain_name(server_name.clone());
    let endpoint = Endpoint::from_shared(format!("https://{host}:{port}"))?.tls_config(tls)?;
    let channel = timeout(Duration::from_secs(5), endpoint.connect()).await??;

    let pinned_certificate_result = call_connect(
        channel.clone(),
        RelayHello {
            role: RelayParticipantRole::Frontend as i32,
            session_credential: String::new(),
            user: None,
            organization_id: String::new(),
            workspace_id: String::new(),
            device: None,
        },
        None,
    )
    .await?;
    require_status(
        pinned_certificate_result,
        Code::Unauthenticated,
        "RELAY_CREDENTIAL_INVALID",
        "valid BFF mTLS certificate was not accepted through the pin verifier",
    )?;

    let valid_certificate_xfcc_result = call_connect(
        channel,
        RelayHello {
            role: RelayParticipantRole::Frontend as i32,
            session_credential: "invalid-local-acceptance-handoff".into(),
            user: None,
            organization_id: String::new(),
            workspace_id: String::new(),
            device: None,
        },
        Some(XFCC_VALUE),
    )
    .await?;
    require_status(
        valid_certificate_xfcc_result,
        Code::Unauthenticated,
        "RELAY_XFCC_NOT_ALLOWED_IN_NATIVE_MODE",
        "native Relay did not reject caller-supplied XFCC metadata",
    )?;

    // A second, identity-free client proves XFCC cannot substitute for a TLS certificate.
    let no_identity_tls = ClientTlsConfig::new()
        .ca_certificate(Certificate::from_pem(server_ca_bytes.clone()))
        .domain_name(server_name.clone());
    let no_identity_endpoint =
        Endpoint::from_shared(format!("https://{host}:{port}"))?.tls_config(no_identity_tls)?;
    let no_identity_channel = no_identity_endpoint.connect_lazy();
    let no_identity_result = call_connect(
        no_identity_channel,
        RelayHello {
            role: RelayParticipantRole::WorkspaceConnector as i32,
            session_credential: String::new(),
            user: None,
            organization_id: String::new(),
            workspace_id: String::new(),
            device: None,
        },
        Some(XFCC_VALUE),
    )
    .await?;
    let no_identity_status =
        require_transport_rejection(no_identity_result, "client without a certificate")?;

    // An unrelated self-signed client leaf must fail the real Tonic RPC at
    // TLS/transport setup, even when it supplies the same forged XFCC value.
    let untrusted_tls = ClientTlsConfig::new()
        .ca_certificate(Certificate::from_pem(server_ca_bytes))
        .identity(Identity::from_pem(
            untrusted_certificate,
            untrusted_private_key,
        ))
        .domain_name(server_name.clone());
    let untrusted_endpoint =
        Endpoint::from_shared(format!("https://{host}:{port}"))?.tls_config(untrusted_tls)?;
    let untrusted_channel = untrusted_endpoint.connect_lazy();
    let untrusted_result = call_connect(
        untrusted_channel,
        RelayHello {
            role: RelayParticipantRole::WorkspaceConnector as i32,
            session_credential: String::new(),
            user: None,
            organization_id: String::new(),
            workspace_id: String::new(),
            device: None,
        },
        Some(XFCC_VALUE),
    )
    .await?;
    let untrusted_status =
        require_transport_rejection(untrusted_result, "client with an untrusted certificate")?;

    let platform_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .ok_or("could not locate the Platform source root")?;
    let source = source_details(platform_root)?;
    let report_path = acceptance_root.join("native-tonic-xfcc-negative-report.json");
    let report = json!({
        "status": "PASS",
        "category": "REAL_TONIC_MTLS_XFCC_REJECTION_LOCAL_ONLY",
        "endpoint": format!("{host}:{port}"),
        "serverName": server_name,
        "rpc": "cyrene.workspace.v1.WorkspaceRelayService/Connect",
        "checks": {
            "trustedBffClientCertificate": {
                "result": "tls_handshake_completed_and_pin_accepted",
                "applicationStatus": "Unauthenticated",
                "applicationMessage": "RELAY_CREDENTIAL_INVALID",
                "scope": "invalid local handoff rejected; no user identity asserted"
            },
            "validCertificateWithForgedXfcc": {
                "result": "rejected",
                "applicationStatus": "Unauthenticated",
                "applicationMessage": "RELAY_XFCC_NOT_ALLOWED_IN_NATIVE_MODE"
            },
            "missingCertificateWithForgedXfcc": {
                "result": "rejected",
                "transportStatus": no_identity_status.code,
                "transportMessage": no_identity_status.message,
                "transportSourceChain": no_identity_status.source_chain
            },
            "untrustedCertificateWithForgedXfcc": {
                "result": "rejected",
                "transportStatus": untrusted_status.code,
                "transportMessage": untrusted_status.message,
                "transportSourceChain": untrusted_status.source_chain
            }
        },
        "observedTonicStatuses": {
            "missingCertificate": {
                "code": no_identity_status.code,
                "message": no_identity_status.message,
                "sourceChain": no_identity_status.source_chain
            },
            "untrustedCertificate": {
                "code": untrusted_status.code,
                "message": untrusted_status.message,
                "sourceChain": untrusted_status.source_chain
            }
        },
        "pairedTlsProbeChecks": tls_probe_report["checks"],
        "transportEvidence": "Tonic completed a verified mTLS handshake with the pinned local BFF workload certificate; native Relay rejected caller XFCC, while clients with no certificate and an untrusted certificate failed at TLS/transport setup",
        "pairedTlsProbeReport": tls_report_path,
        "pairedTlsProbeReportSha256": tls_report_hash,
        "scope": "Local workload-certificate transport only; invalid handoff was rejected; no AAD principal, device approval, ACK, CRL, or Product dispatch",
        "runAtUtc": utc_timestamp(),
        "source": source,
    });
    write_private_report(&report_path, &report)?;
    println!(
        "REAL_TONIC_MTLS_XFCC_REJECTION_LOCAL_ONLY PASS: report={}",
        report_path.display()
    );
    Ok(())
}

async fn call_connect(
    channel: Channel,
    hello: RelayHello,
    forwarded_certificate: Option<&str>,
) -> Result<Result<(), tonic::Status>, Box<dyn Error>> {
    let (sender, receiver) = mpsc::channel(1);
    sender
        .send(RelayFrame {
            frame_id: "local-mtls-acceptance-probe".into(),
            body: Some(relay_frame::Body::Hello(hello)),
        })
        .await?;
    drop(sender);

    let mut request = Request::new(ReceiverStream::new(receiver));
    if let Some(value) = forwarded_certificate {
        request
            .metadata_mut()
            .insert("x-forwarded-client-cert", MetadataValue::try_from(value)?);
    }
    let mut client = WorkspaceRelayServiceClient::new(channel);
    let response = match timeout(Duration::from_secs(5), client.connect(request)).await? {
        Ok(response) => response,
        Err(status) => return Ok(Err(status)),
    };
    let mut inbound = response.into_inner();
    match timeout(Duration::from_secs(5), inbound.message()).await? {
        Ok(Some(_)) => Ok(Ok(())),
        Ok(None) => Ok(Err(tonic::Status::unknown(
            "Relay closed the local acceptance stream without an authentication status",
        ))),
        Err(status) => Ok(Err(status)),
    }
}

fn require_status(
    result: Result<(), tonic::Status>,
    expected_code: Code,
    expected_message: &str,
    failure_message: &str,
) -> Result<(), Box<dyn Error>> {
    match result {
        Err(status) if status.code() == expected_code && status.message() == expected_message => {
            Ok(())
        }
        Err(status) => Err(format!(
            "{failure_message}: got {:?} {}",
            status.code(),
            status.message()
        )
        .into()),
        Ok(()) => Err(failure_message.into()),
    }
}

fn require_transport_rejection(
    result: Result<(), tonic::Status>,
    client_label: &str,
) -> Result<TransportRejection, Box<dyn Error>> {
    let status = match result {
        Err(status) => status,
        Ok(()) => return Err(format!("Relay accepted the {client_label}").into()),
    };
    if status.code() != Code::Unknown || status.message() != "transport error" {
        return Err(format!(
            "the {client_label} did not produce the expected source-backed Tonic transport failure: {:?} {}",
            status.code(),
            status.message()
        )
        .into());
    }
    let first_source = Error::source(&status)
        .ok_or_else(|| format!("the {client_label} transport status has no error source"))?;
    let source_debug = format!("{first_source:?}");
    if !source_debug.contains("tonic::transport::Error(Transport") {
        return Err(format!(
            "the {client_label} source-backed Unknown did not originate from tonic::transport::Error: {}",
            bounded_error_text(&source_debug)
        )
        .into());
    }
    let source_chain = bounded_error_chain(&status);
    if source_chain.is_empty() {
        return Err(format!("the {client_label} transport error chain was empty").into());
    }
    Ok(TransportRejection {
        code: "Unknown",
        message: status.message().to_string(),
        source_chain,
    })
}

struct TransportRejection {
    code: &'static str,
    message: String,
    source_chain: Vec<Value>,
}

fn bounded_error_chain(status: &tonic::Status) -> Vec<Value> {
    let mut chain = Vec::new();
    let mut source = Error::source(status);
    for _ in 0..5 {
        let Some(error) = source else {
            break;
        };
        chain.push(json!({
            "display": bounded_error_text(&error.to_string()),
            "debug": bounded_error_text(&format!("{error:?}")),
        }));
        source = error.source();
    }
    chain
}

fn bounded_error_text(value: &str) -> String {
    let mut text = value.chars().take(512).collect::<String>();
    if value.chars().count() > 512 {
        text.push_str("...");
    }
    text
}

fn required_path(name: &str) -> Result<PathBuf, Box<dyn Error>> {
    let value = env::var_os(name).ok_or_else(|| format!("{name} is required"))?;
    let path = PathBuf::from(value).canonicalize()?;
    if !path.is_file() {
        return Err(format!("{name} must reference a regular file").into());
    }
    Ok(path)
}

fn generate_untrusted_client_identity(
    acceptance_root: &Path,
) -> Result<(Vec<u8>, Vec<u8>), Box<dyn Error>> {
    use std::os::unix::fs::PermissionsExt;

    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let directory = acceptance_root.join(format!(
        ".native-mtls-untrusted-{}-{timestamp}",
        std::process::id()
    ));
    fs::create_dir(&directory)?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
    let _private_directory = PrivateTempDirectory(directory.clone());
    let certificate_path = directory.join("untrusted-client.crt");
    let key_path = directory.join("untrusted-client.key");
    let result = Command::new("openssl")
        .args([
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-subj",
            "/CN=native-relay-tonic-untrusted-acceptance-client",
            "-keyout",
        ])
        .arg(&key_path)
        .arg("-out")
        .arg(&certificate_path)
        .args([
            "-days",
            "1",
            "-addext",
            "basicConstraints=critical,CA:FALSE",
            "-addext",
            "keyUsage=critical,digitalSignature,keyEncipherment",
            "-addext",
            "extendedKeyUsage=clientAuth",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if !result.success() {
        return Err("could not generate the temporary untrusted Tonic client certificate".into());
    }
    fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600))?;
    fs::set_permissions(&certificate_path, fs::Permissions::from_mode(0o600))?;
    Ok((fs::read(certificate_path)?, fs::read(key_path)?))
}

struct PrivateTempDirectory(PathBuf);

impl Drop for PrivateTempDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn read_tls_probe_report(
    path: &Path,
    host: &str,
    port: u16,
    server_name: &str,
    server_ca_hash: &str,
) -> Result<Value, Box<dyn Error>> {
    let report: Value = serde_json::from_slice(&fs::read(path)?)?;
    let status_is_supported = report["status"] == "PASS";
    let missing_result = report["checks"]["missingClientCertificate"]["result"]
        .as_str()
        .unwrap_or_default();
    let untrusted_result = report["checks"]["untrustedClientCertificate"]["result"]
        .as_str()
        .unwrap_or_default();
    if !status_is_supported
        || report["category"] != "REAL_NATIVE_RELAY_TLS_NEGATIVE_LOCAL_ONLY"
        || report["endpoint"] != format!("{host}:{port}")
        || report["serverName"] != server_name
        || report["serverCaSha256"] != server_ca_hash
        || missing_result != "rejected"
        || untrusted_result != "rejected"
        || report["checks"]["missingClientCertificate"]["tlsVersion"] != "TLSv1.3"
        || report["checks"]["untrustedClientCertificate"]["tlsVersion"] != "TLSv1.3"
        || report["checks"]["missingClientCertificate"]["phase"] != "application_read"
        || report["checks"]["untrustedClientCertificate"]["phase"] != "application_read"
        || !report["checks"]["missingClientCertificate"]["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("CERTIFICATE_REQUIRED")
        || !report["checks"]["untrustedClientCertificate"]["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("UNKNOWN_CA")
    {
        return Err("paired Native Relay TLS probe report does not match this endpoint".into());
    }
    Ok(report)
}

fn source_details(platform_root: &Path) -> Result<Value, Box<dyn Error>> {
    fn git(root: &Path, arguments: &[&str]) -> Result<String, Box<dyn Error>> {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(arguments)
            .output()?;
        if !output.status.success() {
            return Err("could not capture Platform source revision".into());
        }
        Ok(String::from_utf8(output.stdout)?.trim().to_string())
    }

    let dirty_paths = git(
        platform_root,
        &["status", "--short", "--untracked-files=all"],
    )?
    .lines()
    .map(str::to_string)
    .collect::<Vec<_>>();
    Ok(json!({
        "platformHead": git(platform_root, &["rev-parse", "HEAD"])? ,
        "platformBranch": git(platform_root, &["branch", "--show-current"])? ,
        "platformDirtyPaths": dirty_paths,
    }))
}

fn hex_sha256(contents: &[u8]) -> String {
    format!("{:x}", Sha256::digest(contents))
}

fn write_private_report(path: &Path, report: &Value) -> Result<(), Box<dyn Error>> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let contents = serde_json::to_vec_pretty(report)?;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(&contents)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

fn utc_timestamp() -> String {
    let output = Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output();
    output
        .ok()
        .filter(|value| value.status.success())
        .and_then(|value| String::from_utf8(value.stdout).ok())
        .unwrap_or_else(|| "unavailable".into())
        .trim()
        .to_string()
}
