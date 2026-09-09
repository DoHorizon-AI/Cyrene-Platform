// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: kernel/crates/cy-kernel-daemon/src/service_manager.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! Generic Service Supervision Manager & gRPC Service Provider.
//!
//! Provides the external wire boundary for clients and Product adapters
//! to submit, observe, stop, cancel, and stream events for long-running
//! generic service workloads.

use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Duration};

use cy_kernel_api::{
    BackoffConfig, CgroupLimits, DeviceBinding, LaunchPlan, ProbeConfig, ReadinessProbe,
    RestartPolicy, SandboxBackend, ServiceEndpointSpec, ServiceSpec as DomainServiceSpec,
    ServiceState as DomainServiceState, ServiceStatus as DomainServiceStatus,
};
use cy_proto::core_v1::{
    readiness_probe::Probe, restart_policy::Policy,
    service_supervision_service_server::ServiceSupervisionService, CancelServiceRequest,
    GetServiceStatusRequest, HttpGetProbe, RestartPolicyAlways, RestartPolicyOnFailure,
    ServiceEvent as ProtoServiceEvent, ServiceSpec as ProtoServiceSpec,
    ServiceState as ProtoServiceState, ServiceStatus as ProtoServiceStatus, StartServiceRequest,
    StopServiceRequest, TcpSocketProbe, WatchServiceEventsRequest,
};
use tokio::sync::{Mutex, RwLock};
use tokio_stream::{wrappers::BroadcastStream, StreamExt};
use tonic::{Request, Response, Status};

use crate::watchdog::ServiceSupervisor;

/// Manager for generic service workloads hosted by the Cyrene Kernel.
pub struct ServiceSupervisionManager {
    runtime: Arc<dyn SandboxBackend>,
    default_binding: DeviceBinding,
    supervisors: Arc<RwLock<HashMap<String, Arc<Mutex<ServiceSupervisor>>>>>,
}

impl ServiceSupervisionManager {
    /// Create a new ServiceSupervisionManager with the given sandbox backend.
    pub fn new(runtime: Arc<dyn SandboxBackend>, default_binding: DeviceBinding) -> Self {
        Self {
            runtime,
            default_binding,
            supervisors: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Convert a domain ServiceStatus into its Protobuf wire representation.
    pub fn domain_status_to_proto(status: DomainServiceStatus) -> ProtoServiceStatus {
        let (exit_code, oom_killed, reason_code) = if let Some(report) = &status.last_exit_report {
            (
                report.exit_code,
                report.oom_killed,
                report.reason_code.clone(),
            )
        } else {
            (None, false, status.state.as_reason_code().to_string())
        };

        ProtoServiceStatus {
            name: status.name,
            state: match status.state {
                DomainServiceState::Starting => ProtoServiceState::Starting as i32,
                DomainServiceState::Ready => ProtoServiceState::Ready as i32,
                DomainServiceState::Running => ProtoServiceState::Running as i32,
                DomainServiceState::Stopping => ProtoServiceState::Stopping as i32,
                DomainServiceState::Stopped => ProtoServiceState::Stopped as i32,
                DomainServiceState::Failed => ProtoServiceState::Failed as i32,
                DomainServiceState::Restarting => ProtoServiceState::Restarting as i32,
                DomainServiceState::Quarantined => ProtoServiceState::Quarantined as i32,
            },
            restart_count: status.restart_count,
            exit_code,
            oom_killed,
            reason_code,
            published_endpoint: status
                .published_endpoint
                .as_ref()
                .map(crate::convert::to_semantic_proto_endpoint),
            last_error: status.last_error,
        }
    }

    /// Convert a Protobuf wire ServiceSpec into a domain ServiceSpec.
    // ════════════════════════════════════════════════════════════════════════
    // 🔧 FUNCTION: ServiceSupervisionManager::proto_spec_to_domain
    //
    //   Converts the transport request into the domain launch model and keeps
    //   validation at the wire-to-domain boundary.
    //
    //   将传输层请求转换为领域启动模型，并把输入校验集中在协议到领域的边界。
    // ════════════════════════════════════════════════════════════════════════
    pub fn proto_spec_to_domain(spec: ProtoServiceSpec) -> Result<DomainServiceSpec, Status> {
        if spec.name.trim().is_empty() {
            return Err(Status::invalid_argument("service name must not be empty"));
        }
        if spec.executable.trim().is_empty() {
            return Err(Status::invalid_argument("executable must not be empty"));
        }

        let plan = LaunchPlan {
            instance_name: spec.name.clone(),
            executable: PathBuf::from(&spec.executable),
            args: spec.args,
            environment: spec.environment.into_iter().collect(),
            cgroup_name: if spec.cgroup_name.is_empty() {
                format!("service-{}", spec.name)
            } else {
                spec.cgroup_name
            },
            limits: CgroupLimits::default(),
            working_dir: spec
                .working_dir
                .filter(|s| !s.is_empty())
                .map(PathBuf::from),
            transport_socket: spec
                .transport_socket
                .filter(|s| !s.is_empty())
                .map(PathBuf::from),
        };

        let probe = if let Some(proto_probe) = spec.readiness_probe {
            let config = if let Some(c) = spec.probe_config {
                ProbeConfig {
                    initial_delay: c
                        .initial_delay
                        .map(|d| Duration::new(d.seconds as u64, d.nanos as u32))
                        .unwrap_or_default(),
                    period: c
                        .period
                        .map(|d| Duration::new(d.seconds as u64, d.nanos as u32))
                        .unwrap_or(Duration::from_millis(50)),
                    timeout: c
                        .timeout
                        .map(|d| Duration::new(d.seconds as u64, d.nanos as u32))
                        .unwrap_or(Duration::from_millis(500)),
                    success_threshold: c.success_threshold.max(1),
                    failure_threshold: c.failure_threshold.max(1),
                }
            } else {
                ProbeConfig::default()
            };

            let domain_probe = match proto_probe.probe {
                Some(Probe::ProcessAlive(_)) => ReadinessProbe::ProcessAlive,
                Some(Probe::TcpSocket(TcpSocketProbe { host, port })) => {
                    ReadinessProbe::TcpSocket {
                        host,
                        port: port as u16,
                    }
                }
                Some(Probe::HttpGet(HttpGetProbe {
                    host,
                    port,
                    path,
                    expected_status,
                })) => ReadinessProbe::HttpGet {
                    host,
                    port: port as u16,
                    path,
                    expected_status: expected_status.map(|s| s as u16),
                },
                Some(Probe::WorkerControl(_)) => ReadinessProbe::WorkerControl,
                None => ReadinessProbe::ProcessAlive,
            };
            Some((domain_probe, config))
        } else {
            None
        };

        let restart_policy = if let Some(p) = spec.restart_policy {
            match p.policy {
                Some(Policy::Never(_)) => RestartPolicy::Never,
                Some(Policy::OnFailure(RestartPolicyOnFailure {
                    max_retries,
                    backoff,
                })) => {
                    let backoff_cfg = backoff
                        .map(|b| BackoffConfig {
                            initial_delay: b
                                .initial_delay
                                .map(|d| Duration::new(d.seconds as u64, d.nanos as u32))
                                .unwrap_or(Duration::from_millis(100)),
                            max_delay: b
                                .max_delay
                                .map(|d| Duration::new(d.seconds as u64, d.nanos as u32))
                                .unwrap_or(Duration::from_secs(30)),
                            multiplier: if b.multiplier > 0.0 {
                                b.multiplier
                            } else {
                                2.0
                            },
                            reset_after: b
                                .reset_after
                                .map(|d| Duration::new(d.seconds as u64, d.nanos as u32))
                                .unwrap_or(Duration::from_secs(60)),
                        })
                        .unwrap_or_default();
                    RestartPolicy::OnFailure {
                        max_retries,
                        backoff: backoff_cfg,
                    }
                }
                Some(Policy::Always(RestartPolicyAlways {
                    max_retries,
                    backoff,
                })) => {
                    let backoff_cfg = backoff
                        .map(|b| BackoffConfig {
                            initial_delay: b
                                .initial_delay
                                .map(|d| Duration::new(d.seconds as u64, d.nanos as u32))
                                .unwrap_or(Duration::from_millis(100)),
                            max_delay: b
                                .max_delay
                                .map(|d| Duration::new(d.seconds as u64, d.nanos as u32))
                                .unwrap_or(Duration::from_secs(30)),
                            multiplier: if b.multiplier > 0.0 {
                                b.multiplier
                            } else {
                                2.0
                            },
                            reset_after: b
                                .reset_after
                                .map(|d| Duration::new(d.seconds as u64, d.nanos as u32))
                                .unwrap_or(Duration::from_secs(60)),
                        })
                        .unwrap_or_default();
                    RestartPolicy::Always {
                        max_retries,
                        backoff: backoff_cfg,
                    }
                }
                None => RestartPolicy::default(),
            }
        } else {
            RestartPolicy::default()
        };

        let graceful_stop_timeout = spec
            .graceful_stop_timeout
            .map(|d| Duration::new(d.seconds as u64, d.nanos as u32))
            .unwrap_or(Duration::from_secs(5));

        let endpoint = spec.endpoint.map(|e| ServiceEndpointSpec {
            transport: e.transport,
            schema_id: e.schema_id,
            port: e.port.map(|p| p as u16),
            path: e.path,
            attributes: e.attributes.into_iter().collect(),
            connection_ref: e.connection_ref,
            credential_ref: e.credential_ref,
        });

        Ok(DomainServiceSpec {
            name: spec.name,
            plan,
            readiness_probe: probe,
            restart_policy,
            graceful_stop_timeout,
            endpoint,
        })
    }
}

#[tonic::async_trait]
impl ServiceSupervisionService for ServiceSupervisionManager {
    async fn start_service(
        &self,
        request: Request<StartServiceRequest>,
    ) -> Result<Response<ProtoServiceStatus>, Status> {
        let req = request.into_inner();
        let proto_spec = req
            .spec
            .ok_or_else(|| Status::invalid_argument("service spec is required"))?;
        let name = proto_spec.name.clone();
        let domain_spec = Self::proto_spec_to_domain(proto_spec)?;

        let supervisor_arc = {
            let mut map = self.supervisors.write().await;
            if let Some(existing) = map.get(&name) {
                existing.clone()
            } else {
                let supervisor = ServiceSupervisor::new(
                    domain_spec,
                    self.runtime.clone(),
                    self.default_binding.clone(),
                );
                let arc = Arc::new(Mutex::new(supervisor));
                map.insert(name.clone(), arc.clone());
                arc
            }
        };

        let mut supervisor = supervisor_arc.lock().await;
        let status = supervisor
            .start()
            .await
            .map_err(|e| Status::internal(format!("failed to start service {name}: {e}")))?;

        Ok(Response::new(Self::domain_status_to_proto(status)))
    }

    async fn get_service_status(
        &self,
        request: Request<GetServiceStatusRequest>,
    ) -> Result<Response<ProtoServiceStatus>, Status> {
        let req = request.into_inner();
        let map = self.supervisors.read().await;
        let supervisor_arc = map
            .get(&req.service_name)
            .ok_or_else(|| Status::not_found(format!("service {} not found", req.service_name)))?;

        let supervisor = supervisor_arc.lock().await;
        Ok(Response::new(Self::domain_status_to_proto(
            supervisor.status(),
        )))
    }

    async fn stop_service(
        &self,
        request: Request<StopServiceRequest>,
    ) -> Result<Response<ProtoServiceStatus>, Status> {
        let req = request.into_inner();
        let supervisor_arc = {
            let map = self.supervisors.read().await;
            map.get(&req.service_name).cloned().ok_or_else(|| {
                Status::not_found(format!("service {} not found", req.service_name))
            })?
        };

        let mut supervisor = supervisor_arc.lock().await;
        let status = supervisor.stop().await.map_err(|e| {
            Status::internal(format!("failed to stop service {}: {e}", req.service_name))
        })?;

        Ok(Response::new(Self::domain_status_to_proto(status)))
    }

    async fn cancel_service(
        &self,
        request: Request<CancelServiceRequest>,
    ) -> Result<Response<ProtoServiceStatus>, Status> {
        let req = request.into_inner();
        let supervisor_arc = {
            let map = self.supervisors.read().await;
            map.get(&req.service_name).cloned().ok_or_else(|| {
                Status::not_found(format!("service {} not found", req.service_name))
            })?
        };

        let mut supervisor = supervisor_arc.lock().await;
        let status = supervisor.cancel().await.map_err(|e| {
            Status::internal(format!(
                "failed to cancel service {}: {e}",
                req.service_name
            ))
        })?;

        Ok(Response::new(Self::domain_status_to_proto(status)))
    }

    type WatchServiceEventsStream = std::pin::Pin<
        Box<dyn tokio_stream::Stream<Item = Result<ProtoServiceEvent, Status>> + Send + 'static>,
    >;

    async fn watch_service_events(
        &self,
        request: Request<WatchServiceEventsRequest>,
    ) -> Result<Response<Self::WatchServiceEventsStream>, Status> {
        let req = request.into_inner();
        let rx = {
            let map = self.supervisors.read().await;
            let supervisor_arc = map.get(&req.service_name).ok_or_else(|| {
                Status::not_found(format!("service {} not found", req.service_name))
            })?;
            let supervisor = supervisor_arc.lock().await;
            supervisor.subscribe_events()
        };

        let stream = BroadcastStream::new(rx).filter_map(|item| match item {
            Ok(event) => Some(Ok(ProtoServiceEvent {
                service_name: event.service_name,
                state: match event.state {
                    DomainServiceState::Starting => ProtoServiceState::Starting as i32,
                    DomainServiceState::Ready => ProtoServiceState::Ready as i32,
                    DomainServiceState::Running => ProtoServiceState::Running as i32,
                    DomainServiceState::Stopping => ProtoServiceState::Stopping as i32,
                    DomainServiceState::Stopped => ProtoServiceState::Stopped as i32,
                    DomainServiceState::Failed => ProtoServiceState::Failed as i32,
                    DomainServiceState::Restarting => ProtoServiceState::Restarting as i32,
                    DomainServiceState::Quarantined => ProtoServiceState::Quarantined as i32,
                },
                timestamp_unix_ms: event.timestamp_unix_ms,
                reason_code: event.reason_code,
                message: event.message,
            })),
            Err(_) => None,
        });

        Ok(Response::new(Box::pin(stream)))
    }
}
