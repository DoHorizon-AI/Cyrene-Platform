//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 controller.rs                                                   │
//! │  Module: cy_execution_control::controller                           │
//! │  Role: One-shot placement, Lease acquisition, and dispatch.         │
//! │                                                                     │
//! │  模块职责：执行单次 placement、Lease 获取与 assignment 下发。          │
//! └─────────────────────────────────────────────────────────────────────┘

use std::fmt;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cy_execution_fabric::{
    place_execution_target, semantic_lease_from_proto, ExecutionPlacementRequest,
    ExecutionTargetCandidate, FabricContractError, RuntimeAssignmentBuilder,
};
use cy_kernel_api::{AuthorityCallContext, DEFAULT_NAMESPACE};
use cy_kernel_contract as semantic;
use cy_proto::core_v1::{self, AssignmentAckDisposition, NodeRef, RuntimeObservation};
use thiserror::Error;

use crate::intent::{
    ExecutionIntentRecord, ExecutionIntentStore, IntentDisposition, IntentStoreError,
    IntentStoreErrorKind,
};
use crate::server::{HostRouteSnapshot, LeaseAcquisition};
use crate::{ExecutionControlService, RuntimeLauncher};

/// Stable control-plane failure with explicit unknown-outcome classification.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[error("{reason_code}: {message}")]
pub struct DispatchError {
    pub reason_code: String,
    pub message: String,
    pub reconciliation_required: bool,
}

impl DispatchError {
    pub(crate) fn input(reason_code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            reason_code: reason_code.into(),
            message: message.into(),
            reconciliation_required: false,
        }
    }

    pub(crate) fn transient(reason_code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::input(reason_code, message)
    }

    pub(crate) fn unknown(message: impl Into<String>) -> Self {
        Self {
            reason_code: "UNKNOWN_REQUIRES_RECONCILIATION".to_string(),
            message: message.into(),
            reconciliation_required: true,
        }
    }

    pub(crate) fn authority(rejection: semantic::Rejection) -> Self {
        Self {
            reason_code: rejection.reason_code,
            message: rejection.message,
            reconciliation_required: false,
        }
    }
}

impl From<FabricContractError> for DispatchError {
    fn from(error: FabricContractError) -> Self {
        Self::input(error.reason_code, error.message)
    }
}

/// Fully specified intent. Every correlation and idempotency identity comes
/// from durable caller state; the controller never replaces one after timeout.
#[derive(Debug, Clone, PartialEq)]
pub struct ExecutionDispatchRequest {
    pub placement: ExecutionPlacementRequest,
    pub candidates: Vec<ExecutionTargetCandidate>,
    pub assignment: RuntimeAssignmentBuilder,
    /// Caller-computed digest over the immutable Product execution intent.
    pub intent_payload_digest: String,
    pub acquire_command_id: String,
    pub acquire_context: AuthorityCallContext,
    pub release_command_id: String,
    pub release_context: AuthorityCallContext,
    pub lease_ttl: Duration,
}

/// Successful delivery evidence. The canonical Lease remains Kernel-owned;
/// the intent store retains only its immutable identity/fence observation.
#[derive(Debug, Clone, PartialEq)]
pub struct DispatchReceipt {
    pub node: NodeRef,
    pub assignment: core_v1::RuntimeAssignment,
    pub lease: semantic::Lease,
    pub ack_disposition: AssignmentAckDisposition,
}

/// Exact authority information required to release a completed assignment.
#[derive(Debug, Clone, PartialEq)]
pub struct ExecutionReleaseRequest {
    pub assignment_id: String,
    pub node: NodeRef,
    pub lease: semantic::Lease,
    pub command_id: String,
    pub context: AuthorityCallContext,
}

/// Exact correlation and shutdown policy for one accepted assignment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionStopRequest {
    pub assignment_id: String,
    pub command_id: String,
    pub grace_period: Duration,
    pub reason_code: String,
}

/// Whether the Runtime accepted this command or terminal evidence already made
/// the requested outcome true before command delivery completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopDisposition {
    Accepted,
    AlreadyTerminal,
}

/// Stop-command acknowledgement. `Accepted` is not terminal completion;
/// callers must await durable terminal evidence before releasing authority.
#[derive(Debug, Clone, PartialEq)]
pub struct StopReceipt {
    pub disposition: StopDisposition,
    pub terminal_observation: Option<RuntimeObservation>,
}

/// Product-neutral orchestrator over the one canonical NodeControl service.
#[derive(Clone)]
pub struct ExecutionController {
    service: ExecutionControlService,
    response_timeout: Duration,
    intent_store: Arc<dyn ExecutionIntentStore>,
}

impl fmt::Debug for ExecutionController {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExecutionController")
            .field("response_timeout", &self.response_timeout)
            .finish_non_exhaustive()
    }
}

impl ExecutionController {
    pub fn new(
        service: ExecutionControlService,
        response_timeout: Duration,
        intent_store: Arc<dyn ExecutionIntentStore>,
    ) -> Result<Self, DispatchError> {
        if response_timeout.is_zero() {
            return Err(DispatchError::input(
                "CONTROL_TIMEOUT_INVALID",
                "Agent response timeout must be positive",
            ));
        }
        Ok(Self {
            service,
            response_timeout,
            intent_store,
        })
    }

    pub fn intent_record(
        &self,
        assignment_id: &str,
    ) -> Result<Option<ExecutionIntentRecord>, DispatchError> {
        self.intent_store
            .load(assignment_id)
            .map_err(|error| store_error(error, false))
    }

    pub fn intent_disposition(
        &self,
        assignment_id: &str,
    ) -> Result<Option<IntentDisposition>, DispatchError> {
        self.intent_record(assignment_id)
            .map(|record| record.map(|record| record.disposition()))
    }

    /// Select a target, acquire exactly one canonical Lease, and deliver the
    /// resulting immutable assignment to the authenticated Runtime generation.
    pub async fn dispatch(
        &self,
        request: ExecutionDispatchRequest,
    ) -> Result<DispatchReceipt, DispatchError> {
        self.dispatch_inner(request, None).await
    }

    /// Acquire and durably correlate one Lease before launching an initially
    /// offline Runtime. Authentication and assignment delivery use that Lease.
    pub async fn dispatch_with_launcher(
        &self,
        request: ExecutionDispatchRequest,
        launcher: &dyn RuntimeLauncher,
    ) -> Result<DispatchReceipt, DispatchError> {
        self.dispatch_inner(request, Some(launcher)).await
    }

    /// Reconcile the exact Runtime generation and canonical Lease retained by
    /// an earlier unknown launch or assignment delivery. This path never calls
    /// AcquireLease and never selects a replacement Node. The caller must first
    /// read the still-active Lease from Kernel authority and supply that exact
    /// observation; the durable intent record only stores correlation evidence.
    pub async fn reconcile_with_launcher(
        &self,
        mut request: ExecutionDispatchRequest,
        lease: semantic::Lease,
        launcher: &dyn RuntimeLauncher,
    ) -> Result<DispatchReceipt, DispatchError> {
        let now = now_unix_ms();
        request.placement.now_unix_ms = now;
        validate_request(&request, now)?;
        request.assignment.validate(now)?;
        lease.validate().map_err(|error| {
            DispatchError::input(
                error.reason_code,
                format!("invalid reconciliation Lease: {}", error.message),
            )
        })?;

        let target = place_execution_target(&request.placement, &request.candidates)?;
        request
            .assignment
            .validate_artifact_projection(&request.placement, target)?;
        let node = target.node.clone();
        let runtime = request.assignment.runtime().clone();
        let assignment_id = request.assignment.assignment_id().to_string();
        let mut intent = self.intent_record(&assignment_id)?.ok_or_else(|| {
            DispatchError::input(
                "EXECUTION_INTENT_NOT_FOUND",
                "reconciliation requires the original durable execution intent",
            )
        })?;
        if intent.payload_digest() != request.intent_payload_digest {
            return Err(DispatchError::input(
                "EXECUTION_INTENT_PAYLOAD_MISMATCH",
                "reconciliation request does not match the immutable intent digest",
            ));
        }
        if intent.disposition() != IntentDisposition::UnknownRequiresReconciliation {
            return Err(DispatchError::input(
                "EXECUTION_INTENT_NOT_RECONCILABLE",
                "only an unknown execution intent can enter launch reconciliation",
            ));
        }
        if !intent.matches_authority(&node, &lease) {
            return Err(DispatchError::input(
                "RECONCILIATION_AUTHORITY_MISMATCH",
                "the observed Node or Lease does not match durable authority evidence",
            ));
        }
        if lease
            .expires_at_unix_ms
            .is_some_and(|expires_at| expires_at <= now)
        {
            return Err(DispatchError::unknown(
                "the original Lease expired; fence the old execution before creating a new generation",
            ));
        }
        let assignment = request.assignment.build(&lease, now)?;
        let _host_route = self.service.snapshot_host_route(&node).map_err(|_| {
            DispatchError::unknown(
                "the original Node route is unavailable; retain the intent until authority can be reconciled",
            )
        })?;

        tokio::time::timeout(
            self.response_timeout,
            launcher.reconcile(&node, &assignment, &lease),
        )
        .await
        .map_err(|_| DispatchError::unknown("Runtime launch reconciliation timed out"))?
        .map_err(|_| {
            DispatchError::unknown(
                "the original Runtime instance was not confirmed; no replacement was created",
            )
        })?;

        let deadline = tokio::time::Instant::now() + self.response_timeout;
        let route = loop {
            match self.service.snapshot_route(&node, &runtime) {
                Ok(route) => {
                    let session = self.service.runtime_session_on_route(&route).map_err(|_| {
                        DispatchError::unknown(
                            "the reconnected Runtime session could not be verified",
                        )
                    })?;
                    validate_workload_scope(&request.assignment, &session).map_err(|_| {
                        DispatchError::unknown(
                            "the reconnected Runtime scope does not match the original workload",
                        )
                    })?;
                    break route;
                }
                Err(error)
                    if error.reason_code == "RUNTIME_SESSION_UNAVAILABLE"
                        && tokio::time::Instant::now() < deadline =>
                {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                Err(_) => {
                    return Err(DispatchError::unknown(
                        "the original Runtime did not authenticate during reconciliation",
                    ));
                }
            }
        };

        intent = self
            .transition_intent(
                &intent,
                IntentDisposition::AssignmentDispatching,
                Some((&node, &lease)),
                now_unix_ms(),
            )
            .map_err(|error| store_error(error, true))?;
        let ack = match self
            .service
            .dispatch_assignment_on_route(&route.runtime, assignment.clone(), self.response_timeout)
            .await
        {
            Ok(ack) => ack,
            Err(error) => {
                let _ = self.transition_intent(
                    &intent,
                    IntentDisposition::UnknownRequiresReconciliation,
                    None,
                    now_unix_ms(),
                );
                return Err(DispatchError::unknown(format!(
                    "reconciled assignment delivery remains unknown: {error}"
                )));
            }
        };
        let disposition = AssignmentAckDisposition::try_from(ack.disposition).map_err(|_| {
            let _ = self.transition_intent(
                &intent,
                IntentDisposition::UnknownRequiresReconciliation,
                None,
                now_unix_ms(),
            );
            DispatchError::unknown("Runtime returned an unknown reconciliation disposition")
        })?;
        match disposition {
            AssignmentAckDisposition::Accepted | AssignmentAckDisposition::Duplicate => {
                self.transition_intent(&intent, IntentDisposition::Completed, None, now_unix_ms())
                    .map_err(|error| store_error(error, true))?;
                Ok(DispatchReceipt {
                    node,
                    assignment,
                    lease,
                    ack_disposition: disposition,
                })
            }
            AssignmentAckDisposition::Rejected | AssignmentAckDisposition::Unspecified => {
                let _ = self.transition_intent(
                    &intent,
                    IntentDisposition::UnknownRequiresReconciliation,
                    None,
                    now_unix_ms(),
                );
                Err(DispatchError::unknown(
                    "Runtime did not confirm the original assignment; fence it before retrying",
                ))
            }
        }
    }

    async fn dispatch_inner(
        &self,
        mut request: ExecutionDispatchRequest,
        launcher: Option<&dyn RuntimeLauncher>,
    ) -> Result<DispatchReceipt, DispatchError> {
        let placement_time = now_unix_ms();
        request.placement.now_unix_ms = placement_time;
        validate_request(&request, placement_time)?;
        request.assignment.validate(placement_time)?;

        let target = place_execution_target(&request.placement, &request.candidates)?;
        request
            .assignment
            .validate_artifact_projection(&request.placement, target)?;
        let target_node = target.node.clone();
        let runtime = request.assignment.runtime().clone();
        let initial_route = if launcher.is_none() {
            let route = self.service.snapshot_route(&target_node, &runtime)?;
            let runtime_session = self.service.runtime_session_on_route(&route)?;
            validate_workload_scope(&request.assignment, &runtime_session)?;
            Some(route)
        } else {
            None
        };
        let host_route = match &initial_route {
            Some(route) => route.host.clone(),
            None => self.service.snapshot_host_route(&target_node)?,
        };

        let assignment_id = request.assignment.assignment_id().to_string();
        let mut intent = self.begin_intent(
            &assignment_id,
            &request.intent_payload_digest,
            placement_time,
        )?;
        let acquisition = LeaseAcquisition {
            node: &target_node,
            command_id: &request.acquire_command_id,
            context: &request.acquire_context,
            holder: &runtime,
            query: &request.placement.resource_query,
            ttl: request.lease_ttl,
            response_timeout: self.response_timeout,
        };
        let acquired = match &initial_route {
            Some(route) => {
                self.service
                    .acquire_lease_on_route(route, acquisition)
                    .await
            }
            None => {
                self.service
                    .acquire_lease_on_host_route(&host_route, acquisition)
                    .await
            }
        };
        let lease_proto = match acquired {
            Ok(lease) => lease,
            Err(error) => {
                if let Err(store_failure) = self.transition_intent(
                    &intent,
                    if error.reconciliation_required {
                        IntentDisposition::UnknownRequiresReconciliation
                    } else {
                        IntentDisposition::Failed
                    },
                    None,
                    now_unix_ms(),
                ) {
                    return Err(DispatchError::unknown(format!(
                        "Lease acquisition failed ({error}); durable intent transition also failed ({store_failure})"
                    )));
                }
                return Err(error);
            }
        };
        let lease = match semantic_lease_from_proto(&lease_proto) {
            Ok(lease) => lease,
            Err(error) => {
                let _ = self.transition_intent(
                    &intent,
                    IntentDisposition::UnknownRequiresReconciliation,
                    None,
                    now_unix_ms(),
                );
                return Err(DispatchError::unknown(format!(
                    "Kernel returned an invalid Lease after acquisition: {error}"
                )));
            }
        };
        intent = match self.transition_intent(
            &intent,
            IntentDisposition::LeaseAcquired,
            Some((&target_node, &lease)),
            now_unix_ms(),
        ) {
            Ok(intent) => intent,
            Err(error) => {
                return self
                    .rollback_after_known_lease(
                        &host_route,
                        &target_node,
                        &request,
                        intent,
                        lease,
                        store_error(error, true),
                    )
                    .await;
            }
        };

        let workload_expiry = request
            .assignment
            .workload_identity()
            .expires_at
            .as_ref()
            .and_then(timestamp_unix_ms)
            .expect("validated workload identity expiry");
        if lease
            .expires_at_unix_ms
            .is_none_or(|lease_expiry| lease_expiry > workload_expiry)
        {
            return self
                .rollback_after_known_lease(
                    &host_route,
                    &target_node,
                    &request,
                    intent,
                    lease,
                    DispatchError::input(
                        "WORKLOAD_IDENTITY_WINDOW_TOO_SHORT",
                        "authority-returned Lease outlives the authenticated workload identity",
                    ),
                )
                .await;
        }

        let assignment_time = now_unix_ms();
        request.placement.now_unix_ms = assignment_time;
        let refreshed_target = match place_execution_target(&request.placement, &request.candidates)
        {
            Ok(target) if target.node == target_node => target,
            Ok(_) => {
                return self
                    .rollback_after_known_lease(
                        &host_route,
                        &target_node,
                        &request,
                        intent,
                        lease,
                        DispatchError::transient(
                            "PLACEMENT_TARGET_CHANGED",
                            "placement changed while acquiring the Lease; a fresh dispatch is required",
                        ),
                    )
                    .await;
            }
            Err(error) => {
                return self
                    .rollback_after_known_lease(
                        &host_route,
                        &target_node,
                        &request,
                        intent,
                        lease,
                        error.into(),
                    )
                    .await;
            }
        };
        if let Err(error) = request
            .assignment
            .validate_artifact_projection(&request.placement, refreshed_target)
        {
            return self
                .rollback_after_known_lease(
                    &host_route,
                    &target_node,
                    &request,
                    intent,
                    lease,
                    error.into(),
                )
                .await;
        }
        let assignment = match request.assignment.build(&lease, assignment_time) {
            Ok(assignment) => assignment,
            Err(error) => {
                return self
                    .rollback_after_known_lease(
                        &host_route,
                        &target_node,
                        &request,
                        intent,
                        lease,
                        error.into(),
                    )
                    .await;
            }
        };
        let route = match initial_route {
            Some(route) => route,
            None => {
                let launch = tokio::time::timeout(
                    self.response_timeout,
                    launcher
                        .expect("launcher required for offline Runtime")
                        .launch(&target_node, &assignment, &lease),
                )
                .await
                .unwrap_or_else(|_| Err(DispatchError::unknown("Provider launch timed out")));
                if let Err(error) = launch {
                    if !error.reconciliation_required {
                        return self
                            .rollback_after_known_lease(
                                &host_route,
                                &target_node,
                                &request,
                                intent,
                                lease,
                                error,
                            )
                            .await;
                    }
                    let _ = self.transition_intent(
                        &intent,
                        IntentDisposition::UnknownRequiresReconciliation,
                        None,
                        now_unix_ms(),
                    );
                    return Err(DispatchError::unknown("Provider launch outcome is unknown; retain the original Lease and reconcile the same Runtime generation"));
                }
                let deadline = tokio::time::Instant::now() + self.response_timeout;
                loop {
                    match self.service.snapshot_route(&target_node, &runtime) {
                        Ok(route) => {
                            let scope =
                                self.service
                                    .runtime_session_on_route(&route)
                                    .and_then(|session| {
                                        validate_workload_scope(&request.assignment, &session)
                                    });
                            if let Err(error) = scope {
                                return self
                                    .rollback_after_known_lease(
                                        &host_route,
                                        &target_node,
                                        &request,
                                        intent,
                                        lease,
                                        error,
                                    )
                                    .await;
                            }
                            break route;
                        }
                        Err(error)
                            if error.reason_code == "RUNTIME_SESSION_UNAVAILABLE"
                                && tokio::time::Instant::now() < deadline =>
                        {
                            tokio::time::sleep(Duration::from_millis(25)).await;
                        }
                        Err(_) => {
                            let _ = self.transition_intent(
                                &intent,
                                IntentDisposition::UnknownRequiresReconciliation,
                                None,
                                now_unix_ms(),
                            );
                            return Err(DispatchError::unknown("Launched Runtime has not authenticated on the selected Node; reconcile before acquiring another Lease"));
                        }
                    }
                }
            }
        };
        intent = match self.transition_intent(
            &intent,
            IntentDisposition::AssignmentDispatching,
            None,
            now_unix_ms(),
        ) {
            Ok(intent) => intent,
            Err(error) => {
                return self
                    .rollback_after_known_lease(
                        &host_route,
                        &target_node,
                        &request,
                        intent,
                        lease,
                        store_error(error, true),
                    )
                    .await;
            }
        };

        let ack = self
            .service
            .dispatch_assignment_on_route(&route.runtime, assignment.clone(), self.response_timeout)
            .await;
        let ack = match ack {
            Ok(ack) => ack,
            Err(error) if !error.reconciliation_required => {
                return self
                    .rollback_after_known_lease(
                        &host_route,
                        &target_node,
                        &request,
                        intent,
                        lease,
                        error,
                    )
                    .await;
            }
            Err(error) => {
                if let Err(store_failure) = self.transition_intent(
                    &intent,
                    IntentDisposition::UnknownRequiresReconciliation,
                    None,
                    now_unix_ms(),
                ) {
                    return Err(DispatchError::unknown(format!(
                        "Assignment delivery outcome is unknown ({error}); durable intent transition also failed ({store_failure})"
                    )));
                }
                return Err(DispatchError::unknown(format!(
                    "Assignment delivery outcome is unknown: {error}"
                )));
            }
        };
        let disposition = match AssignmentAckDisposition::try_from(ack.disposition) {
            Ok(disposition) => disposition,
            Err(_) => {
                let _ = self.transition_intent(
                    &intent,
                    IntentDisposition::UnknownRequiresReconciliation,
                    None,
                    now_unix_ms(),
                );
                return Err(DispatchError::unknown(
                    "Runtime returned an unknown AssignmentAck disposition",
                ));
            }
        };
        match disposition {
            AssignmentAckDisposition::Accepted | AssignmentAckDisposition::Duplicate => {
                self.transition_intent(&intent, IntentDisposition::Completed, None, now_unix_ms())
                    .map_err(|error| store_error(error, true))?;
                Ok(DispatchReceipt {
                    node: target_node,
                    assignment,
                    lease,
                    ack_disposition: disposition,
                })
            }
            AssignmentAckDisposition::Rejected | AssignmentAckDisposition::Unspecified => {
                let rejection = ack
                    .rejection
                    .unwrap_or_else(|| cy_proto::semantic_v1::Rejection {
                        reason_code: "ASSIGNMENT_REJECTED".to_string(),
                        message: "Runtime rejected the assignment without structured detail"
                            .to_string(),
                    });
                self.rollback_after_known_lease(
                    &host_route,
                    &target_node,
                    &request,
                    intent,
                    lease,
                    DispatchError::input(rejection.reason_code, rejection.message),
                )
                .await
            }
        }
    }

    /// Return terminal evidence that the control service durably accepted for
    /// an assignment. Product state remains owned by the caller.
    pub fn terminal_observation(
        &self,
        assignment_id: &str,
    ) -> Result<Option<RuntimeObservation>, DispatchError> {
        self.service.terminal_observation(assignment_id)
    }

    /// Await durable terminal evidence for an assignment. This does not rely
    /// on a caller retaining the service's best-effort observation broadcast.
    pub async fn wait_terminal_observation(
        &self,
        assignment_id: &str,
        timeout: Duration,
    ) -> Result<RuntimeObservation, DispatchError> {
        self.service
            .wait_terminal_observation(assignment_id, timeout)
            .await
    }

    /// Ask the authenticated Runtime generation that accepted an assignment
    /// to stop it gracefully. A successful receipt only confirms command
    /// acceptance; terminal evidence must be awaited before Lease release.
    pub async fn stop(&self, request: ExecutionStopRequest) -> Result<StopReceipt, DispatchError> {
        validate_stop_request(&request)?;
        if let Some(observation) = self.terminal_observation(&request.assignment_id)? {
            return Ok(StopReceipt {
                disposition: StopDisposition::AlreadyTerminal,
                terminal_observation: Some(observation),
            });
        }
        let intent = self.intent_record(&request.assignment_id)?.ok_or_else(|| {
            DispatchError::input(
                "EXECUTION_INTENT_NOT_FOUND",
                "stop requires a durable completed execution intent",
            )
        })?;
        if intent.disposition() != IntentDisposition::Completed {
            return Err(match intent.disposition() {
                IntentDisposition::UnknownRequiresReconciliation => DispatchError::unknown(
                    "cannot stop an assignment whose prior outcome requires reconciliation",
                ),
                _ => DispatchError::input(
                    "EXECUTION_INTENT_NOT_STOPPABLE",
                    "only a completed dispatch can receive a stop command",
                ),
            });
        }
        match self
            .service
            .stop_assignment(
                &request.assignment_id,
                &request.command_id,
                request.grace_period,
                &request.reason_code,
                self.response_timeout,
            )
            .await
        {
            Ok(ack)
                if ack.command_id == request.command_id
                    && ack.runtime.is_some()
                    && ack.accepted_at.is_some() =>
            {
                Ok(StopReceipt {
                    disposition: StopDisposition::Accepted,
                    terminal_observation: None,
                })
            }
            Ok(_) => {
                if let Some(observation) = self.terminal_observation(&request.assignment_id)? {
                    Ok(StopReceipt {
                        disposition: StopDisposition::AlreadyTerminal,
                        terminal_observation: Some(observation),
                    })
                } else {
                    Err(DispatchError::unknown(
                        "Runtime returned an invalid StopAck; reconcile durable terminal evidence",
                    ))
                }
            }
            Err(error) => {
                if let Some(observation) = self.terminal_observation(&request.assignment_id)? {
                    Ok(StopReceipt {
                        disposition: StopDisposition::AlreadyTerminal,
                        terminal_observation: Some(observation),
                    })
                } else {
                    Err(error)
                }
            }
        }
    }

    /// Release the exact canonical Lease returned by a completed dispatch.
    pub async fn release(&self, request: ExecutionReleaseRequest) -> Result<(), DispatchError> {
        if request.assignment_id.is_empty() || request.command_id.is_empty() {
            return Err(DispatchError::input(
                "RELEASE_IDENTITY_REQUIRED",
                "assignment and release command identities are required",
            ));
        }
        if request.context.namespace.as_str() != DEFAULT_NAMESPACE {
            return Err(DispatchError::input(
                "CORE_V1_NAMESPACE_UNSUPPORTED",
                "NodeControl Core v1 authority routing supports only the default namespace",
            ));
        }
        request
            .context
            .validate_for(&semantic::ContractRevision::current())
            .map_err(DispatchError::authority)?;
        request.lease.validate().map_err(|error| {
            DispatchError::input(
                error.reason_code,
                format!("invalid release Lease: {}", error.message),
            )
        })?;
        let intent = self.intent_record(&request.assignment_id)?.ok_or_else(|| {
            DispatchError::input(
                "EXECUTION_INTENT_NOT_FOUND",
                "release requires a durable completed execution intent",
            )
        })?;
        if intent.disposition() != IntentDisposition::Completed {
            return Err(match intent.disposition() {
                IntentDisposition::UnknownRequiresReconciliation => DispatchError::unknown(
                    "cannot release an assignment whose prior outcome requires reconciliation",
                ),
                _ => DispatchError::input(
                    "EXECUTION_INTENT_NOT_RELEASABLE",
                    "only a completed dispatch can release its canonical Lease",
                ),
            });
        }
        if !intent.matches_authority(&request.node, &request.lease) {
            return Err(DispatchError::input(
                "RELEASE_AUTHORITY_MISMATCH",
                "release Node or Lease does not match the durable dispatch evidence",
            ));
        }
        match self
            .service
            .release_lease(
                &request.node,
                &request.lease,
                &request.command_id,
                &request.context,
                self.response_timeout,
            )
            .await
        {
            Ok(()) => {
                self.transition_intent(&intent, IntentDisposition::Released, None, now_unix_ms())
                    .map_err(|error| store_error(error, true))?;
                Ok(())
            }
            Err(error) if error.reconciliation_required => {
                if let Err(store_failure) = self.transition_intent(
                    &intent,
                    IntentDisposition::UnknownRequiresReconciliation,
                    None,
                    now_unix_ms(),
                ) {
                    return Err(DispatchError::unknown(format!(
                        "Lease release outcome is unknown ({error}); durable intent transition also failed ({store_failure})"
                    )));
                }
                Err(error)
            }
            Err(error) => Err(error),
        }
    }

    fn begin_intent(
        &self,
        assignment_id: &str,
        payload_digest: &str,
        now_unix_ms: u64,
    ) -> Result<ExecutionIntentRecord, DispatchError> {
        let record = ExecutionIntentRecord::pending(assignment_id, payload_digest, now_unix_ms)
            .map_err(|error| store_error(error, false))?;
        match self.intent_store.create(record.clone()) {
            Ok(()) => Ok(record),
            Err(error) if error.kind == IntentStoreErrorKind::AlreadyExists => {
                let existing = self
                    .intent_store
                    .load(assignment_id)
                    .map_err(|error| store_error(error, false))?
                    .ok_or_else(|| {
                        DispatchError::transient(
                            "INTENT_STORE_INCONSISTENT",
                            "intent tombstone disappeared after duplicate detection",
                        )
                    })?;
                if existing.payload_digest() != payload_digest {
                    return Err(DispatchError::input(
                        "EXECUTION_INTENT_PAYLOAD_MISMATCH",
                        "assignment id is already bound to a different immutable intent digest",
                    ));
                }
                Err(match existing.disposition() {
                    IntentDisposition::UnknownRequiresReconciliation
                    | IntentDisposition::LeaseAcquired
                    | IntentDisposition::AssignmentDispatching => DispatchError::unknown(
                        "execution intent may have external effects; reconcile canonical Lease and Runtime observations before retrying",
                    ),
                    disposition => DispatchError::input(
                        "EXECUTION_INTENT_ALREADY_RECORDED",
                        format!("execution intent is already {disposition:?}"),
                    ),
                })
            }
            Err(error) => Err(store_error(error, false)),
        }
    }

    fn transition_intent(
        &self,
        current: &ExecutionIntentRecord,
        disposition: IntentDisposition,
        authority: Option<(&NodeRef, &semantic::Lease)>,
        now_unix_ms: u64,
    ) -> Result<ExecutionIntentRecord, IntentStoreError> {
        let next = current.transition(disposition, authority, now_unix_ms)?;
        self.intent_store
            .compare_and_set(current.revision(), next.clone())?;
        Ok(next)
    }

    async fn rollback_after_known_lease(
        &self,
        route: &HostRouteSnapshot,
        node: &NodeRef,
        request: &ExecutionDispatchRequest,
        intent: ExecutionIntentRecord,
        lease: semantic::Lease,
        original: DispatchError,
    ) -> Result<DispatchReceipt, DispatchError> {
        let first_release = self
            .service
            .release_lease_on_route(
                route,
                &lease,
                &request.release_command_id,
                &request.release_context,
                self.response_timeout,
            )
            .await;
        let release = match first_release {
            Ok(()) => Ok(()),
            Err(error) if rollback_release_can_retry(&error) => self
                .service
                .release_lease(
                    node,
                    &lease,
                    &request.release_command_id,
                    &request.release_context,
                    self.response_timeout,
                )
                .await
                .map_err(|retry_error| {
                    DispatchError::unknown(format!(
                        "captured-route Lease release failed ({error}); exact idempotent retry on the current Host route failed ({retry_error})"
                    ))
                }),
            Err(error) => Err(error),
        };

        match release {
            Ok(()) => {
                match self.transition_intent(
                    &intent,
                    IntentDisposition::Failed,
                    Some((node, &lease)),
                    now_unix_ms(),
                ) {
                    Ok(_) => Err(original),
                    Err(store_failure) => Err(DispatchError::unknown(format!(
                        "canonical Lease rollback succeeded after assignment failure ({original}), but durable intent transition failed ({store_failure})"
                    ))),
                }
            }
            Err(release_error) => {
                if let Err(store_failure) = self.transition_intent(
                    &intent,
                    IntentDisposition::UnknownRequiresReconciliation,
                    Some((node, &lease)),
                    now_unix_ms(),
                ) {
                    return Err(DispatchError::unknown(format!(
                        "assignment failed ({original}); canonical Lease release is unconfirmed ({release_error}); durable intent transition also failed ({store_failure})"
                    )));
                }
                Err(DispatchError::unknown(format!(
                    "assignment failed ({original}); canonical Lease release is unconfirmed ({release_error})"
                )))
            }
        }
    }
}

fn rollback_release_can_retry(error: &DispatchError) -> bool {
    error.reconciliation_required
        || matches!(
            error.reason_code.as_str(),
            "SESSION_FENCED" | "AGENT_SESSION_CLOSED" | "HOST_SESSION_UNAVAILABLE"
        )
}

fn store_error(error: IntentStoreError, external_effect_possible: bool) -> DispatchError {
    if external_effect_possible {
        return DispatchError::unknown(format!(
            "durable execution-intent state could not be committed: {error}"
        ));
    }
    let reason_code = match error.kind {
        IntentStoreErrorKind::InvalidRecord => "EXECUTION_INTENT_INVALID",
        IntentStoreErrorKind::AlreadyExists => "EXECUTION_INTENT_ALREADY_RECORDED",
        IntentStoreErrorKind::NotFound => "EXECUTION_INTENT_NOT_FOUND",
        IntentStoreErrorKind::RevisionConflict => "EXECUTION_INTENT_REVISION_CONFLICT",
        IntentStoreErrorKind::Persistence => "INTENT_STORE_UNAVAILABLE",
    };
    DispatchError::transient(reason_code, error.message)
}

fn validate_stop_request(request: &ExecutionStopRequest) -> Result<(), DispatchError> {
    if request.assignment_id.is_empty() || request.assignment_id.len() > 256 {
        return Err(DispatchError::input(
            "STOP_ASSIGNMENT_ID_INVALID",
            "stop assignment id must contain 1 to 256 bytes",
        ));
    }
    if request.command_id.is_empty() || request.command_id.len() > 256 {
        return Err(DispatchError::input(
            "STOP_COMMAND_ID_INVALID",
            "stop command id must contain 1 to 256 bytes",
        ));
    }
    if request.grace_period.is_zero() || request.grace_period > Duration::from_secs(60 * 60) {
        return Err(DispatchError::input(
            "STOP_GRACE_PERIOD_INVALID",
            "stop grace period must be positive and at most one hour",
        ));
    }
    if request.reason_code.is_empty()
        || request.reason_code.len() > 128
        || !request.reason_code.bytes().all(|byte| {
            byte.is_ascii_uppercase() || byte.is_ascii_digit() || b"_.-".contains(&byte)
        })
    {
        return Err(DispatchError::input(
            "STOP_REASON_CODE_INVALID",
            "stop reason code must contain 1 to 128 uppercase ASCII letters, digits, '_', '.', or '-'",
        ));
    }
    Ok(())
}

fn validate_request(
    request: &ExecutionDispatchRequest,
    now_unix_ms: u64,
) -> Result<(), DispatchError> {
    if request.acquire_command_id.is_empty()
        || request.release_command_id.is_empty()
        || request.acquire_command_id == request.release_command_id
    {
        return Err(DispatchError::input(
            "COMMAND_ID_INVALID",
            "distinct non-empty acquire and rollback-release command ids are required",
        ));
    }
    for context in [&request.acquire_context, &request.release_context] {
        if context.namespace.as_str() != DEFAULT_NAMESPACE {
            return Err(DispatchError::input(
                "CORE_V1_NAMESPACE_UNSUPPORTED",
                "NodeControl Core v1 authority routing supports only the default namespace",
            ));
        }
        context
            .validate_for(&semantic::ContractRevision::current())
            .map_err(DispatchError::authority)?;
    }
    if request.acquire_context.effective_idempotency_key()
        == request.release_context.effective_idempotency_key()
    {
        return Err(DispatchError::input(
            "IDEMPOTENCY_KEY_REUSED",
            "acquire and rollback release require distinct stable idempotency keys",
        ));
    }
    if request.lease_ttl.is_zero() {
        return Err(DispatchError::input(
            "LEASE_TTL_INVALID",
            "Lease TTL must be positive",
        ));
    }
    let ttl_ms = u64::try_from(request.lease_ttl.as_millis()).map_err(|_| {
        DispatchError::input("LEASE_TTL_INVALID", "Lease TTL exceeds the supported range")
    })?;
    let workload_expiry = request
        .assignment
        .workload_identity()
        .expires_at
        .as_ref()
        .and_then(timestamp_unix_ms)
        .ok_or_else(|| {
            DispatchError::input(
                "WORKLOAD_IDENTITY_EXPIRY_INVALID",
                "workload identity requires an exact positive millisecond expiry",
            )
        })?;
    if now_unix_ms.saturating_add(ttl_ms) > workload_expiry {
        return Err(DispatchError::input(
            "WORKLOAD_IDENTITY_WINDOW_TOO_SHORT",
            "workload identity must remain valid for the requested Lease lifetime",
        ));
    }
    Ok(())
}

fn validate_workload_scope(
    assignment: &RuntimeAssignmentBuilder,
    session: &crate::server::RuntimeSessionView,
) -> Result<(), DispatchError> {
    let scope = assignment
        .workload_identity()
        .scope
        .as_ref()
        .ok_or_else(|| {
            DispatchError::input("WORKLOAD_SCOPE_REQUIRED", "workload scope is required")
        })?;
    if scope.organization_id != session.organization_id
        || scope.workspace_id != session.workspace_id
    {
        return Err(DispatchError::input(
            "WORKLOAD_SCOPE_MISMATCH",
            "workload identity scope does not match the authenticated Runtime session",
        ));
    }
    let workload_identity = assignment
        .workload_identity()
        .identity
        .as_ref()
        .ok_or_else(|| {
            DispatchError::input(
                "WORKLOAD_IDENTITY_REQUIRED",
                "workload identity is required",
            )
        })?;
    if workload_identity.id != session.workload_identity.id
        || workload_identity.generation != session.workload_identity.generation
        || assignment
            .workload_identity()
            .expires_at
            .as_ref()
            .and_then(timestamp_unix_ms)
            != Some(session.workload_identity_expires_at_unix_ms)
    {
        return Err(DispatchError::input(
            "WORKLOAD_IDENTITY_GRANT_MISMATCH",
            "assignment workload identity does not match the authenticated enrollment grant",
        ));
    }
    Ok(())
}

fn timestamp_unix_ms(timestamp: &prost_types::Timestamp) -> Option<u64> {
    if timestamp.seconds <= 0 || timestamp.nanos < 0 || timestamp.nanos % 1_000_000 != 0 {
        return None;
    }
    u64::try_from(timestamp.seconds)
        .ok()?
        .checked_mul(1000)?
        .checked_add(u64::try_from(timestamp.nanos / 1_000_000).ok()?)
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}
