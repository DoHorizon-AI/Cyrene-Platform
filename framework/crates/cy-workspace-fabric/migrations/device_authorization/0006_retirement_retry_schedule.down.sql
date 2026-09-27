-- ┌─────────────────────────────────────────────────────────────────────┐
-- │  Migration: 0006_retirement_retry_schedule.down.sql                  │
-- │  Schema: cyrene_workspace_device_authorization                       │
-- │  Role: Remove the durable certificate-retirement retry schedule.    │
-- │                                                                      │
-- │  用途：回滚设备证书撤销的持久重试计划                                      │
-- └─────────────────────────────────────────────────────────────────────┘

DROP INDEX cyrene_workspace_device_authorization.authorizations_retirement_recovery_idx;

ALTER TABLE cyrene_workspace_device_authorization.authorizations
    DROP CONSTRAINT authorizations_retirement_next_attempt_shape,
    DROP CONSTRAINT authorizations_retirement_attempt_count,
    DROP COLUMN retirement_next_attempt_at_unix_ms,
    DROP COLUMN retirement_attempt_count;

CREATE INDEX authorizations_retirement_recovery_idx
    ON cyrene_workspace_device_authorization.authorizations (created_at_unix_ms, id)
    WHERE state_kind = 'retirement_pending';
