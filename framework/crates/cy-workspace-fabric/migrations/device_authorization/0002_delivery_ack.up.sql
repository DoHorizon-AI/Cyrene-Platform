-- Persist the phase-three certificate delivery lifecycle and index its recovery work.
-- Legacy Approved rows have no delivery ID/deadline. Reinterpretation would make
-- an issued certificate appear unacknowledged without a valid replay identity.
DO $migration$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM cyrene_workspace_device_authorization.authorizations
        WHERE state_kind = 'approved'
    ) THEN
        RAISE EXCEPTION 'cannot upgrade legacy approved device authorizations safely';
    END IF;
END
$migration$;

ALTER TABLE cyrene_workspace_device_authorization.authorizations
    ADD COLUMN state_deadline_unix_ms BIGINT;

ALTER TABLE cyrene_workspace_device_authorization.authorizations
    DROP CONSTRAINT authorizations_state_kind,
    DROP CONSTRAINT authorizations_approval_id_shape;

ALTER TABLE cyrene_workspace_device_authorization.authorizations
    ADD CONSTRAINT authorizations_state_kind CHECK (
        state_kind IN (
            'pending', 'awaiting_webauthn', 'verifying_webauthn', 'issuing',
            'delivery_pending', 'delivered', 'retirement_pending',
            'delivery_expired', 'issuance_failed', 'denied', 'consumed', 'expired'
        )
    ),
    ADD CONSTRAINT authorizations_approval_id_shape CHECK (
        (approval_id IS NULL OR octet_length(approval_id) = 16)
        AND ((state_kind IN (
            'awaiting_webauthn', 'verifying_webauthn', 'issuing',
            'delivery_pending', 'delivered', 'retirement_pending',
            'delivery_expired', 'issuance_failed'
        )) = (approval_id IS NOT NULL))
    ),
    ADD CONSTRAINT authorizations_state_deadline_shape CHECK (
        (state_kind = 'delivery_pending') = (state_deadline_unix_ms IS NOT NULL)
        AND (state_deadline_unix_ms IS NULL OR state_deadline_unix_ms >= 0)
    );

COMMENT ON COLUMN cyrene_workspace_device_authorization.authorizations.state_payload IS
    'Versioned typed state, including confidential WebAuthn state and certificate delivery receipts.';
COMMENT ON COLUMN cyrene_workspace_device_authorization.authorizations.state_deadline_unix_ms IS
    'Derived delivery ACK deadline; set only while state_kind is delivery_pending.';

DROP INDEX cyrene_workspace_device_authorization.authorizations_expiration_scan_idx;
CREATE INDEX cyrene_workspace_device_authorization.authorizations_expiration_scan_idx
    ON cyrene_workspace_device_authorization.authorizations (expires_at_unix_ms, id)
    WHERE state_kind IN ('pending', 'awaiting_webauthn', 'verifying_webauthn');

CREATE INDEX cyrene_workspace_device_authorization.authorizations_delivery_recovery_idx
    ON cyrene_workspace_device_authorization.authorizations (state_deadline_unix_ms, id)
    WHERE state_kind = 'delivery_pending';

CREATE INDEX cyrene_workspace_device_authorization.authorizations_retirement_recovery_idx
    ON cyrene_workspace_device_authorization.authorizations (created_at_unix_ms, id)
    WHERE state_kind = 'retirement_pending';

GRANT UPDATE (state_deadline_unix_ms)
    ON cyrene_workspace_device_authorization.authorizations
    TO cyrene_workspace_device_authorization_app;
