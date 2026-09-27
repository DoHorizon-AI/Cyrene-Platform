//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 device_authorization_sweeper.rs                                │
//! │  Module: cy_workspace_fabric::device_authorization_sweeper          │
//! │  Role: Bounded recovery of expired certificate deliveries.          │
//! │                                                                     │
//! │  模块职责：有界扫描并恢复到期设备证书交付的撤销流程。                     │
//! └─────────────────────────────────────────────────────────────────────┘
//!
//! This module does not own authorization state transitions. It queries an
//! advisory work source, then delegates each candidate to the manager's
//! durable reservation and idempotent retirement path. A production source
//! must read the database clock and scan the same primary database used by the
//! manager store.

use std::collections::BTreeSet;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use thiserror::Error;

use crate::device_authorization::{
    DeviceAuthorizationError, DeviceAuthorizationId, DeviceAuthorizationManager,
    DeviceAuthorizationStore, DeviceAuthorizationStoreError, DeviceCertificateRetirementPort,
    DeviceCertificateRetirementWorkOutcome, DeviceDeliveryRecoveryStatus, UserCodeAttemptLimiter,
};

/// Maximum number of authorization rows one sweep pass may attempt.
pub(crate) const MAX_RETIREMENT_ATTEMPTS_PER_PASS: usize = 100;

/// Database-backed advisory selector and trusted time source used by one pass.
///
/// Implementations must use the authorization primary database. The due scan
/// is advisory: the manager's reservation CAS remains responsible for checking
/// the row revision, state, and deadline atomically before contacting the CA.
pub(crate) trait DeviceCertificateRetirementSweepSource: Send + Sync {
    /// Reads the authorization database clock in Unix milliseconds.
    fn database_time_unix_ms(&self) -> Result<u64, DeviceAuthorizationStoreError>;

    /// Returns at most `limit` due delivery IDs, using the sampled DB time.
    fn due_certificate_delivery_ids(
        &self,
        database_now_unix_ms: u64,
        limit: usize,
    ) -> Result<Vec<DeviceAuthorizationId>, DeviceAuthorizationStoreError>;

    /// Returns at most `limit` persisted `RetirementPending` authorization IDs.
    fn recoverable_retirement_ids(
        &self,
        limit: usize,
    ) -> Result<Vec<DeviceAuthorizationId>, DeviceAuthorizationStoreError>;
}

pub(crate) trait DeviceCertificateRetirementProcessor<P> {
    fn process_retirement_work(
        &self,
        authorization_id: &DeviceAuthorizationId,
        database_now_unix_ms: u64,
        retirement_port: &P,
    ) -> Result<DeviceCertificateRetirementWorkOutcome, DeviceAuthorizationError>;
}

impl<S, L, P> DeviceCertificateRetirementProcessor<P> for DeviceAuthorizationManager<S, L>
where
    S: DeviceAuthorizationStore,
    L: UserCodeAttemptLimiter,
    P: DeviceCertificateRetirementPort,
{
    fn process_retirement_work(
        &self,
        authorization_id: &DeviceAuthorizationId,
        database_now_unix_ms: u64,
        retirement_port: &P,
    ) -> Result<DeviceCertificateRetirementWorkOutcome, DeviceAuthorizationError> {
        self.retire_certificate_work_by_authorization_id(
            authorization_id,
            database_now_unix_ms,
            retirement_port,
        )
    }
}

/// Aggregate counts from one bounded pass. No authorization or device IDs are retained.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct DeviceCertificateRetirementSweepReport {
    pub due_candidates: usize,
    pub recovery_candidates: usize,
    pub attempted: usize,
    pub retired: usize,
    pub no_work: usize,
    pub revocation_pending: usize,
    pub recovery_blocked: usize,
    pub failed_attempts: usize,
    pub deferred_candidates: usize,
}

/// Stable failures reported by the bounded retirement service wrapper.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub(crate) enum DeviceCertificateRetirementSweepError {
    #[error("retirement sweep configuration is invalid")]
    InvalidConfiguration,
    #[error("retirement sweep was called before its minimum interval")]
    Throttled,
    #[error("retirement sweep coordination is unavailable")]
    CoordinationUnavailable,
    #[error("retirement sweep storage is unavailable")]
    StorageUnavailable,
}

/// A bounded one-pass retirement worker with a process-local minimum interval.
///
/// The caller schedules `run_once` at a fixed interval. Each pass processes no
/// more than `max_attempts_per_pass` distinct records and never hot-loops a
/// failed CA request. Multi-replica deployments must still use an idempotent
/// CA retirement port; this local interval is not a fleet-wide rate limiter.
pub(crate) struct DeviceCertificateRetirementSweeper<M, S, P> {
    manager: M,
    source: S,
    retirement_port: P,
    max_attempts_per_pass: usize,
    minimum_interval: Duration,
    last_run: Mutex<Option<Instant>>,
}

impl<M, S, P> DeviceCertificateRetirementSweeper<M, S, P>
where
    M: DeviceCertificateRetirementProcessor<P>,
    S: DeviceCertificateRetirementSweepSource,
    P: DeviceCertificateRetirementPort,
{
    /// Creates a worker with bounded work and a minimum delay between passes.
    pub(crate) fn new(
        manager: M,
        source: S,
        retirement_port: P,
        max_attempts_per_pass: usize,
        minimum_interval: Duration,
    ) -> Result<Self, DeviceCertificateRetirementSweepError> {
        if max_attempts_per_pass == 0
            || max_attempts_per_pass > MAX_RETIREMENT_ATTEMPTS_PER_PASS
            || minimum_interval < Duration::from_secs(1)
        {
            return Err(DeviceCertificateRetirementSweepError::InvalidConfiguration);
        }
        Ok(Self {
            manager,
            source,
            retirement_port,
            max_attempts_per_pass,
            minimum_interval,
            last_run: Mutex::new(None),
        })
    }

    /// Processes one database-time snapshot and a bounded number of work rows.
    pub(crate) fn run_once(
        &self,
    ) -> Result<DeviceCertificateRetirementSweepReport, DeviceCertificateRetirementSweepError> {
        self.reserve_run_slot()?;

        let database_now_unix_ms = self.source.database_time_unix_ms().map_err(|_| {
            tracing::error!(
                target: "cy_workspace_fabric::device_authorization_sweeper",
                stage = "database_time",
                "device certificate retirement sweep could not read database time"
            );
            DeviceCertificateRetirementSweepError::StorageUnavailable
        })?;
        let due_ids = self
            .source
            .due_certificate_delivery_ids(database_now_unix_ms, self.max_attempts_per_pass)
            .map_err(|_| {
                tracing::error!(
                    target: "cy_workspace_fabric::device_authorization_sweeper",
                    stage = "due_delivery_scan",
                    "device certificate retirement sweep could not scan due deliveries"
                );
                DeviceCertificateRetirementSweepError::StorageUnavailable
            })?;
        let recovery_ids = self
            .source
            .recoverable_retirement_ids(self.max_attempts_per_pass)
            .map_err(|_| {
                tracing::error!(
                    target: "cy_workspace_fabric::device_authorization_sweeper",
                    stage = "retirement_recovery_scan",
                    "device certificate retirement sweep could not scan pending retirements"
                );
                DeviceCertificateRetirementSweepError::StorageUnavailable
            })?;

        let mut report = DeviceCertificateRetirementSweepReport {
            due_candidates: due_ids.len(),
            recovery_candidates: recovery_ids.len(),
            ..DeviceCertificateRetirementSweepReport::default()
        };
        let work_ids = interleave_unique_ids(due_ids, recovery_ids);
        report.deferred_candidates = work_ids.len().saturating_sub(self.max_attempts_per_pass);

        for authorization_id in work_ids.into_iter().take(self.max_attempts_per_pass) {
            report.attempted += 1;
            match self.manager.process_retirement_work(
                &authorization_id,
                database_now_unix_ms,
                &self.retirement_port,
            ) {
                Ok(DeviceCertificateRetirementWorkOutcome::NoWork) => report.no_work += 1,
                Ok(DeviceCertificateRetirementWorkOutcome::Retired) => report.retired += 1,
                Ok(DeviceCertificateRetirementWorkOutcome::Pending(
                    DeviceDeliveryRecoveryStatus::RevocationPending,
                )) => report.revocation_pending += 1,
                Ok(DeviceCertificateRetirementWorkOutcome::Pending(
                    DeviceDeliveryRecoveryStatus::RecoveryBlocked,
                )) => report.recovery_blocked += 1,
                Err(_) => report.failed_attempts += 1,
            }
        }

        if report.failed_attempts == 0 {
            tracing::info!(
                target: "cy_workspace_fabric::device_authorization_sweeper",
                due_candidates = report.due_candidates,
                recovery_candidates = report.recovery_candidates,
                attempted = report.attempted,
                retired = report.retired,
                no_work = report.no_work,
                revocation_pending = report.revocation_pending,
                recovery_blocked = report.recovery_blocked,
                deferred = report.deferred_candidates,
                "device certificate retirement sweep completed"
            );
        } else {
            tracing::warn!(
                target: "cy_workspace_fabric::device_authorization_sweeper",
                due_candidates = report.due_candidates,
                recovery_candidates = report.recovery_candidates,
                attempted = report.attempted,
                retired = report.retired,
                no_work = report.no_work,
                revocation_pending = report.revocation_pending,
                recovery_blocked = report.recovery_blocked,
                failed_attempts = report.failed_attempts,
                deferred = report.deferred_candidates,
                "device certificate retirement sweep completed with failures"
            );
        }
        Ok(report)
    }

    fn reserve_run_slot(&self) -> Result<(), DeviceCertificateRetirementSweepError> {
        let now = Instant::now();
        let mut last_run = self
            .last_run
            .lock()
            .map_err(|_| DeviceCertificateRetirementSweepError::CoordinationUnavailable)?;
        if last_run.is_some_and(|last| now.duration_since(last) < self.minimum_interval) {
            return Err(DeviceCertificateRetirementSweepError::Throttled);
        }
        *last_run = Some(now);
        Ok(())
    }
}

fn interleave_unique_ids(
    due_ids: Vec<DeviceAuthorizationId>,
    recovery_ids: Vec<DeviceAuthorizationId>,
) -> Vec<DeviceAuthorizationId> {
    let mut due = due_ids.into_iter();
    let mut recovery = recovery_ids.into_iter();
    let mut seen = BTreeSet::new();
    let mut result = Vec::new();

    loop {
        let due_id = due.next();
        let recovery_id = recovery.next();
        if due_id.is_none() && recovery_id.is_none() {
            break;
        }
        for authorization_id in [due_id, recovery_id].into_iter().flatten() {
            if seen.insert(authorization_id) {
                result.push(authorization_id);
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct TestSweepSource {
        due_ids: Vec<DeviceAuthorizationId>,
        recovery_ids: Vec<DeviceAuthorizationId>,
        requested_limits: Mutex<Vec<usize>>,
    }

    impl DeviceCertificateRetirementSweepSource for TestSweepSource {
        fn database_time_unix_ms(&self) -> Result<u64, DeviceAuthorizationStoreError> {
            Ok(123_456)
        }

        fn due_certificate_delivery_ids(
            &self,
            _database_now_unix_ms: u64,
            limit: usize,
        ) -> Result<Vec<DeviceAuthorizationId>, DeviceAuthorizationStoreError> {
            self.requested_limits.lock().unwrap().push(limit);
            Ok(self.due_ids.iter().copied().take(limit).collect())
        }

        fn recoverable_retirement_ids(
            &self,
            limit: usize,
        ) -> Result<Vec<DeviceAuthorizationId>, DeviceAuthorizationStoreError> {
            self.requested_limits.lock().unwrap().push(limit);
            Ok(self.recovery_ids.iter().copied().take(limit).collect())
        }
    }

    struct TestRetirementPort;

    impl DeviceCertificateRetirementPort for TestRetirementPort {
        fn declared_hard_timeout(&self) -> Option<std::time::Duration> {
            Some(std::time::Duration::from_secs(30))
        }

        fn retire_or_confirm(
            &self,
            _authorization_id: &DeviceAuthorizationId,
            _certificate_sha256: &[u8; 32],
            _certificate: &crate::device_authorization::IssuedDeviceCertificate,
            _reason: crate::device_authorization::DeviceCertificateRetirementReason,
        ) -> Result<(), crate::device_authorization::DeviceCertificateRetirementError> {
            Ok(())
        }
    }

    struct TestProcessor {
        processed: Mutex<Vec<DeviceAuthorizationId>>,
    }

    impl DeviceCertificateRetirementProcessor<TestRetirementPort> for TestProcessor {
        fn process_retirement_work(
            &self,
            authorization_id: &DeviceAuthorizationId,
            database_now_unix_ms: u64,
            _retirement_port: &TestRetirementPort,
        ) -> Result<DeviceCertificateRetirementWorkOutcome, DeviceAuthorizationError> {
            assert_eq!(database_now_unix_ms, 123_456);
            self.processed.lock().unwrap().push(*authorization_id);
            match authorization_id[0] {
                1 => Ok(DeviceCertificateRetirementWorkOutcome::Retired),
                2 => Ok(DeviceCertificateRetirementWorkOutcome::Pending(
                    DeviceDeliveryRecoveryStatus::RevocationPending,
                )),
                _ => Err(DeviceAuthorizationError::StorageUnavailable),
            }
        }
    }

    fn id(value: u8) -> DeviceAuthorizationId {
        [value; 16]
    }

    #[test]
    fn pass_interleaves_work_and_caps_attempts_with_aggregate_report() {
        let source = TestSweepSource {
            due_ids: vec![id(1), id(2), id(3), id(4)],
            recovery_ids: vec![id(5), id(6), id(7), id(8)],
            requested_limits: Mutex::new(Vec::new()),
        };
        let processor = TestProcessor {
            processed: Mutex::new(Vec::new()),
        };
        let sweeper = DeviceCertificateRetirementSweeper::new(
            processor,
            source,
            TestRetirementPort,
            3,
            Duration::from_secs(1),
        )
        .expect("bounded worker configuration");

        let report = sweeper.run_once().expect("one bounded pass");
        assert_eq!(report.due_candidates, 3);
        assert_eq!(report.recovery_candidates, 3);
        assert_eq!(report.attempted, 3);
        assert_eq!(report.retired, 1);
        assert_eq!(report.revocation_pending, 1);
        assert_eq!(report.failed_attempts, 1);
        assert_eq!(report.deferred_candidates, 3);
    }

    #[test]
    fn rejects_unbounded_or_unthrottled_configuration() {
        let source = TestSweepSource {
            due_ids: Vec::new(),
            recovery_ids: Vec::new(),
            requested_limits: Mutex::new(Vec::new()),
        };
        let processor = TestProcessor {
            processed: Mutex::new(Vec::new()),
        };
        assert!(matches!(
            DeviceCertificateRetirementSweeper::new(
                processor,
                source,
                TestRetirementPort,
                MAX_RETIREMENT_ATTEMPTS_PER_PASS + 1,
                Duration::from_secs(1),
            ),
            Err(DeviceCertificateRetirementSweepError::InvalidConfiguration)
        ));
    }

    #[test]
    fn enforces_minimum_interval_between_passes() {
        let source = TestSweepSource {
            due_ids: Vec::new(),
            recovery_ids: Vec::new(),
            requested_limits: Mutex::new(Vec::new()),
        };
        let processor = TestProcessor {
            processed: Mutex::new(Vec::new()),
        };
        let sweeper = DeviceCertificateRetirementSweeper::new(
            processor,
            source,
            TestRetirementPort,
            1,
            Duration::from_secs(1),
        )
        .expect("bounded worker configuration");

        *sweeper.last_run.lock().expect("run slot") = Some(Instant::now());
        assert_eq!(
            sweeper.run_once(),
            Err(DeviceCertificateRetirementSweepError::Throttled)
        );
    }
}
