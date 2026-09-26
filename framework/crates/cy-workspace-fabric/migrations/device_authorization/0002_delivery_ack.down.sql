-- Refuse rollback while phase-three states need the delivery/recovery fields.
DO $migration$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM cyrene_workspace_device_authorization.authorizations
        WHERE state_kind IN (
            'delivery_pending', 'delivered', 'retirement_pending',
            'delivery_expired', 'issuance_failed'
        )
    ) THEN
        RAISE EXCEPTION 'cannot downgrade active phase-three device authorizations';
    END IF;
END
$migration$;

DROP INDEX cyrene_workspace_device_authorization.authorizations_delivery_recovery_idx;
DROP INDEX cyrene_workspace_device_authorization.authorizations_retirement_recovery_idx;
DROP INDEX cyrene_workspace_device_authorization.authorizations_expiration_scan_idx;

ALTER TABLE cyrene_workspace_device_authorization.authorizations
    DROP CONSTRAINT authorizations_state_kind,
    DROP CONSTRAINT authorizations_approval_id_shape,
    DROP CONSTRAINT authorizations_state_deadline_shape;

REVOKE UPDATE (state_deadline_unix_ms)
    ON cyrene_workspace_device_authorization.authorizations
    FROM cyrene_workspace_device_authorization_app;

ALTER TABLE cyrene_workspace_device_authorization.authorizations
    DROP COLUMN state_deadline_unix_ms;

ALTER TABLE cyrene_workspace_device_authorization.authorizations
    ADD CONSTRAINT authorizations_state_kind CHECK (
        state_kind IN (
            'pending', 'awaiting_webauthn', 'verifying_webauthn', 'issuing',
            'approved', 'denied', 'consumed', 'expired'
        )
    ),
    ADD CONSTRAINT authorizations_approval_id_shape CHECK (
        (approval_id IS NULL OR octet_length(approval_id) = 16)
        AND ((state_kind IN (
            'awaiting_webauthn', 'verifying_webauthn', 'issuing', 'approved'
        )) = (approval_id IS NOT NULL))
    );

CREATE INDEX cyrene_workspace_device_authorization.authorizations_expiration_scan_idx
    ON cyrene_workspace_device_authorization.authorizations (expires_at_unix_ms, id)
    WHERE state_kind IN ('pending', 'awaiting_webauthn', 'verifying_webauthn', 'approved');
