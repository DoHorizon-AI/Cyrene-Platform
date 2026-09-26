-- Durable Workspace device authorization records.
-- Raw device and user codes are never columns in this schema. The user code
-- index is over the configured HMAC key version and its opaque 32-byte MAC.

DO $roles$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_roles WHERE rolname = 'cyrene_workspace_device_authorization_app'
    ) THEN
        CREATE ROLE cyrene_workspace_device_authorization_app NOLOGIN;
    END IF;
END
$roles$;

ALTER ROLE cyrene_workspace_device_authorization_app NOLOGIN;

CREATE SCHEMA IF NOT EXISTS cyrene_workspace_device_authorization;
REVOKE ALL ON SCHEMA cyrene_workspace_device_authorization FROM PUBLIC;
GRANT USAGE ON SCHEMA cyrene_workspace_device_authorization
    TO cyrene_workspace_device_authorization_app;

CREATE TABLE IF NOT EXISTS cyrene_workspace_device_authorization.authorizations (
    id BYTEA NOT NULL,
    device_code_hash BYTEA NOT NULL,
    user_code_key_version BIGINT NOT NULL,
    user_code_mac BYTEA NOT NULL,
    organization_id TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    csr_der BYTEA NOT NULL,
    csr_sha256 BYTEA NOT NULL,
    spki_sha256 BYTEA NOT NULL,
    created_at_unix_ms BIGINT NOT NULL,
    expires_at_unix_ms BIGINT NOT NULL,
    poll_interval_ms BIGINT NOT NULL,
    last_poll_at_unix_ms BIGINT,
    revision BIGINT NOT NULL,
    state_kind TEXT NOT NULL,
    approval_id BYTEA,
    -- Versioned JSON encoding of the typed state, including opaque WebAuthn
    -- verifier state and the issued certificate while it is Approved.
    state_payload BYTEA NOT NULL,
    CONSTRAINT authorizations_pk PRIMARY KEY (id),
    CONSTRAINT authorizations_device_code_hash_unique UNIQUE (device_code_hash),
    CONSTRAINT authorizations_user_code_digest_unique
        UNIQUE (user_code_key_version, user_code_mac),
    CONSTRAINT authorizations_id_length CHECK (octet_length(id) = 16),
    CONSTRAINT authorizations_device_code_hash_length CHECK (octet_length(device_code_hash) = 32),
    CONSTRAINT authorizations_user_code_key_version CHECK (user_code_key_version > 0),
    CONSTRAINT authorizations_user_code_mac_length CHECK (octet_length(user_code_mac) = 32),
    CONSTRAINT authorizations_organization CHECK (
        btrim(organization_id) <> '' AND length(organization_id) <= 256
    ),
    CONSTRAINT authorizations_workspace CHECK (
        btrim(workspace_id) <> '' AND length(workspace_id) <= 256
    ),
    CONSTRAINT authorizations_csr_length CHECK (octet_length(csr_der) BETWEEN 1 AND 16384),
    CONSTRAINT authorizations_csr_sha256_length CHECK (octet_length(csr_sha256) = 32),
    CONSTRAINT authorizations_spki_sha256_length CHECK (octet_length(spki_sha256) = 32),
    CONSTRAINT authorizations_time_range CHECK (
        created_at_unix_ms >= 0 AND expires_at_unix_ms > created_at_unix_ms
    ),
    CONSTRAINT authorizations_poll_interval CHECK (poll_interval_ms > 0),
    CONSTRAINT authorizations_revision CHECK (revision >= 0),
    CONSTRAINT authorizations_state_kind CHECK (
        state_kind IN (
            'pending', 'awaiting_webauthn', 'verifying_webauthn', 'issuing',
            'approved', 'denied', 'consumed', 'expired'
        )
    ),
    CONSTRAINT authorizations_approval_id_shape CHECK (
        (approval_id IS NULL OR octet_length(approval_id) = 16)
        AND ((state_kind IN ('awaiting_webauthn', 'verifying_webauthn', 'issuing', 'approved'))
             = (approval_id IS NOT NULL))
    ),
    CONSTRAINT authorizations_state_payload_length CHECK (
        octet_length(state_payload) BETWEEN 2 AND 33554432
    )
);

CREATE UNIQUE INDEX IF NOT EXISTS authorizations_approval_id_unique
    ON cyrene_workspace_device_authorization.authorizations (approval_id)
    WHERE approval_id IS NOT NULL;

-- Expiration workers read the short-lived states in due-time order.
CREATE INDEX IF NOT EXISTS authorizations_expiration_scan_idx
    ON cyrene_workspace_device_authorization.authorizations (expires_at_unix_ms, id)
    WHERE state_kind IN ('pending', 'awaiting_webauthn', 'verifying_webauthn', 'approved');

-- Issuing remains recoverable beyond the normal authorization TTL because a
-- CA may have committed before a caller lost its response.
CREATE INDEX IF NOT EXISTS authorizations_issuance_recovery_idx
    ON cyrene_workspace_device_authorization.authorizations (created_at_unix_ms, id)
    WHERE state_kind = 'issuing';

REVOKE ALL ON ALL TABLES IN SCHEMA cyrene_workspace_device_authorization FROM PUBLIC;
GRANT SELECT, INSERT
    ON cyrene_workspace_device_authorization.authorizations
    TO cyrene_workspace_device_authorization_app;
GRANT UPDATE (
    poll_interval_ms,
    last_poll_at_unix_ms,
    revision,
    state_kind,
    approval_id,
    state_payload
)
    ON cyrene_workspace_device_authorization.authorizations
    TO cyrene_workspace_device_authorization_app;

ALTER DEFAULT PRIVILEGES IN SCHEMA cyrene_workspace_device_authorization
    REVOKE ALL ON TABLES FROM PUBLIC;
