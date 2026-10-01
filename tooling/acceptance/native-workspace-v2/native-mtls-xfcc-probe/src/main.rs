use std::env;
use std::error::Error;
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

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
    let server_ca_bytes = fs::read(&server_ca_path)?;
    let server_ca_hash = hex_sha256(&server_ca_bytes);
    read_tls_probe_report(&tls_report_path, &host, port, &server_name, &server_ca_hash)?;
    let tls_report_hash = hex_sha256(&fs::read(&tls_report_path)?);
    let bff_certificate = fs::read(&bff_certificate_path)?;
    let bff_private_key = fs::read(&bff_key_path)?;

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
        .ca_certificate(Certificate::from_pem(server_ca_bytes))
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
    let status = match no_identity_result {
        Err(status) => status,
        Ok(()) => {
            return Err("Relay accepted XFCC without a TLS client certificate".into());
        }
    };
    if status.code() != Code::Unavailable {
        return Err(format!(
            "identity-free Tonic Connect failed outside the expected transport layer: {:?}",
            status.code()
        )
        .into());
    }
    let transport_message = status.message().to_ascii_lowercase();
    if !["transport", "tls", "certificate", "handshake"]
        .iter()
        .any(|needle| transport_message.contains(needle))
    {
        return Err("Tonic Connect returned Unavailable without a TLS/transport failure".into());
    }

    let platform_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .ok_or("could not locate the Platform source root")?;
    let source = source_details(platform_root)?;
    let acceptance_root = PathBuf::from(
        env::var("CYRENE_NATIVE_ACCEPTANCE_DIR")
            .unwrap_or_else(|_| "/tmp/cyrene-components-v2-acceptance/native-relay".into()),
    );
    fs::create_dir_all(&acceptance_root)?;
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&acceptance_root, fs::Permissions::from_mode(0o700))?;
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
                "transportStatus": "Unavailable",
                "transportMessage": status.message()
            }
        },
        "observedTonicStatus": { "code": "Unavailable", "message": status.message() },
        "transportEvidence": "Tonic completed a verified mTLS handshake with the pinned local BFF workload certificate; native Relay rejected caller XFCC, and a client with no certificate failed at TLS/transport setup",
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

fn required_path(name: &str) -> Result<PathBuf, Box<dyn Error>> {
    let value = env::var_os(name).ok_or_else(|| format!("{name} is required"))?;
    let path = PathBuf::from(value).canonicalize()?;
    if !path.is_file() {
        return Err(format!("{name} must reference a regular file").into());
    }
    Ok(path)
}

fn read_tls_probe_report(
    path: &Path,
    host: &str,
    port: u16,
    server_name: &str,
    server_ca_hash: &str,
) -> Result<Value, Box<dyn Error>> {
    let report: Value = serde_json::from_slice(&fs::read(path)?)?;
    if report["status"] != "PASS"
        || report["category"] != "REAL_NATIVE_RELAY_TLS_NEGATIVE_LOCAL_ONLY"
        || report["endpoint"] != format!("{host}:{port}")
        || report["serverName"] != server_name
        || report["serverCaSha256"] != server_ca_hash
        || report["checks"]["missingClientCertificate"]["result"] != "rejected"
        || report["checks"]["untrustedClientCertificate"]["result"] != "rejected"
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
