//! KernelServiceAdapter Worker 会话控制、心跳监测与优雅停机调度。

use std::{
    sync::atomic::Ordering,
    thread,
    time::{Duration, Instant},
};

use cy_kernel_api::{semantic, LeaseState, RuntimeJournalEvent};
use cy_proto::core_v1;
use tonic::Status;

use crate::{
    adapter::KernelServiceAdapter,
    convert::{
        now_timestamp, provider_status, semantic_identity_from_proto, semantic_status,
        to_proto_duration, to_semantic_proto_identity,
    },
    session::{
        PendingWorkerShutdown, SemanticWorkerControlSender, SemanticWorkerControlSession,
        WorkerControlSender, WorkerControlSession,
    },
};

impl KernelServiceAdapter {
    pub(crate) fn heartbeat_response(
        &self,
        disposition: core_v1::HeartbeatDisposition,
        sequence: u64,
        generation: u64,
    ) -> core_v1::ReportHeartbeatResponse {
        core_v1::ReportHeartbeatResponse {
            disposition: disposition as i32,
            accepted_sequence_number: sequence,
            server_time: Some(now_timestamp()),
            next_heartbeat_after: Some(prost_types::Duration {
                seconds: self.heartbeat.interval.as_secs() as i64,
                nanos: self.heartbeat.interval.subsec_nanos() as i32,
            }),
            desired_state: core_v1::DesiredPluginState::Running as i32,
            desired_generation: generation,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn accept_heartbeat(
        &self,
        plugin_instance_name: &str,
        generation: u64,
        sequence_number: u64,
        observed_at: Option<prost_types::Timestamp>,
        runtime_state: i32,
        health: Option<core_v1::HealthReport>,
        restart_count: u32,
    ) -> Result<core_v1::ReportHeartbeatResponse, Status> {
        if plugin_instance_name.is_empty() || sequence_number == 0 {
            return Err(Status::invalid_argument(
                "plugin_instance_name and a non-zero sequence_number are required",
            ));
        }
        let mut instances = self.instances.lock().expect("instance lock poisoned");
        let Some(process) = instances.get_mut(plugin_instance_name) else {
            return Ok(self.heartbeat_response(
                core_v1::HeartbeatDisposition::UnknownInstance,
                0,
                generation,
            ));
        };
        if generation != process.generation {
            return Ok(self.heartbeat_response(
                core_v1::HeartbeatDisposition::StaleGeneration,
                process.accepted_sequence,
                process.generation,
            ));
        }
        if process.watchdog_triggered {
            return Ok(core_v1::ReportHeartbeatResponse {
                disposition: core_v1::HeartbeatDisposition::Duplicate as i32,
                accepted_sequence_number: process.accepted_sequence,
                server_time: Some(now_timestamp()),
                next_heartbeat_after: None,
                desired_state: core_v1::DesiredPluginState::Stopped as i32,
                desired_generation: process.generation,
            });
        }
        if sequence_number <= process.accepted_sequence {
            return Ok(self.heartbeat_response(
                core_v1::HeartbeatDisposition::Duplicate,
                process.accepted_sequence,
                process.generation,
            ));
        }
        process.accepted_sequence = sequence_number;
        process.last_heartbeat = Instant::now();
        process.last_heartbeat_at = observed_at.or_else(|| Some(now_timestamp()));
        process.runtime_state = runtime_state;
        process.health = health;
        process.restart_count = restart_count;
        Ok(self.heartbeat_response(
            core_v1::HeartbeatDisposition::Accepted,
            process.accepted_sequence,
            process.generation,
        ))
    }

    pub(crate) fn register_worker_control(
        &self,
        hello: &core_v1::WorkerHello,
        outbound: WorkerControlSender,
    ) -> Result<(u64, core_v1::WorkerWelcome), Status> {
        if hello.plugin_instance_name.is_empty()
            || hello.generation == 0
            || hello.protocol_version != 1
        {
            return Err(Status::invalid_argument(
                "WorkerHello requires instance name, non-zero generation, and protocol_version=1",
            ));
        }
        let connection_id = self.next_control_connection.fetch_add(1, Ordering::Relaxed);
        let mut instances = self.instances.lock().expect("instance lock poisoned");
        let process = instances
            .get_mut(&hello.plugin_instance_name)
            .ok_or_else(|| Status::not_found("worker instance is not managed by this Kernel"))?;
        if process.generation != hello.generation {
            return Err(Status::failed_precondition("worker generation is stale"));
        }
        process.control = Some(WorkerControlSession {
            connection_id,
            outbound,
        });
        Ok((
            connection_id,
            core_v1::WorkerWelcome {
                desired_state: if process.watchdog_triggered {
                    core_v1::DesiredPluginState::Stopped as i32
                } else {
                    core_v1::DesiredPluginState::Running as i32
                },
                desired_generation: process.generation,
                next_heartbeat_after: Some(to_proto_duration(self.heartbeat.interval)),
            },
        ))
    }

    pub(crate) fn unregister_worker_control(
        &self,
        instance_name: &str,
        generation: u64,
        connection_id: u64,
    ) {
        let mut instances = self.instances.lock().expect("instance lock poisoned");
        let Some(process) = instances.get_mut(instance_name) else {
            return;
        };
        if process.generation == generation
            && process
                .control
                .as_ref()
                .is_some_and(|control| control.connection_id == connection_id)
        {
            process.control = None;
        }
    }

    pub(crate) fn register_semantic_worker_control(
        &self,
        hello: &core_v1::WorkerControlHello,
        outbound: SemanticWorkerControlSender,
    ) -> Result<(u64, semantic::Worker), Status> {
        let worker_identity = semantic_identity_from_proto(hello.worker.clone(), "worker")?;
        let lease_identity = semantic_identity_from_proto(hello.lease.clone(), "lease")?;
        let lease = self
            .daemon
            .lease(&lease_identity.id)
            .map_err(provider_status)?;
        if lease.generation != lease_identity.generation
            || lease.fence_token != hello.fence_token
            || lease.holder != worker_identity
            || lease.state != LeaseState::Active
        {
            return Err(semantic_status(
                tonic::Code::FailedPrecondition,
                "FENCE_MISMATCH",
                "worker control hello no longer has active lease authority",
            ));
        }
        let connection_id = self.next_control_connection.fetch_add(1, Ordering::Relaxed);
        let mut instances = self.instances.lock().expect("instance lock poisoned");
        let process = instances.get_mut(&worker_identity.id).ok_or_else(|| {
            semantic_status(
                tonic::Code::NotFound,
                "WORKER_NOT_FOUND",
                "worker is not managed by this Kernel",
            )
        })?;
        let worker = process.semantic_worker.as_ref().ok_or_else(|| {
            semantic_status(
                tonic::Code::FailedPrecondition,
                "WORKER_COMPATIBILITY_ONLY",
                "legacy plugin process cannot use semantic Worker control",
            )
        })?;
        if worker.identity != worker_identity || worker.lease != lease_identity {
            return Err(semantic_status(
                tonic::Code::FailedPrecondition,
                "STALE_GENERATION",
                "worker identity or lease generation is stale",
            ));
        }
        let worker = worker.clone();
        process.semantic_control = Some(SemanticWorkerControlSession {
            connection_id,
            outbound,
        });
        Ok((connection_id, worker))
    }

    pub(crate) fn unregister_semantic_worker_control(
        &self,
        worker_id: &str,
        generation: u64,
        connection_id: u64,
    ) {
        let mut instances = self.instances.lock().expect("instance lock poisoned");
        let Some(process) = instances.get_mut(worker_id) else {
            return;
        };
        if process
            .semantic_worker
            .as_ref()
            .is_some_and(|worker| worker.identity.generation == generation)
            && process
                .semantic_control
                .as_ref()
                .is_some_and(|control| control.connection_id == connection_id)
        {
            process.semantic_control = None;
        }
    }

    pub(crate) fn accept_semantic_shutdown_ack(
        &self,
        ack: &core_v1::WorkerControlShutdownAck,
    ) -> Result<(), Status> {
        let worker_identity = semantic_identity_from_proto(ack.worker.clone(), "worker")?;
        let lease_identity = semantic_identity_from_proto(ack.lease.clone(), "lease")?;
        let mut instances = self.instances.lock().expect("instance lock poisoned");
        let process = instances.get_mut(&worker_identity.id).ok_or_else(|| {
            semantic_status(
                tonic::Code::NotFound,
                "WORKER_NOT_FOUND",
                "worker is not managed by this Kernel",
            )
        })?;
        let worker = process.semantic_worker.as_ref().ok_or_else(|| {
            semantic_status(
                tonic::Code::FailedPrecondition,
                "WORKER_COMPATIBILITY_ONLY",
                "legacy plugin process cannot use semantic Worker control",
            )
        })?;
        let fence_matches = process
            .lease
            .as_ref()
            .is_some_and(|lease| lease.fence_token == ack.fence_token);
        if worker.identity != worker_identity || worker.lease != lease_identity || !fence_matches {
            return Err(semantic_status(
                tonic::Code::FailedPrecondition,
                "FENCE_MISMATCH",
                "worker shutdown acknowledgement is stale",
            ));
        }
        let pending = process.pending_shutdown.as_mut().ok_or_else(|| {
            semantic_status(
                tonic::Code::FailedPrecondition,
                "SHUTDOWN_NOT_REQUESTED",
                "Kernel did not request a worker shutdown",
            )
        })?;
        if pending.shutdown_id != ack.shutdown_id {
            return Err(semantic_status(
                tonic::Code::FailedPrecondition,
                "STALE_GENERATION",
                "worker shutdown acknowledgement is stale",
            ));
        }
        pending.acknowledged = true;
        pending.drained = ack.drained;
        Ok(())
    }

    pub(crate) fn accept_shutdown_ack(
        &self,
        ack: &core_v1::WorkerShutdownAck,
    ) -> Result<(), Status> {
        let mut instances = self.instances.lock().expect("instance lock poisoned");
        let process = instances
            .get_mut(&ack.plugin_instance_name)
            .ok_or_else(|| Status::not_found("worker instance is not managed by this Kernel"))?;
        if ack.generation != process.generation {
            return Err(Status::failed_precondition("worker generation is stale"));
        }
        let pending = process.pending_shutdown.as_mut().ok_or_else(|| {
            Status::failed_precondition("Kernel did not request a worker shutdown")
        })?;
        if pending.shutdown_id != ack.shutdown_id {
            return Err(Status::failed_precondition(
                "worker shutdown acknowledgement is stale",
            ));
        }
        pending.acknowledged = true;
        pending.drained = ack.drained;
        Ok(())
    }

    pub(crate) fn request_worker_shutdown(
        &self,
        instance_name: &str,
        reason_code: &str,
        immediate: bool,
    ) -> bool {
        if immediate {
            return false;
        }
        let shutdown_id = format!(
            "shutdown-{}",
            self.next_control_connection.fetch_add(1, Ordering::Relaxed)
        );
        let outbound = {
            let mut instances = self.instances.lock().expect("instance lock poisoned");
            let Some(process) = instances.get_mut(instance_name) else {
                return false;
            };
            let Some(control) = process.control.as_ref() else {
                return false;
            };
            process.pending_shutdown = Some(PendingWorkerShutdown {
                shutdown_id: shutdown_id.clone(),
                acknowledged: false,
                drained: false,
            });
            control.outbound.clone()
        };
        let sent = outbound.try_send(Ok(core_v1::KernelToWorker {
            body: Some(core_v1::kernel_to_worker::Body::Shutdown(
                core_v1::WorkerShutdown {
                    shutdown_id: shutdown_id.clone(),
                    mode: core_v1::StopMode::Graceful as i32,
                    ack_deadline: Some(to_proto_duration(self.heartbeat.shutdown_ack_timeout)),
                    reason_code: reason_code.to_string(),
                },
            )),
        }));
        if sent.is_err() {
            self.unregister_pending_shutdown(instance_name, &shutdown_id);
            return false;
        }
        let deadline = Instant::now() + self.heartbeat.shutdown_ack_timeout;
        loop {
            let acknowledged = self
                .instances
                .lock()
                .expect("instance lock poisoned")
                .get(instance_name)
                .and_then(|process| process.pending_shutdown.as_ref())
                .is_some_and(|pending| pending.shutdown_id == shutdown_id && pending.acknowledged);
            if acknowledged || Instant::now() >= deadline {
                return acknowledged;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    pub(crate) fn request_semantic_worker_shutdown(
        &self,
        worker_id: &str,
        reason_code: &str,
    ) -> bool {
        let shutdown_id = format!(
            "shutdown-{}",
            self.next_control_connection.fetch_add(1, Ordering::Relaxed)
        );
        let (outbound, worker, lease) = {
            let mut instances = self.instances.lock().expect("instance lock poisoned");
            let Some(process) = instances.get_mut(worker_id) else {
                return false;
            };
            let Some(control) = process.semantic_control.as_ref() else {
                return false;
            };
            let Some(worker) = process.semantic_worker.as_ref() else {
                return false;
            };
            let Some(lease) = process.lease.as_ref() else {
                return false;
            };
            process.pending_shutdown = Some(PendingWorkerShutdown {
                shutdown_id: shutdown_id.clone(),
                acknowledged: false,
                drained: false,
            });
            (control.outbound.clone(), worker.clone(), lease.clone())
        };
        let sent = outbound.try_send(Ok(core_v1::KernelToWorkerControl {
            body: Some(core_v1::kernel_to_worker_control::Body::Shutdown(
                core_v1::WorkerControlShutdown {
                    worker: Some(to_semantic_proto_identity(&worker.identity)),
                    lease: Some(to_semantic_proto_identity(&worker.lease)),
                    fence_token: lease.fence_token,
                    shutdown_id: shutdown_id.clone(),
                    ack_deadline: Some(to_proto_duration(self.heartbeat.shutdown_ack_timeout)),
                    reason_code: reason_code.to_string(),
                },
            )),
        }));
        if sent.is_err() {
            self.unregister_pending_shutdown(worker_id, &shutdown_id);
            return false;
        }
        let deadline = Instant::now() + self.heartbeat.shutdown_ack_timeout;
        loop {
            let acknowledged = self
                .instances
                .lock()
                .expect("instance lock poisoned")
                .get(worker_id)
                .and_then(|process| process.pending_shutdown.as_ref())
                .is_some_and(|pending| pending.shutdown_id == shutdown_id && pending.acknowledged);
            if acknowledged || Instant::now() >= deadline {
                return acknowledged;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    pub(crate) fn accept_semantic_worker_heartbeat(
        &self,
        worker_identity: semantic::Identity,
        lease_identity: semantic::Identity,
        fence_token: u64,
    ) -> Result<semantic::Worker, Status> {
        let lease = self
            .daemon
            .lease(&lease_identity.id)
            .map_err(provider_status)?;
        if lease.generation != lease_identity.generation
            || lease.fence_token != fence_token
            || lease.holder != worker_identity
            || lease.state != LeaseState::Active
        {
            return Err(semantic_status(
                tonic::Code::FailedPrecondition,
                "FENCE_MISMATCH",
                "worker heartbeat no longer has active lease authority",
            ));
        }
        let mut instances = self.instances.lock().expect("instance lock poisoned");
        let process = instances.get_mut(&worker_identity.id).ok_or_else(|| {
            semantic_status(
                tonic::Code::NotFound,
                "WORKER_NOT_FOUND",
                "worker is not managed by this Kernel",
            )
        })?;
        let worker = process.semantic_worker.as_mut().ok_or_else(|| {
            semantic_status(
                tonic::Code::FailedPrecondition,
                "WORKER_COMPATIBILITY_ONLY",
                "legacy plugin process is not a semantic Worker",
            )
        })?;
        if worker.identity != worker_identity || worker.lease != lease_identity {
            return Err(semantic_status(
                tonic::Code::FailedPrecondition,
                "STALE_GENERATION",
                "worker identity or lease generation is stale",
            ));
        }
        if !worker
            .state
            .can_transition_to(semantic::WorkerState::Running)
        {
            return Err(semantic_status(
                tonic::Code::FailedPrecondition,
                "STATE_TRANSITION_INVALID",
                "worker cannot enter RUNNING from its current state",
            ));
        }
        worker.state = semantic::WorkerState::Running;
        process.last_heartbeat = Instant::now();
        process.last_heartbeat_at = Some(now_timestamp());
        let response = worker.clone();
        drop(instances);
        self.publish_semantic_event(
            response.identity.clone(),
            "worker.running",
            "cyrene.worker.v1",
            Vec::new(),
        );
        Ok(response)
    }

    pub(crate) fn unregister_pending_shutdown(&self, instance_name: &str, shutdown_id: &str) {
        if let Some(process) = self
            .instances
            .lock()
            .expect("instance lock poisoned")
            .get_mut(instance_name)
        {
            if process
                .pending_shutdown
                .as_ref()
                .is_some_and(|pending| pending.shutdown_id == shutdown_id)
            {
                process.pending_shutdown = None;
            }
        }
    }

    pub(crate) fn enforce_heartbeat_deadlines(&self) {
        let overdue = {
            let instances = self.instances.lock().expect("instance lock poisoned");
            instances
                .iter()
                .filter(|(_, process)| {
                    !process.watchdog_triggered
                        && process.last_heartbeat.elapsed() > self.heartbeat.timeout
                })
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>()
        };
        for name in overdue {
            let is_semantic_worker = self
                .instances
                .lock()
                .expect("instance lock poisoned")
                .get(&name)
                .is_some_and(|process| process.semantic_worker.is_some());
            let _acknowledged = if is_semantic_worker {
                self.request_semantic_worker_shutdown(&name, "HEARTBEAT_TIMEOUT")
            } else {
                self.request_worker_shutdown(&name, "HEARTBEAT_TIMEOUT", false)
            };
            self.publish_runtime_event(
                core_v1::RuntimeEventType::WatchdogTriggered,
                &name,
                "HEARTBEAT_TIMEOUT",
                "worker missed its mandatory heartbeat deadline",
            );
            let (lease, report) = {
                let mut instances = self.instances.lock().expect("instance lock poisoned");
                let Some(process) = instances.get_mut(&name) else {
                    continue;
                };
                process.watchdog_triggered = true;
                let lease = process.lease.clone();
                match process.instance.stop(&cy_kernel_api::StopRequest {
                    grace_period: self.heartbeat.graceful_stop,
                    immediate: false,
                }) {
                    Ok(report) => (lease, Some(report.clone())),
                    Err(_) => (lease, None),
                }
            };
            if let Some(report) = report.as_ref() {
                self.publish_cleanup_events(&name, report);
                if report.complete {
                    if let Some(lease) = lease.as_ref() {
                        if self
                            .daemon
                            .release(&lease.lease_name, lease.fence_token)
                            .is_ok()
                        {
                            if let Err(error) = self.record_runtime(
                                RuntimeJournalEvent::WatchdogReaped,
                                Some(&name),
                                Some(lease),
                                "HEARTBEAT_TIMEOUT_REAPED",
                            ) {
                                eprintln!(
                                    "runtime journal WatchdogReaped write failed: {error}"
                                );
                            }
                            self.instances
                                .lock()
                                .expect("instance lock poisoned")
                                .remove(&name);
                        }
                    }
                } else {
                    if let Err(error) = self.record_runtime(
                        RuntimeJournalEvent::InstanceCleanupFailed,
                        Some(&name),
                        lease.as_ref(),
                        &report.reason_code,
                    ) {
                        eprintln!(
                            "runtime journal InstanceCleanupFailed write failed: {error}"
                        );
                    }
                }
            } else {
                if let Err(error) = self.record_runtime(
                    RuntimeJournalEvent::InstanceCleanupFailed,
                    Some(&name),
                    lease.as_ref(),
                    "WATCHDOG_STOP_FAILED",
                ) {
                    eprintln!(
                        "runtime journal InstanceCleanupFailed write failed: {error}"
                    );
                }
            }
        }
    }
}
