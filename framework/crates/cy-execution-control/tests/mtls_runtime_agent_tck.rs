//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 mtls_runtime_agent_tck.rs                                       │
//! │  Module: cy_execution_control integration tests                     │
//! │  Role: Linux mTLS, Kernel UDS, Runtime Agent, and durable release.   │
//! │                                                                     │
//! │  模块：cy_execution_control 集成测试                                │
//! │  职责：验证 Linux 上真实 mTLS、Kernel UDS、Runtime Agent 与持久释放。 │
//! └─────────────────────────────────────────────────────────────────────┘

#![cfg(target_os = "linux")]

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs;
use std::future::Future;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cy_execution_control::{
    AuthenticatedAgent, CertificateFingerprintAuthenticator, DispatchReceipt,
    ExecutionControlService, ExecutionController, ExecutionDispatchRequest,
    ExecutionReleaseRequest, FileExecutionIntentStore, IntentDisposition,
};
use cy_execution_fabric::{
    execution_capability, plan_execution_placement, ArtifactAvailability, ArtifactPlacementQuote,
    DevelopmentEnrollmentProvider, ExecutionPlacementRequest, ExecutionTargetCandidate,
    NetworkRequirements, PlacementPolicy, RuntimeAssignmentBuilder,
};
use cy_kernel_api::{
    AuthorityCallContext, CleanupReport, DeviceBinding, HostInventoryProvider,
    InstalledPluginResolver, InventorySnapshot, LaunchPlan, NamespaceId, NodeCapabilities,
    ProcessHandle, ProcessRuntime, ProviderError, SandboxBackend, StopRequest,
    VerifiedInstallation,
};
use cy_kernel_contract as semantic;
use cy_kernel_daemon::{
    peer_cred::{inject_authority_principal, PeerCredAccept},
    KernelDaemon, KernelServiceAdapter,
};
use cy_manifest::{ArtifactKind, ArtifactRef};
use cy_node_agent::{NodeCommandBridge, NodeControlSession, UdsKernelCommandExecutor};
use cy_proto::core_v1 as core;
use cy_proto::core_v1::{
    node_control_service_client::NodeControlServiceClient,
    node_control_service_server::NodeControlServiceServer, node_to_control_plane,
    AssignmentAckDisposition, RuntimeObservedState, TerminationClassification,
};
use cy_resource_manager::InMemoryResourceManager;
use cy_runtime_agent::{run_runtime_agent, RuntimeAgentConfig};
use cyrene_linux_sys_adapter::LinuxSystemProvider;
use sha2::{Digest, Sha256};
use tempfile::tempdir;
use tokio::net::{TcpListener, UnixListener};
use tokio::sync::broadcast;
use tokio_stream::wrappers::{ReceiverStream, TcpListenerStream, UnixListenerStream};
use tonic::transport::{
    Certificate, Channel, ClientTlsConfig, Endpoint, Identity, Server, ServerTlsConfig,
};
use tonic::{Code, Request};

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

const NODE_ID: &str = "node-mtls-runtime-tck";
const NODE_EPOCH: u64 = 1;
const RUNTIME_ID: &str = "runtime-mtls-runtime-tck";
const ORGANIZATION_ID: &str = "organization-mtls-runtime-tck";
const WORKSPACE_ID: &str = "workspace-mtls-runtime-tck";
const CONTROL_SERVER_NAME: &str = "control.test";

/// Exercise the real Linux control path from certificate-authenticated peers
/// through a UDS Kernel lease to a Runtime Agent child process.
///
/// The test intentionally composes production components inside one test OS
/// process. It is not proof of an Agent OS-process restart, Docker, or a
/// multi-container deployment; the transport, credential, lease, assignment,
/// workload, and durable release boundaries are nevertheless real.
/// 中文：覆盖从证书认证的对端、经由 UDS 获取 Kernel 租约，到 Runtime Agent 子进程的真实 Linux 控制路径。
///
/// 中文：本测试有意在同一个测试操作系统进程中组合生产组件。它不能证明 Agent 操作系统进程重启、Docker 或多容器部署；不过传输、凭据、租约、分配、工作负载和持久化释放边界都是真实的。
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn mtls_runtime_agent_kernel_uds_and_durable_release_are_real() -> TestResult {
    let temporary = tempdir()?;
    let certificates = Certificates::generate(temporary.path())?;
    let provider = Arc::new(LinuxSystemProvider::new("linux-system-mtls-tck"));
    let inventory = provider.probe_inventory()?;
    assert!(
        inventory.capabilities.ready,
        "LinuxSystemProvider must report a ready Linux cgroup environment"
    );
    let cpu = inventory
        .resources
        .iter()
        .find(|resource| {
            resource.resource_class == "compute.cpu"
                && resource.state == semantic::ResourceState::Ready
        })
        .cloned()
        .ok_or("LinuxSystemProvider did not expose a ready compute.cpu Resource")?;

    let resources = Arc::new(InMemoryResourceManager::new(NODE_ID, vec![cpu.clone()]));
    resources.refresh_inventory(InventorySnapshot {
        generation: inventory.generation,
        resources: vec![cpu.clone()],
        capabilities: inventory.capabilities.clone(),
    })?;

    let kernel_socket = temporary.path().join("kernel.sock");
    let kernel_task = start_kernel(
        &kernel_socket,
        Arc::clone(&provider),
        Arc::clone(&resources),
    )
    .await?;

    let runtime = semantic::Identity {
        id: RUNTIME_ID.to_string(),
        generation: 1,
    };
    let authenticator = Arc::new(CertificateFingerprintAuthenticator::new([
        (
            certificates.host_der.clone(),
            AuthenticatedAgent::Host { node: node_ref() },
        ),
        (
            certificates.runtime_der.clone(),
            AuthenticatedAgent::Runtime {
                runtime: runtime.clone(),
                node: node_ref(),
            },
        ),
    ])?);
    let service = ExecutionControlService::new(
        authenticator,
        Arc::new(DevelopmentEnrollmentProvider::new(
            ["mtls-runtime-enrollment".to_string()],
            120_000,
        )),
        Duration::from_millis(100),
        Duration::from_secs(5),
    )?;
    let (control_endpoint, control_task) = start_control(service.clone(), &certificates).await?;

    assert_unbound_certificate_is_rejected(&control_endpoint, &certificates).await?;

    let mut host_task = tokio::spawn(run_host(
        control_endpoint.clone(),
        kernel_socket.clone(),
        certificates.clone(),
    ));
    wait_for_host_session(&service, &mut host_task).await?;

    let observations = service.subscribe();
    let intent_path = temporary.path().join("execution-intents.json");
    let runtime_state = temporary.path().join("runtime-state");
    let artifact_root = temporary.path().join("artifact-root");
    let local_artifact = artifact_ref(b"verified local Artifact input");
    fs::create_dir_all(&artifact_root)?;
    fs::write(
        artifact_root.join(&local_artifact.digest[7..]),
        b"verified local Artifact input",
    )?;
    let controller_store = Arc::new(FileExecutionIntentStore::open(&intent_path)?);
    let controller = ExecutionController::new(
        service.clone(),
        Duration::from_secs(5),
        controller_store.clone(),
    )?;

    let mut runtime_task = spawn_runtime_agent(runtime_config(
        &certificates,
        &control_endpoint,
        runtime_state.clone(),
        artifact_root.clone(),
        "CYRENE_REAL_WORKLOAD_ONE",
    ));
    wait_for_runtime_session(
        "authenticated Runtime session",
        &service,
        &runtime,
        &mut runtime_task,
    )
    .await?;

    let first_receipt = dispatch_with_context(
        &controller,
        "first workload",
        dispatch_request(
            &service,
            &runtime,
            &cpu,
            DispatchIds {
                assignment: "assignment-mtls-one",
                operation: "operation-mtls-one",
                attempt: "attempt-mtls-one",
                acquire: "acquire-mtls-one",
                release: "release-mtls-one",
            },
            Some(&local_artifact),
        )?,
    )
    .await?;
    assert_eq!(
        first_receipt.ack_disposition,
        AssignmentAckDisposition::Accepted
    );
    assert_eq!(first_receipt.lease.state, semantic::LeaseState::Active);
    assert_eq!(first_receipt.lease.resources.len(), 1);
    assert_eq!(first_receipt.lease.resources[0].id, cpu.identity.id);
    assert!(resources.is_allocated(&cpu.identity.id));
    assert_eq!(
        controller.intent_disposition("assignment-mtls-one")?,
        Some(IntentDisposition::Completed)
    );

    let mut observations = observations;
    wait_for_runtime_state(&mut observations, &runtime, RuntimeObservedState::Running).await?;
    wait_for_workload_log(&mut observations, &runtime, "CYRENE_REAL_WORKLOAD_ONE").await?;
    let terminal = wait_for_terminal_observation(&mut observations, &runtime).await?;
    assert_eq!(
        terminal.observed_state,
        RuntimeObservedState::Stopped as i32
    );
    assert_eq!(
        terminal.termination,
        TerminationClassification::External as i32
    );
    assert_eq!(terminal.reason_code, "WORKLOAD_EXITED");
    runtime_task.await??;
    wait_until("Runtime session removal after terminal workload", || {
        !service.has_runtime_session(&runtime)
    })
    .await?;

    let persisted_ledger: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&intent_path)?)?;
    let persisted_record = persisted_ledger
        .get("records")
        .and_then(|records| records.get("assignment-mtls-one"))
        .ok_or("durable ledger omitted assignment-mtls-one")?;
    assert_eq!(
        persisted_record
            .get("assignment_id")
            .and_then(|value| value.as_str()),
        Some("assignment-mtls-one")
    );
    assert_eq!(
        persisted_record
            .get("disposition")
            .and_then(|value| value.as_str()),
        Some("completed")
    );

    let resume_state_path = runtime_resume_token_path(&runtime_state, &runtime, &node_ref());
    let persisted_resume_state_text = fs::read_to_string(&resume_state_path)?;
    let persisted_resume_state: serde_json::Value =
        serde_json::from_str(&persisted_resume_state_text)?;
    let _persisted_resume_token = persisted_resume_state
        .get("resume_token")
        .and_then(|value| value.as_str())
        .filter(|token| !token.is_empty())
        .ok_or("Runtime Agent did not persist a non-empty resume token")?;
    assert_eq!(
        persisted_resume_state
            .get("runtime_id")
            .and_then(|value| value.as_str()),
        Some(RUNTIME_ID)
    );
    assert_eq!(
        persisted_resume_state
            .get("node_id")
            .and_then(|value| value.as_str()),
        Some(NODE_ID)
    );
    assert_eq!(
        persisted_resume_state
            .get("node_epoch")
            .and_then(|value| value.as_u64()),
        Some(NODE_EPOCH)
    );
    assert!(!persisted_resume_state_text.contains("mtls-runtime-enrollment"));

    drop(controller);
    drop(controller_store);
    let restarted_store = Arc::new(FileExecutionIntentStore::open(&intent_path)?);
    let restarted_controller = ExecutionController::new(
        service.clone(),
        Duration::from_secs(5),
        restarted_store.clone(),
    )?;
    assert_eq!(
        restarted_controller.intent_disposition("assignment-mtls-one")?,
        Some(IntentDisposition::Completed)
    );
    restarted_controller
        .release(ExecutionReleaseRequest {
            assignment_id: "assignment-mtls-one".to_string(),
            node: first_receipt.node.clone(),
            lease: first_receipt.lease.clone(),
            command_id: "release-mtls-one".to_string(),
            context: context("release-mtls-one"),
        })
        .await?;
    assert_eq!(
        restarted_controller.intent_disposition("assignment-mtls-one")?,
        Some(IntentDisposition::Released)
    );
    assert!(!resources.is_allocated(&cpu.identity.id));

    let second_runtime_config = runtime_config(
        &certificates,
        &control_endpoint,
        runtime_state.clone(),
        artifact_root.clone(),
        "CYRENE_REAL_WORKLOAD_TWO",
    );
    assert_eq!(
        second_runtime_config.enrollment_proof,
        "mtls-runtime-enrollment"
    );
    assert!(second_runtime_config.resume_token.is_empty());
    // The one-shot enrollment provider has no second proof. A successful
    // reconnect therefore proves the Agent resolved the persisted token.
    // 中文：一次性注册 Provider 没有第二份证明。因此，成功重新连接即可证明 Agent 已解析持久化的令牌。
    let mut second_runtime_task = spawn_runtime_agent(second_runtime_config);
    wait_for_runtime_session(
        "reconnected Runtime session",
        &service,
        &runtime,
        &mut second_runtime_task,
    )
    .await?;
    let second_receipt = dispatch_with_context(
        &restarted_controller,
        "resumed workload",
        dispatch_request(
            &service,
            &runtime,
            &cpu,
            DispatchIds {
                assignment: "assignment-mtls-two",
                operation: "operation-mtls-two",
                attempt: "attempt-mtls-two",
                acquire: "acquire-mtls-two",
                release: "release-mtls-two",
            },
            None,
        )?,
    )
    .await?;
    assert_eq!(second_receipt.lease.resources[0].id, cpu.identity.id);
    assert!(resources.is_allocated(&cpu.identity.id));
    wait_for_runtime_state(&mut observations, &runtime, RuntimeObservedState::Running).await?;
    wait_for_workload_log(&mut observations, &runtime, "CYRENE_REAL_WORKLOAD_TWO").await?;
    let terminal = wait_for_terminal_observation(&mut observations, &runtime).await?;
    assert_eq!(
        terminal.observed_state,
        RuntimeObservedState::Stopped as i32
    );
    second_runtime_task.await??;
    wait_until("second Runtime session removal", || {
        !service.has_runtime_session(&runtime)
    })
    .await?;

    restarted_controller
        .release(ExecutionReleaseRequest {
            assignment_id: "assignment-mtls-two".to_string(),
            node: second_receipt.node,
            lease: second_receipt.lease,
            command_id: "release-mtls-two".to_string(),
            context: context("release-mtls-two"),
        })
        .await?;
    assert!(!resources.is_allocated(&cpu.identity.id));
    assert_eq!(
        restarted_controller.intent_disposition("assignment-mtls-two")?,
        Some(IntentDisposition::Released)
    );

    let workload_log = runtime_state.join(format!(
        "workload-{}-{}.log",
        runtime.id, runtime.generation
    ));
    let completed_log = fs::read(&workload_log)?;
    let missing_artifact = artifact_ref(b"missing local Artifact input");
    let mut rejected_runtime_task = spawn_runtime_agent(runtime_config(
        &certificates,
        &control_endpoint,
        runtime_state,
        artifact_root,
        "CYRENE_REJECTION_RECOVERY_WORKLOAD",
    ));
    wait_for_runtime_session(
        "Runtime session for rejected local Artifact",
        &service,
        &runtime,
        &mut rejected_runtime_task,
    )
    .await?;
    let missing_artifact_request = dispatch_request(
        &service,
        &runtime,
        &cpu,
        DispatchIds {
            assignment: "assignment-mtls-local-missing",
            operation: "operation-mtls-local-missing",
            attempt: "attempt-mtls-local-missing",
            acquire: "acquire-mtls-local-missing",
            release: "release-mtls-local-missing",
        },
        Some(&missing_artifact),
    )?;
    let placement = plan_execution_placement(
        &missing_artifact_request.placement,
        &missing_artifact_request.candidates,
    )?;
    assert!(
        placement.selected_node.is_some(),
        "missing-local Artifact scenario must reach Runtime staging: {:?}",
        placement.evaluations
    );
    let rejected = restarted_controller
        .dispatch(missing_artifact_request)
        .await
        .expect_err("missing local CAS input must reject the assignment");
    assert_eq!(
        rejected.reason_code, "ARTIFACT_STAGING_FAILED",
        "unexpected dispatch rejection: {rejected:?}"
    );
    assert!(!rejected.reconciliation_required);
    assert_eq!(
        restarted_controller.intent_disposition("assignment-mtls-local-missing")?,
        Some(IntentDisposition::Failed)
    );
    assert!(!resources.is_allocated(&cpu.identity.id));
    let failure = wait_for_observation(&mut observations, &runtime, |observation| {
        observation.observed_state == RuntimeObservedState::Failed as i32
            && observation.reason_code == "ARTIFACT_STAGING_FAILED"
    })
    .await?;
    assert_eq!(failure.reason_code, "ARTIFACT_STAGING_FAILED");
    assert_eq!(fs::read(&workload_log)?, completed_log);

    // A rejected assignment leaves the Agent connected so it can accept later
    // work. Dispatch one valid recovery assignment and await normal Agent exit;
    // aborting a spawn_blocking JoinHandle cannot cancel the running Agent.
    // 中文：被拒绝的分配不会断开 Agent，因此它仍能接受后续工作。派发一个有效的恢复分配，并等待 Agent 正常退出；中止 spawn_blocking 的 JoinHandle 无法取消正在运行的 Agent。
    let recovery_receipt = dispatch_with_context(
        &restarted_controller,
        "recovery workload",
        dispatch_request(
            &service,
            &runtime,
            &cpu,
            DispatchIds {
                assignment: "assignment-mtls-rejection-recovery",
                operation: "operation-mtls-rejection-recovery",
                attempt: "attempt-mtls-rejection-recovery",
                acquire: "acquire-mtls-rejection-recovery",
                release: "release-mtls-rejection-recovery",
            },
            None,
        )?,
    )
    .await?;
    assert!(resources.is_allocated(&cpu.identity.id));
    wait_for_runtime_state(&mut observations, &runtime, RuntimeObservedState::Running).await?;
    wait_for_workload_log(
        &mut observations,
        &runtime,
        "CYRENE_REJECTION_RECOVERY_WORKLOAD",
    )
    .await?;
    let terminal = wait_for_terminal_observation(&mut observations, &runtime).await?;
    assert_eq!(
        terminal.observed_state,
        RuntimeObservedState::Stopped as i32
    );
    rejected_runtime_task.await??;
    wait_until("recovery Runtime session removal", || {
        !service.has_runtime_session(&runtime)
    })
    .await?;
    restarted_controller
        .release(ExecutionReleaseRequest {
            assignment_id: "assignment-mtls-rejection-recovery".to_string(),
            node: recovery_receipt.node,
            lease: recovery_receipt.lease,
            command_id: "release-mtls-rejection-recovery".to_string(),
            context: context("release-mtls-rejection-recovery"),
        })
        .await?;
    assert!(!resources.is_allocated(&cpu.identity.id));

    host_task.abort();
    control_task.abort();
    kernel_task.abort();
    Ok(())
}

#[derive(Debug, Clone)]
struct Certificates {
    ca_pem: PathBuf,
    server_pem: PathBuf,
    server_key: PathBuf,
    host_pem: PathBuf,
    host_key: PathBuf,
    host_der: Vec<u8>,
    runtime_pem: PathBuf,
    runtime_key: PathBuf,
    runtime_der: Vec<u8>,
    rogue_pem: PathBuf,
    rogue_key: PathBuf,
}

impl Certificates {
    /// Generate a short-lived local CA and signed server/client leaves.
    ///
    /// `openssl` is an explicit acceptance prerequisite. Any missing binary,
    /// failed invocation, or malformed output returns an error and fails this
    /// test; there is deliberately no skip path.
    /// 中文：生成有效期较短的本地 CA，并签发服务器端与客户端叶证书。
    ///
    /// 中文：`openssl` 是明确的验收前置条件。任何二进制缺失、命令执行失败或输出格式错误都会返回错误并使测试失败；这里有意不设置跳过路径。
    fn generate(directory: &Path) -> TestResult<Self> {
        let version = Command::new("openssl").arg("version").output();
        let version = version.map_err(|error| {
            format!("openssl is required for the mTLS acceptance test: {error}")
        })?;
        if !version.status.success() {
            return Err(format!(
                "openssl version failed: {}",
                String::from_utf8_lossy(&version.stderr)
            )
            .into());
        }

        let ca_database = directory.join("ca-index.txt");
        let ca_serial = directory.join("ca-serial");
        let ca_certificates = directory.join("ca-certificates");
        fs::write(&ca_database, "")?;
        fs::write(&ca_serial, "03E8\n")?;
        fs::create_dir(&ca_certificates)?;
        let ca_config = directory.join("openssl-ca.cnf");
        fs::write(
            &ca_config,
            format!(
                "[ca]\ndefault_ca=local_ca\n\
                 [local_ca]\ndatabase={}\nnew_certs_dir={}\nserial={}\ndefault_md=sha256\npolicy=common_name\nunique_subject=no\n\
                 [common_name]\ncommonName=supplied\n\
                 [root_certificate]\nbasicConstraints=critical,CA:TRUE\nkeyUsage=critical,keyCertSign,cRLSign\nsubjectKeyIdentifier=hash\nauthorityKeyIdentifier=keyid:always\n",
                ca_database.display(),
                ca_certificates.display(),
                ca_serial.display(),
            ),
        )?;
        let now = chrono::Utc::now();
        let not_before = (now - chrono::Duration::hours(24))
            .format("%Y%m%d%H%M%SZ")
            .to_string();
        let not_after = (now + chrono::Duration::days(2))
            .format("%Y%m%d%H%M%SZ")
            .to_string();

        let ca_pem = directory.join("ca.pem");
        let ca_key = directory.join("ca.key");
        let ca_csr = directory.join("ca.csr");
        run_openssl(vec![
            arg("req"),
            arg("-new"),
            arg("-newkey"),
            arg("rsa:2048"),
            arg("-nodes"),
            arg("-keyout"),
            arg(&ca_key),
            arg("-out"),
            arg(&ca_csr),
            arg("-subj"),
            arg("/CN=cyrene-mtls-runtime-tck-ca"),
        ])?;
        run_openssl(vec![
            arg("ca"),
            arg("-batch"),
            arg("-selfsign"),
            arg("-notext"),
            arg("-config"),
            arg(&ca_config),
            arg("-keyfile"),
            arg(&ca_key),
            arg("-in"),
            arg(&ca_csr),
            arg("-out"),
            arg(&ca_pem),
            arg("-startdate"),
            arg(&not_before),
            arg("-enddate"),
            arg(&not_after),
            arg("-extensions"),
            arg("root_certificate"),
        ])?;
        set_private_key_permissions(&ca_key)?;

        let (server_pem, server_key, _) = issue_leaf(
            directory,
            &ca_config,
            &ca_pem,
            &ca_key,
            &not_before,
            &not_after,
            LeafCertificateSpec {
                name: "server",
                subject: "/CN=cyrene-mtls-runtime-tck-server",
                subject_alt_name: "DNS:control.test,IP:127.0.0.1",
                extended_key_usage: "serverAuth",
            },
        )?;
        let (host_pem, host_key, host_der) = issue_leaf(
            directory,
            &ca_config,
            &ca_pem,
            &ca_key,
            &not_before,
            &not_after,
            LeafCertificateSpec {
                name: "host",
                subject: "/CN=cyrene-mtls-runtime-tck-host",
                subject_alt_name: "DNS:host.test",
                extended_key_usage: "clientAuth",
            },
        )?;
        let (runtime_pem, runtime_key, runtime_der) = issue_leaf(
            directory,
            &ca_config,
            &ca_pem,
            &ca_key,
            &not_before,
            &not_after,
            LeafCertificateSpec {
                name: "runtime",
                subject: "/CN=cyrene-mtls-runtime-tck-runtime",
                subject_alt_name: "DNS:runtime.test",
                extended_key_usage: "clientAuth",
            },
        )?;
        let (rogue_pem, rogue_key, _) = issue_leaf(
            directory,
            &ca_config,
            &ca_pem,
            &ca_key,
            &not_before,
            &not_after,
            LeafCertificateSpec {
                name: "rogue",
                subject: "/CN=cyrene-mtls-runtime-tck-rogue",
                subject_alt_name: "DNS:rogue.test",
                extended_key_usage: "clientAuth",
            },
        )?;

        Ok(Self {
            ca_pem,
            server_pem,
            server_key,
            host_pem,
            host_key,
            host_der,
            runtime_pem,
            runtime_key,
            runtime_der,
            rogue_pem,
            rogue_key,
        })
    }
}

struct LeafCertificateSpec<'a> {
    name: &'a str,
    subject: &'a str,
    subject_alt_name: &'a str,
    extended_key_usage: &'a str,
}

fn issue_leaf(
    directory: &Path,
    ca_config: &Path,
    ca_pem: &Path,
    ca_key: &Path,
    not_before: &str,
    not_after: &str,
    spec: LeafCertificateSpec<'_>,
) -> TestResult<(PathBuf, PathBuf, Vec<u8>)> {
    let LeafCertificateSpec {
        name,
        subject,
        subject_alt_name,
        extended_key_usage,
    } = spec;
    let key = directory.join(format!("{name}.key"));
    let csr = directory.join(format!("{name}.csr"));
    let pem = directory.join(format!("{name}.pem"));
    let der = directory.join(format!("{name}.der"));
    let extensions = directory.join(format!("{name}.ext"));
    fs::write(
        &extensions,
        format!(
            "[leaf_certificate]\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage={extended_key_usage}\nsubjectAltName={subject_alt_name}\n"
        ),
    )?;
    run_openssl(vec![
        arg("req"),
        arg("-new"),
        arg("-newkey"),
        arg("rsa:2048"),
        arg("-nodes"),
        arg("-keyout"),
        arg(&key),
        arg("-out"),
        arg(&csr),
        arg("-subj"),
        arg(subject),
    ])?;
    run_openssl(vec![
        arg("ca"),
        arg("-batch"),
        arg("-notext"),
        arg("-config"),
        arg(ca_config),
        arg("-cert"),
        arg(ca_pem),
        arg("-keyfile"),
        arg(ca_key),
        arg("-in"),
        arg(&csr),
        arg("-out"),
        arg(&pem),
        arg("-startdate"),
        arg(not_before),
        arg("-enddate"),
        arg(not_after),
        arg("-extfile"),
        arg(&extensions),
        arg("-extensions"),
        arg("leaf_certificate"),
    ])?;
    run_openssl(vec![
        arg("x509"),
        arg("-in"),
        arg(&pem),
        arg("-outform"),
        arg("DER"),
        arg("-out"),
        arg(&der),
    ])?;
    set_private_key_permissions(&key)?;
    Ok((pem, key, fs::read(der)?))
}

fn run_openssl(args: Vec<std::ffi::OsString>) -> TestResult {
    let output = Command::new("openssl").args(args.iter()).output()?;
    if !output.status.success() {
        return Err(format!(
            "openssl command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(())
}

fn arg(value: impl AsRef<std::ffi::OsStr>) -> std::ffi::OsString {
    value.as_ref().to_os_string()
}

fn set_private_key_permissions(path: &Path) -> TestResult {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

async fn start_control(
    service: ExecutionControlService,
    certificates: &Certificates,
) -> TestResult<(
    String,
    tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let tls = ServerTlsConfig::new()
        .identity(Identity::from_pem(
            fs::read(&certificates.server_pem)?,
            fs::read(&certificates.server_key)?,
        ))
        .client_ca_root(Certificate::from_pem(fs::read(&certificates.ca_pem)?));
    let mut server = Server::builder().tls_config(tls)?;
    let task = tokio::spawn(async move {
        server
            .add_service(NodeControlServiceServer::new(service))
            .serve_with_incoming(TcpListenerStream::new(listener))
            .await
    });
    Ok((format!("https://{address}"), task))
}

async fn start_kernel(
    socket: &Path,
    provider: Arc<LinuxSystemProvider>,
    resources: Arc<InMemoryResourceManager>,
) -> TestResult<tokio::task::JoinHandle<Result<(), tonic::transport::Error>>> {
    let daemon = Arc::new(KernelDaemon::new(
        provider.clone(),
        provider,
        resources,
        Arc::new(LeaseOnlySandbox),
        NODE_ID,
        NODE_EPOCH,
    ));
    let adapter = KernelServiceAdapter::new(daemon, Arc::new(UnusedResolver));
    let listener = UnixListener::bind(socket).map_err(|error| {
        format!(
            "cannot bind real Kernel UDS {} (run this acceptance outside a restricted sandbox): {error}",
            socket.display()
        )
    })?;
    let task = tokio::spawn(async move {
        Server::builder()
            .add_service(
                core::kernel_authority_service_server::KernelAuthorityServiceServer::with_interceptor(
                    adapter.clone(),
                    inject_authority_principal,
                ),
            )
            .add_service(adapter.server())
            .serve_with_incoming(PeerCredAccept::new(UnixListenerStream::new(listener)))
            .await
    });
    Ok(task)
}

async fn assert_unbound_certificate_is_rejected(
    endpoint: &str,
    certificates: &Certificates,
) -> TestResult {
    let channel = control_channel(
        endpoint,
        &certificates.ca_pem,
        &certificates.rogue_pem,
        &certificates.rogue_key,
    )
    .await?;
    let mut client = NodeControlServiceClient::new(channel);
    let mut session =
        NodeControlSession::new(NODE_ID, NODE_EPOCH, "mtls-runtime-tck-rogue", 1, 1, "");
    let error = client
        .connect(Request::new(tokio_stream::iter([session.hello()])))
        .await
        .expect_err("an unbound but CA-signed client certificate must be rejected");
    assert_eq!(
        error.code(),
        Code::Unauthenticated,
        "unexpected unbound-certificate status: {error:?}"
    );
    assert!(error.message().contains("PEER_CERTIFICATE_UNAUTHORIZED"));
    Ok(())
}

async fn control_channel(
    endpoint: &str,
    ca: &Path,
    certificate: &Path,
    key: &Path,
) -> TestResult<Channel> {
    let tls = ClientTlsConfig::new()
        .domain_name(CONTROL_SERVER_NAME)
        .ca_certificate(Certificate::from_pem(fs::read(ca)?))
        .identity(Identity::from_pem(fs::read(certificate)?, fs::read(key)?));
    Ok(Endpoint::from_shared(endpoint.to_string())?
        .tls_config(tls)?
        .connect()
        .await?)
}

async fn run_host(
    endpoint: String,
    kernel_socket: PathBuf,
    certificates: Certificates,
) -> TestResult {
    let executor = UdsKernelCommandExecutor::new(kernel_socket)?;
    let node = executor.discover_node().await?;
    let bridge = NodeCommandBridge::new(executor);
    let mut session = NodeControlSession::new(
        node.node_id,
        node.node_epoch,
        "mtls-runtime-tck-host",
        1,
        1,
        "",
    );
    let channel = control_channel(
        &endpoint,
        &certificates.ca_pem,
        &certificates.host_pem,
        &certificates.host_key,
    )
    .await?;
    let mut client = NodeControlServiceClient::new(channel);
    let (outbound, inbound) = tokio::sync::mpsc::channel(32);
    outbound.send(session.hello()).await?;
    let mut incoming = client
        .connect(Request::new(ReceiverStream::new(inbound)))
        .await?
        .into_inner();
    let welcome = incoming
        .message()
        .await?
        .ok_or("Host did not receive NodeWelcome")?;
    session.accept_welcome(welcome)?;
    while let Some(frame) = incoming.message().await? {
        let result = bridge.forward(&mut session, frame).await?;
        outbound.send(result).await?;
    }
    Ok(())
}

fn runtime_config(
    certificates: &Certificates,
    endpoint: &str,
    state_dir: PathBuf,
    artifact_destination_root: PathBuf,
    marker: &str,
) -> RuntimeAgentConfig {
    RuntimeAgentConfig {
        control_plane_endpoint: endpoint.to_string(),
        control_plane_server_name: CONTROL_SERVER_NAME.to_string(),
        control_plane_ca: certificates.ca_pem.clone(),
        client_certificate: certificates.runtime_pem.clone(),
        client_key: certificates.runtime_key.clone(),
        artifact_ca: certificates.ca_pem.clone(),
        artifact_ticket_key: None,
        organization_id: ORGANIZATION_ID.to_string(),
        workspace_id: WORKSPACE_ID.to_string(),
        node: node_ref(),
        node_type: "container".to_string(),
        persistent: false,
        runtime: semantic::Identity {
            id: RUNTIME_ID.to_string(),
            generation: 1,
        },
        agent_version: "mtls-runtime-tck".to_string(),
        enrollment_proof: "mtls-runtime-enrollment".to_string(),
        resume_token: String::new(),
        state_dir,
        artifact_destination_root,
        reconnect_min: Duration::from_millis(50),
        reconnect_max: Duration::from_millis(250),
        workload: vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            format!("printf '{marker}\\n'; exit 0"),
        ],
    }
}

fn spawn_runtime_agent(
    config: RuntimeAgentConfig,
) -> tokio::task::JoinHandle<Result<(), cy_runtime_agent::RuntimeAgentError>> {
    // Poll the real Agent from a blocking-pool thread with only a Tokio handle
    // entered. This keeps cy-artifact-transfer's debug-only blocking client
    // guard outside an already-entered Tokio runtime while its async I/O and
    // spawned tasks still use the shared runtime.
    // 中文：仅进入 Tokio handle 的阻塞线程池线程会轮询真实 Agent。这样可避免 cy-artifact-transfer 仅用于调试的阻塞客户端保护逻辑运行在已进入的 Tokio runtime 中，同时其异步 I/O 和派生任务仍使用共享 runtime。
    let handle = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        let mut future = Box::pin(run_runtime_agent(config));
        let waker: Waker = Arc::new(ThreadWaker(std::thread::current())).into();
        let mut context = Context::from_waker(&waker);
        loop {
            let poll = {
                let _entered = handle.enter();
                future.as_mut().poll(&mut context)
            };
            match poll {
                Poll::Ready(result) => return result,
                Poll::Pending => std::thread::park(),
            }
        }
    })
}

#[derive(Debug)]
struct ThreadWaker(std::thread::Thread);

impl std::task::Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

fn runtime_resume_token_path(
    state_dir: &Path,
    runtime: &semantic::Identity,
    node: &core::NodeRef,
) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(b"cyrene.runtime-agent.resume-token.v1");
    hash_resume_string(&mut hasher, &runtime.id);
    hasher.update(runtime.generation.to_be_bytes());
    hash_resume_string(&mut hasher, &node.node_id);
    hasher.update(node.node_epoch.to_be_bytes());
    state_dir.join(format!("runtime-resume-token-{:x}.json", hasher.finalize()))
}

fn hash_resume_string(hasher: &mut Sha256, value: &str) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value.as_bytes());
}

struct DispatchIds<'a> {
    assignment: &'a str,
    operation: &'a str,
    attempt: &'a str,
    acquire: &'a str,
    release: &'a str,
}

fn dispatch_request(
    service: &ExecutionControlService,
    runtime: &semantic::Identity,
    resource: &semantic::Resource,
    ids: DispatchIds<'_>,
    local_artifact: Option<&ArtifactRef>,
) -> TestResult<ExecutionDispatchRequest> {
    let now = now_unix_ms();
    // Placement consumes wall-clock evidence while the test's timeouts use a
    // monotonic clock. Leave a bounded skew margin so a host clock correction
    // cannot make freshly constructed evidence appear sampled in the future.
    // 中文：放置逻辑使用墙上时钟证据，而测试超时使用单调时钟。保留有界偏差余量，避免主机时钟校正使刚构造的证据看起来像是在未来采样。
    let observed_at = now.saturating_sub(30_000);
    let valid_until = now.saturating_add(300_000);
    let workload_identity = service.workload_identity(
        runtime,
        "mtls-runtime-user",
        vec!["operation.report".to_string()],
    )?;
    let provider = semantic::Provider {
        identity: resource.provider.clone(),
        state: semantic::ProviderState::Ready,
        capabilities: Vec::new(),
    };
    let local_artifacts = local_artifact
        .map(|artifact| {
            vec![core::ArtifactLocalInput {
                artifact_uri: artifact.uri.clone(),
                digest: artifact.digest.clone(),
                size_bytes: artifact.size_bytes,
                artifact_kind: "generic".to_string(),
                manifest_digest: artifact.manifest_digest.clone().unwrap_or_default(),
            }]
        })
        .unwrap_or_default();
    let artifact_quotes = local_artifact
        .map(|artifact| {
            vec![ArtifactPlacementQuote {
                quote_id: format!("local-{}", artifact.digest),
                artifact: artifact.clone(),
                destination_peer_id: "peer-node-mtls-runtime-tck".to_string(),
                policy_scope: "workspace-local-cas-v1".to_string(),
                observed_at_unix_ms: observed_at,
                valid_until_unix_ms: valid_until,
                availability: ArtifactAvailability::VerifiedLocal {
                    inventory_generation: 1,
                },
            }]
        })
        .unwrap_or_default();
    let candidate = ExecutionTargetCandidate {
        node: node_ref(),
        lifecycle_state: core::NodeLifecycleState::Online,
        attachment: core::ExecutionAttachmentType::ContainerAgent,
        persistent: false,
        restart_capability: core::RestartCapability::None,
        capabilities: vec![execution_capability(
            core::ExecutionAttachmentType::ContainerAgent,
            false,
            core::RestartCapability::None,
        )],
        provider: provider.clone(),
        provider_snapshot: semantic::ProviderSnapshot {
            provider: provider.identity,
            snapshot_generation: 1,
            resources: vec![resource.clone()],
            workers: Vec::new(),
            endpoints: Vec::new(),
            sampled_at_unix_ms: observed_at,
            expires_at_unix_ms: valid_until,
        },
        residency: "local".to_string(),
        trust_domain: "workspace".to_string(),
        classifications: BTreeSet::new(),
        policy_tags: BTreeSet::new(),
        artifact_destination_peer_id: "peer-node-mtls-runtime-tck".to_string(),
        artifact_quotes,
        execution_cost_microunits: 1,
        available_at_unix_ms: observed_at,
        reliability_score: 100,
    };
    Ok(ExecutionDispatchRequest {
        placement: ExecutionPlacementRequest {
            capability_requirements: Vec::new(),
            resource_query: semantic::ResourceQuery {
                resource_class: resource.resource_class.clone(),
                count: 1,
                required_capabilities: Vec::new(),
                minimum_capacity: BTreeMap::new(),
            },
            allowed_attachments: BTreeSet::from([core::ExecutionAttachmentType::ContainerAgent]),
            persistent: Some(false),
            restart_capability: Some(core::RestartCapability::None),
            checkpoint_resume: false,
            network: NetworkRequirements::default(),
            artifacts: local_artifact
                .iter()
                .map(|artifact| (*artifact).clone())
                .collect(),
            artifact_policy_scope: if local_artifact.is_some() {
                "workspace-local-cas-v1".to_string()
            } else {
                String::new()
            },
            policy: PlacementPolicy::default(),
            latest_start_unix_ms: Some(now.saturating_add(120_000)),
            now_unix_ms: now,
        },
        candidates: vec![candidate],
        assignment: RuntimeAssignmentBuilder::new(
            ids.assignment,
            runtime.clone(),
            semantic::Identity {
                id: ids.operation.to_string(),
                generation: 1,
            },
            ids.attempt,
            workload_identity,
            core::RuntimeProfile {
                image_digest: format!("sha256:{}", "1".repeat(64)),
                resolved_digest: format!("sha256:{}", "2".repeat(64)),
            },
            Vec::new(),
        )
        .with_local_artifacts(local_artifacts),
        intent_payload_digest: digest(format!("{}-intent", ids.assignment)),
        acquire_command_id: format!("command-{}", ids.acquire),
        acquire_context: context(ids.acquire),
        release_command_id: format!("command-{}", ids.release),
        release_context: context(ids.release),
        lease_ttl: Duration::from_secs(30),
    })
}

async fn dispatch_with_context(
    controller: &ExecutionController,
    description: &str,
    request: ExecutionDispatchRequest,
) -> TestResult<DispatchReceipt> {
    let diagnostic_request = request.clone();
    controller.dispatch(request).await.map_err(|error| {
        let mut placement = diagnostic_request.placement.clone();
        placement.now_unix_ms = now_unix_ms();
        let decision = plan_execution_placement(&placement, &diagnostic_request.candidates);
        format!(
            "{description} dispatch failed: {error:?}; current placement decision: {decision:?}"
        )
        .into()
    })
}

async fn wait_for_runtime_state(
    observations: &mut broadcast::Receiver<cy_execution_control::ControlObservation>,
    runtime: &semantic::Identity,
    expected: RuntimeObservedState,
) -> TestResult<core::RuntimeObservation> {
    wait_for_observation(observations, runtime, move |observation| {
        observation.observed_state == expected as i32
    })
    .await
}

async fn wait_for_terminal_observation(
    observations: &mut broadcast::Receiver<cy_execution_control::ControlObservation>,
    runtime: &semantic::Identity,
) -> TestResult<core::RuntimeObservation> {
    wait_for_observation(observations, runtime, |observation| {
        matches!(
            RuntimeObservedState::try_from(observation.observed_state),
            Ok(RuntimeObservedState::Stopped)
                | Ok(RuntimeObservedState::Failed)
                | Ok(RuntimeObservedState::Lost)
        ) && observation.termination != TerminationClassification::Unspecified as i32
    })
    .await
}

async fn wait_for_workload_log(
    observations: &mut broadcast::Receiver<cy_execution_control::ControlObservation>,
    runtime: &semantic::Identity,
    marker: &str,
) -> TestResult {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(format!("timed out waiting for workload log {marker}").into());
        }
        let observation = tokio::time::timeout(remaining, observations.recv()).await??;
        let cy_execution_control::AuthenticatedAgent::Runtime {
            runtime: observed_runtime,
            ..
        } = &observation.agent
        else {
            continue;
        };
        if observed_runtime != runtime {
            continue;
        }
        let Some(node_to_control_plane::Body::StructuredEvent(event)) =
            observation.frame.body.as_ref()
        else {
            continue;
        };
        if event.kind == "workload.log" && String::from_utf8_lossy(&event.body).contains(marker) {
            return Ok(());
        }
    }
}

async fn wait_for_observation(
    observations: &mut broadcast::Receiver<cy_execution_control::ControlObservation>,
    runtime: &semantic::Identity,
    predicate: impl Fn(&core::RuntimeObservation) -> bool,
) -> TestResult<core::RuntimeObservation> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err("timed out waiting for RuntimeObservation".into());
        }
        let observation = tokio::time::timeout(remaining, observations.recv()).await??;
        let AuthenticatedAgent::Runtime {
            runtime: observed_runtime,
            ..
        } = &observation.agent
        else {
            continue;
        };
        if observed_runtime != runtime {
            continue;
        }
        let Some(node_to_control_plane::Body::RuntimeObservation(runtime_observation)) =
            observation.frame.body.as_ref()
        else {
            continue;
        };
        if predicate(runtime_observation) {
            return Ok(runtime_observation.clone());
        }
    }
}

async fn wait_for_host_session(
    service: &ExecutionControlService,
    host_task: &mut tokio::task::JoinHandle<TestResult>,
) -> TestResult {
    tokio::select! {
        result = host_task => match result {
            Ok(Ok(())) => Err("Host Agent ended before publishing an authenticated session".into()),
            Ok(Err(error)) => Err(format!("Host Agent failed before session admission: {error}").into()),
            Err(error) => Err(format!("Host Agent task failed before session admission: {error}").into()),
        },
        result = wait_until("authenticated Host session", || service.has_host_session(&node_ref())) => result,
    }
}

async fn wait_for_runtime_session(
    description: &str,
    service: &ExecutionControlService,
    runtime: &semantic::Identity,
    runtime_task: &mut tokio::task::JoinHandle<Result<(), cy_runtime_agent::RuntimeAgentError>>,
) -> TestResult {
    tokio::select! {
        result = runtime_task => match result {
            Ok(Ok(())) => Err(format!("Runtime Agent ended before {description}").into()),
            Ok(Err(error)) => Err(format!("Runtime Agent failed before {description}: {error}").into()),
            Err(error) => Err(format!("Runtime Agent task failed before {description}: {error}").into()),
        },
        result = wait_until(description, || service.has_runtime_session(runtime)) => result,
    }
}

async fn wait_until(description: &str, mut ready: impl FnMut() -> bool) -> TestResult {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if ready() {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!("timed out waiting for {description}").into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn node_ref() -> core::NodeRef {
    core::NodeRef {
        node_id: NODE_ID.to_string(),
        node_epoch: NODE_EPOCH,
    }
}

fn context(id: &str) -> AuthorityCallContext {
    AuthorityCallContext {
        contract: semantic::ContractRevision::current(),
        namespace: NamespaceId::default(),
        request_id: id.to_string(),
        idempotency_key: id.to_string(),
    }
}

fn digest(value: impl AsRef<[u8]>) -> String {
    format!("sha256:{:x}", Sha256::digest(value))
}

fn artifact_ref(value: &[u8]) -> ArtifactRef {
    let digest = digest(value);
    ArtifactRef {
        uri: format!("artifact://sha256/{}", &digest[7..]),
        digest,
        size_bytes: value.len() as u64,
        kind: ArtifactKind::generic(),
        manifest_digest: None,
    }
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before UNIX epoch")
        .as_millis()
        .try_into()
        .expect("system clock milliseconds fit u64")
}

#[derive(Debug)]
struct LeaseOnlySandbox;

impl ProcessRuntime for LeaseOnlySandbox {
    fn preflight(&self) -> NodeCapabilities {
        NodeCapabilities {
            ready: true,
            facts: Vec::new(),
            enforcement: Vec::new(),
        }
    }

    fn launch(
        &self,
        _plan: &LaunchPlan,
        _binding: &DeviceBinding,
    ) -> Result<ProcessHandle, ProviderError> {
        Err(ProviderError::new(
            "mtls-runtime-tck",
            "UNUSED",
            "this TCK runs the real workload through cy_runtime_agent",
        ))
    }

    fn stop(
        &self,
        _handle: &ProcessHandle,
        _request: &StopRequest,
    ) -> Result<CleanupReport, ProviderError> {
        Err(ProviderError::new(
            "mtls-runtime-tck",
            "UNUSED",
            "this TCK has no Kernel Worker instance to stop",
        ))
    }
}

impl SandboxBackend for LeaseOnlySandbox {
    fn backend_id(&self) -> &str {
        "lease-only-mtls-runtime-tck"
    }
}

struct UnusedResolver;

impl InstalledPluginResolver for UnusedResolver {
    fn resolve_launch_plan(
        &self,
        _installation: &VerifiedInstallation,
        _instance_name: &str,
    ) -> Result<cy_kernel_api::ResolvedLaunchPlan, ProviderError> {
        Err(ProviderError::new(
            "mtls-runtime-tck",
            "UNUSED",
            "this TCK does not launch a Kernel Worker",
        ))
    }
}
