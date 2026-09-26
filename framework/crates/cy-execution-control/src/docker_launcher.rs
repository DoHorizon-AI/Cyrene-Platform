//! Administrator-configured Docker launch after canonical Lease acquisition.
//! No Docker endpoint, command or credential is accepted from a graph document.
use std::{
    collections::BTreeMap,
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use cy_execution_fabric::validate_assignment;
use cy_kernel_contract::{Identity, Lease};
use cy_proto::core_v1::{NodeRef, RuntimeAssignment};
use prost::Message;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::{DispatchError, ExecutionSessionStore, FileExecutionSessionStore, RuntimeLauncher};

#[derive(Clone, Serialize, Deserialize)]
pub struct DockerResourceBinding {
    pub resource_id: String,
    pub generation: u64,
    /// Exact GPU UUID admitted by the trusted Node provider, never "all".
    /// Non-GPU resources must have None and are bounded by CPU/memory limits.
    pub gpu_uuid: Option<String>,
}

/// One immutable, caller-persisted Runtime generation. The image contains the
/// Runtime Agent; /run/cyrene/bootstrap.env supplies its mTLS/enrollment settings.
/// Paths refer to the daemon's host, and the named Docker context is preconfigured
/// by its administrator. This type must never be exposed as a public HTTP DTO.
#[derive(Clone, Serialize, Deserialize)]
pub struct DockerLaunchConfig {
    pub docker_binary: PathBuf,
    pub docker_context: String,
    pub node_id: String,
    pub node_epoch: u64,
    pub runtime_id: String,
    pub runtime_generation: u64,
    pub image: String,
    pub resolved_digest: String,
    pub bootstrap_directory: String,
    pub network: String,
    pub user: String,
    pub memory_bytes: u64,
    pub cpu_millis: u32,
    pub resources: Vec<DockerResourceBinding>,
    pub workload: Vec<String>,
}

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
enum Phase {
    CreateRequested,
    StartRequested,
}
#[derive(Clone, Serialize, Deserialize)]
struct Record {
    fingerprint: String,
    phase: Phase,
}
#[derive(Default, Serialize, Deserialize)]
struct Ledger {
    version: u32,
    records: BTreeMap<String, Record>,
}
type CommandFuture<'a> = Pin<Box<dyn Future<Output = Result<Vec<u8>, DispatchError>> + Send + 'a>>;
trait DockerCommand: Send + Sync {
    fn execute<'a>(&'a self, args: Vec<String>) -> CommandFuture<'a>;
}
struct Cli {
    binary: PathBuf,
    context: String,
}
impl DockerCommand for Cli {
    fn execute<'a>(&'a self, args: Vec<String>) -> CommandFuture<'a> {
        Box::pin(async move {
            let output = tokio::time::timeout(
                Duration::from_secs(30),
                tokio::process::Command::new(&self.binary)
                    .args(["--context", &self.context])
                    .args(args)
                    .stdin(std::process::Stdio::null())
                    .kill_on_drop(true)
                    .output(),
            )
            .await
            .map_err(|_| unknown())?
            .map_err(|_| unknown())?;
            // Docker errors can contain paths or bootstrap credentials. Do not
            // include raw stdout/stderr in the control-plane error ledger.
            if !output.status.success() || output.stdout.len() > 1_048_576 {
                return Err(unknown());
            }
            Ok(output.stdout)
        })
    }
}

pub struct DockerRuntimeLauncher {
    config: DockerLaunchConfig,
    store: Arc<dyn ExecutionSessionStore>,
    ledger: Mutex<Ledger>,
    command: Arc<dyn DockerCommand>,
}

impl DockerRuntimeLauncher {
    /// The private ledger directory must be distinct from the control session
    /// directory. Keep it and the task volumes across controller replacement.
    pub fn open(
        config: DockerLaunchConfig,
        directory: impl Into<PathBuf>,
    ) -> Result<Self, DispatchError> {
        validate_config(&config)?;
        let store = Arc::new(FileExecutionSessionStore::open(directory)?);
        let command = Arc::new(Cli {
            binary: config.docker_binary.clone(),
            context: config.docker_context.clone(),
        });
        Self::restore(config, store, command)
    }
    fn restore(
        config: DockerLaunchConfig,
        store: Arc<dyn ExecutionSessionStore>,
        command: Arc<dyn DockerCommand>,
    ) -> Result<Self, DispatchError> {
        validate_config(&config)?;
        let ledger = match store.load()? {
            Some(bytes) => serde_json::from_slice::<Ledger>(&bytes).map_err(|_| unknown())?,
            None => Ledger {
                version: 1,
                records: BTreeMap::new(),
            },
        };
        if ledger.version != 1 {
            return Err(unknown());
        }
        Ok(Self {
            config,
            store,
            ledger: Mutex::new(ledger),
            command,
        })
    }
    fn name(&self) -> String {
        let key = serde_json::to_vec(&(
            &self.config.node_id,
            self.config.node_epoch,
            &self.config.runtime_id,
            self.config.runtime_generation,
        ))
        .expect("identity serialization");
        format!("cyrene-runtime-{:x}", Sha256::digest(key))
    }
    fn persist(&self, ledger: &Ledger) -> Result<(), DispatchError> {
        self.store
            .save(&serde_json::to_vec(ledger).map_err(|_| unknown())?)
    }
    async fn inspect(
        &self,
        name: &str,
        fingerprint: &str,
    ) -> Result<Option<String>, DispatchError> {
        let listed = self
            .command
            .execute(vec![
                "container".into(),
                "ls".into(),
                "--all".into(),
                "--filter".into(),
                format!("name=^/{name}$"),
                "--format".into(),
                "{{.ID}}".into(),
            ])
            .await?;
        if listed.iter().all(u8::is_ascii_whitespace) {
            return Ok(None);
        }
        let raw = self
            .command
            .execute(vec!["container".into(), "inspect".into(), name.into()])
            .await?;
        let values: Vec<serde_json::Value> = serde_json::from_slice(&raw).map_err(|_| unknown())?;
        let value = values
            .first()
            .filter(|_| values.len() == 1)
            .ok_or_else(unknown)?;
        if value["Name"].as_str() != Some(&format!("/{name}"))
            || value["Config"]["Image"].as_str() != Some(&self.config.image)
            || value["Config"]["Labels"]["cyrene.launch.fingerprint"].as_str() != Some(fingerprint)
        {
            return Err(unknown());
        }
        Ok(Some(
            value["State"]["Status"]
                .as_str()
                .ok_or_else(unknown)?
                .into(),
        ))
    }
    fn create_args(&self, name: &str, fingerprint: &str, lease: &Lease) -> Vec<String> {
        let config = &self.config;
        let mut args: Vec<String> = [
            "container",
            "create",
            "--name",
            name,
            "--pull",
            "never",
            "--restart",
            "no",
            "--read-only",
            "--cap-drop",
            "ALL",
            "--security-opt",
            "no-new-privileges",
            "--pids-limit",
            "512",
            "--user",
            &config.user,
            "--network",
            &config.network,
            "--entrypoint",
            "/usr/local/bin/cy-runtime-agent",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        args.extend([
            "--memory".into(),
            config.memory_bytes.to_string(),
            "--cpus".into(),
            format!(
                "{}.{:03}",
                config.cpu_millis / 1000,
                config.cpu_millis % 1000
            ),
            "--label".into(),
            format!("cyrene.launch.fingerprint={fingerprint}"),
            "--tmpfs".into(),
            "/tmp:rw,nosuid,nodev,size=67108864".into(),
        ]);
        args.extend([
            "--mount".into(),
            format!(
                "type=bind,src={},dst=/run/cyrene,readonly",
                config.bootstrap_directory
            ),
            "--mount".into(),
            format!("type=volume,src={name}-state,dst=/var/lib/cyrene/runtime-agent"),
            "--mount".into(),
            format!("type=volume,src={name}-artifacts,dst=/var/lib/cyrene/artifacts"),
        ]);
        // No Docker socket, host PID namespace, host network, arbitrary mounts,
        // published ports, automatic restart, or unleased GPU can enter Runtime.
        for resource in &lease.resources {
            let binding = config
                .resources
                .iter()
                .find(|b| b.resource_id == resource.id && b.generation == resource.generation)
                .expect("validated resource");
            if let Some(uuid) = &binding.gpu_uuid {
                args.extend(["--gpus".into(), format!("device={uuid}")]);
            }
        }
        // The image entrypoint reads bootstrap.env itself; credentials never
        // enter argv, environment variables passed by Docker, or the ledger.
        args.extend([
            config.image.clone(),
            "run".into(),
            "--bootstrap-file".into(),
            "/run/cyrene/bootstrap.env".into(),
            "--node-id".into(),
            config.node_id.clone(),
            "--node-epoch".into(),
            config.node_epoch.to_string(),
            "--runtime-id".into(),
            config.runtime_id.clone(),
            "--runtime-generation".into(),
            config.runtime_generation.to_string(),
            "--state-dir".into(),
            "/var/lib/cyrene/runtime-agent".into(),
            "--artifact-root".into(),
            "/var/lib/cyrene/artifacts".into(),
            "--".into(),
        ]);
        args.extend(config.workload.clone());
        args
    }
    async fn launch_once(
        &self,
        node: &NodeRef,
        assignment: &RuntimeAssignment,
        lease: &Lease,
    ) -> Result<(), DispatchError> {
        // All errors here conservatively retain authority correlation. A prior
        // invocation may already have created an instance of this generation.
        let mut ledger = self.ledger.lock().await;
        let config = &self.config;
        let runtime = Identity {
            id: config.runtime_id.clone(),
            generation: config.runtime_generation,
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| unknown())?
            .as_millis() as u64;
        if node.node_id != config.node_id
            || node.node_epoch != config.node_epoch
            || validate_assignment(&runtime, assignment, now).map_err(|_| unknown())? != *lease
            || assignment.profile.as_ref().is_none_or(|p| {
                config.image.rsplit_once('@').map(|(_, d)| d) != Some(p.image_digest.as_str())
                    || p.resolved_digest != config.resolved_digest
            })
            || lease.resources.iter().any(|r| {
                !config
                    .resources
                    .iter()
                    .any(|b| b.resource_id == r.id && b.generation == r.generation)
            })
        {
            return Err(unknown());
        }
        let name = self.name();
        let mut hash = Sha256::new();
        hash.update(assignment.encode_to_vec());
        hash.update(serde_json::to_vec(config).map_err(|_| unknown())?);
        let fingerprint = format!("{:x}", hash.finalize());
        if ledger
            .records
            .get(&name)
            .is_some_and(|r| r.fingerprint != fingerprint)
        {
            return Err(unknown());
        }
        let existing = ledger.records.contains_key(&name);
        let status = self.inspect(&name, &fingerprint).await?;
        if !existing {
            // Even a correctly labelled orphan is not adopted without our own
            // durable intent. A name collision must be reconciled explicitly.
            if status.is_some() {
                return Err(unknown());
            }
            ledger.records.insert(
                name.clone(),
                Record {
                    fingerprint: fingerprint.clone(),
                    phase: Phase::CreateRequested,
                },
            );
            self.persist(&ledger)?;
            self.command
                .execute(self.create_args(&name, &fingerprint, lease))
                .await?;
        } else {
            match status.as_deref() {
                Some("running") if ledger.records[&name].phase == Phase::StartRequested => {
                    return Ok(())
                }
                Some("created") if ledger.records[&name].phase == Phase::CreateRequested => {}
                // Never recreate a missing/deleted container, restart an exited
                // one, or replay a start with unknown outcome.
                _ => return Err(unknown()),
            }
        }
        if self.inspect(&name, &fingerprint).await?.as_deref() != Some("created") {
            return Err(unknown());
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| unknown())?
            .as_millis() as u64;
        validate_assignment(&runtime, assignment, now).map_err(|_| unknown())?;
        ledger.records.get_mut(&name).expect("intent exists").phase = Phase::StartRequested;
        self.persist(&ledger)?;
        self.command
            .execute(vec!["container".into(), "start".into(), name])
            .await?;
        Ok(())
    }
}
impl RuntimeLauncher for DockerRuntimeLauncher {
    fn launch<'a>(
        &'a self,
        node: &'a NodeRef,
        assignment: &'a RuntimeAssignment,
        lease: &'a Lease,
    ) -> Pin<Box<dyn Future<Output = Result<(), DispatchError>> + Send + 'a>> {
        Box::pin(self.launch_once(node, assignment, lease))
    }

    fn reconcile<'a>(
        &'a self,
        node: &'a NodeRef,
        assignment: &'a RuntimeAssignment,
        lease: &'a Lease,
    ) -> Pin<Box<dyn Future<Output = Result<(), DispatchError>> + Send + 'a>> {
        // launch_once is a persisted state machine. With an existing ledger it
        // can observe the same running container or continue CreateRequested;
        // it refuses missing, exited, foreign, or ambiguous instances.
        Box::pin(self.launch_once(node, assignment, lease))
    }
}
fn unknown() -> DispatchError {
    DispatchError::unknown(
        "Docker launch requires reconciliation of the original Runtime generation",
    )
}
fn digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|v| {
        v.len() == 64
            && v.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}
fn name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
        && !value.starts_with('-')
}
fn validate_config(c: &DockerLaunchConfig) -> Result<(), DispatchError> {
    let image = c.image.rsplit_once('@');
    if image.is_none_or(|(repo, d)| {
        repo.is_empty()
            || repo.starts_with('-')
            || !digest(d)
            || !repo
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"/_.:-".contains(&b))
    }) || !digest(&c.resolved_digest)
        || !name(&c.docker_context)
        || !name(&c.network)
        || ["host", "none", "bridge"].contains(&c.network.as_str())
        || c.node_id.is_empty()
        || c.node_epoch == 0
        || c.runtime_id.is_empty()
        || c.runtime_generation == 0
        || c.memory_bytes < 16 * 1024 * 1024
        || c.cpu_millis == 0
        || c.resources.is_empty()
        || c.workload.is_empty()
        || c.workload.len() > 128
        || c.workload
            .iter()
            .any(|a| a.len() > 8192 || a.contains('\0'))
        || !c.bootstrap_directory.starts_with('/')
        || c.bootstrap_directory.len() < 2
        || c.bootstrap_directory.contains([',', '\n', '\0'])
        || c.bootstrap_directory.split('/').any(|part| part == "..")
        || c.user.split_once(':').is_none_or(|(uid, gid)| {
            uid.parse::<u32>().ok().is_none_or(|u| u == 0) || gid.parse::<u32>().is_err()
        })
        || c.resources.iter().any(|r| {
            r.resource_id.is_empty()
                || r.generation == 0
                || r.gpu_uuid
                    .as_ref()
                    .is_some_and(|g| !g.starts_with("GPU-") || !name(g))
        })
    {
        return Err(DispatchError::input("DOCKER_LAUNCH_CONFIG_INVALID", "Docker launch requires a pinned image, scoped identities, non-root user, named network and bounded resources"));
    }
    let mut ids = std::collections::BTreeSet::new();
    let mut devices = std::collections::BTreeSet::new();
    if c.resources.iter().any(|r| {
        !ids.insert((&r.resource_id, r.generation))
            || r.gpu_uuid.as_ref().is_some_and(|g| !devices.insert(g))
    }) {
        return Err(DispatchError::input(
            "DOCKER_RESOURCE_DUPLICATE",
            "Docker resource identities and GPU devices must be unique",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
