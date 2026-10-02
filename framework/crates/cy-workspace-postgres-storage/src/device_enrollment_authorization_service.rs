//! ┌─────────────────────────────────────────────────────────────────────┐
//! │ Module: cy_workspace_fabric::device_enrollment_authorization_service │
//! │ Role: Composes durable device approval, delivery, and ACK operations. │
//! │                                                                     │
//! │ 模块职责：组合设备审批、证书交付与确认所需的持久化服务。                 │
//! └─────────────────────────────────────────────────────────────────────┘
//!
//! Every production dependency is explicit. Hosts leave the HTTP service
//! absent when any required provider is missing, which keeps the route at 503.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use cy_proto::workspace_v1::UserIdentityRef;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::sync::Semaphore;
use zeroize::Zeroize;
use zeroize::Zeroizing;

use crate::device_authorization::{
    DeviceAuthorizationApprovalSnapshot, DeviceAuthorizationApprovalStarted,
    DeviceAuthorizationApprovalState, DeviceAuthorizationClockPort, DeviceAuthorizationError,
    DeviceAuthorizationId, DeviceAuthorizationManager, DeviceAuthorizationPolicy,
    DeviceAuthorizationPoll, DeviceAuthorizationPollResult, DeviceAuthorizationPortError,
    DeviceAuthorizationRecord, DeviceAuthorizationScope, DeviceAuthorizationState,
    DeviceAuthorizationStore, DeviceAuthorizationStoreError, DeviceCertificateDelivery,
    DeviceCertificateDeliveryAcknowledgement, DeviceCertificateDeliveryReceipt,
    DeviceCertificateIssuer, DeviceCertificateRetirementError, DeviceCertificateRetirementPort,
    DeviceCertificateRetirementReason, DeviceCertificateRetirementWorkOutcome, DeviceCsrValidator,
    DeviceDeliveryRecoveryStatus, IssuedDeviceCertificate, UserCodeAttemptLimiter,
    WebAuthnAuthenticationContext, WebAuthnAuthenticationPort, WebAuthnAuthenticationStart,
    WorkspaceMembershipPort,
};
use crate::device_authorization_postgres::PostgresDeviceAuthorizationStore;
use crate::device_certificate_validation::{
    DeviceCertificateResponseValidator, DeviceCertificateRevocationChecker,
    MAX_DELIVERABLE_CERTIFICATE_BUNDLE_BYTES, MAX_DELIVERABLE_CERTIFICATE_DER_BYTES,
};
use crate::device_enrollment_http::{
    AcknowledgeDeviceDeliveryCommand, CompleteApprovalHttpResponse,
    DeviceEnrollmentAuthorizationPort, DeviceEnrollmentAuthorizationSnapshot,
    DeviceEnrollmentHttpError, PollHttpResponse, PollHttpStatus, SecretBytes,
};
use crate::device_registry_postgres::{
    DeviceCertificateRegistryActivation, PostgresWorkspaceDeviceRegistry,
};
use crate::directory::WorkspaceDirectory;
use crate::user_code_attempt_limiter_postgres::PostgresUserCodeAttemptReservation;

type AuthorizationManager =
    DeviceAuthorizationManager<Arc<PostgresDeviceAuthorizationStore>, RequestScopedLimiter>;

// The authorization Postgres worker has a 32-command queue. Keep anonymous
// poll work below it so approval and acknowledgement commands retain capacity.
// This is process-local admission and deliberately stores no per-code state.
const MAX_CONCURRENT_DEVICE_AUTHORIZATION_POLLS: usize = 8;
const MAX_BACKGROUND_RECONCILIATION_RECORDS: usize = 100;

/// Aggregate counters from one bounded durable issuance/ACK/retirement pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeviceEnrollmentReconciliationReport {
    /// Rows found in the recoverable `Issuing` state.
    pub issuance_candidates: usize,
    /// Issuance rows moved into durable delivery state.
    pub issuance_recovered: usize,
    /// Issuance rows that remain recoverable after an ambiguous signer result.
    pub issuance_pending: usize,
    /// Issuance rows whose current state or CA result needs operator review.
    pub issuance_failures: usize,
    /// Committed deliveries whose inactive registry staging could not be restored.
    pub delivery_staging_failures: usize,
    /// Registry rows activated after their durable delivery ACK was verified.
    pub registry_activations: usize,
    /// Expired or previously pending certificate retirements attempted.
    pub retirement_candidates: usize,
    /// Retirements confirmed by the CA and persisted as terminal.
    pub retirements_completed: usize,
    /// Retirements still pending or blocked by a safe failure.
    pub retirement_pending: usize,
}

/// Non-secret public certificate metadata supplied by a trusted issuer parser.
/// `issuer_id` must come from the configured issuer authority, and
/// `not_before_unix_ms` must be parsed from the exact leaf DER.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceCertificatePublicMetadata {
    pub issuer_id: String,
    pub not_before_unix_ms: u64,
}

/// Required projection port for the public delivery response. The persisted
/// certificate state deliberately does not invent an issuer identifier or
/// duplicate parsed validity facts.
pub trait DeviceCertificatePublicMetadataPort: Send + Sync {
    /// Extracts issuer and validity metadata from the exact certificate DER.
    ///
    /// Implementations must verify the CA identity against configured issuer
    /// metadata and must not trust values from the browser or CSR.
    fn public_metadata(
        &self,
        delivery: &DeviceCertificateDelivery,
    ) -> Result<DeviceCertificatePublicMetadata, DeviceAuthorizationPortError>;
}

/// Required production dependencies for the authorization service.
///
/// Every provider is explicit: constructing this value with an in-memory or
/// unconfigured adapter is a host configuration decision, and there is no
/// partial/default service constructor that could enable only some routes.
pub struct DeviceEnrollmentAuthorizationServiceConfig {
    /// Durable authorization state with atomic Directory-binding operations.
    pub store: Arc<PostgresDeviceAuthorizationStore>,
    /// Shared PostgreSQL-backed user-code attempt reservation.
    pub attempt_reservation: Arc<PostgresUserCodeAttemptReservation>,
    /// Current Directory membership authority.
    pub directory: Arc<dyn WorkspaceDirectory>,
    /// Production WebAuthn verifier and durable ceremony binding.
    pub webauthn: Arc<dyn WebAuthnAuthenticationPort>,
    /// PKCS#10 parser and proof-of-possession validator.
    pub csr_validator: Arc<dyn DeviceCsrValidator>,
    /// Idempotent CA signer bound to the authorization ID and Directory tuple.
    pub issuer: Arc<dyn DeviceCertificateIssuer>,
    /// Durable certificate retirement/revocation authority.
    pub retirement: Arc<dyn DeviceCertificateRetirementPort>,
    /// Registry that stages before delivery and activates only after ACK.
    pub registry: Arc<PostgresWorkspaceDeviceRegistry>,
    /// Trusted issuer metadata parser for the exact issued leaf DER.
    pub certificate_metadata: Arc<dyn DeviceCertificatePublicMetadataPort>,
    /// Private client-CA trust anchors used to validate every issuer response.
    pub device_certificate_trust_roots_der: Vec<Vec<u8>>,
    /// Checker that proves current good revocation status for the exact leaf and path.
    pub device_certificate_revocation_checker: Arc<dyn DeviceCertificateRevocationChecker>,
    /// Runtime-injected user-code HMAC key ring.
    pub user_code_keys: crate::UserCodeKeyRing,
    /// Bounded device-authorization timing and abuse policy.
    pub policy: DeviceAuthorizationPolicy,
}

impl<T> DeviceAuthorizationStore for Arc<T>
where
    T: DeviceAuthorizationStore + ?Sized,
{
    fn insert(
        &self,
        record: DeviceAuthorizationRecord,
    ) -> Result<(), DeviceAuthorizationStoreError> {
        (**self).insert(record)
    }

    fn insert_registered(
        &self,
        record: DeviceAuthorizationRecord,
    ) -> Result<(), DeviceAuthorizationStoreError> {
        (**self).insert_registered(record)
    }

    fn start_or_recover_registered(
        &self,
        candidate: crate::device_authorization::DeviceAuthorizationStartCandidate,
    ) -> Result<
        crate::device_authorization::DeviceAuthorizationCommittedSnapshot,
        DeviceAuthorizationStoreError,
    > {
        (**self).start_or_recover_registered(candidate)
    }

    fn require_current_registered_record(
        &self,
        record: &DeviceAuthorizationRecord,
    ) -> Result<(), DeviceAuthorizationStoreError> {
        (**self).require_current_registered_record(record)
    }

    fn by_device_code_hash(
        &self,
        code_hash: &crate::device_authorization::DeviceAuthorizationCodeHash,
    ) -> Result<Option<DeviceAuthorizationRecord>, DeviceAuthorizationStoreError> {
        (**self).by_device_code_hash(code_hash)
    }

    fn current_poll_snapshot(
        &self,
        code_hash: &crate::device_authorization::DeviceAuthorizationCodeHash,
        expected_revision: Option<u64>,
        observed_at_unix_ms: u64,
    ) -> Result<
        Option<crate::device_authorization::DeviceAuthorizationPollSnapshot>,
        DeviceAuthorizationStoreError,
    > {
        (**self).current_poll_snapshot(code_hash, expected_revision, observed_at_unix_ms)
    }

    fn by_user_code_candidates(
        &self,
        candidates: &[crate::VersionedUserCodeDigest],
    ) -> Result<Option<DeviceAuthorizationRecord>, DeviceAuthorizationStoreError> {
        (**self).by_user_code_candidates(candidates)
    }

    fn user_code_key_versions(&self) -> Result<Vec<u32>, DeviceAuthorizationStoreError> {
        (**self).user_code_key_versions()
    }

    fn by_approval_id(
        &self,
        approval_id: &DeviceAuthorizationId,
    ) -> Result<Option<DeviceAuthorizationRecord>, DeviceAuthorizationStoreError> {
        (**self).by_approval_id(approval_id)
    }

    fn by_authorization_id(
        &self,
        authorization_id: &DeviceAuthorizationId,
    ) -> Result<Option<DeviceAuthorizationRecord>, DeviceAuthorizationStoreError> {
        (**self).by_authorization_id(authorization_id)
    }

    fn claim_retirement_retry(
        &self,
        authorization_id: &DeviceAuthorizationId,
        expected_revision: u64,
        lease_duration_ms: u64,
    ) -> Result<bool, DeviceAuthorizationStoreError> {
        (**self).claim_retirement_retry(authorization_id, expected_revision, lease_duration_ms)
    }

    fn compare_and_swap(
        &self,
        expected_revision: u64,
        replacement: DeviceAuthorizationRecord,
    ) -> Result<(), DeviceAuthorizationStoreError> {
        (**self).compare_and_swap(expected_revision, replacement)
    }

    fn compare_and_swap_due_delivery_to_retirement(
        &self,
        expected_revision: u64,
        replacement: DeviceAuthorizationRecord,
    ) -> Result<bool, DeviceAuthorizationStoreError> {
        (**self).compare_and_swap_due_delivery_to_retirement(expected_revision, replacement)
    }

    fn compare_and_swap_registered(
        &self,
        expected_revision: u64,
        replacement: DeviceAuthorizationRecord,
    ) -> Result<(), DeviceAuthorizationStoreError> {
        (**self).compare_and_swap_registered(expected_revision, replacement)
    }

    fn compare_and_swap_registered_issuance(
        &self,
        expected_revision: u64,
        replacement: DeviceAuthorizationRecord,
    ) -> Result<bool, DeviceAuthorizationStoreError> {
        (**self).compare_and_swap_registered_issuance(expected_revision, replacement)
    }

    fn compare_and_swap_delivery_ack(
        &self,
        expected_revision: u64,
        replacement: DeviceAuthorizationRecord,
    ) -> Result<(), DeviceAuthorizationStoreError> {
        (**self).compare_and_swap_delivery_ack(expected_revision, replacement)
    }

    fn compare_and_swap_registered_delivery_ack(
        &self,
        expected_revision: u64,
        replacement: DeviceAuthorizationRecord,
    ) -> Result<(), DeviceAuthorizationStoreError> {
        (**self).compare_and_swap_registered_delivery_ack(expected_revision, replacement)
    }
}

/// Full approval runtime. Each constructor argument is mandatory so a partial
/// or development-only provider cannot silently enable browser approvals.
pub struct DeviceEnrollmentAuthorizationService {
    /// Immutable coordinator; durable transitions are fenced by storage revisions.
    manager: Arc<AuthorizationManager>,
    /// Serializes interactive operations that share the request-scoped attempt slot.
    manager_gate: Arc<Mutex<()>>,
    /// Admits only a fixed number of anonymous polls into blocking storage work.
    poll_admission: Arc<Semaphore>,
    store: Arc<PostgresDeviceAuthorizationStore>,
    attempt_reservation: Arc<PostgresUserCodeAttemptReservation>,
    limiter_state: Arc<Mutex<Option<ReservedAttempt>>>,
    directory: Arc<dyn WorkspaceDirectory>,
    webauthn: Arc<dyn WebAuthnAuthenticationPort>,
    csr_validator: Arc<dyn DeviceCsrValidator>,
    issuer: Arc<dyn DeviceCertificateIssuer>,
    retirement: Arc<dyn DeviceCertificateRetirementPort>,
    registry: Arc<PostgresWorkspaceDeviceRegistry>,
    certificate_metadata: Arc<dyn DeviceCertificatePublicMetadataPort>,
    user_code_keys: crate::UserCodeKeyRing,
    policy: DeviceAuthorizationPolicy,
}

impl DeviceEnrollmentAuthorizationService {
    /// Build the complete service from configured production dependencies.
    ///
    /// Call this during asynchronous host startup. Manager construction reads
    /// the authorization key versions through its bounded synchronous storage
    /// worker on `spawn_blocking`; no Tokio worker is blocked on PostgreSQL.
    pub async fn new(
        config: DeviceEnrollmentAuthorizationServiceConfig,
    ) -> Result<Self, DeviceEnrollmentHttpError> {
        let DeviceEnrollmentAuthorizationServiceConfig {
            store,
            attempt_reservation,
            directory,
            webauthn,
            csr_validator,
            issuer,
            retirement,
            registry,
            certificate_metadata,
            device_certificate_trust_roots_der,
            device_certificate_revocation_checker,
            user_code_keys,
            policy,
        } = config;
        let certificate_validator = Arc::new(
            DeviceCertificateResponseValidator::new(
                device_certificate_trust_roots_der,
                device_certificate_revocation_checker,
            )
            .map_err(|_| DeviceEnrollmentHttpError::Unavailable)?,
        );
        let limiter_state = Arc::new(Mutex::new(None));
        let limiter = RequestScopedLimiter {
            state: Arc::clone(&limiter_state),
        };
        let manager_store = Arc::clone(&store);
        let manager_keys = user_code_keys.clone();
        let manager_certificate_validator = Arc::clone(&certificate_validator);
        let manager = tokio::task::spawn_blocking(move || {
            DeviceAuthorizationManager::new_with_certificate_validator(
                manager_store,
                limiter,
                manager_keys,
                policy.clone(),
                manager_certificate_validator,
            )
            .map(|manager| (manager, policy))
        })
        .await
        .map_err(|_| DeviceEnrollmentHttpError::Unavailable)?
        .map_err(map_authorization_error)?;
        let (manager, policy) = manager;

        Ok(Self {
            manager: Arc::new(manager),
            manager_gate: Arc::new(Mutex::new(())),
            poll_admission: Arc::new(Semaphore::new(MAX_CONCURRENT_DEVICE_AUTHORIZATION_POLLS)),
            store,
            attempt_reservation,
            limiter_state,
            directory,
            webauthn,
            csr_validator,
            issuer,
            retirement,
            registry,
            certificate_metadata,
            user_code_keys,
            policy,
        })
    }

    /// Reconcile committed but incomplete issuance, ACK activation, and retirement work.
    ///
    /// Each pass is bounded to [`MAX_BACKGROUND_RECONCILIATION_RECORDS`] rows,
    /// uses the authorization database clock, and calls only idempotent CA
    /// operations. A signer timeout leaves the durable `Issuing` row in place
    /// for the next pass; registry activation continues to require a durable
    /// delivery ACK.
    pub async fn reconcile_pending_work_once(
        &self,
    ) -> Result<DeviceEnrollmentReconciliationReport, DeviceEnrollmentHttpError> {
        let manager = Arc::clone(&self.manager);
        let manager_gate = Arc::clone(&self.manager_gate);
        let store = Arc::clone(&self.store);
        let registry = Arc::clone(&self.registry);
        let webauthn = Arc::clone(&self.webauthn);
        let csr_validator = Arc::clone(&self.csr_validator);
        let issuer = Arc::clone(&self.issuer);
        let retirement = Arc::clone(&self.retirement);

        tokio::task::spawn_blocking(move || {
            let _manager_gate = manager_gate
                .lock()
                .map_err(|_| DeviceEnrollmentHttpError::Unavailable)?;
            let database_now = store
                .database_time_unix_ms()
                .map_err(|_| DeviceEnrollmentHttpError::Unavailable)?;
            let clock = DatabaseClock(Arc::clone(&store));
            let membership = RejectBackgroundMembership;
            let call_ports = ApprovalPorts {
                membership: &membership,
                webauthn: webauthn.as_ref(),
                csr_validator: csr_validator.as_ref(),
                issuer: issuer.as_ref(),
                retirement: retirement.as_ref(),
                clock: &clock,
            };
            let mut report = DeviceEnrollmentReconciliationReport::default();

            let issuance_records = store
                .recoverable_issuances(MAX_BACKGROUND_RECONCILIATION_RECORDS)
                .map_err(|_| DeviceEnrollmentHttpError::Unavailable)?;
            report.issuance_candidates = issuance_records.len();
            for record in issuance_records {
                let DeviceAuthorizationState::Issuing {
                    approval_id,
                    approver,
                    ..
                } = record.state
                else {
                    continue;
                };
                match manager.complete_approval_for_user(
                    &approval_id,
                    &approver,
                    &[],
                    database_now,
                    &call_ports,
                ) {
                    Ok(snapshot)
                        if snapshot.state()
                            == DeviceAuthorizationApprovalState::DeliveryPending
                            || snapshot.state() == DeviceAuthorizationApprovalState::Delivered =>
                    {
                        report.issuance_recovered += 1;
                        if snapshot.state() == DeviceAuthorizationApprovalState::DeliveryPending
                            && registry
                                .stage_pending_delivery(snapshot.authorization_id())
                                .is_err()
                        {
                            report.issuance_failures += 1;
                        }
                    }
                    Ok(snapshot)
                        if snapshot.state() == DeviceAuthorizationApprovalState::Issuing =>
                    {
                        report.issuance_pending += 1;
                    }
                    Ok(_) | Err(_) => report.issuance_failures += 1,
                }
            }

            let due = store
                .due_certificate_deliveries(database_now, MAX_BACKGROUND_RECONCILIATION_RECORDS)
                .map_err(|_| DeviceEnrollmentHttpError::Unavailable)?;
            let recovery = store
                .recoverable_retirements(MAX_BACKGROUND_RECONCILIATION_RECORDS)
                .map_err(|_| DeviceEnrollmentHttpError::Unavailable)?;
            let mut authorization_ids = Vec::with_capacity(due.len() + recovery.len());
            for record in due.into_iter().chain(recovery) {
                if !authorization_ids.contains(&record.id) {
                    authorization_ids.push(record.id);
                }
            }
            report.retirement_candidates = authorization_ids.len();
            for authorization_id in authorization_ids {
                match manager.retire_certificate_work_by_authorization_id(
                    &authorization_id,
                    database_now,
                    retirement.as_ref(),
                ) {
                    Ok(DeviceCertificateRetirementWorkOutcome::Retired) => {
                        report.retirements_completed += 1
                    }
                    Ok(DeviceCertificateRetirementWorkOutcome::Pending(_)) | Err(_) => {
                        report.retirement_pending += 1
                    }
                    Ok(DeviceCertificateRetirementWorkOutcome::NoWork) => {}
                }
            }

            let pending_deliveries = store
                .pending_certificate_deliveries(MAX_BACKGROUND_RECONCILIATION_RECORDS)
                .map_err(|_| DeviceEnrollmentHttpError::Unavailable)?;
            for record in pending_deliveries {
                if registry.stage_pending_delivery(&record.id).is_err() {
                    report.delivery_staging_failures += 1;
                }
            }
            report.registry_activations = registry
                .reconcile_pending_deliveries(MAX_BACKGROUND_RECONCILIATION_RECORDS)
                .map_err(|_| DeviceEnrollmentHttpError::Unavailable)?;
            Ok(report)
        })
        .await
        .map_err(|_| DeviceEnrollmentHttpError::Unavailable)?
    }

    async fn reserve_user_code_attempt(
        &self,
        abuse_key: &[u8; 32],
    ) -> Result<ReservedAttempt, DeviceEnrollmentHttpError> {
        let allowed = self
            .attempt_reservation
            .reserve_attempt(
                abuse_key,
                self.policy.user_code_attempt_window_ms,
                self.policy.maximum_user_code_attempts,
            )
            .await
            .map_err(|_| DeviceEnrollmentHttpError::Unavailable)?;
        if !allowed {
            return Err(DeviceEnrollmentHttpError::RateLimited {
                retry_after_seconds: ceil_seconds(self.policy.user_code_attempt_window_ms),
            });
        }
        Ok(ReservedAttempt {
            abuse_key: *abuse_key,
            window_ms: self.policy.user_code_attempt_window_ms,
            maximum_attempts: self.policy.maximum_user_code_attempts,
        })
    }

    async fn require_member(
        &self,
        user: &UserIdentityRef,
        scope: &DeviceAuthorizationScope,
    ) -> Result<(), DeviceEnrollmentHttpError> {
        match self
            .directory
            .is_member(user, &scope.organization_id, &scope.workspace_id)
            .await
        {
            Ok(true) => Ok(()),
            Ok(false) => Err(DeviceEnrollmentHttpError::Forbidden),
            Err(_) => Err(DeviceEnrollmentHttpError::Unavailable),
        }
    }

    fn port_handles(&self) -> ApprovalPortHandles {
        ApprovalPortHandles {
            webauthn: Arc::clone(&self.webauthn),
            csr_validator: Arc::clone(&self.csr_validator),
            issuer: Arc::clone(&self.issuer),
            retirement: Arc::clone(&self.retirement),
        }
    }

    async fn find_by_approval_id(
        &self,
        approval_id: DeviceAuthorizationId,
    ) -> Result<DeviceAuthorizationRecord, DeviceEnrollmentHttpError> {
        let store = Arc::clone(&self.store);
        tokio::task::spawn_blocking(move || {
            store
                .by_approval_id(&approval_id)?
                .ok_or(DeviceAuthorizationStoreError::Conflict)
        })
        .await
        .map_err(|_| DeviceEnrollmentHttpError::Unavailable)?
        .map_err(|_| DeviceEnrollmentHttpError::InvalidGrant)
    }
}

struct ReservedAttempt {
    abuse_key: [u8; 32],
    window_ms: u64,
    maximum_attempts: u32,
}

impl Drop for ReservedAttempt {
    fn drop(&mut self) {
        self.abuse_key.zeroize();
    }
}

struct RequestScopedLimiter {
    state: Arc<Mutex<Option<ReservedAttempt>>>,
}

impl UserCodeAttemptLimiter for RequestScopedLimiter {
    fn record_attempt(
        &self,
        abuse_key: &[u8; 32],
        _now_unix_ms: u64,
        window_ms: u64,
        maximum_attempts: u32,
    ) -> Result<bool, crate::device_authorization::DeviceAuthorizationRateLimitError> {
        let mut reservation = self
            .state
            .lock()
            .map_err(|_| {
                crate::device_authorization::DeviceAuthorizationRateLimitError::Unavailable
            })?
            .take()
            .ok_or(crate::device_authorization::DeviceAuthorizationRateLimitError::Unavailable)?;
        let key_matches = bool::from(reservation.abuse_key.ct_eq(abuse_key));
        let matches = key_matches
            && reservation.window_ms == window_ms
            && reservation.maximum_attempts == maximum_attempts;
        reservation.abuse_key.zeroize();
        if matches {
            Ok(true)
        } else {
            Err(crate::device_authorization::DeviceAuthorizationRateLimitError::Unavailable)
        }
    }
}

struct InstalledAttempt<'a> {
    state: &'a Mutex<Option<ReservedAttempt>>,
}

impl Drop for InstalledAttempt<'_> {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            if let Some(mut attempt) = state.take() {
                attempt.abuse_key.zeroize();
            }
        }
    }
}

fn install_attempt(
    state: &Mutex<Option<ReservedAttempt>>,
    attempt: ReservedAttempt,
) -> Result<InstalledAttempt<'_>, DeviceEnrollmentHttpError> {
    let mut current = state
        .lock()
        .map_err(|_| DeviceEnrollmentHttpError::Unavailable)?;
    if current.is_some() {
        return Err(DeviceEnrollmentHttpError::Unavailable);
    }
    *current = Some(attempt);
    drop(current);
    Ok(InstalledAttempt { state })
}

struct MembershipEvidence {
    user: UserIdentityRef,
    scope: DeviceAuthorizationScope,
    consumed: AtomicBool,
}

impl MembershipEvidence {
    fn new(user: UserIdentityRef, scope: DeviceAuthorizationScope) -> Self {
        Self {
            user,
            scope,
            consumed: AtomicBool::new(false),
        }
    }
}

impl WorkspaceMembershipPort for MembershipEvidence {
    fn is_member(
        &self,
        approver: &UserIdentityRef,
        scope: &DeviceAuthorizationScope,
    ) -> Result<bool, DeviceAuthorizationPortError> {
        if &self.user != approver || &self.scope != scope {
            return Err(DeviceAuthorizationPortError::Rejected);
        }
        if self.consumed.swap(true, Ordering::AcqRel) {
            return Err(DeviceAuthorizationPortError::Unavailable);
        }
        // This is one-use evidence from the immediately preceding async
        // Directory query, not a shared Directory/authorization transaction.
        Ok(true)
    }
}

struct DatabaseClock(Arc<PostgresDeviceAuthorizationStore>);

impl DeviceAuthorizationClockPort for DatabaseClock {
    fn current_unix_ms(&self) -> Result<u64, DeviceAuthorizationPortError> {
        self.0
            .database_time_unix_ms()
            .map_err(|_| DeviceAuthorizationPortError::Unavailable)
    }
}

/// Any accidental membership query during a background `Issuing` recovery fails closed.
struct RejectBackgroundMembership;

impl WorkspaceMembershipPort for RejectBackgroundMembership {
    fn is_member(
        &self,
        _approver: &UserIdentityRef,
        _scope: &DeviceAuthorizationScope,
    ) -> Result<bool, DeviceAuthorizationPortError> {
        Err(DeviceAuthorizationPortError::Unavailable)
    }
}

struct ApprovalPorts<'a> {
    membership: &'a dyn WorkspaceMembershipPort,
    webauthn: &'a dyn WebAuthnAuthenticationPort,
    csr_validator: &'a dyn DeviceCsrValidator,
    issuer: &'a dyn DeviceCertificateIssuer,
    retirement: &'a dyn DeviceCertificateRetirementPort,
    clock: &'a dyn DeviceAuthorizationClockPort,
}

impl WorkspaceMembershipPort for ApprovalPorts<'_> {
    fn is_member(
        &self,
        approver: &UserIdentityRef,
        scope: &DeviceAuthorizationScope,
    ) -> Result<bool, DeviceAuthorizationPortError> {
        self.membership.is_member(approver, scope)
    }
}

impl WebAuthnAuthenticationPort for ApprovalPorts<'_> {
    fn start_authentication(
        &self,
        context: &WebAuthnAuthenticationContext,
    ) -> Result<WebAuthnAuthenticationStart, DeviceAuthorizationPortError> {
        self.webauthn.start_authentication(context)
    }

    fn finish_authentication(
        &self,
        context: &WebAuthnAuthenticationContext,
        opaque_state: &[u8],
        assertion: &[u8],
        now_unix_ms: u64,
    ) -> Result<(), DeviceAuthorizationPortError> {
        self.webauthn
            .finish_authentication(context, opaque_state, assertion, now_unix_ms)
    }
}

impl DeviceCsrValidator for ApprovalPorts<'_> {
    fn validate_and_hash_spki(
        &self,
        csr_der: &[u8],
    ) -> Result<[u8; 32], DeviceAuthorizationPortError> {
        self.csr_validator.validate_and_hash_spki(csr_der)
    }
}

impl DeviceCertificateIssuer for ApprovalPorts<'_> {
    fn issue_device_certificate(
        &self,
        authorization_id: &DeviceAuthorizationId,
        registration_binding: &crate::device_authorization::DeviceAuthorizationRegistrationBinding,
        csr_der: &[u8],
        issued_at_unix_ms: u64,
    ) -> Result<IssuedDeviceCertificate, crate::device_authorization::DeviceCertificateIssuanceError>
    {
        self.issuer.issue_device_certificate(
            authorization_id,
            registration_binding,
            csr_der,
            issued_at_unix_ms,
        )
    }
}

impl DeviceCertificateRetirementPort for ApprovalPorts<'_> {
    fn declared_hard_timeout(&self) -> Option<std::time::Duration> {
        self.retirement.declared_hard_timeout()
    }

    fn retire_or_confirm(
        &self,
        authorization_id: &DeviceAuthorizationId,
        certificate_sha256: &[u8; 32],
        certificate: &IssuedDeviceCertificate,
        reason: DeviceCertificateRetirementReason,
    ) -> Result<(), DeviceCertificateRetirementError> {
        self.retirement
            .retire_or_confirm(authorization_id, certificate_sha256, certificate, reason)
    }
}

impl DeviceAuthorizationClockPort for ApprovalPorts<'_> {
    fn current_unix_ms(&self) -> Result<u64, DeviceAuthorizationPortError> {
        self.clock.current_unix_ms()
    }
}

#[derive(Clone)]
struct ApprovalPortHandles {
    webauthn: Arc<dyn WebAuthnAuthenticationPort>,
    csr_validator: Arc<dyn DeviceCsrValidator>,
    issuer: Arc<dyn DeviceCertificateIssuer>,
    retirement: Arc<dyn DeviceCertificateRetirementPort>,
}

impl ApprovalPortHandles {
    fn borrow<'a>(
        &'a self,
        membership: &'a dyn WorkspaceMembershipPort,
        clock: &'a dyn DeviceAuthorizationClockPort,
    ) -> ApprovalPorts<'a> {
        ApprovalPorts {
            membership,
            webauthn: self.webauthn.as_ref(),
            csr_validator: self.csr_validator.as_ref(),
            issuer: self.issuer.as_ref(),
            retirement: self.retirement.as_ref(),
            clock,
        }
    }
}

struct DeliveryPorts<'a> {
    retirement: &'a dyn DeviceCertificateRetirementPort,
    clock: &'a dyn DeviceAuthorizationClockPort,
}

impl DeviceCertificateRetirementPort for DeliveryPorts<'_> {
    fn declared_hard_timeout(&self) -> Option<std::time::Duration> {
        self.retirement.declared_hard_timeout()
    }

    fn retire_or_confirm(
        &self,
        authorization_id: &DeviceAuthorizationId,
        certificate_sha256: &[u8; 32],
        certificate: &IssuedDeviceCertificate,
        reason: DeviceCertificateRetirementReason,
    ) -> Result<(), DeviceCertificateRetirementError> {
        self.retirement
            .retire_or_confirm(authorization_id, certificate_sha256, certificate, reason)
    }
}

impl DeviceAuthorizationClockPort for DeliveryPorts<'_> {
    fn current_unix_ms(&self) -> Result<u64, DeviceAuthorizationPortError> {
        self.clock.current_unix_ms()
    }
}

fn approval_challenge_body(
    started: &DeviceAuthorizationApprovalStarted,
    database_now_unix_ms: u64,
) -> Result<Value, DeviceEnrollmentHttpError> {
    let snapshot = &started.committed_snapshot;
    if started.challenge.approval_id != *snapshot.approval_id()
        || snapshot.approver().issuer.trim().is_empty()
        || snapshot.approver().subject.trim().is_empty()
    {
        return Err(DeviceEnrollmentHttpError::Unavailable);
    }
    let options: Value = serde_json::from_slice(&started.challenge.credential_request_options_json)
        .map_err(|_| DeviceEnrollmentHttpError::Unavailable)?;
    if !options.is_object() {
        return Err(DeviceEnrollmentHttpError::Unavailable);
    }
    let remaining_authorization_ms = snapshot
        .authorization_expires_at_unix_ms()
        .saturating_sub(database_now_unix_ms);
    let timeout_ms = match options.get("timeout") {
        Some(Value::Number(timeout)) => timeout
            .as_u64()
            .filter(|timeout| (1..=10 * 60 * 1_000).contains(timeout))
            .ok_or(DeviceEnrollmentHttpError::Unavailable)?,
        Some(_) => return Err(DeviceEnrollmentHttpError::Unavailable),
        None => remaining_authorization_ms,
    };
    let challenge_expires_at = database_now_unix_ms
        .checked_add(timeout_ms)
        .ok_or(DeviceEnrollmentHttpError::Unavailable)?
        .min(snapshot.authorization_expires_at_unix_ms());
    if challenge_expires_at <= database_now_unix_ms {
        return Err(DeviceEnrollmentHttpError::Expired);
    }

    Ok(json!({
        "authorization": approval_authorization_reference(snapshot)?,
        "approvalId": URL_SAFE_NO_PAD.encode(snapshot.approval_id()),
        "webauthnOptions": options,
        "challengeExpiresAt": format_unix_ms(challenge_expires_at)?,
    }))
}

fn complete_approval_body(
    snapshot: &DeviceAuthorizationApprovalSnapshot,
) -> Result<CompleteApprovalHttpResponse, DeviceEnrollmentHttpError> {
    let (state, accepted) = match snapshot.state() {
        DeviceAuthorizationApprovalState::Issuing => {
            ("DEVICE_AUTHORIZATION_LIFECYCLE_STATE_ISSUING", false)
        }
        DeviceAuthorizationApprovalState::DeliveryPending => (
            "DEVICE_AUTHORIZATION_LIFECYCLE_STATE_DELIVERY_PENDING",
            true,
        ),
        DeviceAuthorizationApprovalState::Delivered => {
            ("DEVICE_AUTHORIZATION_LIFECYCLE_STATE_DELIVERED", true)
        }
        _ => return Err(DeviceEnrollmentHttpError::Conflict),
    };
    let approved_at = snapshot
        .approved_at_unix_ms()
        .ok_or(DeviceEnrollmentHttpError::Unavailable)?;
    let body = json!({
        "authorization": approval_authorization_reference(snapshot)?,
        "state": state,
        "approvedBy": identity_json(snapshot.approver()),
        "approvedAt": format_unix_ms(approved_at)?,
    });
    Ok(CompleteApprovalHttpResponse { body, accepted })
}

fn approval_authorization_reference(
    snapshot: &DeviceAuthorizationApprovalSnapshot,
) -> Result<Value, DeviceEnrollmentHttpError> {
    if !(16..=128).contains(&snapshot.device_id().len())
        || snapshot.authorization_generation() == 0
        || snapshot.authorization_expires_at_unix_ms() == 0
    {
        return Err(DeviceEnrollmentHttpError::Unavailable);
    }
    Ok(json!({
        "authorizationId": URL_SAFE_NO_PAD.encode(snapshot.authorization_id()),
        "deviceId": snapshot.device_id(),
        "scope": scope_json(snapshot.scope()),
        "csrSha256": STANDARD.encode(snapshot.csr_sha256()),
        "csrSpkiSha256": STANDARD.encode(snapshot.spki_sha256()),
        "expiresAt": format_unix_ms(snapshot.authorization_expires_at_unix_ms())?,
        "authorizationGeneration": snapshot.authorization_generation(),
    }))
}

fn authorization_reference_from_record(
    record: &DeviceAuthorizationRecord,
) -> Result<Value, DeviceEnrollmentHttpError> {
    let binding = &record.registration_binding;
    if !(16..=128).contains(&binding.key().device_id.len())
        || binding.authorization_generation() == 0
        || binding.key().organization_id != record.scope.organization_id
        || binding.key().workspace_id != record.scope.workspace_id
        || binding.csr_sha256() != &record.csr_sha256
        || binding.spki_sha256() != &record.spki_sha256
        || record.expires_at_unix_ms == 0
    {
        return Err(DeviceEnrollmentHttpError::Unavailable);
    }
    Ok(json!({
        "authorizationId": URL_SAFE_NO_PAD.encode(record.id),
        "deviceId": binding.key().device_id,
        "scope": scope_json(&record.scope),
        "csrSha256": STANDARD.encode(record.csr_sha256),
        "csrSpkiSha256": STANDARD.encode(record.spki_sha256),
        "expiresAt": format_unix_ms(record.expires_at_unix_ms)?,
        "authorizationGeneration": binding.authorization_generation(),
    }))
}

fn denial_body(
    record: &DeviceAuthorizationRecord,
    user: UserIdentityRef,
) -> Result<Value, DeviceEnrollmentHttpError> {
    let decided_at = match &record.state {
        DeviceAuthorizationState::Denied {
            approver,
            decided_at_unix_ms,
        } if approver == &user => *decided_at_unix_ms,
        _ => return Err(DeviceEnrollmentHttpError::Conflict),
    };
    Ok(json!({
        "authorization": authorization_reference_from_record(record)?,
        "deniedBy": identity_json(&user),
        "deniedAt": format_unix_ms(decided_at)?,
    }))
}

fn identity_json(user: &UserIdentityRef) -> Value {
    json!({ "issuer": user.issuer, "subject": user.subject })
}

fn scope_json(scope: &DeviceAuthorizationScope) -> Value {
    json!({
        "organizationId": scope.organization_id,
        "workspaceId": scope.workspace_id,
    })
}

fn state_approver(state: &DeviceAuthorizationState) -> Option<&UserIdentityRef> {
    match state {
        DeviceAuthorizationState::AwaitingWebAuthn { approver, .. }
        | DeviceAuthorizationState::VerifyingWebAuthn { approver, .. }
        | DeviceAuthorizationState::Issuing { approver, .. }
        | DeviceAuthorizationState::DeliveryPending { approver, .. }
        | DeviceAuthorizationState::RetirementPending { approver, .. }
        | DeviceAuthorizationState::IssuanceFailed { approver, .. }
        | DeviceAuthorizationState::RegistrationRetired { approver, .. } => Some(approver),
        DeviceAuthorizationState::Delivered {
            approver: Some(approver),
            ..
        } => Some(approver),
        DeviceAuthorizationState::Denied { approver, .. }
        | DeviceAuthorizationState::Consumed { approver, .. } => Some(approver),
        _ => None,
    }
}

fn decode_authorization_id(
    value: &str,
) -> Result<DeviceAuthorizationId, DeviceEnrollmentHttpError> {
    if value.len() != 22 {
        return Err(DeviceEnrollmentHttpError::InvalidRequest);
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| DeviceEnrollmentHttpError::InvalidRequest)?;
    if URL_SAFE_NO_PAD.encode(&bytes) != value {
        return Err(DeviceEnrollmentHttpError::InvalidRequest);
    }
    bytes
        .try_into()
        .map_err(|_| DeviceEnrollmentHttpError::InvalidRequest)
}

fn normalize_user_code(value: &str) -> String {
    value
        .bytes()
        .filter(|byte| *byte != b'-' && !byte.is_ascii_whitespace())
        .map(|byte| byte.to_ascii_uppercase() as char)
        .collect()
}

fn ceil_seconds(milliseconds: u64) -> u64 {
    milliseconds
        .saturating_add(999)
        .saturating_div(1_000)
        .max(1)
}

fn format_unix_ms(milliseconds: u64) -> Result<String, DeviceEnrollmentHttpError> {
    let seconds = milliseconds / 1_000;
    let days =
        i64::try_from(seconds / 86_400).map_err(|_| DeviceEnrollmentHttpError::Unavailable)?;
    let second_of_day = seconds % 86_400;

    // Gregorian civil date conversion, with Unix day zero at 1970-01-01.
    let shifted_day = days
        .checked_add(719_468)
        .ok_or(DeviceEnrollmentHttpError::Unavailable)?;
    let era = shifted_day.div_euclid(146_097);
    let day_of_era = shifted_day - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    if !(0..=9_999).contains(&year) {
        return Err(DeviceEnrollmentHttpError::Unavailable);
    }

    let hour = second_of_day / 3_600;
    let minute = (second_of_day % 3_600) / 60;
    let second = second_of_day % 60;
    let millis = milliseconds % 1_000;
    if millis == 0 {
        Ok(format!(
            "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z"
        ))
    } else {
        Ok(format!(
            "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z"
        ))
    }
}

fn map_authorization_error(error: DeviceAuthorizationError) -> DeviceEnrollmentHttpError {
    use DeviceAuthorizationError as AuthorizationError;
    match error {
        AuthorizationError::InvalidRequest | AuthorizationError::InvalidCsr => {
            DeviceEnrollmentHttpError::InvalidRequest
        }
        AuthorizationError::TooManyAttempts => DeviceEnrollmentHttpError::RateLimited {
            retry_after_seconds: 60,
        },
        AuthorizationError::Expired | AuthorizationError::DeliveryExpired => {
            DeviceEnrollmentHttpError::Expired
        }
        AuthorizationError::MembershipRequired | AuthorizationError::ScopeMismatch => {
            DeviceEnrollmentHttpError::Forbidden
        }
        AuthorizationError::InvalidCode
        | AuthorizationError::Denied
        | AuthorizationError::AlreadyConsumed
        | AuthorizationError::InvalidDeliveryAcknowledgement => {
            DeviceEnrollmentHttpError::InvalidGrant
        }
        AuthorizationError::AlreadyFinal
        | AuthorizationError::Pending
        | AuthorizationError::ConcurrentTransition
        | AuthorizationError::CertificateBindingMismatch
        | AuthorizationError::InvalidWebAuthnAssertion => DeviceEnrollmentHttpError::Conflict,
        AuthorizationError::SlowDown { retry_after_ms } => DeviceEnrollmentHttpError::RateLimited {
            retry_after_seconds: ceil_seconds(retry_after_ms),
        },
        _ => DeviceEnrollmentHttpError::Unavailable,
    }
}

fn activate_acknowledged_delivery(
    registry: &PostgresWorkspaceDeviceRegistry,
    authorization_id: &DeviceAuthorizationId,
) -> Result<(), DeviceEnrollmentHttpError> {
    match registry
        .activate_acknowledged_delivery(authorization_id)
        .map_err(|_| DeviceEnrollmentHttpError::Unavailable)?
    {
        DeviceCertificateRegistryActivation::Activated
        | DeviceCertificateRegistryActivation::AlreadyActive => Ok(()),
        DeviceCertificateRegistryActivation::AwaitingAcknowledgement
        | DeviceCertificateRegistryActivation::Ineligible
        | DeviceCertificateRegistryActivation::NotStaged => {
            Err(DeviceEnrollmentHttpError::Unavailable)
        }
    }
}

fn project_poll_result(
    result: &DeviceAuthorizationPollResult,
    registry: &PostgresWorkspaceDeviceRegistry,
    certificate_metadata: &dyn DeviceCertificatePublicMetadataPort,
) -> Result<PollHttpResponse, DeviceEnrollmentHttpError> {
    let snapshot = DeviceEnrollmentAuthorizationSnapshot::from_poll_snapshot(&result.snapshot)?;
    let authorization = authorization_reference_from_record(&result.snapshot.record)?;
    let (body, status, retry_after_seconds) = match &result.response {
        DeviceAuthorizationPoll::Pending { interval_ms } => {
            let interval_seconds = ceil_seconds(*interval_ms);
            let next_poll_at = result
                .snapshot
                .database_now_unix_ms
                .saturating_add(*interval_ms);
            (
                json!({
                    "status": "DEVICE_AUTHORIZATION_POLL_STATUS_PENDING",
                    "error": "authorization_pending",
                    "authorization": authorization,
                    "intervalSeconds": interval_seconds,
                    "nextPollAt": format_unix_ms(next_poll_at)?,
                }),
                PollHttpStatus::ProtocolError,
                None,
            )
        }
        DeviceAuthorizationPoll::SlowDown {
            interval_ms,
            retry_after_ms,
        } => {
            let interval_seconds = ceil_seconds(*interval_ms);
            let retry_after_seconds = ceil_seconds(*retry_after_ms);
            let next_poll_at = result
                .snapshot
                .database_now_unix_ms
                .saturating_add(*retry_after_ms);
            (
                json!({
                    "status": "DEVICE_AUTHORIZATION_POLL_STATUS_SLOW_DOWN",
                    "error": "slow_down",
                    "authorization": authorization,
                    "intervalSeconds": interval_seconds,
                    "retryAfterSeconds": retry_after_seconds,
                    "nextPollAt": format_unix_ms(next_poll_at)?,
                }),
                PollHttpStatus::ProtocolError,
                Some(retry_after_seconds),
            )
        }
        DeviceAuthorizationPoll::CertificateReady(delivery) => {
            let record = &result.snapshot.record;
            let binding = &record.registration_binding;
            if delivery.certificate_der.is_empty()
                || delivery.certificate_der.len() > MAX_DELIVERABLE_CERTIFICATE_DER_BYTES
                || delivery.ca_chain_der.len() > 8
                || delivery
                    .ca_chain_der
                    .iter()
                    .any(|der| der.is_empty() || der.len() > MAX_DELIVERABLE_CERTIFICATE_DER_BYTES)
                || delivery
                    .ca_chain_der
                    .iter()
                    .try_fold(delivery.certificate_der.len(), |total_bytes, der| {
                        total_bytes.checked_add(der.len())
                    })
                    .is_none_or(|total_bytes| {
                        total_bytes > MAX_DELIVERABLE_CERTIFICATE_BUNDLE_BYTES
                    })
                || <[u8; 32]>::from(Sha256::digest(&delivery.certificate_der))
                    != delivery.certificate_sha256
                || delivery.authorization_id != record.id
                || delivery.delivery_id == [0; 16]
                || delivery.device_id != binding.key().device_id
                || delivery.authorization_generation != binding.authorization_generation()
                || delivery.scope != record.scope
                || delivery.csr_sha256 != record.csr_sha256
                || delivery.csr_spki_sha256 != record.spki_sha256
                || delivery.not_after_unix_ms <= result.snapshot.database_now_unix_ms
                || delivery.acknowledgement_deadline_unix_ms <= result.snapshot.database_now_unix_ms
                || delivery.acknowledgement_deadline_unix_ms > delivery.not_after_unix_ms
            {
                return Err(DeviceEnrollmentHttpError::Unavailable);
            }
            let metadata = certificate_metadata
                .public_metadata(delivery)
                .map_err(|_| DeviceEnrollmentHttpError::Unavailable)?;
            if metadata.issuer_id.trim().is_empty()
                || metadata.issuer_id.len() > 256
                || metadata.not_before_unix_ms == 0
                || metadata.not_before_unix_ms >= delivery.not_after_unix_ms
            {
                return Err(DeviceEnrollmentHttpError::Unavailable);
            }
            registry
                .stage_pending_delivery(&delivery.authorization_id)
                .map_err(|_| DeviceEnrollmentHttpError::Unavailable)?;
            let certificate_metadata = json!({
                "deviceId": delivery.device_id,
                "authorizationId": URL_SAFE_NO_PAD.encode(delivery.authorization_id),
                "scope": scope_json(&delivery.scope),
                "serialNumber": encode_hex(&delivery.serial_number),
                "certificateSha256": STANDARD.encode(delivery.certificate_sha256),
                "csrSpkiSha256": STANDARD.encode(delivery.csr_spki_sha256),
                "csrSha256": STANDARD.encode(delivery.csr_sha256),
                "issuerId": metadata.issuer_id,
                "notBefore": format_unix_ms(metadata.not_before_unix_ms)?,
                "notAfter": format_unix_ms(delivery.not_after_unix_ms)?,
                "status": "WORKSPACE_DEVICE_CERTIFICATE_STATUS_DELIVERY_PENDING",
                "purpose": "WORKSPACE_DEVICE_CERTIFICATE_PURPOSE_CONTROL_CLIENT_AUTH",
            });
            (
                json!({
                    "status": "DEVICE_AUTHORIZATION_POLL_STATUS_APPROVED",
                    "authorization": authorization,
                    "delivery": {
                        "deliveryId": URL_SAFE_NO_PAD.encode(delivery.delivery_id),
                        "certificateDer": STANDARD.encode(&delivery.certificate_der),
                        "caChainDer": delivery
                            .ca_chain_der
                            .iter()
                            .map(|der| STANDARD.encode(der))
                            .collect::<Vec<_>>(),
                        "certificate": certificate_metadata,
                        "acknowledgementDeadline": format_unix_ms(delivery.acknowledgement_deadline_unix_ms)?,
                    },
                }),
                PollHttpStatus::Approved,
                None,
            )
        }
        DeviceAuthorizationPoll::Delivered(receipt) => {
            let record = &result.snapshot.record;
            let binding = &record.registration_binding;
            if receipt.authorization_id != record.id
                || receipt.delivery_id == [0; 16]
                || receipt.device_id != binding.key().device_id
                || receipt.authorization_generation != binding.authorization_generation()
                || receipt.csr_sha256 != record.csr_sha256
                || receipt.csr_spki_sha256 != record.spki_sha256
            {
                return Err(DeviceEnrollmentHttpError::Unavailable);
            }
            // Recover activation if a prior ACK committed but the process
            // stopped before it could update the registry.
            activate_acknowledged_delivery(registry, &receipt.authorization_id)?;
            (
                json!({
                    "status": "DEVICE_AUTHORIZATION_POLL_STATUS_DELIVERY_CONSUMED",
                    "error": "invalid_grant",
                    "authorization": authorization,
                }),
                PollHttpStatus::ProtocolError,
                None,
            )
        }
        DeviceAuthorizationPoll::DeliveryExpired { .. } => (
            json!({
                "status": "DEVICE_AUTHORIZATION_POLL_STATUS_DELIVERY_EXPIRED",
                "error": "delivery_expired",
                "authorization": authorization,
                "recoveryStatus": "DEVICE_DELIVERY_RECOVERY_STATUS_CERTIFICATE_RETIRED",
            }),
            PollHttpStatus::ProtocolError,
            None,
        ),
        DeviceAuthorizationPoll::RecoveryRequired { status } => {
            let recovery_status = match status {
                DeviceDeliveryRecoveryStatus::RevocationPending => {
                    "DEVICE_DELIVERY_RECOVERY_STATUS_REVOCATION_PENDING"
                }
                DeviceDeliveryRecoveryStatus::RecoveryBlocked => {
                    "DEVICE_DELIVERY_RECOVERY_STATUS_RECOVERY_BLOCKED"
                }
            };
            (
                json!({
                    "status": "DEVICE_AUTHORIZATION_POLL_STATUS_DELIVERY_RECOVERY_BLOCKED",
                    "error": "delivery_recovery_blocked",
                    "authorization": authorization,
                    "recoveryStatus": recovery_status,
                }),
                PollHttpStatus::ProtocolError,
                None,
            )
        }
        DeviceAuthorizationPoll::IssuanceFailed { .. } => {
            return Err(DeviceEnrollmentHttpError::Conflict);
        }
    };

    Ok(PollHttpResponse {
        body,
        status,
        retry_after_seconds,
        committed_snapshot: snapshot,
    })
}

fn terminal_poll_response(
    snapshot: crate::device_authorization::DeviceAuthorizationPollSnapshot,
) -> Result<PollHttpResponse, DeviceEnrollmentHttpError> {
    let authorization = authorization_reference_from_record(&snapshot.record)?;
    let (status, error, decided_at) = match &snapshot.record.state {
        DeviceAuthorizationState::Denied {
            decided_at_unix_ms, ..
        } => (
            "DEVICE_AUTHORIZATION_POLL_STATUS_DENIED",
            "access_denied",
            Some(*decided_at_unix_ms),
        ),
        DeviceAuthorizationState::Expired => (
            "DEVICE_AUTHORIZATION_POLL_STATUS_EXPIRED",
            "expired_token",
            None,
        ),
        DeviceAuthorizationState::Consumed { .. } | DeviceAuthorizationState::Delivered { .. } => (
            "DEVICE_AUTHORIZATION_POLL_STATUS_DELIVERY_CONSUMED",
            "invalid_grant",
            None,
        ),
        _ => return Err(DeviceEnrollmentHttpError::Conflict),
    };
    let mut body = json!({
        "status": status,
        "error": error,
        "authorization": authorization,
    });
    if let Some(decided_at_unix_ms) = decided_at {
        body["decidedAt"] = Value::String(format_unix_ms(decided_at_unix_ms)?);
    }
    Ok(PollHttpResponse {
        body,
        status: PollHttpStatus::ProtocolError,
        retry_after_seconds: None,
        committed_snapshot: DeviceEnrollmentAuthorizationSnapshot::from_poll_snapshot(&snapshot)?,
    })
}

fn acknowledgement_body(
    receipt: &DeviceCertificateDeliveryReceipt,
    record: &DeviceAuthorizationRecord,
    expected_authorization_id: &str,
    expected_delivery_id: &str,
    expected_certificate_sha256: &[u8; 32],
    expected_csr_sha256: &[u8; 32],
    expected_spki_sha256: &[u8; 32],
) -> Result<Value, DeviceEnrollmentHttpError> {
    if URL_SAFE_NO_PAD.encode(receipt.authorization_id) != expected_authorization_id
        || URL_SAFE_NO_PAD.encode(receipt.delivery_id) != expected_delivery_id
        || receipt.delivery_id == [0; 16]
        || record.id != receipt.authorization_id
        || record.registration_binding.key().device_id != receipt.device_id
        || record.registration_binding.authorization_generation()
            != receipt.authorization_generation
        || &receipt.certificate_sha256 != expected_certificate_sha256
        || &receipt.csr_sha256 != expected_csr_sha256
        || &receipt.csr_spki_sha256 != expected_spki_sha256
        || record.csr_sha256 != receipt.csr_sha256
        || record.spki_sha256 != receipt.csr_spki_sha256
        || !matches!(
            &record.state,
            DeviceAuthorizationState::Delivered {
                receipt: committed_receipt,
                ..
            } if committed_receipt == receipt
        )
    {
        return Err(DeviceEnrollmentHttpError::Unavailable);
    }
    Ok(json!({
        "authorization": authorization_reference_from_record(record)?,
        "deliveryId": URL_SAFE_NO_PAD.encode(receipt.delivery_id),
        "certificateSha256": STANDARD.encode(receipt.certificate_sha256),
        "acknowledgedAt": format_unix_ms(receipt.acknowledged_at_unix_ms)?,
        "state": "DEVICE_AUTHORIZATION_LIFECYCLE_STATE_DELIVERED",
    }))
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

#[async_trait]
impl DeviceEnrollmentAuthorizationPort for DeviceEnrollmentAuthorizationService {
    async fn begin_approval(
        &self,
        user: &UserIdentityRef,
        abuse_key: &[u8; 32],
        user_code: &str,
        scope: DeviceAuthorizationScope,
    ) -> Result<Value, DeviceEnrollmentHttpError> {
        let reservation = self.reserve_user_code_attempt(abuse_key).await?;
        self.require_member(user, &scope).await?;

        let manager = Arc::clone(&self.manager);
        let manager_gate = Arc::clone(&self.manager_gate);
        let limiter_state = Arc::clone(&self.limiter_state);
        let store = Arc::clone(&self.store);
        let user = user.clone();
        let scope_for_membership = scope.clone();
        let user_code = Zeroizing::new(user_code.to_owned());
        let abuse_key = Zeroizing::new(*abuse_key);
        let ports = self.port_handles();
        let result = tokio::task::spawn_blocking(move || {
            let _manager_gate = manager_gate
                .lock()
                .map_err(|_| DeviceEnrollmentHttpError::Unavailable)?;
            let _installed = install_attempt(&limiter_state, reservation)?;
            let now = store
                .database_time_unix_ms()
                .map_err(|_| DeviceEnrollmentHttpError::Unavailable)?;
            let membership = MembershipEvidence::new(user.clone(), scope_for_membership);
            let clock = DatabaseClock(Arc::clone(&store));
            let call_ports = ports.borrow(&membership, &clock);
            manager
                .begin_approval_with_snapshot(
                    &user_code,
                    &scope,
                    &user,
                    &abuse_key,
                    now,
                    &call_ports,
                )
                .map(|started| (started, now))
                .map_err(map_authorization_error)
        })
        .await
        .map_err(|_| DeviceEnrollmentHttpError::Unavailable)??;

        approval_challenge_body(&result.0, result.1)
    }

    async fn complete_approval(
        &self,
        user: &UserIdentityRef,
        approval_id: String,
        assertion_json: Option<SecretBytes>,
    ) -> Result<CompleteApprovalHttpResponse, DeviceEnrollmentHttpError> {
        let approval_id = decode_authorization_id(&approval_id)?;
        let record = self.find_by_approval_id(approval_id).await?;
        if state_approver(&record.state) != Some(user) {
            return Err(DeviceEnrollmentHttpError::InvalidGrant);
        }
        self.require_member(user, &record.scope).await?;

        let manager = Arc::clone(&self.manager);
        let manager_gate = Arc::clone(&self.manager_gate);
        let store = Arc::clone(&self.store);
        let registry = Arc::clone(&self.registry);
        let webauthn = Arc::clone(&self.webauthn);
        let csr_validator = Arc::clone(&self.csr_validator);
        let issuer = Arc::clone(&self.issuer);
        let retirement = Arc::clone(&self.retirement);
        let user = user.clone();
        let assertion = assertion_json;
        let membership_scope = record.scope.clone();
        let snapshot = tokio::task::spawn_blocking(move || {
            let _manager_gate = manager_gate
                .lock()
                .map_err(|_| DeviceEnrollmentHttpError::Unavailable)?;
            let now = store
                .database_time_unix_ms()
                .map_err(|_| DeviceEnrollmentHttpError::Unavailable)?;
            let membership = MembershipEvidence::new(user.clone(), membership_scope);
            let clock = DatabaseClock(Arc::clone(&store));
            let call_ports = ApprovalPorts {
                membership: &membership,
                webauthn: webauthn.as_ref(),
                csr_validator: csr_validator.as_ref(),
                issuer: issuer.as_ref(),
                retirement: retirement.as_ref(),
                clock: &clock,
            };
            let assertion = assertion.as_ref().map_or(&[][..], SecretBytes::expose);
            let snapshot = manager
                .complete_approval_for_user(&approval_id, &user, assertion, now, &call_ports)
                .map_err(map_authorization_error)?;
            if snapshot.state() == DeviceAuthorizationApprovalState::DeliveryPending {
                registry
                    .stage_pending_delivery(snapshot.authorization_id())
                    .map_err(|_| DeviceEnrollmentHttpError::Unavailable)?;
            } else if snapshot.state() == DeviceAuthorizationApprovalState::Delivered {
                activate_acknowledged_delivery(registry.as_ref(), snapshot.authorization_id())?;
            }
            Ok(snapshot)
        })
        .await
        .map_err(|_| DeviceEnrollmentHttpError::Unavailable)??;

        complete_approval_body(&snapshot)
    }

    async fn deny(
        &self,
        user: &UserIdentityRef,
        abuse_key: &[u8; 32],
        user_code: &str,
        scope: DeviceAuthorizationScope,
    ) -> Result<Value, DeviceEnrollmentHttpError> {
        let reservation = self.reserve_user_code_attempt(abuse_key).await?;
        self.require_member(user, &scope).await?;
        let candidates = self
            .user_code_keys
            .lookup_candidates(&normalize_user_code(user_code))
            .map_err(|_| DeviceEnrollmentHttpError::InvalidGrant)?;

        let manager = Arc::clone(&self.manager);
        let manager_gate = Arc::clone(&self.manager_gate);
        let limiter_state = Arc::clone(&self.limiter_state);
        let store = Arc::clone(&self.store);
        let user = user.clone();
        let user_code = Zeroizing::new(user_code.to_owned());
        let abuse_key = Zeroizing::new(*abuse_key);
        let scope_for_membership = scope.clone();
        let response_user = user.clone();
        let result = tokio::task::spawn_blocking(move || {
            let _manager_gate = manager_gate
                .lock()
                .map_err(|_| DeviceEnrollmentHttpError::Unavailable)?;
            let _installed = install_attempt(&limiter_state, reservation)?;
            let now = store
                .database_time_unix_ms()
                .map_err(|_| DeviceEnrollmentHttpError::Unavailable)?;
            let membership = MembershipEvidence::new(user.clone(), scope_for_membership);
            manager
                .deny(&user_code, &scope, &user, &abuse_key, now, &membership)
                .map_err(map_authorization_error)?;
            let record = store
                .by_user_code_candidates(&candidates)
                .map_err(|_| DeviceEnrollmentHttpError::Unavailable)?
                .ok_or(DeviceEnrollmentHttpError::Conflict)?;
            store
                .require_current_registered_record(&record)
                .map_err(|_| DeviceEnrollmentHttpError::Conflict)?;
            Ok::<_, DeviceEnrollmentHttpError>(record)
        })
        .await
        .map_err(|_| DeviceEnrollmentHttpError::Unavailable)??;

        denial_body(&result, response_user)
    }

    async fn poll(&self, device_code: &str) -> Result<PollHttpResponse, DeviceEnrollmentHttpError> {
        let admission = Arc::clone(&self.poll_admission)
            .try_acquire_owned()
            .map_err(|_| DeviceEnrollmentHttpError::Unavailable)?;
        let device_code = Zeroizing::new(device_code.to_owned());
        let manager = Arc::clone(&self.manager);
        let store = Arc::clone(&self.store);
        let registry = Arc::clone(&self.registry);
        let retirement = Arc::clone(&self.retirement);
        let certificate_metadata = Arc::clone(&self.certificate_metadata);
        let result = tokio::task::spawn_blocking(move || {
            let _admission = admission;
            let now = store
                .database_time_unix_ms()
                .map_err(|_| DeviceEnrollmentHttpError::Unavailable)?;
            let clock = DatabaseClock(Arc::clone(&store));
            let call_ports = DeliveryPorts {
                retirement: retirement.as_ref(),
                clock: &clock,
            };
            match manager.poll_with_snapshot(&device_code, now, &call_ports) {
                Ok(result) => {
                    project_poll_result(&result, registry.as_ref(), certificate_metadata.as_ref())
                }
                Err(
                    error @ (DeviceAuthorizationError::Denied
                    | DeviceAuthorizationError::Expired
                    | DeviceAuthorizationError::AlreadyConsumed),
                ) => {
                    let _ = error;
                    let code_hash = crate::device_authorization::device_code_hash(&device_code)
                        .map_err(|_| DeviceEnrollmentHttpError::InvalidGrant)?;
                    let snapshot = store
                        .current_poll_snapshot(&code_hash, None, now)
                        .map_err(|_| DeviceEnrollmentHttpError::Unavailable)?
                        .ok_or(DeviceEnrollmentHttpError::InvalidGrant)?;
                    terminal_poll_response(snapshot)
                }
                Err(error) => Err(map_authorization_error(error)),
            }
        })
        .await
        .map_err(|_| DeviceEnrollmentHttpError::Unavailable)??;
        Ok(result)
    }

    async fn acknowledge_delivery(
        &self,
        request: AcknowledgeDeviceDeliveryCommand,
    ) -> Result<Value, DeviceEnrollmentHttpError> {
        let authorization_id = decode_authorization_id(&request.authorization_id)?;
        let expected_delivery_id = decode_authorization_id(&request.delivery_id)?;
        let device_code = Zeroizing::new(request.device_code.expose().to_owned());
        let acknowledgement = DeviceCertificateDeliveryAcknowledgement {
            authorization_id,
            device_code: (*device_code).clone(),
            delivery_id: expected_delivery_id,
            certificate_sha256: request.certificate_sha256,
            csr_sha256: request.csr_sha256,
            csr_spki_sha256: request.csr_spki_sha256,
        };
        let manager = Arc::clone(&self.manager);
        let store = Arc::clone(&self.store);
        let registry = Arc::clone(&self.registry);
        let retirement = Arc::clone(&self.retirement);
        let expected_authorization_id = request.authorization_id.clone();
        let expected_delivery_id = request.delivery_id.clone();
        let expected_certificate_sha256 = request.certificate_sha256;
        let expected_csr_sha256 = request.csr_sha256;
        let expected_spki_sha256 = request.csr_spki_sha256;
        let manager_gate = Arc::clone(&self.manager_gate);
        let result = tokio::task::spawn_blocking(move || {
            let _manager_gate = manager_gate
                .lock()
                .map_err(|_| DeviceEnrollmentHttpError::Unavailable)?;
            let clock = DatabaseClock(Arc::clone(&store));
            let delivery_ports = DeliveryPorts {
                retirement: retirement.as_ref(),
                clock: &clock,
            };
            let mut acknowledgement = acknowledgement;
            let receipt_result = manager.acknowledge_delivery(&acknowledgement, &delivery_ports);
            acknowledgement.device_code.zeroize();
            let receipt = receipt_result.map_err(map_authorization_error)?;
            activate_acknowledged_delivery(registry.as_ref(), &authorization_id)?;
            let record = store
                .by_authorization_id(&authorization_id)
                .map_err(|_| DeviceEnrollmentHttpError::Unavailable)?
                .ok_or(DeviceEnrollmentHttpError::Conflict)?;
            store
                .require_current_registered_record(&record)
                .map_err(|_| DeviceEnrollmentHttpError::Conflict)?;
            Ok::<_, DeviceEnrollmentHttpError>((receipt, record))
        })
        .await
        .map_err(|_| DeviceEnrollmentHttpError::Unavailable)??;
        acknowledgement_body(
            &result.0,
            &result.1,
            &expected_authorization_id,
            &expected_delivery_id,
            &expected_certificate_sha256,
            &expected_csr_sha256,
            &expected_spki_sha256,
        )
    }
}
