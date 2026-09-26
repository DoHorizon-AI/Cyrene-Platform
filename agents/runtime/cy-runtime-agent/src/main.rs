//! Container-oriented `cy-runtime-agent run -- <workload command>` entry point.

use std::collections::BTreeMap;
use std::env;
use std::path::PathBuf;
use std::time::Duration;

use cy_kernel_contract::Identity;
use cy_proto::core_v1::NodeRef;
use cy_runtime_agent::{run_runtime_agent, RuntimeAgentConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    run_runtime_agent(Args::parse()?.into_config()?).await?;
    Ok(())
}

struct Args {
    control_plane: String,
    server_name: String,
    control_ca: PathBuf,
    client_certificate: PathBuf,
    client_key: PathBuf,
    artifact_ca: PathBuf,
    artifact_ticket_key: Option<PathBuf>,
    organization_id: String,
    workspace_id: String,
    node_id: String,
    node_epoch: u64,
    node_type: String,
    persistent: bool,
    runtime_id: String,
    runtime_generation: u64,
    enrollment_proof: String,
    resume_token: String,
    state_dir: PathBuf,
    artifact_root: PathBuf,
    workload: Vec<String>,
}

impl Args {
    fn parse() -> Result<Self, Box<dyn std::error::Error>> {
        let arguments = env::args().skip(1).collect::<Vec<_>>();
        if arguments.first().map(String::as_str) != Some("run") {
            return Err(usage().into());
        }
        let separator = arguments
            .iter()
            .position(|argument| argument == "--")
            .ok_or_else(usage)?;
        let workload = arguments[separator + 1..].to_vec();
        if workload.is_empty() {
            return Err(usage().into());
        }
        let bootstrap = flag_value(&arguments[1..separator], "--bootstrap-file")
            .map(|path| load_bootstrap(&PathBuf::from(path)))
            .transpose()?
            .unwrap_or_default();
        let setting = |key: &str| {
            bootstrap
                .get(key)
                .cloned()
                .or_else(|| env::var(key).ok())
                .filter(|value| !value.is_empty())
        };
        let value = |name: &str, environment: &str| -> Result<String, std::io::Error> {
            flag_value(&arguments[1..separator], name)
                .or_else(|| setting(environment))
                .ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        format!("{name} or {environment} is required"),
                    )
                })
        };
        Ok(Self {
            control_plane: value("--control-plane", "CYRENE_CONTROL_PLANE_ENDPOINT")?,
            server_name: value(
                "--control-plane-server-name",
                "CYRENE_CONTROL_PLANE_SERVER_NAME",
            )?,
            control_ca: value("--control-plane-ca", "CYRENE_CONTROL_PLANE_CA")
                .map(PathBuf::from)?,
            client_certificate: value("--client-certificate", "CYRENE_AGENT_CLIENT_CERT")
                .map(PathBuf::from)?,
            client_key: value("--client-key", "CYRENE_AGENT_CLIENT_KEY").map(PathBuf::from)?,
            artifact_ca: value("--artifact-ca", "CYRENE_ARTIFACT_CA").map(PathBuf::from)?,
            artifact_ticket_key: flag_value(&arguments[1..separator], "--artifact-ticket-key")
                .or_else(|| setting("CYRENE_ARTIFACT_TICKET_KEY"))
                .filter(|value| !value.is_empty())
                .map(PathBuf::from),
            organization_id: value("--organization-id", "CYRENE_ORGANIZATION_ID")?,
            workspace_id: value("--workspace-id", "CYRENE_WORKSPACE_ID")?,
            node_id: value("--node-id", "CYRENE_NODE_ID")?,
            node_epoch: value("--node-epoch", "CYRENE_NODE_EPOCH")?.parse()?,
            node_type: value("--node-type", "CYRENE_NODE_TYPE")?,
            persistent: parse_bool(&value("--persistent", "CYRENE_NODE_PERSISTENT")?)?,
            runtime_id: value("--runtime-id", "CYRENE_RUNTIME_ID")?,
            runtime_generation: value("--runtime-generation", "CYRENE_RUNTIME_GENERATION")?
                .parse()?,
            enrollment_proof: flag_value(&arguments[1..separator], "--enrollment-proof")
                .or_else(|| setting("CYRENE_ENROLLMENT_PROOF"))
                .unwrap_or_default(),
            resume_token: flag_value(&arguments[1..separator], "--resume-token")
                .or_else(|| setting("CYRENE_AGENT_RESUME_TOKEN"))
                .unwrap_or_default(),
            state_dir: flag_value(&arguments[1..separator], "--state-dir")
                .or_else(|| setting("CYRENE_AGENT_STATE_DIR"))
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/var/lib/cyrene/runtime-agent")),
            artifact_root: flag_value(&arguments[1..separator], "--artifact-root")
                .or_else(|| setting("CYRENE_ARTIFACT_ROOT"))
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/var/lib/cyrene/artifacts")),
            workload,
        })
    }

    fn into_config(self) -> Result<RuntimeAgentConfig, Box<dyn std::error::Error>> {
        let config = RuntimeAgentConfig {
            control_plane_endpoint: self.control_plane,
            control_plane_server_name: self.server_name,
            control_plane_ca: self.control_ca,
            client_certificate: self.client_certificate,
            client_key: self.client_key,
            artifact_ca: self.artifact_ca,
            artifact_ticket_key: self.artifact_ticket_key,
            organization_id: self.organization_id,
            workspace_id: self.workspace_id,
            node: NodeRef {
                node_id: self.node_id,
                node_epoch: self.node_epoch,
            },
            node_type: self.node_type,
            persistent: self.persistent,
            runtime: Identity {
                id: self.runtime_id,
                generation: self.runtime_generation,
            },
            agent_version: env!("CARGO_PKG_VERSION").to_string(),
            enrollment_proof: self.enrollment_proof,
            resume_token: self.resume_token,
            state_dir: self.state_dir,
            artifact_destination_root: self.artifact_root,
            reconnect_min: Duration::from_millis(100),
            reconnect_max: Duration::from_secs(5),
            workload: self.workload,
        };
        config.validate()?;
        Ok(config)
    }
}

// Plain KEY=value, never sourced by a shell or copied to the process environment.
// Bootstrap secrets can be mounted read-only without appearing in Docker inspect.
fn load_bootstrap(path: &std::path::Path) -> Result<BTreeMap<String, String>, std::io::Error> {
    use std::io::{Error, ErrorKind};
    let invalid = || Error::new(ErrorKind::InvalidInput, "invalid Runtime bootstrap file");
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 65536 {
        return Err(invalid());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(invalid());
        }
    }
    let mut values = BTreeMap::new();
    for line in std::fs::read_to_string(path)?.lines() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let (key, value) = line.split_once('=').ok_or_else(invalid)?;
        if ![
            "CYRENE_CONTROL_PLANE_ENDPOINT",
            "CYRENE_CONTROL_PLANE_SERVER_NAME",
            "CYRENE_CONTROL_PLANE_CA",
            "CYRENE_AGENT_CLIENT_CERT",
            "CYRENE_AGENT_CLIENT_KEY",
            "CYRENE_ARTIFACT_CA",
            "CYRENE_ARTIFACT_TICKET_KEY",
            "CYRENE_ORGANIZATION_ID",
            "CYRENE_WORKSPACE_ID",
            "CYRENE_NODE_ID",
            "CYRENE_NODE_EPOCH",
            "CYRENE_NODE_TYPE",
            "CYRENE_NODE_PERSISTENT",
            "CYRENE_RUNTIME_ID",
            "CYRENE_RUNTIME_GENERATION",
            "CYRENE_ENROLLMENT_PROOF",
            "CYRENE_AGENT_RESUME_TOKEN",
            "CYRENE_AGENT_STATE_DIR",
            "CYRENE_ARTIFACT_ROOT",
        ]
        .contains(&key)
            || value.contains('\0')
            || values.insert(key.into(), value.into()).is_some()
        {
            return Err(invalid());
        }
    }
    Ok(values)
}

fn flag_value(arguments: &[String], name: &str) -> Option<String> {
    arguments
        .windows(2)
        .find_map(|pair| (pair[0] == name).then(|| pair[1].clone()))
}

fn parse_bool(value: &str) -> Result<bool, std::io::Error> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "persistent must be explicitly true or false",
        )),
    }
}

fn usage() -> std::io::Error {
    std::io::Error::other(
        "usage: cy-runtime-agent run [control and identity options] -- <fixed workload command>",
    )
}

#[cfg(test)]
mod bootstrap_tests {
    use super::*;
    #[test]
    fn reads_literals_without_shell_expansion_and_rejects_ambiguous_or_public_files() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("bootstrap.env");
        std::fs::write(
            &path,
            "CYRENE_ENROLLMENT_PROOF=$(do-not-execute)=secret\nCYRENE_NODE_TYPE=container\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert!(load_bootstrap(&path).is_err());
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        assert_eq!(
            load_bootstrap(&path).unwrap()["CYRENE_ENROLLMENT_PROOF"],
            "$(do-not-execute)=secret"
        );
        std::fs::write(&path, "CYRENE_NODE_TYPE=container\nCYRENE_NODE_TYPE=host\n").unwrap();
        assert!(load_bootstrap(&path).is_err());
        std::fs::write(&path, "LD_PRELOAD=untrusted\n").unwrap();
        assert!(load_bootstrap(&path).is_err());
    }
}
