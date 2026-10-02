//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 outbox.rs                                                       │
//! │  Module: cy_workspace_postgres_storage                             │
//! │  Role: Durable Authority invocation lifecycle and dispatch fencing.│
//! │                                                                     │
//! │  模块职责：持久化 Authority 调用生命周期并栅栏设备与会话授权。          │
//! └─────────────────────────────────────────────────────────────────────┘

use cy_proto::cyrene::workspace::authority::v2::{
    ApprovedInvocation, CanonicalInvocationEnvelope, CanonicalInvocationResultEnvelope,
    ExecutionCredential, ExecutionOutcomeStatus, IdempotencySemantics, InvocationState,
};
use cy_workspace_control_plane::authority::outbox::{
    AuthorityInvocationRecord, AuthorityOutboxError, AuthorityOutboxStore, EnqueueOutcome,
    ExecutionDevicePeer, InvocationWaitScope, NewAuthorityInvocation, PersistedInvocationResult,
    SubmitResultOutcome,
};
use prost::Message;
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::postgres::PgRow;
use sqlx::{PgPool, Row, Transaction};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::time::sleep;
use uuid::Uuid;

const OUTBOX_TABLE: &str = "cyrene_workspace_device_registry.workspace_invocation_outbox";
const SESSION_TABLE: &str = "cyrene_workspace_device_registry.authority_web_sessions";
const REGISTRY_TABLE: &str = "cyrene_workspace_device_registry.certificate_records";
const IDENTITY_TABLE: &str = "cyrene_workspace_directory.workspace_device_identities";
const TARGET_TABLE: &str = "cyrene_workspace_device_registry.authority_execution_target_bindings";
const CLAIM_LEASE_MS: u64 = 120_000;
const WAIT_POLL_INTERVAL: Duration = Duration::from_millis(50);
const OUTBOX_COLUMNS: &str = "invocation_id, workspace_id, idempotency_key, request_digest_sha256, target_component, resolved_target_url, raw_invocation_json, device_generation, session_generation, contract_activation_generation, status, claimed_by_connector_id, claimed_at_unix_ms, delivery_receipt, delivery_acknowledged_at_unix_ms, outcome_status, response_status_code, response_json_body, response_content_type, error_code, error_message, created_at_unix_ms, updated_at_unix_ms, expires_at_unix_ms, record_schema_version, organization_id, principal_issuer, principal_subject, operation_owner_id, operation_id, scope, resource_id, product_invocation_proto, approved_envelope_proto, execution_credential_proto, invocation_digest_sha256, idempotency_semantics, idempotency_scope_sha256, execution_device_id, certificate_fingerprint_sha256, authorization_id, session_id, execution_target_json, signing_key_id, target_binding_manifest_sha256, bundle_manifest_sha256, owner_source_commit, credential_issued_at_unix_ms, credential_expires_at_unix_ms, claim_credential_digest_sha256, claim_lease_expires_at_unix_ms, claim_count, result_digest_sha256, result_receipt, stable_idempotency_request_digest_sha256";

/// PostgreSQL-backed durable outbox for the Authority invocation lifecycle.
#[derive(Clone)]
pub struct PostgresWorkspaceOutbox {
    pool: PgPool,
}

impl PostgresWorkspaceOutbox {
    /// Creates a store with the runtime database pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Connects with the runtime device-registry database configuration.
    pub async fn connect_from_environment() -> Result<Self, AuthorityOutboxError> {
        let database_url = std::env::var("CYRENE_WORKSPACE_DEVICE_REGISTRY_DATABASE_URL")
            .or_else(|_| std::env::var("CYRENE_WORKSPACE_DIRECTORY_DATABASE_URL"))
            .map_err(|_| AuthorityOutboxError::Unavailable)?;
        let pool = PgPool::connect(&database_url)
            .await
            .map_err(|_| AuthorityOutboxError::Unavailable)?;
        Ok(Self::new(pool))
    }
}

#[async_trait::async_trait]
impl AuthorityOutboxStore for PostgresWorkspaceOutbox {
    async fn enqueue_authorized(
        &self,
        invocation: NewAuthorityInvocation,
    ) -> Result<EnqueueOutcome, AuthorityOutboxError> {
        let values = validate_new_invocation(&invocation)?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| AuthorityOutboxError::Unavailable)?;

        // Use the same lock order as claim/result paths. Registry mutations update these exact
        // rows, so a revoke either wins before approval or waits until the approval commits.
        lock_execution_device(
            &mut tx,
            &values.organization_id,
            &values.workspace_id,
            &values.execution_device_id,
            values.execution_device_generation,
            &values.execution_authorization_id,
            &values.certificate_fingerprint_sha256,
        )
        .await?;
        lock_target_binding_for_enqueue(&mut tx, &values).await?;
        lock_session(
            &mut tx,
            SessionLockFence {
                session_id: &values.session_id,
                organization_id: &values.organization_id,
                workspace_id: &values.workspace_id,
                issuer: &values.principal_issuer,
                subject: &values.principal_subject,
                generation: values.session_generation,
                now_ms: values.now_unix_ms,
            },
        )
        .await?;

        if let Some(existing) = find_idempotent_existing(&mut tx, &values).await? {
            if existing.stable_request_digest_sha256
                != values.stable_idempotency_request_digest_sha256
            {
                return Err(AuthorityOutboxError::IdempotencyConflict);
            }
            lock_record_authorizations(&mut tx, &existing.record, values.now_unix_ms).await?;
            tx.commit()
                .await
                .map_err(|_| AuthorityOutboxError::Unavailable)?;
            return Ok(EnqueueOutcome::Existing(existing.record));
        }

        let now =
            i64::try_from(values.now_unix_ms).map_err(|_| AuthorityOutboxError::InvalidRecord)?;
        let expires_at = i64::try_from(values.expires_at_unix_ms)
            .map_err(|_| AuthorityOutboxError::InvalidRecord)?;
        let idempotency_key = nonempty(&values.idempotency_key);
        let inserted = sqlx::query(&format!(
            r#"INSERT INTO {OUTBOX_TABLE} (
                invocation_id, workspace_id, idempotency_key, request_digest_sha256,
                target_component, resolved_target_url, raw_invocation_json,
                device_generation, session_generation, contract_activation_generation,
                status, created_at_unix_ms, updated_at_unix_ms, expires_at_unix_ms,
                record_schema_version, organization_id, principal_issuer,
                principal_subject, operation_owner_id, operation_id, scope,
                resource_id, product_invocation_proto, approved_envelope_proto,
                execution_credential_proto, invocation_digest_sha256,
                idempotency_semantics, idempotency_scope_sha256,
                execution_device_id, certificate_fingerprint_sha256,
                authorization_id, session_id, execution_target_json, signing_key_id,
                target_binding_manifest_sha256, bundle_manifest_sha256,
                owner_source_commit, credential_issued_at_unix_ms,
                credential_expires_at_unix_ms, claim_credential_digest_sha256,
                stable_idempotency_request_digest_sha256
            ) VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'PENDING', $11, $11,
                $12, 2, $13, $14, $15, $16, $17, $18, $19, $20, $21, $22,
                $23, $24, $25, $26, $27, $28, $29, $30, $31, $32, $33, $34,
                $35, $36, $37, $38
            )
            ON CONFLICT DO NOTHING
            RETURNING {OUTBOX_COLUMNS}"#
        ))
        .bind(&values.invocation_id)
        .bind(&values.workspace_id)
        .bind(idempotency_key)
        .bind(&values.request_digest_sha256)
        .bind(&values.target_component)
        .bind(&values.endpoint)
        .bind(&values.invocation_json_body)
        .bind(
            i64::try_from(values.execution_device_generation)
                .map_err(|_| AuthorityOutboxError::InvalidRecord)?,
        )
        .bind(
            i64::try_from(values.session_generation)
                .map_err(|_| AuthorityOutboxError::InvalidRecord)?,
        )
        .bind(
            i64::try_from(values.contract_activation_generation)
                .map_err(|_| AuthorityOutboxError::InvalidRecord)?,
        )
        .bind(now)
        .bind(expires_at)
        .bind(&values.organization_id)
        .bind(&values.principal_issuer)
        .bind(&values.principal_subject)
        .bind(&values.operation_owner_id)
        .bind(&values.operation_id)
        .bind(&values.scope)
        .bind(&values.resource_id)
        .bind(&values.product_invocation_proto)
        .bind(&values.canonical_envelope_proto)
        .bind(&values.execution_credential_proto)
        .bind(&values.invocation_digest_sha256)
        .bind(&values.idempotency_semantics)
        .bind(&values.idempotency_scope_sha256)
        .bind(&values.execution_device_id)
        .bind(hex(&values.certificate_fingerprint_sha256))
        .bind(values.execution_authorization_id.as_bytes().as_slice())
        .bind(values.session_id)
        .bind(&values.execution_target_json)
        .bind(&values.signing_key_id)
        .bind(&values.target_binding_manifest_sha256)
        .bind(&values.bundle_manifest_sha256)
        .bind(&values.owner_source_commit)
        .bind(
            i64::try_from(values.issued_at_unix_ms)
                .map_err(|_| AuthorityOutboxError::InvalidRecord)?,
        )
        .bind(expires_at)
        .bind(&values.credential_digest_sha256)
        .bind(&values.stable_idempotency_request_digest_sha256)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|_| AuthorityOutboxError::Unavailable)?;

        let Some(row) = inserted else {
            let existing = find_idempotent_existing(&mut tx, &values).await?;
            let existing = existing.ok_or(AuthorityOutboxError::IdempotencyConflict)?;
            if existing.stable_request_digest_sha256
                != values.stable_idempotency_request_digest_sha256
            {
                return Err(AuthorityOutboxError::IdempotencyConflict);
            }
            lock_record_authorizations(&mut tx, &existing.record, values.now_unix_ms).await?;
            tx.commit()
                .await
                .map_err(|_| AuthorityOutboxError::Unavailable)?;
            return Ok(EnqueueOutcome::Existing(existing.record));
        };

        let record = row_to_invocation_record(row)?;
        tx.commit()
            .await
            .map_err(|_| AuthorityOutboxError::Unavailable)?;
        Ok(EnqueueOutcome::Inserted(record))
    }

    async fn claim_for_execution_device(
        &self,
        peer: &ExecutionDevicePeer,
        max_batch_size: usize,
    ) -> Result<Vec<AuthorityInvocationRecord>, AuthorityOutboxError> {
        if max_batch_size == 0 || max_batch_size > 128 {
            return Err(AuthorityOutboxError::InvalidRecord);
        }
        let now = now_unix_ms();
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| AuthorityOutboxError::Unavailable)?;
        lock_peer_device(&mut tx, peer).await?;
        expire_pending_for_device(&mut tx, peer, now).await?;
        recover_expired_claims(&mut tx, peer, now).await?;

        // A retried Claim returns the same live lease and signed credential.
        let active_claims = sqlx::query(&format!(
            r#"SELECT {OUTBOX_COLUMNS} FROM {OUTBOX_TABLE}
            WHERE record_schema_version = 2
              AND organization_id = $1 AND workspace_id = $2
              AND execution_device_id = $3 AND device_generation = $4
              AND certificate_fingerprint_sha256 = $5
              AND status = 'CLAIMED' AND claimed_by_connector_id = $3
              AND claim_lease_expires_at_unix_ms > $6
            ORDER BY claimed_at_unix_ms, invocation_id
            LIMIT $7 FOR UPDATE SKIP LOCKED"#
        ))
        .bind(&peer.organization_id)
        .bind(&peer.workspace_id)
        .bind(&peer.device_id)
        .bind(
            i64::try_from(peer.authorization_generation)
                .map_err(|_| AuthorityOutboxError::InvalidRecord)?,
        )
        .bind(peer.certificate_fingerprint_sha256.as_slice())
        .bind(i64::try_from(now).map_err(|_| AuthorityOutboxError::InvalidRecord)?)
        .bind(i64::try_from(max_batch_size).map_err(|_| AuthorityOutboxError::InvalidRecord)?)
        .fetch_all(&mut *tx)
        .await
        .map_err(|_| AuthorityOutboxError::Unavailable)?;
        if !active_claims.is_empty() {
            let records = active_claims
                .into_iter()
                .map(row_to_invocation_record)
                .collect::<Result<Vec<_>, _>>()?;
            for record in &records {
                lock_record_session(&mut tx, record, now).await?;
                lock_target_binding_for_record(&mut tx, record).await?;
            }
            tx.commit()
                .await
                .map_err(|_| AuthorityOutboxError::Unavailable)?;
            return Ok(records);
        }

        let rows = sqlx::query(&format!(
            r#"SELECT {OUTBOX_COLUMNS}
            FROM {OUTBOX_TABLE} AS invocation
            WHERE invocation.record_schema_version = 2
              AND invocation.organization_id = $1
              AND invocation.workspace_id = $2
              AND invocation.execution_device_id = $3
              AND invocation.device_generation = $4
              AND invocation.certificate_fingerprint_sha256 = $6
              AND invocation.status = 'PENDING'
              AND invocation.expires_at_unix_ms > $5
              AND EXISTS (
                  SELECT 1 FROM {SESSION_TABLE} AS session
                  WHERE session.session_id = invocation.session_id
                    AND session.session_generation = invocation.session_generation
                    AND session.revoked_at_unix_ms IS NULL
                    AND session.expires_at_unix_ms > $5
              )
            ORDER BY invocation.created_at_unix_ms, invocation.invocation_id
            LIMIT $7
            FOR UPDATE OF invocation SKIP LOCKED"#
        ))
        .bind(&peer.organization_id)
        .bind(&peer.workspace_id)
        .bind(&peer.device_id)
        .bind(
            i64::try_from(peer.authorization_generation)
                .map_err(|_| AuthorityOutboxError::InvalidRecord)?,
        )
        .bind(i64::try_from(now).map_err(|_| AuthorityOutboxError::InvalidRecord)?)
        .bind(peer.certificate_fingerprint_sha256.as_slice())
        .bind(i64::try_from(max_batch_size).map_err(|_| AuthorityOutboxError::InvalidRecord)?)
        .fetch_all(&mut *tx)
        .await
        .map_err(|_| AuthorityOutboxError::Unavailable)?;

        let mut claimed = Vec::with_capacity(rows.len());
        for row in rows {
            let record = row_to_invocation_record(row)?;
            lock_target_binding_for_record(&mut tx, &record).await?;
            let approved = record
                .approved_invocation
                .canonical_envelope
                .as_ref()
                .ok_or(AuthorityOutboxError::InvalidRecord)?;
            let session_id = parse_uuid(&approved.session_id)?;
            lock_session(
                &mut tx,
                SessionLockFence {
                    session_id: &session_id,
                    organization_id: &approved.organization_id,
                    workspace_id: &approved.workspace_id,
                    issuer: &approved.principal_issuer,
                    subject: &approved.principal_subject,
                    generation: approved.session_generation,
                    now_ms: now,
                },
            )
            .await?;
            let expires_at = credential_expiry_ms(
                record
                    .approved_invocation
                    .credential
                    .as_ref()
                    .ok_or(AuthorityOutboxError::InvalidRecord)?,
            )?;
            let lease_until = now.saturating_add(CLAIM_LEASE_MS).min(expires_at);
            let updated = sqlx::query(&format!(
                r#"UPDATE {OUTBOX_TABLE}
                SET status = 'CLAIMED', claimed_by_connector_id = $1,
                    claimed_at_unix_ms = $2, claim_lease_expires_at_unix_ms = $3,
                    claim_count = claim_count + 1, updated_at_unix_ms = $2
                WHERE invocation_id = $4 AND status = 'PENDING'
                RETURNING {OUTBOX_COLUMNS}"#
            ))
            .bind(&peer.device_id)
            .bind(i64::try_from(now).map_err(|_| AuthorityOutboxError::InvalidRecord)?)
            .bind(i64::try_from(lease_until).map_err(|_| AuthorityOutboxError::InvalidRecord)?)
            .bind(&record.approved_invocation.invocation_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| AuthorityOutboxError::Unavailable)?;
            if let Some(updated) = updated {
                claimed.push(row_to_invocation_record(updated)?);
            }
        }
        tx.commit()
            .await
            .map_err(|_| AuthorityOutboxError::Unavailable)?;
        Ok(claimed)
    }

    async fn validate_execution_credential(
        &self,
        peer: &ExecutionDevicePeer,
        credential: &ExecutionCredential,
    ) -> Result<AuthorityInvocationRecord, AuthorityOutboxError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| AuthorityOutboxError::Unavailable)?;
        lock_peer_device(&mut tx, peer).await?;
        let row = sqlx::query(&format!(
            "SELECT {OUTBOX_COLUMNS} FROM {OUTBOX_TABLE} WHERE invocation_id = $1 FOR UPDATE"
        ))
        .bind(&credential.invocation_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|_| AuthorityOutboxError::Unavailable)?
        .ok_or(AuthorityOutboxError::NotFound)?;
        let record = row_to_invocation_record(row)?;
        ensure_peer_matches_record(peer, &record)?;
        ensure_credential_matches(credential, &record)?;
        lock_target_binding_for_record(&mut tx, &record).await?;
        if record.state != InvocationState::Claimed {
            return Err(AuthorityOutboxError::InvalidState);
        }
        ensure_active_claim(&mut tx, &credential.invocation_id, now_unix_ms()).await?;
        let envelope = record
            .approved_invocation
            .canonical_envelope
            .as_ref()
            .ok_or(AuthorityOutboxError::InvalidRecord)?;
        let session_id = parse_uuid(&envelope.session_id)?;
        lock_session(
            &mut tx,
            SessionLockFence {
                session_id: &session_id,
                organization_id: &envelope.organization_id,
                workspace_id: &envelope.workspace_id,
                issuer: &envelope.principal_issuer,
                subject: &envelope.principal_subject,
                generation: envelope.session_generation,
                now_ms: now_unix_ms(),
            },
        )
        .await?;
        tx.commit()
            .await
            .map_err(|_| AuthorityOutboxError::Unavailable)?;
        Ok(record)
    }

    async fn acknowledge_delivery(
        &self,
        peer: &ExecutionDevicePeer,
        invocation_id: &str,
        credential: &ExecutionCredential,
        delivery_receipt: &[u8],
    ) -> Result<bool, AuthorityOutboxError> {
        if delivery_receipt.is_empty() || delivery_receipt.len() > 4096 {
            return Err(AuthorityOutboxError::InvalidRecord);
        }
        let now = now_unix_ms();
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| AuthorityOutboxError::Unavailable)?;
        lock_peer_device(&mut tx, peer).await?;
        let row = sqlx::query(&format!(
            "SELECT {OUTBOX_COLUMNS} FROM {OUTBOX_TABLE} WHERE invocation_id = $1 FOR UPDATE"
        ))
        .bind(invocation_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|_| AuthorityOutboxError::Unavailable)?
        .ok_or(AuthorityOutboxError::NotFound)?;
        let record = row_to_invocation_record(row)?;
        ensure_peer_matches_record(peer, &record)?;
        ensure_credential_matches(credential, &record)?;
        lock_target_binding_for_record(&mut tx, &record).await?;
        let envelope = record
            .approved_invocation
            .canonical_envelope
            .as_ref()
            .ok_or(AuthorityOutboxError::InvalidRecord)?;
        let session_id = parse_uuid(&envelope.session_id)?;
        lock_session(
            &mut tx,
            SessionLockFence {
                session_id: &session_id,
                organization_id: &envelope.organization_id,
                workspace_id: &envelope.workspace_id,
                issuer: &envelope.principal_issuer,
                subject: &envelope.principal_subject,
                generation: envelope.session_generation,
                now_ms: now,
            },
        )
        .await?;

        if record.state == InvocationState::Acknowledged
            || matches!(
                record.state,
                InvocationState::Succeeded
                    | InvocationState::Failed
                    | InvocationState::UnknownResult
            )
        {
            let previous: Option<Vec<u8>> = sqlx::query_scalar(&format!(
                "SELECT delivery_receipt FROM {OUTBOX_TABLE} WHERE invocation_id = $1"
            ))
            .bind(invocation_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|_| AuthorityOutboxError::Unavailable)?;
            tx.commit()
                .await
                .map_err(|_| AuthorityOutboxError::Unavailable)?;
            return Ok(previous.as_deref() == Some(delivery_receipt));
        }
        if record.state != InvocationState::Claimed {
            return Err(AuthorityOutboxError::InvalidState);
        }
        ensure_active_claim(&mut tx, invocation_id, now).await?;
        let updated = sqlx::query(&format!(
            r#"UPDATE {OUTBOX_TABLE}
            SET status = 'ACKNOWLEDGED', delivery_receipt = $1,
                delivery_acknowledged_at_unix_ms = $2, updated_at_unix_ms = $2
            WHERE invocation_id = $3 AND status = 'CLAIMED'
              AND claimed_by_connector_id = $4"#
        ))
        .bind(delivery_receipt)
        .bind(i64::try_from(now).map_err(|_| AuthorityOutboxError::InvalidRecord)?)
        .bind(invocation_id)
        .bind(&peer.device_id)
        .execute(&mut *tx)
        .await
        .map_err(|_| AuthorityOutboxError::Unavailable)?;
        if updated.rows_affected() != 1 {
            return Err(AuthorityOutboxError::InvalidState);
        }
        tx.commit()
            .await
            .map_err(|_| AuthorityOutboxError::Unavailable)?;
        Ok(true)
    }

    async fn submit_result(
        &self,
        peer: &ExecutionDevicePeer,
        result: PersistedInvocationResult,
    ) -> Result<SubmitResultOutcome, AuthorityOutboxError> {
        if result.delivery_receipt.is_empty()
            || !result.result_receipt.is_empty()
            || result.error_message.len() > 2048
            || result.error_code.len() > 128
        {
            return Err(AuthorityOutboxError::InvalidRecord);
        }
        let expected_result_digest = digest_result(&result);
        if result.result_digest_sha256.as_slice() != expected_result_digest.as_slice() {
            return Err(AuthorityOutboxError::InvalidRecord);
        }
        let now = now_unix_ms();
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| AuthorityOutboxError::Unavailable)?;
        lock_peer_device(&mut tx, peer).await?;
        let row = sqlx::query(&format!(
            "SELECT {OUTBOX_COLUMNS} FROM {OUTBOX_TABLE} WHERE invocation_id = $1 FOR UPDATE"
        ))
        .bind(&result.invocation_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|_| AuthorityOutboxError::Unavailable)?
        .ok_or(AuthorityOutboxError::NotFound)?;
        let record = row_to_invocation_record(row)?;
        ensure_peer_matches_record(peer, &record)?;
        ensure_credential_matches(&result.credential, &record)?;
        lock_target_binding_for_record(&mut tx, &record).await?;
        let envelope = record
            .approved_invocation
            .canonical_envelope
            .as_ref()
            .ok_or(AuthorityOutboxError::InvalidRecord)?;
        let session_id = parse_uuid(&envelope.session_id)?;
        lock_session(
            &mut tx,
            SessionLockFence {
                session_id: &session_id,
                organization_id: &envelope.organization_id,
                workspace_id: &envelope.workspace_id,
                issuer: &envelope.principal_issuer,
                subject: &envelope.principal_subject,
                generation: envelope.session_generation,
                now_ms: now,
            },
        )
        .await?;
        let mut response_bytes = None;
        let mut response_status = None;
        let mut response_content_type = None;
        if let Some(response) = result.product_response.as_ref() {
            response_bytes = Some(response.json_body.as_slice());
            response_status = Some(
                i32::try_from(response.status_code)
                    .map_err(|_| AuthorityOutboxError::InvalidRecord)?,
            );
            response_content_type = nonempty(&response.content_type);
        }
        let (result_state, result_status) = match result.outcome_status {
            ExecutionOutcomeStatus::Success => ("SUCCEEDED", "SUCCESS"),
            ExecutionOutcomeStatus::Failed => ("FAILED", "FAILED"),
            ExecutionOutcomeStatus::UnknownResult => ("UNKNOWN_RESULT", "UNKNOWN_RESULT"),
            ExecutionOutcomeStatus::Unspecified => return Err(AuthorityOutboxError::InvalidRecord),
        };
        if matches!(
            record.state,
            InvocationState::Succeeded | InvocationState::Failed | InvocationState::UnknownResult
        ) {
            let stored_digest: Option<String> = sqlx::query_scalar(&format!(
                "SELECT result_digest_sha256 FROM {OUTBOX_TABLE} WHERE invocation_id = $1"
            ))
            .bind(&result.invocation_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|_| AuthorityOutboxError::Unavailable)?;
            let receipt: Option<Vec<u8>> = sqlx::query_scalar(&format!(
                "SELECT result_receipt FROM {OUTBOX_TABLE} WHERE invocation_id = $1"
            ))
            .bind(&result.invocation_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|_| AuthorityOutboxError::Unavailable)?;
            if stored_digest.as_deref() != Some(&hex(&expected_result_digest))
                || receipt.as_deref().is_none()
            {
                return Err(AuthorityOutboxError::InvalidState);
            }
            let receipt = receipt.unwrap_or_default();
            tx.commit()
                .await
                .map_err(|_| AuthorityOutboxError::Unavailable)?;
            return Ok(SubmitResultOutcome {
                accepted: true,
                state: record.state,
                result_receipt: receipt,
            });
        }
        if record.state != InvocationState::Acknowledged {
            return Err(AuthorityOutboxError::InvalidState);
        }
        let stored_delivery_receipt: Option<Vec<u8>> = sqlx::query_scalar(&format!(
            "SELECT delivery_receipt FROM {OUTBOX_TABLE} WHERE invocation_id = $1"
        ))
        .bind(&result.invocation_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|_| AuthorityOutboxError::Unavailable)?;
        if stored_delivery_receipt.as_deref() != Some(result.delivery_receipt.as_slice()) {
            return Err(AuthorityOutboxError::InvalidRecord);
        }
        let result_receipt = Uuid::new_v4().as_bytes().to_vec();
        let updated = sqlx::query(&format!(
            r#"UPDATE {OUTBOX_TABLE}
            SET status = $1, outcome_status = $2, response_status_code = $3,
                response_json_body = $4, response_content_type = $5,
                error_code = $6, error_message = $7, result_digest_sha256 = $8,
                result_receipt = $9, updated_at_unix_ms = $10
            WHERE invocation_id = $11 AND status = 'ACKNOWLEDGED'
              AND delivery_receipt = $12"#
        ))
        .bind(result_state)
        .bind(result_status)
        .bind(response_status)
        .bind(response_bytes)
        .bind(response_content_type)
        .bind(nonempty(&result.error_code))
        .bind(nonempty(&result.error_message))
        .bind(hex(&expected_result_digest))
        .bind(&result_receipt)
        .bind(i64::try_from(now).map_err(|_| AuthorityOutboxError::InvalidRecord)?)
        .bind(&result.invocation_id)
        .bind(&result.delivery_receipt)
        .execute(&mut *tx)
        .await
        .map_err(|_| AuthorityOutboxError::Unavailable)?;
        if updated.rows_affected() != 1 {
            return Err(AuthorityOutboxError::InvalidState);
        }
        tx.commit()
            .await
            .map_err(|_| AuthorityOutboxError::Unavailable)?;
        Ok(SubmitResultOutcome {
            accepted: true,
            state: state_from_str(result_state)?,
            result_receipt,
        })
    }

    async fn wait_for_result(
        &self,
        invocation_id: &str,
        scope: &InvocationWaitScope,
        timeout: Duration,
    ) -> Result<Option<AuthorityInvocationRecord>, AuthorityOutboxError> {
        let deadline = tokio::time::Instant::now() + timeout;
        let now = i64::try_from(now_unix_ms()).map_err(|_| AuthorityOutboxError::InvalidRecord)?;
        sqlx::query(&format!(
            r#"UPDATE {OUTBOX_TABLE}
            SET status = 'FAILED', outcome_status = 'FAILED',
                error_code = 'APPROVAL_EXPIRED', updated_at_unix_ms = $2
            WHERE invocation_id = $1 AND record_schema_version = 2
              AND status = 'PENDING' AND expires_at_unix_ms <= $2"#
        ))
        .bind(invocation_id)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(|_| AuthorityOutboxError::Unavailable)?;
        loop {
            let row = sqlx::query(&format!(
                r#"SELECT {OUTBOX_COLUMNS}
                FROM {OUTBOX_TABLE} AS invocation
                WHERE invocation.invocation_id = $1
                  AND invocation.organization_id = $2
                  AND invocation.workspace_id = $3
                  AND invocation.principal_issuer = $4
                  AND invocation.principal_subject = $5
                  AND invocation.session_id = $6
                  AND invocation.session_generation = $7
                  AND EXISTS (
                      SELECT 1 FROM {SESSION_TABLE} AS session
                      WHERE session.session_id = invocation.session_id
                        AND session.session_generation = invocation.session_generation
                        AND session.revoked_at_unix_ms IS NULL
                        AND session.expires_at_unix_ms > $8
                  )"#
            ))
            .bind(invocation_id)
            .bind(&scope.organization_id)
            .bind(&scope.workspace_id)
            .bind(&scope.principal_issuer)
            .bind(&scope.principal_subject)
            .bind(&scope.session_id)
            .bind(
                i64::try_from(scope.session_generation)
                    .map_err(|_| AuthorityOutboxError::InvalidRecord)?,
            )
            .bind(i64::try_from(now_unix_ms()).map_err(|_| AuthorityOutboxError::InvalidRecord)?)
            .fetch_optional(&self.pool)
            .await
            .map_err(|_| AuthorityOutboxError::Unavailable)?;
            if let Some(row) = row {
                let record = row_to_invocation_record(row)?;
                let envelope = record
                    .approved_invocation
                    .canonical_envelope
                    .as_ref()
                    .ok_or(AuthorityOutboxError::InvalidRecord)?;
                if envelope.session_generation != scope.session_generation {
                    return Err(AuthorityOutboxError::NotFound);
                }
                if matches!(
                    record.state,
                    InvocationState::Succeeded
                        | InvocationState::Failed
                        | InvocationState::UnknownResult
                ) {
                    return Ok(Some(record));
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return Ok(None);
            }
            sleep(
                WAIT_POLL_INTERVAL
                    .min(deadline.saturating_duration_since(tokio::time::Instant::now())),
            )
            .await;
        }
    }
}

struct ValidatedInvocation {
    invocation_id: String,
    organization_id: String,
    workspace_id: String,
    principal_issuer: String,
    principal_subject: String,
    operation_owner_id: String,
    operation_id: String,
    scope: String,
    resource_id: String,
    target_component: String,
    endpoint: String,
    idempotency_key: String,
    idempotency_semantics: String,
    idempotency_scope_sha256: String,
    execution_device_id: String,
    execution_device_generation: u64,
    execution_authorization_id: Uuid,
    certificate_fingerprint_sha256: Vec<u8>,
    session_id: Uuid,
    session_generation: u64,
    contract_activation_generation: u64,
    signing_key_id: String,
    issued_at_unix_ms: u64,
    expires_at_unix_ms: u64,
    now_unix_ms: u64,
    invocation_digest_sha256: String,
    request_digest_sha256: String,
    credential_digest_sha256: String,
    stable_idempotency_request_digest_sha256: String,
    product_invocation_proto: Vec<u8>,
    invocation_json_body: Vec<u8>,
    canonical_envelope_proto: Vec<u8>,
    execution_credential_proto: Vec<u8>,
    execution_target_json: serde_json::Value,
    target_binding_manifest_sha256: String,
    bundle_manifest_sha256: String,
    owner_source_commit: String,
}

fn validate_new_invocation(
    new: &NewAuthorityInvocation,
) -> Result<ValidatedInvocation, AuthorityOutboxError> {
    let approved = &new.approved_invocation;
    let envelope = approved
        .canonical_envelope
        .as_ref()
        .ok_or(AuthorityOutboxError::InvalidRecord)?;
    let credential = approved
        .credential
        .as_ref()
        .ok_or(AuthorityOutboxError::InvalidRecord)?;
    let invocation = envelope
        .invocation
        .as_ref()
        .ok_or(AuthorityOutboxError::InvalidRecord)?;
    let target = envelope
        .target
        .as_ref()
        .ok_or(AuthorityOutboxError::InvalidRecord)?;
    if envelope.encode_to_vec() != new.canonical_envelope_proto
        || approved.invocation_id != envelope.invocation_id
        || credential.invocation_id != envelope.invocation_id
        || invocation.owner_id.trim().is_empty()
        || invocation.operation_id.trim().is_empty()
        || target.target_component.trim().is_empty()
        || target.execution_device_id.trim().is_empty()
        || target.endpoint.trim().is_empty()
        || !matches!(
            target.http_method.as_str(),
            "GET" | "POST" | "PUT" | "PATCH" | "DELETE"
        )
        || target.execution_device_generation == 0
        || target.execution_authorization_id.len() != 16
        || target.execution_device_certificate_sha256.len() != 32
        || envelope.session_generation == 0
        || envelope.contract_activation_generation == 0
        || credential.authority_signature.is_empty()
        || credential.signing_key_id.trim().is_empty()
        || credential.invocation_id != envelope.invocation_id
        || credential.organization_id != envelope.organization_id
        || credential.workspace_id != envelope.workspace_id
        || credential.principal_issuer != envelope.principal_issuer
        || credential.principal_subject != envelope.principal_subject
        || credential.operation_owner_id != invocation.owner_id
        || credential.operation_id != invocation.operation_id
        || credential.scope != target.scope
        || credential.resource_id != target.resource_id
        || credential.target_component != target.target_component
        || credential.http_method != target.http_method
        || credential.endpoint != target.endpoint
        || credential.execution_device_id != target.execution_device_id
        || credential.execution_device_generation != target.execution_device_generation
        || credential.execution_authorization_id != target.execution_authorization_id
        || credential.execution_device_certificate_sha256
            != target.execution_device_certificate_sha256
        || credential.session_id != envelope.session_id
        || credential.session_generation != envelope.session_generation
        || credential.contract_activation_generation != envelope.contract_activation_generation
        || credential.idempotency_key != target.idempotency_key
        || credential.idempotency_semantics != target.idempotency_semantics
    {
        return Err(AuthorityOutboxError::InvalidRecord);
    }
    let now = now_unix_ms();
    let issued_at = timestamp_to_unix_ms(envelope.issued_at.as_ref())?;
    let expires_at = timestamp_to_unix_ms(envelope.expires_at.as_ref())?;
    if issued_at > now.saturating_add(30_000) || expires_at <= now || expires_at <= issued_at {
        return Err(AuthorityOutboxError::InvalidRecord);
    }
    if credential.issued_at.as_ref() != envelope.issued_at.as_ref()
        || credential.expires_at.as_ref() != envelope.expires_at.as_ref()
    {
        return Err(AuthorityOutboxError::InvalidRecord);
    }

    let envelope_bytes = envelope.encode_to_vec();
    let request_digest = Sha256::digest(&envelope_bytes).to_vec();
    let invocation_bytes = invocation.encode_to_vec();
    let invocation_digest = Sha256::digest(&invocation_bytes).to_vec();
    if credential.request_digest_sha256 != request_digest
        || credential.invocation_digest_sha256 != invocation_digest
    {
        return Err(AuthorityOutboxError::InvalidRecord);
    }
    let device_authorization_id = Uuid::from_slice(&envelope.execution_authorization_id)
        .map_err(|_| AuthorityOutboxError::InvalidRecord)?;
    let session_id =
        Uuid::parse_str(&envelope.session_id).map_err(|_| AuthorityOutboxError::InvalidRecord)?;
    let semantics = IdempotencySemantics::try_from(target.idempotency_semantics)
        .map_err(|_| AuthorityOutboxError::InvalidRecord)?;
    if semantics == IdempotencySemantics::Unspecified
        || (semantics == IdempotencySemantics::Required && target.idempotency_key.is_empty())
    {
        return Err(AuthorityOutboxError::InvalidRecord);
    }
    let idempotency_scope_sha256 = hex(&Sha256::digest(canonical_idempotency_scope(&[
        &envelope.organization_id,
        &envelope.workspace_id,
        &envelope.principal_issuer,
        &envelope.principal_subject,
        &invocation.owner_id,
        &invocation.operation_id,
        &target.scope,
        &target.resource_id,
    ])));
    let stable_idempotency_request_digest_sha256 =
        stable_idempotency_request_digest(&invocation_bytes, &idempotency_scope_sha256)?;
    let execution_target_json = json!({
        "target_component": target.target_component,
        "http_method": target.http_method,
        "endpoint": target.endpoint,
        "resource_id": target.resource_id,
        "scope": target.scope,
        "idempotency_key": target.idempotency_key,
        "idempotency_semantics": semantics.as_str_name(),
        "execution_device_id": target.execution_device_id,
        "execution_device_generation": target.execution_device_generation,
        "execution_authorization_id": hex(&target.execution_authorization_id),
        "execution_device_certificate_sha256": hex(&target.execution_device_certificate_sha256),
    });
    Ok(ValidatedInvocation {
        invocation_id: envelope.invocation_id.clone(),
        organization_id: envelope.organization_id.clone(),
        workspace_id: envelope.workspace_id.clone(),
        principal_issuer: envelope.principal_issuer.clone(),
        principal_subject: envelope.principal_subject.clone(),
        operation_owner_id: invocation.owner_id.clone(),
        operation_id: invocation.operation_id.clone(),
        scope: target.scope.clone(),
        resource_id: target.resource_id.clone(),
        target_component: target.target_component.clone(),
        endpoint: target.endpoint.clone(),
        idempotency_key: target.idempotency_key.clone(),
        idempotency_semantics: semantics.as_str_name().to_owned(),
        idempotency_scope_sha256,
        execution_device_id: target.execution_device_id.clone(),
        execution_device_generation: target.execution_device_generation,
        execution_authorization_id: device_authorization_id,
        certificate_fingerprint_sha256: target.execution_device_certificate_sha256.clone(),
        session_id,
        session_generation: envelope.session_generation,
        contract_activation_generation: envelope.contract_activation_generation,
        signing_key_id: credential.signing_key_id.clone(),
        issued_at_unix_ms: issued_at,
        expires_at_unix_ms: expires_at,
        now_unix_ms: now,
        invocation_digest_sha256: hex(&invocation_digest),
        request_digest_sha256: hex(&request_digest),
        credential_digest_sha256: hex(&Sha256::digest(credential.encode_to_vec())),
        stable_idempotency_request_digest_sha256,
        product_invocation_proto: invocation_bytes,
        invocation_json_body: invocation.json_body.clone(),
        canonical_envelope_proto: envelope_bytes,
        execution_credential_proto: credential.encode_to_vec(),
        execution_target_json,
        target_binding_manifest_sha256: hex(&new.target_binding_manifest_sha256),
        bundle_manifest_sha256: hex(&new.bundle_manifest_sha256),
        owner_source_commit: new.owner_source_commit.clone(),
    })
}

async fn find_idempotent_existing(
    tx: &mut Transaction<'_, sqlx::Postgres>,
    values: &ValidatedInvocation,
) -> Result<Option<ExistingInvocation>, AuthorityOutboxError> {
    let key = nonempty(&values.idempotency_key);
    let row = if let Some(key) = key {
        sqlx::query(&format!(
            "SELECT {OUTBOX_COLUMNS} FROM {OUTBOX_TABLE} WHERE record_schema_version = 2 AND organization_id = $1 AND workspace_id = $2 AND idempotency_scope_sha256 = $3 AND idempotency_key = $4 FOR UPDATE"
        ))
        .bind(&values.organization_id)
        .bind(&values.workspace_id)
        .bind(&values.idempotency_scope_sha256)
        .bind(key)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|_| AuthorityOutboxError::Unavailable)?
    } else {
        None
    };
    let row = match row {
        Some(row) => Some(row),
        None => sqlx::query(&format!(
            "SELECT {OUTBOX_COLUMNS} FROM {OUTBOX_TABLE} WHERE invocation_id = $1 FOR UPDATE"
        ))
        .bind(&values.invocation_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|_| AuthorityOutboxError::Unavailable)?,
    };
    row.map(|row| {
        let stable_request_digest_sha256: String = row
            .try_get("stable_idempotency_request_digest_sha256")
            .map_err(|_| AuthorityOutboxError::InvalidRecord)?;
        let record = row_to_invocation_record(row)?;
        Ok(ExistingInvocation {
            stable_request_digest_sha256,
            record,
        })
    })
    .transpose()
}

struct ExistingInvocation {
    stable_request_digest_sha256: String,
    record: AuthorityInvocationRecord,
}

async fn lock_peer_device(
    tx: &mut Transaction<'_, sqlx::Postgres>,
    peer: &ExecutionDevicePeer,
) -> Result<(), AuthorityOutboxError> {
    lock_execution_device(
        tx,
        &peer.organization_id,
        &peer.workspace_id,
        &peer.device_id,
        peer.authorization_generation,
        &peer.authorization_id,
        &peer.certificate_fingerprint_sha256,
    )
    .await
}

async fn lock_record_authorizations(
    tx: &mut Transaction<'_, sqlx::Postgres>,
    record: &AuthorityInvocationRecord,
    now_unix_ms: u64,
) -> Result<(), AuthorityOutboxError> {
    let envelope = record
        .approved_invocation
        .canonical_envelope
        .as_ref()
        .ok_or(AuthorityOutboxError::InvalidRecord)?;
    let execution_authorization_id = Uuid::from_slice(&envelope.execution_authorization_id)
        .map_err(|_| AuthorityOutboxError::InvalidRecord)?;
    lock_execution_device(
        tx,
        &envelope.organization_id,
        &envelope.workspace_id,
        &envelope.execution_device_id,
        envelope.execution_device_generation,
        &execution_authorization_id,
        &envelope.execution_device_certificate_sha256,
    )
    .await?;
    lock_target_binding_for_record(tx, record).await?;
    lock_record_session(tx, record, now_unix_ms).await
}

async fn lock_execution_device(
    tx: &mut Transaction<'_, sqlx::Postgres>,
    organization_id: &str,
    workspace_id: &str,
    device_id: &str,
    generation: u64,
    authorization_id: &Uuid,
    fingerprint: &[u8],
) -> Result<(), AuthorityOutboxError> {
    let row = sqlx::query(&format!(
        r#"SELECT identity.current_authorization_generation
        FROM {IDENTITY_TABLE} AS identity
        JOIN {REGISTRY_TABLE} AS certificate
          ON certificate.organization_id = identity.organization_id
         AND certificate.workspace_id = identity.workspace_id
         AND certificate.device_id = identity.device_id
         AND certificate.authorization_generation = identity.current_authorization_generation
        WHERE identity.organization_id = $1
          AND identity.workspace_id = $2
          AND identity.device_id = $3
          AND identity.current_authorization_generation = $4
          AND certificate.authorization_id = $5
          AND certificate.certificate_sha256 = $6
          AND certificate.state = 'active'
        FOR SHARE OF identity, certificate"#
    ))
    .bind(organization_id)
    .bind(workspace_id)
    .bind(device_id)
    .bind(i64::try_from(generation).map_err(|_| AuthorityOutboxError::InvalidRecord)?)
    .bind(authorization_id.as_bytes().as_slice())
    .bind(fingerprint)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|_| AuthorityOutboxError::Unavailable)?;
    if row.is_none() {
        return Err(AuthorityOutboxError::AuthorizationRevoked);
    }
    Ok(())
}

struct SessionLockFence<'a> {
    session_id: &'a Uuid,
    organization_id: &'a str,
    workspace_id: &'a str,
    issuer: &'a str,
    subject: &'a str,
    generation: u64,
    now_ms: u64,
}

async fn lock_session(
    tx: &mut Transaction<'_, sqlx::Postgres>,
    fence: SessionLockFence<'_>,
) -> Result<(), AuthorityOutboxError> {
    let row = sqlx::query(&format!(
        r#"SELECT session_id FROM {SESSION_TABLE}
        WHERE session_id = $1 AND organization_id = $2 AND workspace_id = $3
          AND principal_issuer = $4 AND principal_subject = $5
          AND session_generation = $6 AND revoked_at_unix_ms IS NULL
          AND expires_at_unix_ms > $7
        FOR SHARE"#
    ))
    .bind(fence.session_id)
    .bind(fence.organization_id)
    .bind(fence.workspace_id)
    .bind(fence.issuer)
    .bind(fence.subject)
    .bind(i64::try_from(fence.generation).map_err(|_| AuthorityOutboxError::InvalidRecord)?)
    .bind(i64::try_from(fence.now_ms).map_err(|_| AuthorityOutboxError::InvalidRecord)?)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|_| AuthorityOutboxError::Unavailable)?;
    if row.is_none() {
        return Err(AuthorityOutboxError::AuthorizationRevoked);
    }
    Ok(())
}

async fn lock_target_binding_for_enqueue(
    tx: &mut Transaction<'_, sqlx::Postgres>,
    values: &ValidatedInvocation,
) -> Result<(), AuthorityOutboxError> {
    let row = sqlx::query(&format!(
        r#"SELECT target_component, execution_device_id, execution_device_generation,
                  execution_authorization_id, certificate_fingerprint_sha256,
                  target_binding_manifest_sha256, bundle_manifest_sha256,
                  contract_activation_generation, owner_source_commit, endpoint_base
           FROM {TARGET_TABLE}
           WHERE organization_id = $1 AND workspace_id = $2
             AND operation_owner_id = $3 AND operation_id = $4 AND active = TRUE
           FOR SHARE"#
    ))
    .bind(&values.organization_id)
    .bind(&values.workspace_id)
    .bind(&values.operation_owner_id)
    .bind(&values.operation_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|_| AuthorityOutboxError::Unavailable)?
    .ok_or(AuthorityOutboxError::AuthorizationRevoked)?;

    let target_component: String = row
        .try_get("target_component")
        .map_err(|_| AuthorityOutboxError::InvalidRecord)?;
    let execution_device_id: String = row
        .try_get("execution_device_id")
        .map_err(|_| AuthorityOutboxError::InvalidRecord)?;
    let execution_device_generation: i64 = row
        .try_get("execution_device_generation")
        .map_err(|_| AuthorityOutboxError::InvalidRecord)?;
    let execution_authorization_id: Vec<u8> = row
        .try_get("execution_authorization_id")
        .map_err(|_| AuthorityOutboxError::InvalidRecord)?;
    let fingerprint: Vec<u8> = row
        .try_get("certificate_fingerprint_sha256")
        .map_err(|_| AuthorityOutboxError::InvalidRecord)?;
    let target_manifest: Vec<u8> = row
        .try_get("target_binding_manifest_sha256")
        .map_err(|_| AuthorityOutboxError::InvalidRecord)?;
    let bundle_manifest: Vec<u8> = row
        .try_get("bundle_manifest_sha256")
        .map_err(|_| AuthorityOutboxError::InvalidRecord)?;
    let activation_generation: i64 = row
        .try_get("contract_activation_generation")
        .map_err(|_| AuthorityOutboxError::InvalidRecord)?;
    let owner_source_commit: String = row
        .try_get("owner_source_commit")
        .map_err(|_| AuthorityOutboxError::InvalidRecord)?;
    let endpoint_base: String = row
        .try_get("endpoint_base")
        .map_err(|_| AuthorityOutboxError::InvalidRecord)?;

    let endpoint_matches_binding = values.endpoint == endpoint_base
        || values
            .endpoint
            .strip_prefix(&endpoint_base)
            .is_some_and(|suffix| {
                endpoint_base.ends_with('/') || suffix.starts_with('/') || suffix.starts_with('?')
            });
    let generation_matches = u64::try_from(execution_device_generation).ok()
        == Some(values.execution_device_generation)
        && u64::try_from(activation_generation).ok() == Some(values.contract_activation_generation);
    if target_component != values.target_component
        || execution_device_id != values.execution_device_id
        || !generation_matches
        || execution_authorization_id != values.execution_authorization_id.as_bytes()
        || fingerprint != values.certificate_fingerprint_sha256
        || hex(&target_manifest) != values.target_binding_manifest_sha256
        || hex(&bundle_manifest) != values.bundle_manifest_sha256
        || owner_source_commit != values.owner_source_commit
        || !endpoint_matches_binding
    {
        return Err(AuthorityOutboxError::AuthorizationRevoked);
    }
    Ok(())
}

async fn lock_record_session(
    tx: &mut Transaction<'_, sqlx::Postgres>,
    record: &AuthorityInvocationRecord,
    now_unix_ms: u64,
) -> Result<(), AuthorityOutboxError> {
    let envelope = record
        .approved_invocation
        .canonical_envelope
        .as_ref()
        .ok_or(AuthorityOutboxError::InvalidRecord)?;
    let session_id = parse_uuid(&envelope.session_id)?;
    lock_session(
        tx,
        SessionLockFence {
            session_id: &session_id,
            organization_id: &envelope.organization_id,
            workspace_id: &envelope.workspace_id,
            issuer: &envelope.principal_issuer,
            subject: &envelope.principal_subject,
            generation: envelope.session_generation,
            now_ms: now_unix_ms,
        },
    )
    .await
}

async fn lock_target_binding_for_record(
    tx: &mut Transaction<'_, sqlx::Postgres>,
    record: &AuthorityInvocationRecord,
) -> Result<(), AuthorityOutboxError> {
    let envelope = record
        .approved_invocation
        .canonical_envelope
        .as_ref()
        .ok_or(AuthorityOutboxError::InvalidRecord)?;
    let invocation = envelope
        .invocation
        .as_ref()
        .ok_or(AuthorityOutboxError::InvalidRecord)?;
    let target = envelope
        .target
        .as_ref()
        .ok_or(AuthorityOutboxError::InvalidRecord)?;
    let provenance = sqlx::query(&format!(
        r#"SELECT target_binding_manifest_sha256, bundle_manifest_sha256,
                  owner_source_commit
           FROM {OUTBOX_TABLE}
           WHERE invocation_id = $1 AND record_schema_version = 2"#
    ))
    .bind(&record.approved_invocation.invocation_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|_| AuthorityOutboxError::Unavailable)?
    .ok_or(AuthorityOutboxError::NotFound)?;
    let target_manifest: String = provenance
        .try_get("target_binding_manifest_sha256")
        .map_err(|_| AuthorityOutboxError::InvalidRecord)?;
    let bundle_manifest: String = provenance
        .try_get("bundle_manifest_sha256")
        .map_err(|_| AuthorityOutboxError::InvalidRecord)?;
    let owner_source_commit: String = provenance
        .try_get("owner_source_commit")
        .map_err(|_| AuthorityOutboxError::InvalidRecord)?;

    let row = sqlx::query(&format!(
        r#"SELECT target_component, execution_device_id, execution_device_generation,
                  execution_authorization_id, certificate_fingerprint_sha256,
                  target_binding_manifest_sha256, bundle_manifest_sha256,
                  contract_activation_generation, owner_source_commit, endpoint_base
           FROM {TARGET_TABLE}
           WHERE organization_id = $1 AND workspace_id = $2
             AND operation_owner_id = $3 AND operation_id = $4 AND active = TRUE
           FOR SHARE"#
    ))
    .bind(&envelope.organization_id)
    .bind(&envelope.workspace_id)
    .bind(&invocation.owner_id)
    .bind(&invocation.operation_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|_| AuthorityOutboxError::Unavailable)?
    .ok_or(AuthorityOutboxError::AuthorizationRevoked)?;

    let target_component: String = row
        .try_get("target_component")
        .map_err(|_| AuthorityOutboxError::InvalidRecord)?;
    let execution_device_id: String = row
        .try_get("execution_device_id")
        .map_err(|_| AuthorityOutboxError::InvalidRecord)?;
    let execution_device_generation: i64 = row
        .try_get("execution_device_generation")
        .map_err(|_| AuthorityOutboxError::InvalidRecord)?;
    let execution_authorization_id: Vec<u8> = row
        .try_get("execution_authorization_id")
        .map_err(|_| AuthorityOutboxError::InvalidRecord)?;
    let fingerprint: Vec<u8> = row
        .try_get("certificate_fingerprint_sha256")
        .map_err(|_| AuthorityOutboxError::InvalidRecord)?;
    let current_target_manifest: Vec<u8> = row
        .try_get("target_binding_manifest_sha256")
        .map_err(|_| AuthorityOutboxError::InvalidRecord)?;
    let current_bundle_manifest: Vec<u8> = row
        .try_get("bundle_manifest_sha256")
        .map_err(|_| AuthorityOutboxError::InvalidRecord)?;
    let activation_generation: i64 = row
        .try_get("contract_activation_generation")
        .map_err(|_| AuthorityOutboxError::InvalidRecord)?;
    let current_owner_source_commit: String = row
        .try_get("owner_source_commit")
        .map_err(|_| AuthorityOutboxError::InvalidRecord)?;
    let endpoint_base: String = row
        .try_get("endpoint_base")
        .map_err(|_| AuthorityOutboxError::InvalidRecord)?;
    let endpoint_matches_binding = target.endpoint == endpoint_base
        || target
            .endpoint
            .strip_prefix(&endpoint_base)
            .is_some_and(|suffix| {
                endpoint_base.ends_with('/') || suffix.starts_with('/') || suffix.starts_with('?')
            });
    if target_component != target.target_component
        || execution_device_id != target.execution_device_id
        || u64::try_from(execution_device_generation).ok()
            != Some(target.execution_device_generation)
        || execution_authorization_id != target.execution_authorization_id
        || fingerprint != target.execution_device_certificate_sha256
        || hex(&current_target_manifest) != target_manifest
        || hex(&current_bundle_manifest) != bundle_manifest
        || current_owner_source_commit != owner_source_commit
        || u64::try_from(activation_generation).ok()
            != Some(envelope.contract_activation_generation)
        || !endpoint_matches_binding
    {
        return Err(AuthorityOutboxError::AuthorizationRevoked);
    }
    Ok(())
}

async fn ensure_active_claim(
    tx: &mut Transaction<'_, sqlx::Postgres>,
    invocation_id: &str,
    now_unix_ms: u64,
) -> Result<(), AuthorityOutboxError> {
    let lease_expires_at: Option<i64> = sqlx::query_scalar(&format!(
        "SELECT claim_lease_expires_at_unix_ms FROM {OUTBOX_TABLE} WHERE invocation_id = $1"
    ))
    .bind(invocation_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|_| AuthorityOutboxError::Unavailable)?
    .flatten();
    if lease_expires_at
        .is_some_and(|expiry| u64::try_from(expiry).is_ok_and(|expiry| expiry > now_unix_ms))
    {
        Ok(())
    } else {
        Err(AuthorityOutboxError::InvalidState)
    }
}

async fn recover_expired_claims(
    tx: &mut Transaction<'_, sqlx::Postgres>,
    peer: &ExecutionDevicePeer,
    now: u64,
) -> Result<(), AuthorityOutboxError> {
    sqlx::query(&format!(
        r#"UPDATE {OUTBOX_TABLE}
        SET status = CASE
                WHEN status = 'CLAIMED'
                     AND idempotency_semantics = 'IDEMPOTENCY_SEMANTICS_REQUIRED'
                     AND idempotency_key IS NOT NULL
                     AND expires_at_unix_ms > $5 THEN 'PENDING'
                WHEN status = 'ACKNOWLEDGED'
                     AND idempotency_semantics = 'IDEMPOTENCY_SEMANTICS_REQUIRED'
                     AND idempotency_key IS NOT NULL
                     AND expires_at_unix_ms > $5 THEN 'PENDING'
                ELSE 'UNKNOWN_RESULT'
            END,
            claimed_by_connector_id = NULL,
            claim_lease_expires_at_unix_ms = NULL,
            outcome_status = CASE
                WHEN status = 'ACKNOWLEDGED' THEN 'UNKNOWN_RESULT'
                ELSE outcome_status
            END,
            updated_at_unix_ms = $5,
            error_code = 'CLAIM_LEASE_EXPIRED'
        WHERE record_schema_version = 2
          AND organization_id = $1 AND workspace_id = $2
          AND execution_device_id = $3 AND device_generation = $4
          AND status IN ('CLAIMED', 'ACKNOWLEDGED')
          AND claim_lease_expires_at_unix_ms <= $5"#
    ))
    .bind(&peer.organization_id)
    .bind(&peer.workspace_id)
    .bind(&peer.device_id)
    .bind(
        i64::try_from(peer.authorization_generation)
            .map_err(|_| AuthorityOutboxError::InvalidRecord)?,
    )
    .bind(i64::try_from(now).map_err(|_| AuthorityOutboxError::InvalidRecord)?)
    .execute(&mut **tx)
    .await
    .map_err(|_| AuthorityOutboxError::Unavailable)?;
    Ok(())
}

async fn expire_pending_for_device(
    tx: &mut Transaction<'_, sqlx::Postgres>,
    peer: &ExecutionDevicePeer,
    now: u64,
) -> Result<(), AuthorityOutboxError> {
    sqlx::query(&format!(
        r#"UPDATE {OUTBOX_TABLE}
        SET status = 'FAILED', outcome_status = 'FAILED',
            error_code = 'APPROVAL_EXPIRED', updated_at_unix_ms = $5
        WHERE record_schema_version = 2
          AND organization_id = $1 AND workspace_id = $2
          AND execution_device_id = $3 AND device_generation = $4
          AND status = 'PENDING' AND expires_at_unix_ms <= $5"#
    ))
    .bind(&peer.organization_id)
    .bind(&peer.workspace_id)
    .bind(&peer.device_id)
    .bind(
        i64::try_from(peer.authorization_generation)
            .map_err(|_| AuthorityOutboxError::InvalidRecord)?,
    )
    .bind(i64::try_from(now).map_err(|_| AuthorityOutboxError::InvalidRecord)?)
    .execute(&mut **tx)
    .await
    .map_err(|_| AuthorityOutboxError::Unavailable)?;
    Ok(())
}

fn row_to_invocation_record(row: PgRow) -> Result<AuthorityInvocationRecord, AuthorityOutboxError> {
    let state_text: String = row
        .try_get("status")
        .map_err(|_| AuthorityOutboxError::InvalidRecord)?;
    let approved_envelope_proto: Vec<u8> = row
        .try_get("approved_envelope_proto")
        .map_err(|_| AuthorityOutboxError::InvalidRecord)?;
    let execution_credential_proto: Vec<u8> = row
        .try_get("execution_credential_proto")
        .map_err(|_| AuthorityOutboxError::InvalidRecord)?;
    let canonical_envelope =
        CanonicalInvocationEnvelope::decode(approved_envelope_proto.as_slice())
            .map_err(|_| AuthorityOutboxError::InvalidRecord)?;
    if canonical_envelope.encode_to_vec() != approved_envelope_proto {
        return Err(AuthorityOutboxError::InvalidRecord);
    }
    let credential = ExecutionCredential::decode(execution_credential_proto.as_slice())
        .map_err(|_| AuthorityOutboxError::InvalidRecord)?;
    if credential.encode_to_vec() != execution_credential_proto {
        return Err(AuthorityOutboxError::InvalidRecord);
    }
    let invocation_id: String = row
        .try_get("invocation_id")
        .map_err(|_| AuthorityOutboxError::InvalidRecord)?;
    let invocation_bytes: Vec<u8> = row
        .try_get("product_invocation_proto")
        .map_err(|_| AuthorityOutboxError::InvalidRecord)?;
    if invocation_id != canonical_envelope.invocation_id
        || canonical_envelope
            .invocation
            .as_ref()
            .map(Message::encode_to_vec)
            .as_deref()
            != Some(invocation_bytes.as_slice())
    {
        return Err(AuthorityOutboxError::InvalidRecord);
    }
    let invocation = canonical_envelope
        .invocation
        .as_ref()
        .ok_or(AuthorityOutboxError::InvalidRecord)?;
    let target = canonical_envelope
        .target
        .as_ref()
        .ok_or(AuthorityOutboxError::InvalidRecord)?;
    let request_digest = Sha256::digest(&approved_envelope_proto).to_vec();
    let invocation_digest = Sha256::digest(&invocation_bytes).to_vec();
    let stored_request_digest: String = row
        .try_get("request_digest_sha256")
        .map_err(|_| AuthorityOutboxError::InvalidRecord)?;
    let stored_invocation_digest: String = row
        .try_get("invocation_digest_sha256")
        .map_err(|_| AuthorityOutboxError::InvalidRecord)?;
    let stored_idempotency_scope: String = row
        .try_get("idempotency_scope_sha256")
        .map_err(|_| AuthorityOutboxError::InvalidRecord)?;
    let stored_stable_request_digest: String = row
        .try_get("stable_idempotency_request_digest_sha256")
        .map_err(|_| AuthorityOutboxError::InvalidRecord)?;
    let expected_stable_request_digest =
        stable_idempotency_request_digest(&invocation_bytes, &stored_idempotency_scope)?;
    if credential.invocation_id != invocation_id
        || credential.request_digest_sha256 != request_digest
        || credential.invocation_digest_sha256 != invocation_digest
        || stored_request_digest != hex(&request_digest)
        || stored_invocation_digest != hex(&invocation_digest)
        || stored_stable_request_digest != expected_stable_request_digest
        || credential.organization_id != canonical_envelope.organization_id
        || credential.workspace_id != canonical_envelope.workspace_id
        || credential.principal_issuer != canonical_envelope.principal_issuer
        || credential.principal_subject != canonical_envelope.principal_subject
        || credential.operation_owner_id != invocation.owner_id
        || credential.operation_id != invocation.operation_id
        || credential.scope != target.scope
        || credential.resource_id != target.resource_id
        || credential.target_component != target.target_component
        || credential.http_method != target.http_method
        || credential.endpoint != target.endpoint
        || credential.execution_device_id != target.execution_device_id
        || credential.execution_device_generation != target.execution_device_generation
        || credential.execution_authorization_id != target.execution_authorization_id
        || credential.execution_device_certificate_sha256
            != target.execution_device_certificate_sha256
        || credential.session_id != canonical_envelope.session_id
        || credential.session_generation != canonical_envelope.session_generation
        || credential.contract_activation_generation
            != canonical_envelope.contract_activation_generation
        || credential.idempotency_key != target.idempotency_key
        || credential.idempotency_semantics != target.idempotency_semantics
        || credential.signing_key_id.trim().is_empty()
        || credential.authority_signature.is_empty()
        || credential.issued_at != canonical_envelope.issued_at
        || credential.expires_at != canonical_envelope.expires_at
    {
        return Err(AuthorityOutboxError::InvalidRecord);
    }
    let result = build_persisted_result(&row, credential.clone())?;
    Ok(AuthorityInvocationRecord {
        approved_invocation: ApprovedInvocation {
            invocation_id,
            canonical_envelope: Some(canonical_envelope),
            credential: Some(credential),
        },
        state: state_from_str(&state_text)?,
        result,
    })
}

fn build_persisted_result(
    row: &PgRow,
    credential: ExecutionCredential,
) -> Result<Option<PersistedInvocationResult>, AuthorityOutboxError> {
    let digest: Option<String> = row
        .try_get("result_digest_sha256")
        .map_err(|_| AuthorityOutboxError::InvalidRecord)?;
    let Some(digest) = digest else {
        return Ok(None);
    };
    let outcome: Option<String> = row
        .try_get("outcome_status")
        .map_err(|_| AuthorityOutboxError::InvalidRecord)?;
    let outcome_status = match outcome.as_deref() {
        Some("SUCCESS") => ExecutionOutcomeStatus::Success,
        Some("FAILED") => ExecutionOutcomeStatus::Failed,
        Some("UNKNOWN_RESULT") => ExecutionOutcomeStatus::UnknownResult,
        _ => return Err(AuthorityOutboxError::InvalidRecord),
    };
    let response_status_code: Option<i32> = row
        .try_get("response_status_code")
        .map_err(|_| AuthorityOutboxError::InvalidRecord)?;
    let response_body: Option<Vec<u8>> = row
        .try_get("response_json_body")
        .map_err(|_| AuthorityOutboxError::InvalidRecord)?;
    let response_content_type: Option<String> = row
        .try_get("response_content_type")
        .map_err(|_| AuthorityOutboxError::InvalidRecord)?;
    let product_response = match (response_status_code, response_body, response_content_type) {
        (Some(status_code), Some(json_body), Some(content_type)) => Some(
            cy_proto::cyrene::workspace::product::v2::ProductApiResponseV2 {
                status_code: u32::try_from(status_code)
                    .map_err(|_| AuthorityOutboxError::InvalidRecord)?,
                json_body,
                content_type,
            },
        ),
        (None, None, None) => None,
        _ => return Err(AuthorityOutboxError::InvalidRecord),
    };
    let invocation_id: String = row
        .try_get("invocation_id")
        .map_err(|_| AuthorityOutboxError::InvalidRecord)?;
    let error_code: Option<String> = row
        .try_get("error_code")
        .map_err(|_| AuthorityOutboxError::InvalidRecord)?;
    let error_message: Option<String> = row
        .try_get("error_message")
        .map_err(|_| AuthorityOutboxError::InvalidRecord)?;
    let delivery_receipt: Option<Vec<u8>> = row
        .try_get("delivery_receipt")
        .map_err(|_| AuthorityOutboxError::InvalidRecord)?;
    let result_receipt: Option<Vec<u8>> = row
        .try_get("result_receipt")
        .map_err(|_| AuthorityOutboxError::InvalidRecord)?;
    let delivery_receipt = delivery_receipt.ok_or(AuthorityOutboxError::InvalidRecord)?;
    let result_receipt = result_receipt.ok_or(AuthorityOutboxError::InvalidRecord)?;
    let persisted = PersistedInvocationResult {
        invocation_id,
        credential,
        outcome_status,
        product_response,
        result_digest_sha256: unhex(&digest)?,
        result_receipt,
        error_code: error_code.unwrap_or_default(),
        error_message: error_message.unwrap_or_default(),
        delivery_receipt,
    };
    if digest_result(&persisted) != persisted.result_digest_sha256 {
        return Err(AuthorityOutboxError::InvalidRecord);
    }
    Ok(Some(persisted))
}

fn ensure_peer_matches_record(
    peer: &ExecutionDevicePeer,
    record: &AuthorityInvocationRecord,
) -> Result<(), AuthorityOutboxError> {
    let envelope = record
        .approved_invocation
        .canonical_envelope
        .as_ref()
        .ok_or(AuthorityOutboxError::InvalidRecord)?;
    if envelope.organization_id != peer.organization_id
        || envelope.workspace_id != peer.workspace_id
        || envelope.execution_device_id != peer.device_id
        || envelope.execution_device_generation != peer.authorization_generation
        || envelope.execution_authorization_id != peer.authorization_id.as_bytes()
        || envelope.execution_device_certificate_sha256 != peer.certificate_fingerprint_sha256
    {
        return Err(AuthorityOutboxError::AuthorizationRevoked);
    }
    Ok(())
}

fn ensure_credential_matches(
    credential: &ExecutionCredential,
    record: &AuthorityInvocationRecord,
) -> Result<(), AuthorityOutboxError> {
    let stored = record
        .approved_invocation
        .credential
        .as_ref()
        .ok_or(AuthorityOutboxError::InvalidRecord)?;
    if credential != stored {
        return Err(AuthorityOutboxError::InvalidRecord);
    }
    Ok(())
}

fn state_from_str(value: &str) -> Result<InvocationState, AuthorityOutboxError> {
    match value {
        "PENDING" => Ok(InvocationState::Pending),
        "CLAIMED" => Ok(InvocationState::Claimed),
        "ACKNOWLEDGED" => Ok(InvocationState::Acknowledged),
        "SUCCEEDED" => Ok(InvocationState::Succeeded),
        "FAILED" => Ok(InvocationState::Failed),
        "UNKNOWN_RESULT" => Ok(InvocationState::UnknownResult),
        "CANCELLED" => Ok(InvocationState::Unspecified),
        _ => Err(AuthorityOutboxError::InvalidRecord),
    }
}

fn canonical_idempotency_scope(parts: &[&str]) -> Vec<u8> {
    let mut encoded = Vec::new();
    for part in parts {
        let bytes = part.as_bytes();
        encoded.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
        encoded.extend_from_slice(bytes);
    }
    encoded
}

fn stable_idempotency_request_digest(
    invocation_proto: &[u8],
    idempotency_scope_sha256: &str,
) -> Result<String, AuthorityOutboxError> {
    let scope_digest = unhex(idempotency_scope_sha256)?;
    if scope_digest.len() != 32 {
        return Err(AuthorityOutboxError::InvalidRecord);
    }
    let mut canonical = b"cyrene.workspace.authority.v2/stable-idempotency-request\0".to_vec();
    append_length_prefixed(&mut canonical, invocation_proto);
    append_length_prefixed(&mut canonical, &scope_digest);
    Ok(hex(&Sha256::digest(canonical)))
}

fn append_length_prefixed(target: &mut Vec<u8>, bytes: &[u8]) {
    target.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    target.extend_from_slice(bytes);
}

fn digest_result(result: &PersistedInvocationResult) -> Vec<u8> {
    Sha256::digest(
        CanonicalInvocationResultEnvelope {
            invocation_id: result.invocation_id.clone(),
            request_digest_sha256: result.credential.request_digest_sha256.clone(),
            outcome_status: result.outcome_status as i32,
            product_response: result.product_response.clone(),
            error_code: result.error_code.clone(),
            error_message: result.error_message.clone(),
            delivery_receipt: result.delivery_receipt.clone(),
        }
        .encode_to_vec(),
    )
    .to_vec()
}

fn timestamp_to_unix_ms(
    timestamp: Option<&prost_types::Timestamp>,
) -> Result<u64, AuthorityOutboxError> {
    let timestamp = timestamp.ok_or(AuthorityOutboxError::InvalidRecord)?;
    if timestamp.seconds < 0 || !(0..1_000_000_000).contains(&timestamp.nanos) {
        return Err(AuthorityOutboxError::InvalidRecord);
    }
    let millis = u64::try_from(timestamp.seconds)
        .ok()
        .and_then(|seconds| seconds.checked_mul(1000))
        .and_then(|seconds| seconds.checked_add(u64::try_from(timestamp.nanos / 1_000_000).ok()?))
        .ok_or(AuthorityOutboxError::InvalidRecord)?;
    Ok(millis)
}

fn credential_expiry_ms(credential: &ExecutionCredential) -> Result<u64, AuthorityOutboxError> {
    timestamp_to_unix_ms(credential.expires_at.as_ref())
}

fn parse_uuid(value: &str) -> Result<Uuid, AuthorityOutboxError> {
    Uuid::parse_str(value).map_err(|_| AuthorityOutboxError::InvalidRecord)
}

fn nonempty(value: &str) -> Option<&str> {
    (!value.is_empty()).then_some(value)
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[(byte >> 4) as usize]));
        output.push(char::from(HEX[(byte & 0x0f) as usize]));
    }
    output
}

fn unhex(value: &str) -> Result<Vec<u8>, AuthorityOutboxError> {
    if !value.len().is_multiple_of(2) {
        return Err(AuthorityOutboxError::InvalidRecord);
    }
    let mut bytes = Vec::with_capacity(value.len() / 2);
    for pair in value.as_bytes().chunks_exact(2) {
        let high = hex_nibble(pair[0]).ok_or(AuthorityOutboxError::InvalidRecord)?;
        let low = hex_nibble(pair[1]).ok_or(AuthorityOutboxError::InvalidRecord)?;
        bytes.push((high << 4) | low);
    }
    Ok(bytes)
}

fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        _ => None,
    }
}
