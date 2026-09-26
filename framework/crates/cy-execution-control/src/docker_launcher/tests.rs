use super::*;
use cy_execution_fabric::RuntimeAssignmentBuilder;
use cy_proto::{core_v1, semantic_v1};
use std::sync::Mutex as SyncMutex;

#[derive(Default)]
struct Store {
    bytes: SyncMutex<Option<Vec<u8>>>,
    fail: SyncMutex<bool>,
}
impl ExecutionSessionStore for Store {
    fn load(&self) -> Result<Option<Vec<u8>>, DispatchError> {
        Ok(self.bytes.lock().unwrap().clone())
    }
    fn save(&self, bytes: &[u8]) -> Result<(), DispatchError> {
        if *self.fail.lock().unwrap() {
            return Err(unknown());
        }
        *self.bytes.lock().unwrap() = Some(bytes.to_vec());
        Ok(())
    }
}
#[derive(Default)]
struct DockerState {
    calls: Vec<Vec<String>>,
    container: Option<serde_json::Value>,
    lose_create: bool,
    lose_start: bool,
    create_absent: bool,
}
#[derive(Default)]
struct Docker {
    state: SyncMutex<DockerState>,
}
impl DockerCommand for Docker {
    fn execute<'a>(&'a self, args: Vec<String>) -> CommandFuture<'a> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            state.calls.push(args.clone());
            let flag = |key: &str| {
                args.windows(2)
                    .find(|a| a[0] == key)
                    .map(|a| a[1].clone())
                    .unwrap()
            };
            match args[1].as_str() {
                "ls" => Ok(if state.container.is_some() {
                    b"container-id\n".to_vec()
                } else {
                    vec![]
                }),
                "inspect" => {
                    Ok(serde_json::to_vec(&vec![state.container.clone().unwrap()]).unwrap())
                }
                "create" => {
                    let image = args
                        .iter()
                        .find(|a| a.contains("@sha256:"))
                        .unwrap()
                        .clone();
                    let fingerprint = flag("--label")
                        .strip_prefix("cyrene.launch.fingerprint=")
                        .unwrap()
                        .to_string();
                    if !state.create_absent {
                        state.container = Some(
                            serde_json::json!({ "Name": format!("/{}", flag("--name")), "Config": { "Image": image, "Labels": { "cyrene.launch.fingerprint": fingerprint } }, "State": { "Status": "created" } }),
                        );
                    }
                    if state.lose_create {
                        Err(unknown())
                    } else {
                        Ok(b"container-id".to_vec())
                    }
                }
                "start" => {
                    state.container.as_mut().unwrap()["State"]["Status"] = "running".into();
                    if state.lose_start {
                        Err(unknown())
                    } else {
                        Ok(b"container-id".to_vec())
                    }
                }
                _ => panic!("unexpected Docker operation"),
            }
        })
    }
}
fn config() -> DockerLaunchConfig {
    DockerLaunchConfig {
        docker_binary: "docker".into(),
        docker_context: "node-alpha".into(),
        node_id: "node-alpha".into(),
        node_epoch: 1,
        runtime_id: "runtime-alpha".into(),
        runtime_generation: 1,
        image: format!("registry.example/runtime@sha256:{}", "1".repeat(64)),
        resolved_digest: format!("sha256:{}", "2".repeat(64)),
        bootstrap_directory: "/private/runtime-alpha".into(),
        network: "cyrene-runtime".into(),
        user: "10001:10001".into(),
        memory_bytes: 256 * 1024 * 1024,
        cpu_millis: 1250,
        resources: vec![
            DockerResourceBinding {
                resource_id: "gpu-alpha".into(),
                generation: 1,
                gpu_uuid: Some("GPU-alpha".into()),
            },
            DockerResourceBinding {
                resource_id: "gpu-beta".into(),
                generation: 1,
                gpu_uuid: Some("GPU-beta".into()),
            },
        ],
        workload: vec![
            "/usr/local/bin/workload".into(),
            "literal;not-a-shell".into(),
        ],
    }
}
fn assignment() -> (NodeRef, RuntimeAssignment, Lease) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let runtime = Identity {
        id: "runtime-alpha".into(),
        generation: 1,
    };
    let runtime_ref = core_v1::RuntimeRef {
        identity: Some(semantic_v1::Identity {
            id: runtime.id.clone(),
            generation: 1,
        }),
    };
    let lease = Lease {
        identity: Identity {
            id: "lease-alpha".into(),
            generation: 1,
        },
        holder: runtime.clone(),
        resources: vec![Identity {
            id: "gpu-alpha".into(),
            generation: 1,
        }],
        state: cy_kernel_contract::LeaseState::Active,
        fence_token: 1,
        expires_at_unix_ms: Some(now + 120_000),
    };
    let workload = core_v1::WorkloadIdentity {
        identity: Some(semantic_v1::Identity {
            id: "workload-alpha".into(),
            generation: 1,
        }),
        scope: Some(core_v1::AccountScope {
            user_id: "user-alpha".into(),
            organization_id: "organization-alpha".into(),
            workspace_id: "workspace-alpha".into(),
        }),
        runtime: Some(runtime_ref),
        allowed_actions: vec!["operation.report".into()],
        expires_at: Some(prost_types::Timestamp {
            seconds: ((now + 120_000) / 1000) as i64,
            nanos: 0,
        }),
    };
    let assignment = RuntimeAssignmentBuilder::new(
        "assignment-alpha",
        runtime,
        Identity {
            id: "operation-alpha".into(),
            generation: 1,
        },
        "attempt-alpha",
        workload,
        core_v1::RuntimeProfile {
            image_digest: format!("sha256:{}", "1".repeat(64)),
            resolved_digest: format!("sha256:{}", "2".repeat(64)),
        },
        vec![],
    )
    .build(&lease, now)
    .unwrap();
    (
        NodeRef {
            node_id: "node-alpha".into(),
            node_epoch: 1,
        },
        assignment,
        lease,
    )
}
fn count(docker: &Docker, command: &str) -> usize {
    docker
        .state
        .lock()
        .unwrap()
        .calls
        .iter()
        .filter(|a| a[1] == command)
        .count()
}

#[tokio::test]
async fn launch_and_restart_use_one_container_and_only_leased_devices() {
    let store = Arc::new(Store::default());
    let docker = Arc::new(Docker::default());
    let (node, assignment, lease) = assignment();
    let launcher = DockerRuntimeLauncher::restore(config(), store.clone(), docker.clone()).unwrap();
    let (a, b) = tokio::join!(
        launcher.launch(&node, &assignment, &lease),
        launcher.launch(&node, &assignment, &lease)
    );
    a.unwrap();
    b.unwrap();
    drop(launcher);
    let restarted = DockerRuntimeLauncher::restore(config(), store, docker.clone()).unwrap();
    restarted.launch(&node, &assignment, &lease).await.unwrap();
    assert_eq!(count(&docker, "create"), 1);
    assert_eq!(count(&docker, "start"), 1);
    let state = docker.state.lock().unwrap();
    let args = state.calls.iter().find(|a| a[1] == "create").unwrap();
    assert!(args.contains(&"device=GPU-alpha".into()));
    assert!(!args.contains(&"device=GPU-beta".into()));
    assert!(args.contains(&"--read-only".into()));
    assert!(!args
        .iter()
        .any(|a| a.contains("docker.sock") || a == "--privileged"));
    assert_eq!(args.last().unwrap(), "literal;not-a-shell");
}
#[tokio::test]
async fn lost_create_response_reconciles_the_original_container() {
    let store = Arc::new(Store::default());
    let docker = Arc::new(Docker::default());
    let (node, assignment, lease) = assignment();
    docker.state.lock().unwrap().lose_create = true;
    let launcher = DockerRuntimeLauncher::restore(config(), store.clone(), docker.clone()).unwrap();
    assert!(
        launcher
            .launch(&node, &assignment, &lease)
            .await
            .unwrap_err()
            .reconciliation_required
    );
    drop(launcher);
    let restarted = DockerRuntimeLauncher::restore(config(), store, docker.clone()).unwrap();
    restarted.launch(&node, &assignment, &lease).await.unwrap();
    assert_eq!(count(&docker, "create"), 1);
    assert_eq!(count(&docker, "start"), 1);
}
#[tokio::test]
async fn unknown_absence_never_recreates_and_exited_containers_never_restart() {
    let (node, assignment, lease) = assignment();
    for absence in [true, false] {
        let store = Arc::new(Store::default());
        let docker = Arc::new(Docker::default());
        {
            let mut state = docker.state.lock().unwrap();
            state.create_absent = absence;
            state.lose_create = absence;
        }
        let launcher =
            DockerRuntimeLauncher::restore(config(), store.clone(), docker.clone()).unwrap();
        let _ = launcher.launch(&node, &assignment, &lease).await;
        drop(launcher);
        if !absence {
            docker.state.lock().unwrap().container.as_mut().unwrap()["State"]["Status"] =
                "exited".into();
        }
        let restarted = DockerRuntimeLauncher::restore(config(), store, docker.clone()).unwrap();
        assert!(
            restarted
                .launch(&node, &assignment, &lease)
                .await
                .unwrap_err()
                .reconciliation_required
        );
        assert_eq!(count(&docker, "create"), 1);
        assert_eq!(count(&docker, "start"), usize::from(!absence));
    }
}
#[tokio::test]
async fn lost_start_response_is_observed_without_starting_again() {
    let store = Arc::new(Store::default());
    let docker = Arc::new(Docker::default());
    let (node, assignment, lease) = assignment();
    docker.state.lock().unwrap().lose_start = true;
    let launcher = DockerRuntimeLauncher::restore(config(), store.clone(), docker.clone()).unwrap();
    assert!(launcher.launch(&node, &assignment, &lease).await.is_err());
    drop(launcher);
    let restarted = DockerRuntimeLauncher::restore(config(), store, docker.clone()).unwrap();
    restarted.launch(&node, &assignment, &lease).await.unwrap();
    assert_eq!(count(&docker, "start"), 1);
}
#[tokio::test]
async fn rejects_identity_and_lease_changes_before_docker_and_persists_before_create() {
    let store = Arc::new(Store::default());
    let docker = Arc::new(Docker::default());
    let (mut node, assignment, lease) = assignment();
    let launcher = DockerRuntimeLauncher::restore(config(), store.clone(), docker.clone()).unwrap();
    node.node_epoch = 2;
    assert!(launcher.launch(&node, &assignment, &lease).await.is_err());
    assert!(docker.state.lock().unwrap().calls.is_empty());
    node.node_epoch = 1;
    let mut wrong = lease.clone();
    wrong.fence_token = 2;
    assert!(launcher.launch(&node, &assignment, &wrong).await.is_err());
    assert!(docker.state.lock().unwrap().calls.is_empty());
    *store.fail.lock().unwrap() = true;
    assert!(launcher.launch(&node, &assignment, &lease).await.is_err());
    assert_eq!(count(&docker, "create"), 0);
}
#[tokio::test]
async fn changed_configuration_and_foreign_container_cannot_take_over_a_generation() {
    let store = Arc::new(Store::default());
    let docker = Arc::new(Docker::default());
    let (node, assignment, lease) = assignment();
    let launcher = DockerRuntimeLauncher::restore(config(), store.clone(), docker.clone()).unwrap();
    launcher.launch(&node, &assignment, &lease).await.unwrap();
    drop(launcher);
    let mut changed = config();
    changed.workload.push("different".into());
    let replaced = DockerRuntimeLauncher::restore(changed, store.clone(), docker.clone()).unwrap();
    assert!(replaced.launch(&node, &assignment, &lease).await.is_err());
    drop(replaced);
    docker.state.lock().unwrap().container.as_mut().unwrap()["Config"]["Labels"]
        ["cyrene.launch.fingerprint"] = "foreign".into();
    let original = DockerRuntimeLauncher::restore(config(), store, docker.clone()).unwrap();
    assert!(original.launch(&node, &assignment, &lease).await.is_err());
    assert_eq!(count(&docker, "create"), 1);
    assert_eq!(count(&docker, "start"), 1);
}
#[test]
fn configuration_rejects_mutable_images_and_privileged_placement() {
    let mut c = config();
    c.image = "runtime:latest".into();
    assert!(validate_config(&c).is_err());
    c = config();
    c.user = "0:0".into();
    assert!(validate_config(&c).is_err());
    c = config();
    c.network = "host".into();
    assert!(validate_config(&c).is_err());
    c = config();
    c.resources[0].gpu_uuid = Some("all".into());
    assert!(validate_config(&c).is_err());
}

/// Opt-in real Engine check. No assignment is dispatched and no GPU is used;
/// the real Agent waits for a deliberately unavailable control endpoint.
#[tokio::test]
#[ignore = "requires explicit isolated Docker image and bootstrap fixture"]
async fn real_docker_container_survives_launcher_reopen_without_duplicate_start() {
    let mut c = config();
    c.image =
        std::env::var("CYRENE_DOCKER_LAUNCH_TEST_IMAGE").expect("pinned locally built Agent image");
    c.bootstrap_directory = std::env::var("CYRENE_DOCKER_LAUNCH_BOOTSTRAP_DIR")
        .expect("private bootstrap directory on daemon host");
    c.docker_context = "default".into();
    c.runtime_id = format!("runtime-{}", uuid::Uuid::new_v4());
    c.network = format!("cyrene-launch-test-{}", uuid::Uuid::new_v4());
    for binding in &mut c.resources {
        binding.gpu_uuid = None;
    }
    let (node, mut assignment, mut lease) = assignment();
    lease.holder.id = c.runtime_id.clone();
    assignment
        .runtime
        .as_mut()
        .unwrap()
        .identity
        .as_mut()
        .unwrap()
        .id = c.runtime_id.clone();
    assignment
        .workload_identity
        .as_mut()
        .unwrap()
        .runtime
        .as_mut()
        .unwrap()
        .identity
        .as_mut()
        .unwrap()
        .id = c.runtime_id.clone();
    assignment
        .lease
        .as_mut()
        .unwrap()
        .holder
        .as_mut()
        .unwrap()
        .id = c.runtime_id.clone();
    assignment.profile.as_mut().unwrap().image_digest = c.image.rsplit_once('@').unwrap().1.into();
    let cli = Cli {
        binary: c.docker_binary.clone(),
        context: c.docker_context.clone(),
    };
    cli.execute(vec!["network".into(), "create".into(), c.network.clone()])
        .await
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    let launcher = DockerRuntimeLauncher::open(c.clone(), root.path().join("ledger")).unwrap();
    let container = launcher.name();
    struct Cleanup {
        container: String,
        network: String,
    }
    impl Drop for Cleanup {
        fn drop(&mut self) {
            for args in [
                vec![
                    "container".into(),
                    "rm".into(),
                    "--force".into(),
                    self.container.clone(),
                ],
                vec![
                    "volume".into(),
                    "rm".into(),
                    format!("{}-state", self.container),
                    format!("{}-artifacts", self.container),
                ],
                vec!["network".into(), "rm".into(), self.network.clone()],
            ] {
                let _ = std::process::Command::new("docker")
                    .args(["--context", "default"])
                    .args(args)
                    .output();
            }
        }
    }
    let _cleanup = Cleanup {
        container: container.clone(),
        network: c.network.clone(),
    };
    launcher.launch(&node, &assignment, &lease).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let inspect = || {
        cli.execute(vec![
            "container".into(),
            "inspect".into(),
            container.clone(),
        ])
    };
    let before: Vec<serde_json::Value> = serde_json::from_slice(&inspect().await.unwrap()).unwrap();
    assert_eq!(before[0]["State"]["Running"], true);
    assert_eq!(before[0]["HostConfig"]["Privileged"], false);
    assert_eq!(before[0]["HostConfig"]["ReadonlyRootfs"], true);
    assert_eq!(before[0]["Config"]["User"], "10001:10001");
    drop(launcher);
    let reopened = DockerRuntimeLauncher::open(c, root.path().join("ledger")).unwrap();
    reopened.launch(&node, &assignment, &lease).await.unwrap();
    let after: Vec<serde_json::Value> = serde_json::from_slice(&inspect().await.unwrap()).unwrap();
    assert_eq!(before[0]["Id"], after[0]["Id"]);
    assert_eq!(
        before[0]["State"]["StartedAt"],
        after[0]["State"]["StartedAt"]
    );
    cli.execute(vec![
        "container".into(),
        "rm".into(),
        "--force".into(),
        container.clone(),
    ])
    .await
    .unwrap();
    assert!(
        reopened
            .launch(&node, &assignment, &lease)
            .await
            .unwrap_err()
            .reconciliation_required
    );
    assert!(cli
        .execute(vec![
            "container".into(),
            "ls".into(),
            "--all".into(),
            "--filter".into(),
            format!("name=^/{container}$"),
            "--format".into(),
            "{{.ID}}".into()
        ])
        .await
        .unwrap()
        .is_empty());
}
