-- ┌─────────────────────────────────────────────────────────────────────┐
-- │  Migration: 0006_retirement_retry_schedule.up.sql                    │
-- │  Schema: cyrene_workspace_device_authorization                       │
-- │  Role: Persist DB-clock retry eligibility for certificate retirement.│
-- │                                                                      │
-- │  用途：持久化设备证书撤销的数据库时钟重试资格                              │
-- └─────────────────────────────────────────────────────────────────────┘

-- Persist the database-clock retry schedule for unresolved certificate
-- retirement. Older pending rows are made immediately eligible once so their
-- durable retirement work is not lost during upgrade.

ALTER TABLE cyrene_workspace_device_authorization.authorizations
    ADD COLUMN retirement_attempt_count BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN retirement_next_attempt_at_unix_ms BIGINT;

UPDATE cyrene_workspace_device_authorization.authorizations
SET retirement_next_attempt_at_unix_ms =
    FLOOR(EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT
WHERE state_kind = 'retirement_pending';

ALTER TABLE cyrene_workspace_device_authorization.authorizations
    ADD CONSTRAINT authorizations_retirement_attempt_count CHECK (
        retirement_attempt_count >= 0
    ),
    ADD CONSTRAINT authorizations_retirement_next_attempt_shape CHECK (
        (state_kind = 'retirement_pending') =
            (retirement_next_attempt_at_unix_ms IS NOT NULL)
        AND (retirement_next_attempt_at_unix_ms IS NULL
             OR retirement_next_attempt_at_unix_ms >= 0)
    );

DROP INDEX cyrene_workspace_device_authorization.authorizations_retirement_recovery_idx;
CREATE INDEX authorizations_retirement_recovery_idx
    ON cyrene_workspace_device_authorization.authorizations
        (retirement_next_attempt_at_unix_ms, id)
    WHERE state_kind = 'retirement_pending';

GRANT UPDATE (
    retirement_attempt_count,
    retirement_next_attempt_at_unix_ms
)
    ON cyrene_workspace_device_authorization.authorizations
    TO cyrene_workspace_device_authorization_app;

COMMENT ON COLUMN cyrene_workspace_device_authorization.authorizations.retirement_attempt_count IS
    'Persisted count of failed CA retirement attempts for this authorization.';
COMMENT ON COLUMN cyrene_workspace_device_authorization.authorizations.retirement_next_attempt_at_unix_ms IS
    'Earliest DB-clock retry time; non-NULL only while certificate retirement is pending.';
