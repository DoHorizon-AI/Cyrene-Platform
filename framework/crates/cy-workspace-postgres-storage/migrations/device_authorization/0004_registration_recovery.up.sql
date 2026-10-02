-- Persist the registration credential digest and device-code recovery epoch.
-- Existing V3 records have no recoverable registration digest and remain
-- readable with NULL; only new composite starts may create digest-bearing rows.

ALTER TABLE cyrene_workspace_device_authorization.authorizations
    ADD COLUMN registration_key_digest BYTEA,
    ADD COLUMN device_code_generation BIGINT NOT NULL DEFAULT 1,
    ADD CONSTRAINT authorizations_registration_key_digest_shape CHECK (
        registration_key_digest IS NULL OR octet_length(registration_key_digest) = 32
    ),
    ADD CONSTRAINT authorizations_device_code_generation CHECK (
        device_code_generation > 0
        AND (registration_key_digest IS NOT NULL OR device_code_generation = 1)
    );

CREATE UNIQUE INDEX authorizations_registration_key_digest_unique
    ON cyrene_workspace_device_authorization.authorizations (registration_key_digest)
    WHERE registration_key_digest IS NOT NULL;

-- Directory registration binding already permits exactly one binding per
-- device generation. Mirror that invariant at the authorization boundary so
-- concurrent composite starts cannot create two authorization rows for it.
CREATE UNIQUE INDEX authorizations_directory_generation_unique
    ON cyrene_workspace_device_authorization.authorizations
        (organization_id, workspace_id, device_id, authorization_generation);

ALTER TABLE cyrene_workspace_device_authorization.authorizations
    DROP CONSTRAINT authorizations_state_kind,
    DROP CONSTRAINT authorizations_approval_id_shape;

ALTER TABLE cyrene_workspace_device_authorization.authorizations
    ADD CONSTRAINT authorizations_state_kind CHECK (
        state_kind IN (
            'pending', 'awaiting_webauthn', 'verifying_webauthn', 'issuing',
            'delivery_pending', 'delivered', 'retirement_pending',
            'delivery_expired', 'issuance_failed', 'denied', 'consumed', 'expired',
            'superseded_for_registration_rotation', 'registration_retired'
        )
    ),
    ADD CONSTRAINT authorizations_approval_id_shape CHECK (
        (approval_id IS NULL OR octet_length(approval_id) = 16)
        AND (
            ((state_kind IN (
                'awaiting_webauthn', 'verifying_webauthn', 'issuing',
                'delivery_pending', 'delivered', 'retirement_pending',
                'delivery_expired', 'issuance_failed', 'registration_retired'
            )) = (approval_id IS NOT NULL))
            -- A rotation supersession keeps an approval ID when its old state
            -- was awaiting or verifying WebAuthn, but is also valid without one.
            OR state_kind = 'superseded_for_registration_rotation'
        )
    );

GRANT UPDATE (
    device_code_hash,
    user_code_key_version,
    user_code_mac,
    device_code_generation
)
    ON cyrene_workspace_device_authorization.authorizations
    TO cyrene_workspace_device_authorization_app;

COMMENT ON COLUMN cyrene_workspace_device_authorization.authorizations.registration_key_digest IS
    'Domain-separated digest used for exact-tuple recovery; NULL only for legacy V3 rows.';
COMMENT ON COLUMN cyrene_workspace_device_authorization.authorizations.device_code_generation IS
    'Monotonic code rotation generation; legacy rows start at one and cannot be recovered.';
