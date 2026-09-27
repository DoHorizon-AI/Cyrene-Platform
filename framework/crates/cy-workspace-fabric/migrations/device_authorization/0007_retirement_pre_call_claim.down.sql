-- ┌─────────────────────────────────────────────────────────────────────┐
-- │  Migration: 0007_retirement_pre_call_claim.down.sql                  │
-- │  Schema: cyrene_workspace_device_authorization                       │
-- │  Role: Remove pre-call certificate-retirement leases.                │
-- │                                                                      │
-- │  用途：移除调用前设备证书撤销租约                                       │
-- └─────────────────────────────────────────────────────────────────────┘

DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM cyrene_workspace_device_authorization.authorizations
        WHERE retirement_claimed_until_unix_ms >
              FLOOR(EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT
    ) THEN
        RAISE EXCEPTION 'cannot remove an active certificate-retirement claim';
    END IF;
END
$$;

DROP INDEX cyrene_workspace_device_authorization.authorizations_retirement_recovery_idx;
ALTER TABLE cyrene_workspace_device_authorization.authorizations
    DROP CONSTRAINT authorizations_retirement_claim_state,
    DROP CONSTRAINT authorizations_retirement_claim_time,
    DROP COLUMN retirement_claimed_until_unix_ms;

CREATE INDEX authorizations_retirement_recovery_idx
    ON cyrene_workspace_device_authorization.authorizations
        (retirement_next_attempt_at_unix_ms, id)
    WHERE state_kind = 'retirement_pending';
