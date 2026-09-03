// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: kernel/crates/cy-kernel-daemon/src/rpc/worker_control.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! `core_v1::WorkerControlService` gRPC Worker 通信控制服务实现。

use cy_proto::core_v1;
use tokio::sync::mpsc;
use tokio_stream::{wrappers::ReceiverStream, Stream};
use tonic::{Request, Response, Status};

use crate::{
    adapter::KernelServiceAdapter,
    convert::{
        authority_call_context_from_proto, semantic_identity_from_proto, semantic_status,
        to_proto_duration, to_semantic_proto_worker,
    },
};

#[tonic::async_trait]
impl core_v1::worker_control_service_server::WorkerControlService for KernelServiceAdapter {
    type ConnectStream = std::pin::Pin<
        Box<dyn Stream<Item = Result<core_v1::KernelToWorkerControl, Status>> + Send + 'static>,
    >;

    async fn connect(
        &self,
        request: Request<tonic::Streaming<core_v1::WorkerControlToKernel>>,
    ) -> Result<Response<Self::ConnectStream>, Status> {
        let mut inbound = request.into_inner();
        let first = inbound.message().await?.ok_or_else(|| {
            semantic_status(
                tonic::Code::InvalidArgument,
                "WORKER_HELLO_REQUIRED",
                "WorkerControlHello must be the first control frame",
            )
        })?;
        let context = authority_call_context_from_proto(first.context.as_ref())?;
        let Some(core_v1::worker_control_to_kernel::Body::Hello(hello)) = first.body else {
            return Err(semantic_status(
                tonic::Code::InvalidArgument,
                "WORKER_HELLO_REQUIRED",
                "WorkerControlHello must be the first control frame",
            ));
        };
        let (outbound, receiver) = mpsc::channel(16);
        let (connection_id, worker) =
            self.register_semantic_worker_control(&context, &hello, outbound.clone())?;
        outbound
            .send(Ok(core_v1::KernelToWorkerControl {
                body: Some(core_v1::kernel_to_worker_control::Body::Welcome(
                    core_v1::WorkerControlWelcome {
                        worker: Some(to_semantic_proto_worker(&worker)),
                        next_heartbeat_after: Some(to_proto_duration(self.heartbeat.interval)),
                    },
                )),
            }))
            .await
            .map_err(|_| {
                semantic_status(
                    tonic::Code::Unavailable,
                    "WORKER_CONTROL_CLOSED",
                    "worker control receiver closed during handshake",
                )
            })?;

        let adapter = self.clone();
        let worker_id = worker.identity.id.clone();
        let worker_generation = worker.identity.generation;
        tokio::spawn(async move {
            while let Ok(Some(frame)) = inbound.message().await {
                let result =
                    authority_call_context_from_proto(frame.context.as_ref()).and_then(|context| {
                        match frame.body {
                            Some(core_v1::worker_control_to_kernel::Body::Heartbeat(heartbeat)) => {
                                let worker =
                                    semantic_identity_from_proto(heartbeat.worker, "worker")?;
                                let lease = semantic_identity_from_proto(heartbeat.lease, "lease")?;
                                adapter
                                    .accept_semantic_worker_heartbeat(
                                        &context,
                                        worker,
                                        lease,
                                        heartbeat.fence_token,
                                    )
                                    .map(|worker| core_v1::KernelToWorkerControl {
                                        body: Some(
                                            core_v1::kernel_to_worker_control::Body::HeartbeatAck(
                                                core_v1::WorkerControlHeartbeatAck {
                                                    worker: Some(to_semantic_proto_worker(&worker)),
                                                    next_heartbeat_after: Some(to_proto_duration(
                                                        adapter.heartbeat.interval,
                                                    )),
                                                },
                                            ),
                                        ),
                                    })
                                    .map(Some)
                            }
                            Some(core_v1::worker_control_to_kernel::Body::ShutdownAck(ack)) => {
                                adapter.accept_semantic_shutdown_ack(&context, &ack)?;
                                Ok(None)
                            }
                            Some(core_v1::worker_control_to_kernel::Body::Hello(_)) | None => {
                                Err(semantic_status(
                                    tonic::Code::InvalidArgument,
                                    "WORKER_FRAME_INVALID",
                                    "WorkerControlHello is valid only as the first control frame",
                                ))
                            }
                        }
                    });
                match result {
                    Ok(Some(response)) => {
                        if outbound.send(Ok(response)).await.is_err() {
                            break;
                        }
                    }
                    Ok(None) => {}
                    Err(error) => {
                        let _ = outbound.send(Err(error)).await;
                        break;
                    }
                }
            }
            adapter.unregister_semantic_worker_control(
                &worker_id,
                worker_generation,
                connection_id,
            );
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(receiver))))
    }
}
