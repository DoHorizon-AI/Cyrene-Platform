//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 device_enrollment_registration_postgres.rs                      │
//! │  Module: cy_workspace_fabric::device_enrollment_registration_postgres│
//! │  Role: Adapts registered enrollment starts to the atomic PG store.  │
//! │                                                                     │
//! │  模块职责：将设备注册开始/恢复接入原子 PostgreSQL 事务。                  │
//! └─────────────────────────────────────────────────────────────────────┘
//!
//! This adapter deliberately delegates identity creation and authorization
//! persistence to `start_or_recover_registered`; it never performs a separate
//! Directory bind or authorization insert.

use std::sync::Arc;

use async_trait::async_trait;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use thiserror::Error;
use url::Url;
use zeroize::Zeroizing;

use crate::device_authorization::{
    device_code_hash, DeviceAuthorizationError, DeviceAuthorizationManager,
    DeviceAuthorizationPolicy, DeviceAuthorizationRateLimitError,
    DeviceAuthorizationRegisteredStart, DeviceAuthorizationRegistrationRequest,
    DeviceAuthorizationScope, DeviceAuthorizationStart, DeviceAuthorizationStartDisposition,
    DeviceCsrValidator, UserCodeAttemptLimiter,
};
use crate::device_authorization_postgres::PostgresDeviceAuthorizationStore;
use crate::device_enrollment_http::{
    DeviceAuthorizationCodesWire, DeviceAuthorizationReferenceWire,
    DeviceEnrollmentAuthorizationSnapshot, DeviceEnrollmentHttpError,
    DeviceEnrollmentRegistrationTransactionPort, DeviceEnrollmentStartResult, DeviceScopeWire,
    StartDeviceAuthorizationResponse,
};
use crate::UserCodeKeyRing;

type RegistrationManager =
    DeviceAuthorizationManager<Arc<PostgresDeviceAuthorizationStore>, RegistrationOnlyLimiter>;

/// Safe construction failures for the PostgreSQL registration adapter.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum DeviceEnrollmentRegistrationPostgresError {
    /// Runtime policy, key versions, or verification URI are invalid.
    #[error("device enrollment registration configuration is invalid")]
    Configuration,
    /// The authorization store or its committed schema is unavailable.
    #[error("device enrollment registration storage is unavailable")]
    Unavailable,
}

/// Production transaction port for HTTP registration starts and recovery.
///
/// Construction injects stable user-code keys, policy, CSR validation, and the
/// browser verification URL. The host can keep this port out of HTTP
/// dependencies until its runtime trust providers are ready.
pub struct PostgresDeviceEnrollmentRegistrationTransaction {
    store: Arc<PostgresDeviceAuthorizationStore>,
    manager: Arc<RegistrationManager>,
    csr_validator: Arc<dyn DeviceCsrValidator>,
    verification_uri: String,
}

impl PostgresDeviceEnrollmentRegistrationTransaction {
    /// Creates the manager off the async executor and verifies database key
    /// versions before the adapter can accept a start request.
    pub async fn new(
        store: Arc<PostgresDeviceAuthorizationStore>,
        user_code_keys: UserCodeKeyRing,
        policy: DeviceAuthorizationPolicy,
        csr_validator: Arc<dyn DeviceCsrValidator>,
        verification_uri: &str,
    ) -> Result<Self, DeviceEnrollmentRegistrationPostgresError> {
        let verification_uri = parse_verification_uri(verification_uri)?;
        let manager_store = Arc::clone(&store);
        let manager = tokio::task::spawn_blocking(move || {
            DeviceAuthorizationManager::new(
                manager_store,
                RegistrationOnlyLimiter,
                user_code_keys,
                policy,
            )
        })
        .await
        .map_err(|_| DeviceEnrollmentRegistrationPostgresError::Unavailable)?
        .map_err(map_manager_configuration_error)?;

        Ok(Self {
            store,
            manager: Arc::new(manager),
            csr_validator,
            verification_uri,
        })
    }
}

#[async_trait]
impl DeviceEnrollmentRegistrationTransactionPort
    for PostgresDeviceEnrollmentRegistrationTransaction
{
    async fn bind_and_start(
        &self,
        request: DeviceAuthorizationRegistrationRequest,
    ) -> Result<DeviceEnrollmentStartResult, DeviceEnrollmentHttpError> {
        let expected = RegistrationExpectation::from_request(&request);
        let store = Arc::clone(&self.store);
        let manager = Arc::clone(&self.manager);
        let csr_validator = Arc::clone(&self.csr_validator);
        let verification_uri = self.verification_uri.clone();

        tokio::task::spawn_blocking(move || {
            let database_now = store
                .database_time_unix_ms()
                .map_err(|_| DeviceEnrollmentHttpError::Unavailable)?;
            let validator = SharedCsrValidator(csr_validator);
            let started = manager
                .begin_or_recover_registered_with_snapshot(request, database_now, &validator)
                .map_err(map_manager_error)?;
            project_committed_start(started, &expected, verification_uri)
        })
        .await
        .map_err(|_| DeviceEnrollmentHttpError::Unavailable)?
    }
}

/// This manager instance only creates and recovers starts. Reject any
/// accidental approval attempt so registration wiring cannot become a bypass.
#[derive(Clone, Copy)]
struct RegistrationOnlyLimiter;

impl UserCodeAttemptLimiter for RegistrationOnlyLimiter {
    fn record_attempt(
        &self,
        _abuse_key: &[u8; 32],
        _now_unix_ms: u64,
        _window_ms: u64,
        _maximum_attempts: u32,
    ) -> Result<bool, DeviceAuthorizationRateLimitError> {
        Err(DeviceAuthorizationRateLimitError::Unavailable)
    }
}

struct SharedCsrValidator(Arc<dyn DeviceCsrValidator>);

impl DeviceCsrValidator for SharedCsrValidator {
    fn validate_and_hash_spki(
        &self,
        csr_der: &[u8],
    ) -> Result<[u8; 32], crate::device_authorization::DeviceAuthorizationPortError> {
        self.0.validate_and_hash_spki(csr_der)
    }
}

struct RegistrationExpectation {
    scope: DeviceAuthorizationScope,
    csr_der: Vec<u8>,
    csr_sha256: [u8; 32],
    spki_sha256: [u8; 32],
    registration_key_digest: crate::device_authorization::DeviceRegistrationKeyDigest,
}

impl RegistrationExpectation {
    fn from_request(request: &DeviceAuthorizationRegistrationRequest) -> Self {
        Self {
            scope: request.scope().clone(),
            csr_der: request.csr_der().to_vec(),
            csr_sha256: *request.csr_sha256(),
            spki_sha256: *request.spki_sha256(),
            registration_key_digest: request.registration_key_digest().clone(),
        }
    }
}

fn project_committed_start(
    started: DeviceAuthorizationRegisteredStart,
    expected: &RegistrationExpectation,
    verification_uri: String,
) -> Result<DeviceEnrollmentStartResult, DeviceEnrollmentHttpError> {
    let snapshot = &started.committed_snapshot;
    let record = &snapshot.record;
    let persisted_binding = &record.registration_binding;
    if !persisted_start_matches(record, &started.start, snapshot.disposition, expected) {
        return Err(DeviceEnrollmentHttpError::Unavailable);
    }

    let binding = crate::device_enrollment_http::DirectoryRegistrationBinding {
        binding_id: *persisted_binding.binding_id(),
        device_id: persisted_binding.key().device_id.clone(),
        scope: record.scope.clone(),
        authorization_generation: persisted_binding.authorization_generation(),
        csr_sha256: *persisted_binding.csr_sha256(),
        spki_sha256: *persisted_binding.spki_sha256(),
    };
    let committed_snapshot = DeviceEnrollmentAuthorizationSnapshot::from_start_snapshot(snapshot)?;
    let response = response_from_manager_start(
        started.start,
        &record.scope,
        record.csr_sha256,
        record.spki_sha256,
        verification_uri,
    )?;

    Ok(DeviceEnrollmentStartResult {
        binding,
        response,
        committed_snapshot,
    })
}

fn persisted_start_matches(
    record: &crate::device_authorization::DeviceAuthorizationRecord,
    start: &DeviceAuthorizationStart,
    disposition: DeviceAuthorizationStartDisposition,
    expected: &RegistrationExpectation,
) -> bool {
    let binding = &record.registration_binding;
    let binding_key = binding.key();
    let disposition_matches = match disposition {
        DeviceAuthorizationStartDisposition::Created => {
            record.revision == 0 && record.device_code_generation == 1
        }
        DeviceAuthorizationStartDisposition::Recovered => {
            record.revision > 0 && record.device_code_generation >= 2
        }
    };
    binding.binding_id().iter().any(|byte| *byte != 0)
        && (16..=128).contains(&binding_key.device_id.len())
        && binding.authorization_generation() > 0
        && record.registration_key_digest.as_ref() == Some(&expected.registration_key_digest)
        && record.scope == expected.scope
        && binding_key.organization_id == expected.scope.organization_id
        && binding_key.workspace_id == expected.scope.workspace_id
        && record.csr_der == expected.csr_der
        && record.csr_sha256 == expected.csr_sha256
        && record.spki_sha256 == expected.spki_sha256
        && binding.csr_sha256() == &expected.csr_sha256
        && binding.spki_sha256() == &expected.spki_sha256
        && start.authorization_id == record.id
        && start.device_id == binding_key.device_id
        && start.authorization_generation == binding.authorization_generation()
        && start.device_code_generation == record.device_code_generation
        && start.expires_at_unix_ms == record.expires_at_unix_ms
        && start.poll_interval_ms == record.poll_interval_ms
        && device_code_hash(&start.device_code).is_ok_and(|hash| hash == record.device_code_hash)
        && disposition_matches
}

fn response_from_manager_start(
    start: DeviceAuthorizationStart,
    scope: &DeviceAuthorizationScope,
    csr_sha256: [u8; 32],
    spki_sha256: [u8; 32],
    verification_uri: String,
) -> Result<StartDeviceAuthorizationResponse, DeviceEnrollmentHttpError> {
    let DeviceAuthorizationStart {
        authorization_id,
        device_id,
        authorization_generation,
        device_code_generation,
        device_code,
        user_code,
        expires_at_unix_ms,
        poll_interval_ms,
    } = start;
    let device_code = Zeroizing::new(device_code);
    let user_code = Zeroizing::new(user_code);
    let expires_at = format_unix_ms(expires_at_unix_ms)?;
    let interval_seconds = poll_interval_ms
        .saturating_add(999)
        .saturating_div(1_000)
        .max(1);
    let interval_seconds =
        u32::try_from(interval_seconds).map_err(|_| DeviceEnrollmentHttpError::Unavailable)?;
    if interval_seconds > 60 {
        return Err(DeviceEnrollmentHttpError::Unavailable);
    }

    Ok(StartDeviceAuthorizationResponse {
        authorization: DeviceAuthorizationReferenceWire {
            authorization_id: URL_SAFE_NO_PAD.encode(authorization_id),
            device_id,
            scope: DeviceScopeWire {
                organization_id: scope.organization_id.clone(),
                workspace_id: scope.workspace_id.clone(),
            },
            csr_spki_sha256: STANDARD.encode(spki_sha256),
            csr_sha256: STANDARD.encode(csr_sha256),
            expires_at: expires_at.clone(),
            authorization_generation,
        },
        codes: DeviceAuthorizationCodesWire {
            device_code: device_code.to_string(),
            user_code: user_code.to_string(),
            verification_uri,
            verification_uri_complete: None,
            interval_seconds,
            expires_at,
            device_code_generation,
        },
    })
}

fn parse_verification_uri(
    value: &str,
) -> Result<String, DeviceEnrollmentRegistrationPostgresError> {
    let uri =
        Url::parse(value).map_err(|_| DeviceEnrollmentRegistrationPostgresError::Configuration)?;
    if uri.scheme() != "https"
        || uri.host_str().is_none()
        || !uri.username().is_empty()
        || uri.password().is_some()
        || uri.query().is_some()
        || uri.fragment().is_some()
    {
        return Err(DeviceEnrollmentRegistrationPostgresError::Configuration);
    }
    Ok(uri.to_string())
}

fn map_manager_configuration_error(
    error: DeviceAuthorizationError,
) -> DeviceEnrollmentRegistrationPostgresError {
    match error {
        DeviceAuthorizationError::InvalidPolicy
        | DeviceAuthorizationError::UserCodeKeysUnavailable => {
            DeviceEnrollmentRegistrationPostgresError::Configuration
        }
        _ => DeviceEnrollmentRegistrationPostgresError::Unavailable,
    }
}

fn map_manager_error(error: DeviceAuthorizationError) -> DeviceEnrollmentHttpError {
    match error {
        DeviceAuthorizationError::InvalidRequest | DeviceAuthorizationError::InvalidCsr => {
            DeviceEnrollmentHttpError::InvalidRequest
        }
        DeviceAuthorizationError::ConcurrentTransition
        | DeviceAuthorizationError::AlreadyFinal
        | DeviceAuthorizationError::Pending => DeviceEnrollmentHttpError::Conflict,
        DeviceAuthorizationError::Expired | DeviceAuthorizationError::DeliveryExpired => {
            DeviceEnrollmentHttpError::Expired
        }
        DeviceAuthorizationError::FirstStartQuotaExceeded => {
            DeviceEnrollmentHttpError::FirstStartQuotaExceeded
        }
        // In this start-only manager, this result is the PostgreSQL store's
        // permanent maximum recovery count, not a waitable user-code throttle.
        DeviceAuthorizationError::TooManyAttempts => DeviceEnrollmentHttpError::Conflict,
        _ => DeviceEnrollmentHttpError::Unavailable,
    }
}

fn format_unix_ms(milliseconds: u64) -> Result<String, DeviceEnrollmentHttpError> {
    let seconds = milliseconds / 1_000;
    let days =
        i64::try_from(seconds / 86_400).map_err(|_| DeviceEnrollmentHttpError::Unavailable)?;
    let second_of_day = seconds % 86_400;
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
