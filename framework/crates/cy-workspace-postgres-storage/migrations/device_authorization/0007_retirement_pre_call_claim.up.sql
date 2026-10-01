-- ┌─────────────────────────────────────────────────────────────────────┐
-- │  Migration: 0007_retirement_pre_call_claim.up.sql                    │
-- │  Schema: cyrene_workspace_device_authorization                       │
-- │  Role: Persist a lease before contacting the certificate authority. │
-- │                                                                      │
-- │  用途：调用证书颁发机构前持久化撤销租约                                  │
-- └─────────────────────────────────────────────────────────────────────┘

ALTER TABLE cyrene_workspace_device_authorization.authorizations
    ADD COLUMN retirement_claimed_until_unix_ms BIGINT,
    ADD CONSTRAINT authorizations_retirement_claim_time CHECK (
        retirement_claimed_until_unix_ms IS NULL
        OR retirement_claimed_until_unix_ms >= 0
    ),
    ADD CONSTRAINT authorizations_retirement_claim_state CHECK (
        state_kind = 'retirement_pending'
        OR retirement_claimed_until_unix_ms IS NULL
    );

DROP INDEX cyrene_workspace_device_authorization.authorizations_retirement_recovery_idx;
CREATE INDEX authorizations_retirement_recovery_idx
    ON cyrene_workspace_device_authorization.authorizations
        (retirement_next_attempt_at_unix_ms, retirement_claimed_until_unix_ms, id)
    WHERE state_kind = 'retirement_pending';

GRANT UPDATE (retirement_claimed_until_unix_ms)
    ON cyrene_workspace_device_authorization.authorizations
    TO cyrene_workspace_device_authorization_app;

COMMENT ON COLUMN cyrene_workspace_device_authorization.authorizations.retirement_claimed_until_unix_ms IS
    'DB-clock expiry for the CA retirement claim, including the current retry delay.';
