//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 outbox.rs                                                       │
//! │  Module: cy_workspace_postgres_storage                             │
//! │  Role: Persistent Outbox queue and final dispatch fence storage.   │
//! │                                                                     │
//! │  模块职责：持久化 Outbox 队列与最终派发栅栏存储实现。                    │
//! │  · 事务内原子检查代次与撤销；相同调用 ID 相同摘要幂等返回，不同摘要拒绝。│
//! └─────────────────────────────────────────────────────────────────────┘

use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{PgPool, Row};
use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use thiserror::Error;

const OUTBOX_TABLE: &str = "cyrene_workspace_device_registry.workspace_invocation_outbox";
const REGISTRY_TABLE: &str = "cyrene_workspace_device_registry.certificate_records";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutboxStatus {
    Pending,
    Claimed,
    Delivered,
    Completed,
    UnknownResult,
    Cancelled,
}

impl OutboxStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "PENDING",
            Self::Claimed => "CLAIMED",
            Self::Delivered => "DELIVERED",
            Self::Completed => "COMPLETED",
            Self::UnknownResult => "UNKNOWN_RESULT",
            Self::Cancelled => "CANCELLED",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Self {
        match s {
            "CLAIMED" => Self::Claimed,
            "DELIVERED" => Self::Delivered,
            "COMPLETED" => Self::Completed,
            "UNKNOWN_RESULT" => Self::UnknownResult,
            "CANCELLED" => Self::Cancelled,
            _ => Self::Pending,
        }
    }
}

impl std::str::FromStr for OutboxStatus {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self::from_str(s))
    }
}

impl fmt::Display for OutboxStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboxRecord {
    pub invocation_id: String,
    pub workspace_id: String,
    pub idempotency_key: Option<String>,
    pub request_digest_sha256: String,
    pub target_component: String,
    pub resolved_target_url: String,
    pub raw_invocation_json: Vec<u8>,
    pub device_generation: u64,
    pub session_generation: u64,
    pub contract_activation_generation: u64,
    pub status: OutboxStatus,
    pub claimed_by_connector_id: Option<String>,
    pub claimed_at_unix_ms: Option<u64>,
    pub delivery_receipt: Option<Vec<u8>>,
    pub delivery_acknowledged_at_unix_ms: Option<u64>,
    pub outcome_status: Option<String>,
    pub response_status_code: Option<u32>,
    pub response_json_body: Option<Vec<u8>>,
    pub response_content_type: Option<String>,
    pub error_message: Option<String>,
    pub created_at_unix_ms: u64,
    pub updated_at_unix_ms: u64,
    pub expires_at_unix_ms: u64,
}

#[derive(Debug, Clone)]
pub struct EnqueueInvocationInput {
    pub invocation_id: String,
    pub workspace_id: String,
    pub idempotency_key: Option<String>,
    pub request_digest_sha256: String,
    pub target_component: String,
    pub resolved_target_url: String,
    pub raw_invocation_json: Vec<u8>,
    pub device_generation: u64,
    pub session_generation: u64,
    pub contract_activation_generation: u64,
    pub ttl: Duration,
}

#[derive(Debug, Clone)]
pub enum EnqueueOutcome {
    Enqueued(OutboxRecord),
    Existing(OutboxRecord),
}

#[derive(Debug, Error)]
pub enum OutboxError {
    #[error(
        "Idempotency conflict: invocation ID or key already used with different request digest"
    )]
    IdempotencyConflict,
    #[error("Device revoked or generation mismatch")]
    DeviceRevokedOrGenerationMismatch,
    #[error("Invocation not found: {0}")]
    NotFound(String),
    #[error("Invalid state transition for invocation: {0}")]
    InvalidState(String),
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("Storage unavailable")]
    Unavailable,
}

#[derive(Clone)]
pub struct PostgresWorkspaceOutbox {
    pool: PgPool,
}

pub fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

impl PostgresWorkspaceOutbox {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Enqueue an approved invocation with transactional deduplication and revocation checks.
    pub async fn enqueue_invocation(
        &self,
        input: EnqueueInvocationInput,
    ) -> Result<EnqueueOutcome, OutboxError> {
        let mut tx = self.pool.begin().await?;

        // 1. Check idempotency key if provided
        if let Some(ref ikey) = input.idempotency_key {
            let existing = sqlx::query(&format!(
                "SELECT * FROM {OUTBOX_TABLE} WHERE workspace_id = $1 AND idempotency_key = $2"
            ))
            .bind(&input.workspace_id)
            .bind(ikey)
            .fetch_optional(&mut *tx)
            .await?;

            if let Some(row) = existing {
                let record = row_to_outbox_record(&row)?;
                if record.request_digest_sha256 == input.request_digest_sha256 {
                    tx.commit().await?;
                    return Ok(EnqueueOutcome::Existing(record));
                } else {
                    return Err(OutboxError::IdempotencyConflict);
                }
            }
        }

        // 2. Check invocation_id uniqueness
        let existing = sqlx::query(&format!(
            "SELECT * FROM {OUTBOX_TABLE} WHERE invocation_id = $1"
        ))
        .bind(&input.invocation_id)
        .fetch_optional(&mut *tx)
        .await?;

        if let Some(row) = existing {
            let record = row_to_outbox_record(&row)?;
            if record.request_digest_sha256 == input.request_digest_sha256 {
                tx.commit().await?;
                return Ok(EnqueueOutcome::Existing(record));
            } else {
                return Err(OutboxError::IdempotencyConflict);
            }
        }

        // 3. Verify device generation and non-revocation in same transaction
        let revoked = sqlx::query(&format!(
            "SELECT state FROM {REGISTRY_TABLE} WHERE workspace_id = $1 AND authorization_generation = $2 AND state = 'revoked'"
        ))
        .bind(&input.workspace_id)
        .bind(input.device_generation as i64)
        .fetch_optional(&mut *tx)
        .await?;

        if revoked.is_some() {
            return Err(OutboxError::DeviceRevokedOrGenerationMismatch);
        }

        // 4. Insert new outbox record
        let now = now_unix_ms();
        let expires_at = now.saturating_add(input.ttl.as_millis() as u64);

        let inserted = sqlx::query(&format!(
            r#"INSERT INTO {OUTBOX_TABLE} (
                invocation_id, workspace_id, idempotency_key, request_digest_sha256,
                target_component, resolved_target_url, raw_invocation_json,
                device_generation, session_generation, contract_activation_generation,
                status, created_at_unix_ms, updated_at_unix_ms, expires_at_unix_ms
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'PENDING', $11, $11, $12)
            RETURNING *"#
        ))
        .bind(&input.invocation_id)
        .bind(&input.workspace_id)
        .bind(&input.idempotency_key)
        .bind(&input.request_digest_sha256)
        .bind(&input.target_component)
        .bind(&input.resolved_target_url)
        .bind(&input.raw_invocation_json)
        .bind(input.device_generation as i64)
        .bind(input.session_generation as i64)
        .bind(input.contract_activation_generation as i64)
        .bind(now as i64)
        .bind(expires_at as i64)
        .fetch_one(&mut *tx)
        .await?;

        let record = row_to_outbox_record(&inserted)?;
        tx.commit().await?;

        Ok(EnqueueOutcome::Enqueued(record))
    }

    /// Claim up to `max_batch_size` pending invocations for a connector.
    pub async fn claim_invocations(
        &self,
        connector_id: &str,
        workspace_id: &str,
        supported_components: &[String],
        max_batch_size: usize,
    ) -> Result<Vec<OutboxRecord>, OutboxError> {
        let now = now_unix_ms();
        let mut tx = self.pool.begin().await?;

        let rows = if supported_components.is_empty() {
            sqlx::query(&format!(
                r#"UPDATE {OUTBOX_TABLE}
                SET status = 'CLAIMED',
                    claimed_by_connector_id = $1,
                    claimed_at_unix_ms = $4,
                    updated_at_unix_ms = $4
                WHERE invocation_id IN (
                    SELECT invocation_id FROM {OUTBOX_TABLE}
                    WHERE workspace_id = $2
                      AND status = 'PENDING'
                      AND expires_at_unix_ms > $4
                    ORDER BY created_at_unix_ms ASC
                    LIMIT $3
                    FOR UPDATE SKIP LOCKED
                )
                RETURNING *"#
            ))
            .bind(connector_id)
            .bind(workspace_id)
            .bind(max_batch_size as i64)
            .bind(now as i64)
            .fetch_all(&mut *tx)
            .await?
        } else {
            sqlx::query(&format!(
                r#"UPDATE {OUTBOX_TABLE}
                SET status = 'CLAIMED',
                    claimed_by_connector_id = $1,
                    claimed_at_unix_ms = $5,
                    updated_at_unix_ms = $5
                WHERE invocation_id IN (
                    SELECT invocation_id FROM {OUTBOX_TABLE}
                    WHERE workspace_id = $2
                      AND target_component = ANY($3)
                      AND status = 'PENDING'
                      AND expires_at_unix_ms > $5
                    ORDER BY created_at_unix_ms ASC
                    LIMIT $4
                    FOR UPDATE SKIP LOCKED
                )
                RETURNING *"#
            ))
            .bind(connector_id)
            .bind(workspace_id)
            .bind(supported_components)
            .bind(max_batch_size as i64)
            .bind(now as i64)
            .fetch_all(&mut *tx)
            .await?
        };

        let mut claimed = Vec::with_capacity(rows.len());
        for row in rows {
            claimed.push(row_to_outbox_record(&row)?);
        }

        tx.commit().await?;
        Ok(claimed)
    }

    /// Acknowledge delivery by the connector with a receipt.
    pub async fn acknowledge_delivery(
        &self,
        invocation_id: &str,
        connector_id: &str,
        receipt: &[u8],
    ) -> Result<bool, OutboxError> {
        let now = now_unix_ms();
        let res = sqlx::query(&format!(
            r#"UPDATE {OUTBOX_TABLE}
            SET status = 'DELIVERED',
                delivery_receipt = $1,
                delivery_acknowledged_at_unix_ms = $4,
                updated_at_unix_ms = $4
            WHERE invocation_id = $2
              AND claimed_by_connector_id = $3
              AND status = 'CLAIMED'"#
        ))
        .bind(receipt)
        .bind(invocation_id)
        .bind(connector_id)
        .bind(now as i64)
        .execute(&self.pool)
        .await?;

        Ok(res.rows_affected() > 0)
    }

    /// Submit the execution result from connector or downstream execution.
    pub async fn submit_result(
        &self,
        invocation_id: &str,
        outcome_status: &str,
        status_code: Option<u32>,
        json_body: Option<&[u8]>,
        content_type: Option<&str>,
        error_message: Option<&str>,
    ) -> Result<bool, OutboxError> {
        let now = now_unix_ms();
        let outbox_status = match outcome_status {
            "SUCCESS" => "COMPLETED",
            "UNKNOWN_RESULT" => "UNKNOWN_RESULT",
            _ => "COMPLETED",
        };

        let res = sqlx::query(&format!(
            r#"UPDATE {OUTBOX_TABLE}
            SET status = $1,
                outcome_status = $2,
                response_status_code = $3,
                response_json_body = $4,
                response_content_type = $5,
                error_message = $6,
                updated_at_unix_ms = $7
            WHERE invocation_id = $8
              AND status IN ('CLAIMED', 'DELIVERED', 'PENDING')"#
        ))
        .bind(outbox_status)
        .bind(outcome_status)
        .bind(status_code.map(|c| c as i32))
        .bind(json_body)
        .bind(content_type)
        .bind(error_message)
        .bind(now as i64)
        .bind(invocation_id)
        .execute(&self.pool)
        .await?;

        Ok(res.rows_affected() > 0)
    }

    /// Query an invocation by ID.
    pub async fn get_invocation(
        &self,
        invocation_id: &str,
    ) -> Result<Option<OutboxRecord>, OutboxError> {
        let row = sqlx::query(&format!(
            "SELECT * FROM {OUTBOX_TABLE} WHERE invocation_id = $1"
        ))
        .bind(invocation_id)
        .fetch_optional(&self.pool)
        .await?;

        row.map(|r| row_to_outbox_record(&r)).transpose()
    }

    pub async fn connect_from_environment() -> Result<Self, OutboxError> {
        let database_url = std::env::var("CYRENE_WORKSPACE_DEVICE_REGISTRY_DATABASE_URL")
            .or_else(|_| std::env::var("CYRENE_WORKSPACE_DIRECTORY_DATABASE_URL"))
            .map_err(|_| OutboxError::Unavailable)?;
        let pool = sqlx::PgPool::connect(&database_url)
            .await
            .map_err(OutboxError::Database)?;
        Ok(Self::new(pool))
    }
}

#[async_trait::async_trait]
impl cy_workspace_control_plane::authority_service::AuthorityOutboxStore
    for PostgresWorkspaceOutbox
{
    #[allow(clippy::too_many_arguments)]
    async fn enqueue(
        &self,
        invocation_id: &str,
        workspace_id: &str,
        idempotency_key: Option<&str>,
        request_digest: &str,
        target_component: &str,
        resolved_url: &str,
        raw_json: &[u8],
        device_gen: u64,
        session_gen: u64,
        contract_gen: u64,
        ttl: Duration,
    ) -> Result<bool, String> {
        let input = EnqueueInvocationInput {
            invocation_id: invocation_id.to_string(),
            workspace_id: workspace_id.to_string(),
            idempotency_key: idempotency_key.map(|k| k.to_string()),
            request_digest_sha256: request_digest.to_string(),
            target_component: target_component.to_string(),
            resolved_target_url: resolved_url.to_string(),
            raw_invocation_json: raw_json.to_vec(),
            device_generation: device_gen,
            session_generation: session_gen,
            contract_activation_generation: contract_gen,
            ttl,
        };
        match self.enqueue_invocation(input).await {
            Ok(EnqueueOutcome::Enqueued(_)) => Ok(true),
            Ok(EnqueueOutcome::Existing(_)) => Ok(false),
            Err(OutboxError::IdempotencyConflict) => Err("IDEMPOTENCY_CONFLICT".to_string()),
            Err(e) => Err(e.to_string()),
        }
    }

    async fn claim(
        &self,
        connector_id: &str,
        workspace_id: &str,
        supported_components: &[String],
        max_batch: usize,
    ) -> Result<Vec<cy_proto::cyrene::workspace::authority::v1::ApprovedInvocation>, String> {
        let records = self
            .claim_invocations(connector_id, workspace_id, supported_components, max_batch)
            .await
            .map_err(|e| e.to_string())?;

        let mut invocations = Vec::with_capacity(records.len());
        for rec in records {
            let cred = cy_proto::cyrene::workspace::authority::v1::DeliveryCredential {
                invocation_id: rec.invocation_id.clone(),
                request_digest_sha256: rec.request_digest_sha256.clone(),
                target_component: rec.target_component.clone(),
                workspace_id: rec.workspace_id.clone(),
                device_generation: rec.device_generation,
                session_generation: rec.session_generation,
                contract_activation_generation: rec.contract_activation_generation,
                expires_at: Some(prost_types::Timestamp {
                    seconds: (rec.expires_at_unix_ms / 1000) as i64,
                    nanos: 0,
                }),
                authority_signature: vec![],
            };
            invocations.push(
                cy_proto::cyrene::workspace::authority::v1::ApprovedInvocation {
                    invocation_id: rec.invocation_id,
                    credential: Some(cred),
                    raw_invocation: Some(
                        cy_proto::cyrene::workspace::product::v2::ProductApiInvocationV2 {
                            owner_id: rec.target_component.clone(),
                            operation_id: String::new(),
                            json_body: rec.raw_invocation_json,
                            resource_id: String::new(),
                            idempotency_key: rec.idempotency_key.unwrap_or_default(),
                        },
                    ),
                    resolved_target_url: rec.resolved_target_url,
                    timeout_seconds: 60,
                },
            );
        }
        Ok(invocations)
    }

    async fn acknowledge_delivery(
        &self,
        invocation_id: &str,
        connector_id: &str,
        receipt: &[u8],
    ) -> Result<bool, String> {
        self.acknowledge_delivery(invocation_id, connector_id, receipt)
            .await
            .map_err(|e| e.to_string())
    }

    async fn submit_result(
        &self,
        invocation_id: &str,
        outcome_status: &str,
        status_code: Option<u32>,
        json_body: Option<&[u8]>,
        content_type: Option<&str>,
        error_message: Option<&str>,
    ) -> Result<bool, String> {
        self.submit_result(
            invocation_id,
            outcome_status,
            status_code,
            json_body,
            content_type,
            error_message,
        )
        .await
        .map_err(|e| e.to_string())
    }
}

fn row_to_outbox_record(row: &PgRow) -> Result<OutboxRecord, OutboxError> {
    let status_str: String = row.try_get("status")?;
    let device_gen: i64 = row.try_get("device_generation")?;
    let session_gen: i64 = row.try_get("session_generation")?;
    let contract_gen: i64 = row.try_get("contract_activation_generation")?;
    let status_code: Option<i32> = row.try_get("response_status_code")?;
    let created_at: i64 = row.try_get("created_at_unix_ms")?;
    let updated_at: i64 = row.try_get("updated_at_unix_ms")?;
    let expires_at: i64 = row.try_get("expires_at_unix_ms")?;
    let claimed_at: Option<i64> = row.try_get("claimed_at_unix_ms")?;
    let acked_at: Option<i64> = row.try_get("delivery_acknowledged_at_unix_ms")?;

    Ok(OutboxRecord {
        invocation_id: row.try_get("invocation_id")?,
        workspace_id: row.try_get("workspace_id")?,
        idempotency_key: row.try_get("idempotency_key")?,
        request_digest_sha256: row.try_get("request_digest_sha256")?,
        target_component: row.try_get("target_component")?,
        resolved_target_url: row.try_get("resolved_target_url")?,
        raw_invocation_json: row.try_get("raw_invocation_json")?,
        device_generation: device_gen as u64,
        session_generation: session_gen as u64,
        contract_activation_generation: contract_gen as u64,
        status: OutboxStatus::from_str(&status_str),
        claimed_by_connector_id: row.try_get("claimed_by_connector_id")?,
        claimed_at_unix_ms: claimed_at.map(|v| v as u64),
        delivery_receipt: row.try_get("delivery_receipt")?,
        delivery_acknowledged_at_unix_ms: acked_at.map(|v| v as u64),
        outcome_status: row.try_get("outcome_status")?,
        response_status_code: status_code.map(|c| c as u32),
        response_json_body: row.try_get("response_json_body")?,
        response_content_type: row.try_get("response_content_type")?,
        error_message: row.try_get("error_message")?,
        created_at_unix_ms: created_at as u64,
        updated_at_unix_ms: updated_at as u64,
        expires_at_unix_ms: expires_at as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outbox_status_round_trips_canonical_strings() {
        let statuses = [
            (OutboxStatus::Pending, "PENDING"),
            (OutboxStatus::Claimed, "CLAIMED"),
            (OutboxStatus::Delivered, "DELIVERED"),
            (OutboxStatus::Completed, "COMPLETED"),
            (OutboxStatus::UnknownResult, "UNKNOWN_RESULT"),
            (OutboxStatus::Cancelled, "CANCELLED"),
        ];

        for (status, string) in statuses {
            assert_eq!(status.as_str(), string);
            assert_eq!(status.to_string(), string);
            assert_eq!(OutboxStatus::from_str(string), status);
        }
    }

    #[test]
    fn now_unix_ms_is_reasonable() {
        let now = now_unix_ms();
        // Greater than Sep 2026 (~1.78e12 ms)
        assert!(now > 1_780_000_000_000);
    }
}
