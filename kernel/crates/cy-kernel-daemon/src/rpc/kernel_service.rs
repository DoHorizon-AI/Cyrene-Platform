// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: kernel/crates/cy-kernel-daemon/src/rpc/kernel_service.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! `core_v1::KernelService` gRPC 服务实现。

use std::time::Duration;

use cy_kernel_api::{ProviderError, ResourceRequest, RuntimeJournalEvent, VerifiedInstallation};
use cy_proto::{core_v1, semantic_v1};
use tokio::sync::{broadcast, mpsc};
use tokio_stream::{wrappers::ReceiverStream, Stream};
use tonic::{Request, Response, Status};

use crate::{
    adapter::{KernelServiceAdapter, OPERATION_EVENT_SUBSCRIBER_CAPACITY},
    convert::{
        cgroup_limits, expires_after, inject_heartbeat_environment, legacy_holder,
        operation_event_matches, proto_duration, provider_status, resource_request,
        semantic_identity_from_proto, semantic_query_from_proto, to_semantic_proto_lease,
    },
    session::ManagedProcess,
    watchdog::InstanceActor,
};

#[tonic::async_trait]
impl core_v1::kernel_service_server::KernelService for KernelServiceAdapter {
    async fn get_kernel_capabilities(
        &self,
        request: Request<core_v1::GetKernelCapabilitiesRequest>,
    ) -> Result<Response<core_v1::KernelCapabilities>, Status> {
        let request = request.into_inner();
        self.validate_node(request.node.as_ref())?;
        self.daemon
            .get_kernel_capabilities()
            .map(Response::new)
            .map_err(provider_status)
    }

    async fn acquire_lease(
        &self,
        request: Request<core_v1::LegacyAcquireLeaseRequest>,
    ) -> Result<Response<semantic_v1::Lease>, Status> {
        let request = request.into_inner();
        self.validate_node(request.node.as_ref())?;
        let lease_name = self.lease_name(request.mutation.as_ref())?;
        let generation = request
            .mutation
            .as_ref()
            .and_then(|mutation| mutation.expected_generation)
            .unwrap_or_else(|| self.daemon.resources.inventory().generation);
        let holder = semantic_identity_from_proto(request.holder, "holder")?;
        let query = semantic_query_from_proto(
            request
                .query
                .ok_or_else(|| Status::invalid_argument("resource query is required"))?,
        )?;
        let ttl = proto_duration(
            request
                .ttl
                .ok_or_else(|| Status::invalid_argument("lease ttl is required"))?,
        )?;
        if ttl.is_zero() {
            return Err(Status::invalid_argument("lease ttl must be positive"));
        }
        let limits = cgroup_limits(request.cpu.as_ref(), request.memory.as_ref())?;
        let lease = self
            .daemon
            .acquire(ResourceRequest {
                lease_name,
                expected_inventory_generation: generation,
                holder,
                query,
                expires_at_unix_ms: Some(expires_after(ttl)),
                limits,
            })
            .map_err(provider_status)?;
        let journal_lease = core_v1::ResourceLeaseRef {
            lease_name: lease.name.clone(),
            fence_token: lease.fence_token,
        };
        if let Err(error) = self.record_runtime(
            RuntimeJournalEvent::LeaseAcquired,
            None,
            Some(&journal_lease),
            "LEASE_ACQUIRED",
        ) {
            // Durable fence record could not be persisted: roll back the
            // in-memory lease so it is never externally visible without the
            // durable evidence the contract requires.
            let _ = self.daemon.begin_release(&lease.name, lease.fence_token);
            let _ = self.daemon.complete_release(&lease.name, lease.fence_token);
            return Err(provider_status(error));
        }
        Ok(Response::new(to_semantic_proto_lease(&lease)))
    }

    async fn release_lease(
        &self,
        request: Request<core_v1::LegacyReleaseLeaseRequest>,
    ) -> Result<Response<semantic_v1::Lease>, Status> {
        let request = request.into_inner();
        let lease_identity = semantic_identity_from_proto(request.lease, "lease")?;
        let current = self
            .daemon
            .lease(&lease_identity.id)
            .map_err(provider_status)?;
        if current.generation != lease_identity.generation {
            return Err(Status::failed_precondition("stale lease generation"));
        }
        let journal_lease = core_v1::ResourceLeaseRef {
            lease_name: lease_identity.id.clone(),
            fence_token: request.fence_token,
        };
        // A compatibility caller must not be able to free hardware that a
        // running instance still holds: RELEASED is reached only through
        // RELEASING plus a confirmed sandbox cleanup.
        self.release_lease_with_cleanup(&journal_lease)
            .map_err(provider_status)?;
        let lease = self
            .daemon
            .lease(&lease_identity.id)
            .map_err(provider_status)?;
        Ok(Response::new(to_semantic_proto_lease(&lease)))
    }

    async fn launch_process(
        &self,
        request: Request<core_v1::LaunchProcessRequest>,
    ) -> Result<Response<core_v1::Operation>, Status> {
        let request = request.into_inner();
        self.validate_node(request.node.as_ref())?;
        let plugin = request
            .plugin
            .ok_or_else(|| Status::invalid_argument("installed plugin reference is required"))?;
        if plugin.installation_name.is_empty()
            || plugin.manifest_digest.is_empty()
            || plugin.artifact_digest.is_empty()
            || plugin.verified_signature_identity.is_empty()
        {
            return Err(Status::failed_precondition(
                "LaunchProcess accepts only a verified InstalledPluginRef",
            ));
        }
        let instance_name = plugin.installation_name.clone();
        let operation_name = self.operation_name("launch", &instance_name);
        if self
            .instances
            .lock()
            .expect("instance lock poisoned")
            .contains_key(&instance_name)
        {
            return Err(Status::already_exists(
                "plugin instance is already managed by this Kernel",
            ));
        }
        let (lease, owned_lease) = match request.allocation {
            Some(core_v1::launch_process_request::Allocation::ExistingLease(lease_ref)) => (
                self.daemon
                    .lease(&lease_ref.lease_name)
                    .map_err(provider_status)?,
                false,
            ),
            Some(core_v1::launch_process_request::Allocation::ResourceClaim(requirements)) => {
                let lease_name = self.lease_name(request.mutation.as_ref())?;
                let generation = request
                    .mutation
                    .as_ref()
                    .and_then(|mutation| mutation.expected_generation)
                    .unwrap_or_else(|| self.daemon.resources.inventory().generation);
                let internal = resource_request(
                    &lease_name,
                    generation,
                    legacy_holder(request.mutation.as_ref(), &instance_name),
                    None,
                    &requirements,
                )?;
                (
                    self.daemon.acquire(internal).map_err(provider_status)?,
                    true,
                )
            }
            None => return Err(Status::invalid_argument("allocation is required")),
        };
        let binding = match self.daemon.binding_for_lease(&lease) {
            Ok(binding) => binding,
            Err(error) => {
                return Err(provider_status(
                    self.release_owned_lease(owned_lease, &lease)
                        .err()
                        .unwrap_or(error),
                ));
            }
        };
        let installation = VerifiedInstallation {
            installation_name: plugin.installation_name.clone(),
            manifest_digest: plugin.manifest_digest.clone(),
            artifact_digest: plugin.artifact_digest.clone(),
            verified_signature_identity: plugin.verified_signature_identity.clone(),
        };
        let resolved = match self
            .resolver
            .resolve_launch_plan(&installation, &instance_name)
        {
            Ok(plan) => plan,
            Err(error) => {
                return Err(provider_status(
                    self.release_owned_lease(owned_lease, &lease)
                        .err()
                        .unwrap_or(error),
                ));
            }
        };
        if resolved.installation != installation {
            if let Err(error) = self.release_owned_lease(owned_lease, &lease) {
                return Err(provider_status(error));
            }
            return Err(Status::failed_precondition(
                "resolver returned a launch plan for another verified installation",
            ));
        }
        let mut plan = resolved.plan;
        plan.limits = lease.limits.clone();
        plan.environment = inject_heartbeat_environment(
            plan.environment,
            &self.heartbeat,
            &instance_name,
            lease.fence_token,
        )
        .map_err(|error| {
            provider_status(
                self.release_owned_lease(owned_lease, &lease)
                    .err()
                    .unwrap_or(error),
            )
        })?;
        plan.environment = binding
            .merge_environment(&plan.environment)
            .map_err(|error| {
                provider_status(
                    self.release_owned_lease(owned_lease, &lease)
                        .err()
                        .unwrap_or(error),
                )
            })?;
        let mut actor = InstanceActor::new(
            instance_name.clone(),
            lease.name.clone(),
            lease.fence_token,
            self.daemon.sandbox.clone(),
            plan,
            binding,
            self.heartbeat.timeout,
        );
        let lease_ref = core_v1::ResourceLeaseRef {
            lease_name: lease.name.clone(),
            fence_token: lease.fence_token,
        };
        // Class B durable intent (canonical parity with start_worker): persist
        // BEFORE the physical spawn so restart recovery can classify a launch
        // whose outcome records are lost as intent-without-outcome.
        if let Err(error) = self.record_runtime(
            RuntimeJournalEvent::InstanceLaunching,
            Some(&instance_name),
            Some(&lease_ref),
            "WORKER_LAUNCHING",
        ) {
            return Err(provider_status(
                self.release_owned_lease(owned_lease, &lease)
                    .err()
                    .unwrap_or(error),
            ));
        }
        if let Err(error) = actor.start() {
            return Err(provider_status(
                self.release_owned_lease(owned_lease, &lease)
                    .err()
                    .unwrap_or(error),
            ));
        }
        let evidence = match actor.recovery_evidence() {
            Ok(evidence) => evidence,
            Err(error) => {
                let stop_report = actor.stop(&cy_kernel_api::StopRequest {
                    grace_period: Duration::ZERO,
                    immediate: true,
                });
                if let Ok(report) = &stop_report {
                    if !report.complete {
                        let _ = self.record_runtime(
                            RuntimeJournalEvent::InstanceCleanupFailed,
                            Some(&instance_name),
                            Some(&lease_ref),
                            &report.reason_code,
                        );
                    }
                }
                return Err(provider_status(error));
            }
        };
        if let Err(error) = self.record_runtime_launch(&instance_name, &lease_ref, evidence) {
            let stop_report = actor.stop(&cy_kernel_api::StopRequest {
                grace_period: Duration::ZERO,
                immediate: true,
            });
            if let Ok(report) = &stop_report {
                if !report.complete {
                    let _ = self.record_runtime(
                        RuntimeJournalEvent::InstanceCleanupFailed,
                        Some(&instance_name),
                        Some(&lease_ref),
                        &report.reason_code,
                    );
                }
            }
            return Err(provider_status(error));
        }
        self.instances
            .lock()
            .expect("instance lock poisoned")
            .insert(
                instance_name.clone(),
                ManagedProcess {
                    actor,
                    lease: Some(lease_ref.clone()),
                    semantic_worker: None,
                    plugin,
                    generation: lease_ref.fence_token,
                    accepted_sequence: 0,
                    last_heartbeat_at: None,
                    runtime_state: core_v1::PluginRuntimeState::Starting as i32,
                    health: None,
                    restart_count: 0,
                    watchdog_triggered: false,
                    transport_disconnected: false,
                    control: None,
                    semantic_control: None,
                    pending_shutdown: None,
                },
            );
        self.publish_runtime_event(
            core_v1::RuntimeEventType::InstanceStateChanged,
            &instance_name,
            "WORKER_LAUNCHED",
            "worker process started inside its assigned sandbox",
        );
        Ok(Response::new(
            self.operation_running(operation_name, instance_name),
        ))
    }

    async fn terminate_process(
        &self,
        request: Request<core_v1::TerminateProcessRequest>,
    ) -> Result<Response<core_v1::Operation>, Status> {
        let request = request.into_inner();
        if request.process_name.is_empty() {
            return Err(Status::invalid_argument("process_name is required"));
        }
        let operation_name = self.operation_name("terminate", &request.process_name);
        let grace_period = request
            .grace_period
            .map(proto_duration)
            .transpose()?
            .unwrap_or(Duration::from_secs(30));
        let immediate = request.mode == core_v1::StopMode::Immediate as i32;
        let _acknowledged =
            self.request_worker_shutdown(&request.process_name, "TERMINATE_REQUESTED", immediate);
        let lease = self
            .instances
            .lock()
            .expect("instance lock poisoned")
            .get(&request.process_name)
            .ok_or_else(|| Status::not_found("plugin process is not managed by this Kernel"))?
            .lease
            .clone();
        if let Some(lease) = lease.as_ref() {
            self.record_runtime(
                RuntimeJournalEvent::LeaseReleaseStarted,
                Some(&request.process_name),
                Some(lease),
                "LEASE_RELEASE_STARTED",
            )
            .map_err(provider_status)?;
            self.daemon
                .begin_release(&lease.lease_name, lease.fence_token)
                .map_err(provider_status)?;
        }
        let report = {
            let mut instances = self.instances.lock().expect("instance lock poisoned");
            let process = instances
                .get_mut(&request.process_name)
                .ok_or_else(|| Status::not_found("plugin process is not managed by this Kernel"))?;
            let report = process
                .actor
                .stop(&cy_kernel_api::StopRequest {
                    grace_period,
                    immediate,
                })
                .map_err(|error| {
                    if let Some(lease) = lease.as_ref() {
                        let _ = self
                            .daemon
                            .fail_release(&lease.lease_name, lease.fence_token);
                    }
                    provider_status(error)
                })?
                .clone();
            report
        };
        self.publish_cleanup_events(&request.process_name, &report);
        if !report.complete {
            if let Some(lease) = lease.as_ref() {
                let _ = self
                    .daemon
                    .fail_release(&lease.lease_name, lease.fence_token);
            }
            if let Err(error) = self.record_runtime(
                RuntimeJournalEvent::InstanceCleanupFailed,
                Some(&request.process_name),
                lease.as_ref(),
                &report.reason_code,
            ) {
                // Class C: the Lease was already fail_released (FAILED) with
                // the allocation held; this record is telemetry.
                eprintln!("runtime journal InstanceCleanupFailed write failed: {error}");
            }
            let error =
                ProviderError::new("kernel-daemon", "RESOURCE_QUARANTINED", &report.reason_code);
            return Ok(Response::new(self.operation_failure(
                operation_name,
                request.process_name,
                &error,
            )));
        }
        self.record_runtime(
            RuntimeJournalEvent::InstanceTerminated,
            Some(&request.process_name),
            lease.as_ref(),
            "TERMINATE_COMPLETE",
        )
        .map_err(provider_status)?;
        if let Some(lease) = lease.as_ref() {
            if let Err(error) = self.record_runtime(
                RuntimeJournalEvent::LeaseReleased,
                Some(&request.process_name),
                Some(lease),
                "LEASE_RELEASED",
            ) {
                let _ = self
                    .daemon
                    .fail_release(&lease.lease_name, lease.fence_token);
                return Err(provider_status(error));
            }
            if let Err(error) = self
                .daemon
                .complete_release(&lease.lease_name, lease.fence_token)
            {
                let _ = self
                    .daemon
                    .fail_release(&lease.lease_name, lease.fence_token);
                return Err(provider_status(error));
            }
        }
        // A terminated Worker that held the released Lease no longer has
        // authority: its Endpoint/Grant metadata must not outlive the Lease.
        let worker_identity = self
            .instances
            .lock()
            .expect("instance lock poisoned")
            .get(&request.process_name)
            .and_then(|process| {
                process
                    .semantic_worker
                    .as_ref()
                    .map(|worker| worker.identity.clone())
            });
        self.instances
            .lock()
            .expect("instance lock poisoned")
            .remove(&request.process_name);
        if let Some(identity) = worker_identity {
            self.authority.purge_endpoint_authority(&identity);
        }
        Ok(Response::new(
            self.operation_success(operation_name, request.process_name),
        ))
    }

    async fn get_operation(
        &self,
        request: Request<core_v1::GetOperationRequest>,
    ) -> Result<Response<core_v1::Operation>, Status> {
        let name = request.into_inner().name;
        self.operations
            .lock()
            .expect("operation lock poisoned")
            .get(&name)
            .cloned()
            .map(Response::new)
            .ok_or_else(|| Status::not_found("operation not found"))
    }

    async fn cancel_operation(
        &self,
        request: Request<core_v1::LegacyCancelOperationRequest>,
    ) -> Result<Response<core_v1::Operation>, Status> {
        let name = request.into_inner().name;
        if name.is_empty() {
            return Err(Status::invalid_argument("operation name is required"));
        }
        let operation = self
            .operations
            .lock()
            .expect("operation lock poisoned")
            .get(&name)
            .cloned()
            .ok_or_else(|| Status::not_found("operation not found"))?;
        if operation.state != core_v1::OperationState::Running as i32 || !operation.cancellable {
            return Err(Status::failed_precondition("operation is not cancellable"));
        }
        let target = operation.target_resource_name;
        let _acknowledged = self.request_worker_shutdown(&target, "OPERATION_CANCELLED", false);
        let lease = self
            .instances
            .lock()
            .expect("instance lock poisoned")
            .get(&target)
            .ok_or_else(|| Status::failed_precondition("operation target is no longer managed"))?
            .lease
            .clone();
        if let Some(lease) = lease.as_ref() {
            self.record_runtime(
                RuntimeJournalEvent::LeaseReleaseStarted,
                Some(&target),
                Some(lease),
                "LEASE_RELEASE_STARTED",
            )
            .map_err(provider_status)?;
            self.daemon
                .begin_release(&lease.lease_name, lease.fence_token)
                .map_err(provider_status)?;
        }
        let report = {
            let mut instances = self.instances.lock().expect("instance lock poisoned");
            let process = instances.get_mut(&target).ok_or_else(|| {
                Status::failed_precondition("operation target is no longer managed")
            })?;
            let report = process
                .actor
                .stop(&cy_kernel_api::StopRequest {
                    grace_period: self.heartbeat.graceful_stop,
                    immediate: false,
                })
                .map_err(|error| {
                    if let Some(lease) = lease.as_ref() {
                        let _ = self
                            .daemon
                            .fail_release(&lease.lease_name, lease.fence_token);
                    }
                    provider_status(error)
                })?
                .clone();
            report
        };
        self.publish_cleanup_events(&target, &report);
        if !report.complete {
            if let Some(lease) = lease.as_ref() {
                let _ = self
                    .daemon
                    .fail_release(&lease.lease_name, lease.fence_token);
            }
            if let Err(error) = self.record_runtime(
                RuntimeJournalEvent::InstanceCleanupFailed,
                Some(&target),
                lease.as_ref(),
                &report.reason_code,
            ) {
                // Class C: the Lease was already fail_released (FAILED) with
                // the allocation held; this record is telemetry.
                eprintln!("runtime journal InstanceCleanupFailed write failed: {error}");
            }
            let error =
                ProviderError::new("kernel-daemon", "RESOURCE_QUARANTINED", &report.reason_code);
            return Ok(Response::new(self.operation_failure(name, target, &error)));
        }
        self.record_runtime(
            RuntimeJournalEvent::InstanceTerminated,
            Some(&target),
            lease.as_ref(),
            "CANCEL_COMPLETE",
        )
        .map_err(provider_status)?;
        if let Some(lease) = lease.as_ref() {
            if let Err(error) = self.record_runtime(
                RuntimeJournalEvent::LeaseReleased,
                Some(&target),
                Some(lease),
                "LEASE_RELEASED",
            ) {
                let _ = self
                    .daemon
                    .fail_release(&lease.lease_name, lease.fence_token);
                return Err(provider_status(error));
            }
            if let Err(error) = self
                .daemon
                .complete_release(&lease.lease_name, lease.fence_token)
            {
                let _ = self
                    .daemon
                    .fail_release(&lease.lease_name, lease.fence_token);
                return Err(provider_status(error));
            }
        }
        // A cancelled Worker that held the released Lease no longer has
        // authority: its Endpoint/Grant metadata must not outlive the Lease.
        let worker_identity = self
            .instances
            .lock()
            .expect("instance lock poisoned")
            .get(&target)
            .and_then(|process| {
                process
                    .semantic_worker
                    .as_ref()
                    .map(|worker| worker.identity.clone())
            });
        self.instances
            .lock()
            .expect("instance lock poisoned")
            .remove(&target);
        if let Some(identity) = worker_identity {
            self.authority.purge_endpoint_authority(&identity);
        }
        Ok(Response::new(self.operation_cancelled(name, target)))
    }

    type WatchOperationsStream = std::pin::Pin<
        Box<dyn Stream<Item = Result<core_v1::OperationEvent, Status>> + Send + 'static>,
    >;

    async fn watch_operations(
        &self,
        request: Request<core_v1::WatchOperationsRequest>,
    ) -> Result<Response<Self::WatchOperationsStream>, Status> {
        let request = request.into_inner();
        let resume_sequence = if request.resume_token.is_empty() {
            0
        } else {
            request.resume_token.parse::<u64>().map_err(|_| {
                Status::invalid_argument("resume_token must be an event sequence number")
            })?
        };
        let names = request.operation_names;
        let receiver = self.operation_event_sender.subscribe();
        let history = self
            .operation_events
            .lock()
            .expect("operation event history lock poisoned")
            .iter()
            .filter(|event| event.sequence_number > resume_sequence)
            .filter(|event| operation_event_matches(event, &names))
            .cloned()
            .collect::<Vec<_>>();
        let last_sequence = history
            .last()
            .map(|event| event.sequence_number)
            .unwrap_or(resume_sequence);
        let (sender, stream) = mpsc::channel(OPERATION_EVENT_SUBSCRIBER_CAPACITY);
        tokio::spawn(async move {
            let mut receiver = receiver;
            let mut cursor = last_sequence;
            for event in history {
                if sender.send(Ok(event)).await.is_err() {
                    return;
                }
            }
            loop {
                match receiver.recv().await {
                    Ok(event) if event.sequence_number <= cursor => continue,
                    Ok(event) => {
                        cursor = event.sequence_number;
                        if operation_event_matches(&event, &names)
                            && sender.send(Ok(event)).await.is_err()
                        {
                            return;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        let _ = sender
                            .send(Err(Status::out_of_range(
                                "operation event subscriber lagged beyond the bounded buffer",
                            )))
                            .await;
                        return;
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(stream))))
    }
}
