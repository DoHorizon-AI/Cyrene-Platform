// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: kernel/crates/cy-kernel-daemon/src/rpc/lifecycle_service.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! `core_v1::PluginLifecycleService` gRPC 插件生命周期服务实现。

use std::sync::atomic::Ordering;

use cy_proto::core_v1;
use tokio::sync::mpsc;
use tokio_stream::{iter, wrappers::ReceiverStream, Stream};
use tonic::{Request, Response, Status};

use crate::{adapter::KernelServiceAdapter, convert::to_plugin_instance};

#[tonic::async_trait]
impl core_v1::plugin_lifecycle_service_server::PluginLifecycleService for KernelServiceAdapter {
    async fn install_plugin(
        &self,
        _request: Request<core_v1::InstallPluginRequest>,
    ) -> Result<Response<core_v1::Operation>, Status> {
        Err(Status::unimplemented(
            "installation is an out-of-kernel adapter responsibility",
        ))
    }

    async fn uninstall_plugin(
        &self,
        _request: Request<core_v1::UninstallPluginRequest>,
    ) -> Result<Response<core_v1::Operation>, Status> {
        Err(Status::unimplemented(
            "installation is an out-of-kernel adapter responsibility",
        ))
    }

    async fn set_plugin_enabled(
        &self,
        _request: Request<core_v1::SetPluginEnabledRequest>,
    ) -> Result<Response<core_v1::PluginInstallation>, Status> {
        Err(Status::unimplemented(
            "plugin enablement policy belongs to the control plane",
        ))
    }

    async fn start_plugin(
        &self,
        _request: Request<core_v1::StartPluginRequest>,
    ) -> Result<Response<core_v1::Operation>, Status> {
        Err(Status::unimplemented(
            "use KernelService.LaunchPlugin after policy and installation validation",
        ))
    }

    async fn stop_plugin(
        &self,
        _request: Request<core_v1::StopPluginRequest>,
    ) -> Result<Response<core_v1::Operation>, Status> {
        Err(Status::unimplemented(
            "use KernelService.TerminatePlugin for node-local process termination",
        ))
    }

    async fn get_plugin_instance(
        &self,
        request: Request<core_v1::GetPluginInstanceRequest>,
    ) -> Result<Response<core_v1::PluginInstance>, Status> {
        let name = request.into_inner().name;
        let adapter_available = self.adapter_available.load(Ordering::Relaxed);
        self.instances
            .lock()
            .expect("instance lock poisoned")
            .get(&name)
            .map(|process| {
                Response::new(to_plugin_instance(
                    &self.daemon,
                    &name,
                    process,
                    adapter_available,
                ))
            })
            .ok_or_else(|| Status::not_found("plugin instance is not managed by this Kernel"))
    }

    async fn list_plugin_instances(
        &self,
        request: Request<core_v1::ListPluginInstancesRequest>,
    ) -> Result<Response<core_v1::ListPluginInstancesResponse>, Status> {
        let request = request.into_inner();
        let filters = request.state_filter;
        let adapter_available = self.adapter_available.load(Ordering::Relaxed);
        let plugins = self
            .instances
            .lock()
            .expect("instance lock poisoned")
            .iter()
            .filter(|(_, process)| filters.is_empty() || filters.contains(&process.runtime_state))
            .map(|(name, process)| {
                to_plugin_instance(&self.daemon, name, process, adapter_available)
            })
            .collect();
        Ok(Response::new(core_v1::ListPluginInstancesResponse {
            plugins,
            next_page_token: String::new(),
        }))
    }

    async fn report_heartbeat(
        &self,
        request: Request<core_v1::ReportHeartbeatRequest>,
    ) -> Result<Response<core_v1::ReportHeartbeatResponse>, Status> {
        let request = request.into_inner();
        Ok(Response::new(self.accept_heartbeat(
            &request.plugin_instance_name,
            request.generation,
            request.sequence_number,
            request.observed_at,
            request.runtime_state,
            request.health,
            request.restart_count,
        )?))
    }

    type ConnectWorkerStream = std::pin::Pin<
        Box<dyn Stream<Item = Result<core_v1::KernelToWorker, Status>> + Send + 'static>,
    >;

    async fn connect_worker(
        &self,
        request: Request<tonic::Streaming<core_v1::WorkerToKernel>>,
    ) -> Result<Response<Self::ConnectWorkerStream>, Status> {
        let mut inbound = request.into_inner();
        let hello = inbound.message().await?.ok_or_else(|| {
            Status::invalid_argument("WorkerHello must be the first control frame")
        })?;
        let Some(core_v1::worker_to_kernel::Body::Hello(hello)) = hello.body else {
            return Err(Status::invalid_argument(
                "WorkerHello must be the first control frame",
            ));
        };
        let (outbound, receiver) = mpsc::channel(16);
        let (connection_id, welcome) = self.register_worker_control(&hello, outbound.clone())?;
        outbound
            .send(Ok(core_v1::KernelToWorker {
                body: Some(core_v1::kernel_to_worker::Body::Welcome(welcome)),
            }))
            .await
            .map_err(|_| Status::unavailable("worker control receiver closed during handshake"))?;

        let adapter = self.clone();
        let instance_name = hello.plugin_instance_name.clone();
        let generation = hello.generation;
        tokio::spawn(async move {
            while let Ok(Some(frame)) = inbound.message().await {
                let result = match frame.body {
                    Some(core_v1::worker_to_kernel::Body::Heartbeat(heartbeat)) => adapter
                        .accept_heartbeat(
                            &heartbeat.plugin_instance_name,
                            heartbeat.generation,
                            heartbeat.sequence_number,
                            heartbeat.observed_at,
                            heartbeat.runtime_state,
                            heartbeat.health,
                            heartbeat.restart_count,
                        )
                        .map(|response| core_v1::KernelToWorker {
                            body: Some(core_v1::kernel_to_worker::Body::HeartbeatAck(
                                core_v1::WorkerHeartbeatAck {
                                    disposition: response.disposition,
                                    accepted_sequence_number: response.accepted_sequence_number,
                                    desired_state: response.desired_state,
                                    desired_generation: response.desired_generation,
                                },
                            )),
                        }),
                    Some(core_v1::worker_to_kernel::Body::ShutdownAck(ack)) => adapter
                        .accept_shutdown_ack(&ack)
                        .map(|_| core_v1::KernelToWorker {
                            body: Some(core_v1::kernel_to_worker::Body::HeartbeatAck(
                                core_v1::WorkerHeartbeatAck {
                                    disposition: core_v1::HeartbeatDisposition::Accepted as i32,
                                    accepted_sequence_number: 0,
                                    desired_state: core_v1::DesiredPluginState::Stopped as i32,
                                    desired_generation: ack.generation,
                                },
                            )),
                        }),
                    Some(core_v1::worker_to_kernel::Body::Hello(_)) | None => {
                        Err(Status::invalid_argument(
                            "WorkerHello is valid only as the first control frame",
                        ))
                    }
                };
                match result {
                    Ok(response) => {
                        if outbound.send(Ok(response)).await.is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = outbound.send(Err(error)).await;
                        break;
                    }
                }
            }
            adapter.unregister_worker_control(&instance_name, generation, connection_id);
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(receiver))))
    }

    type WatchPluginEventsStream = std::pin::Pin<
        Box<dyn Stream<Item = Result<core_v1::PluginLifecycleEvent, Status>> + Send + 'static>,
    >;

    async fn watch_plugin_events(
        &self,
        _request: Request<core_v1::WatchPluginEventsRequest>,
    ) -> Result<Response<Self::WatchPluginEventsStream>, Status> {
        Ok(Response::new(Box::pin(iter(Vec::<
            Result<core_v1::PluginLifecycleEvent, Status>,
        >::new()))))
    }
}
