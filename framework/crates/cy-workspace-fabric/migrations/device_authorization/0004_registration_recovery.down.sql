-- Do not discard recovery, rotation, or certificate-retirement state.
DO $migration$
BEGIN
    IF EXISTS (
        SELECT 1 FROM cyrene_workspace_device_authorization.authorizations
        WHERE registration_key_digest IS NOT NULL
           OR device_code_generation <> 1
           OR state_kind IN (
                'superseded_for_registration_rotation', 'registration_retired'
           )
           OR convert_from(state_payload, 'UTF8')::JSONB ->> 'format_version' = '4'
    ) THEN
        RAISE EXCEPTION 'cannot remove registration recovery state while it is in use';
    END IF;
END
$migration$;

REVOKE UPDATE (
    device_code_hash,
    user_code_key_version,
    user_code_mac,
    device_code_generation
)
    ON cyrene_workspace_device_authorization.authorizations
    FROM cyrene_workspace_device_authorization_app;

DROP INDEX cyrene_workspace_device_authorization.authorizations_directory_generation_unique;
DROP INDEX cyrene_workspace_device_authorization.authorizations_registration_key_digest_unique;

ALTER TABLE cyrene_workspace_device_authorization.authorizations
    DROP CONSTRAINT authorizations_approval_id_shape,
    DROP CONSTRAINT authorizations_state_kind;

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
    );

ALTER TABLE cyrene_workspace_device_authorization.authorizations
    DROP CONSTRAINT authorizations_device_code_generation,
    DROP CONSTRAINT authorizations_registration_key_digest_shape,
    DROP COLUMN device_code_generation,
    DROP COLUMN registration_key_digest;
