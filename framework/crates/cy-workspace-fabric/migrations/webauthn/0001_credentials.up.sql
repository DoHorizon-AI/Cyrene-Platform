-- Workspace WebAuthn credential and ceremony persistence. The application
-- role is NOLOGIN and must be granted to a separately provisioned runtime
-- principal. Audit rows are append-only at both the privilege and trigger
-- layers.
DO $roles$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_roles WHERE rolname = 'cyrene_workspace_webauthn_app'
    ) THEN
        CREATE ROLE cyrene_workspace_webauthn_app NOLOGIN;
    END IF;
END
$roles$;

ALTER ROLE cyrene_workspace_webauthn_app
    NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;

CREATE SCHEMA IF NOT EXISTS cyrene_workspace_webauthn;
REVOKE ALL ON SCHEMA cyrene_workspace_webauthn FROM PUBLIC;
GRANT USAGE ON SCHEMA cyrene_workspace_webauthn TO cyrene_workspace_webauthn_app;

CREATE TABLE IF NOT EXISTS cyrene_workspace_webauthn.owners (
    issuer TEXT NOT NULL,
    subject TEXT NOT NULL,
    user_handle BYTEA NOT NULL,
    CONSTRAINT webauthn_owners_pk PRIMARY KEY (issuer, subject),
    CONSTRAINT webauthn_owners_handle_unique UNIQUE (user_handle),
    CONSTRAINT webauthn_owners_handle_length CHECK (octet_length(user_handle) BETWEEN 16 AND 64),
    CONSTRAINT webauthn_owners_identity_length CHECK (
        btrim(issuer) <> '' AND btrim(subject) <> ''
        AND length(issuer) <= 4096 AND length(subject) <= 4096
    )
);

CREATE TABLE IF NOT EXISTS cyrene_workspace_webauthn.credentials (
    credential_id BYTEA NOT NULL,
    issuer TEXT NOT NULL,
    subject TEXT NOT NULL,
    passkey_json BYTEA NOT NULL,
    signature_counter BIGINT NOT NULL,
    CONSTRAINT webauthn_credentials_pk PRIMARY KEY (credential_id),
    CONSTRAINT webauthn_credentials_id_length CHECK (octet_length(credential_id) BETWEEN 1 AND 1024),
    CONSTRAINT webauthn_credentials_counter CHECK (signature_counter BETWEEN 0 AND 4294967295),
    CONSTRAINT webauthn_credentials_passkey_length CHECK (octet_length(passkey_json) BETWEEN 2 AND 65536),
    CONSTRAINT webauthn_credentials_owner_fk FOREIGN KEY (issuer, subject)
        REFERENCES cyrene_workspace_webauthn.owners (issuer, subject)
);
CREATE INDEX IF NOT EXISTS webauthn_credentials_owner_idx
    ON cyrene_workspace_webauthn.credentials (issuer, subject, credential_id);

CREATE TABLE IF NOT EXISTS cyrene_workspace_webauthn.authentication_ceremonies (
    approval_id BYTEA NOT NULL,
    authorization_id BYTEA NOT NULL,
    issuer TEXT NOT NULL,
    subject TEXT NOT NULL,
    user_handle BYTEA NOT NULL,
    context_sha256 BYTEA NOT NULL,
    opaque_state BYTEA NOT NULL,
    state_sha256 BYTEA NOT NULL,
    credential_set_sha256 BYTEA NOT NULL,
    expires_at_unix_ms BIGINT NOT NULL,
    status SMALLINT NOT NULL DEFAULT 0,
    assertion_sha256 BYTEA,
    credential_id BYTEA,
    CONSTRAINT webauthn_authentication_ceremonies_pk PRIMARY KEY (approval_id),
    CONSTRAINT webauthn_authentication_ceremonies_approval_length CHECK (octet_length(approval_id) = 16),
    CONSTRAINT webauthn_authentication_ceremonies_authorization_length CHECK (octet_length(authorization_id) = 16),
    CONSTRAINT webauthn_authentication_ceremonies_hashes CHECK (
        octet_length(context_sha256) = 32 AND octet_length(state_sha256) = 32
        AND octet_length(credential_set_sha256) = 32
    ),
    CONSTRAINT webauthn_authentication_ceremonies_state_length CHECK (octet_length(opaque_state) BETWEEN 1 AND 65536),
    CONSTRAINT webauthn_authentication_ceremonies_handle_length CHECK (octet_length(user_handle) BETWEEN 16 AND 64),
    CONSTRAINT webauthn_authentication_ceremonies_expiry CHECK (expires_at_unix_ms > 0),
    CONSTRAINT webauthn_authentication_ceremonies_status CHECK (status IN (0, 1)),
    CONSTRAINT webauthn_authentication_ceremonies_consumption CHECK (
        (status = 0 AND assertion_sha256 IS NULL AND credential_id IS NULL)
        OR (status = 1 AND assertion_sha256 IS NOT NULL
            AND octet_length(assertion_sha256) = 32 AND credential_id IS NOT NULL)
    ),
    CONSTRAINT webauthn_authentication_ceremonies_owner_fk FOREIGN KEY (issuer, subject)
        REFERENCES cyrene_workspace_webauthn.owners (issuer, subject)
);
CREATE INDEX IF NOT EXISTS webauthn_authentication_expiry_idx
    ON cyrene_workspace_webauthn.authentication_ceremonies (expires_at_unix_ms)
    WHERE status = 0;

CREATE TABLE IF NOT EXISTS cyrene_workspace_webauthn.registration_ceremonies (
    registration_id BYTEA NOT NULL,
    issuer TEXT NOT NULL,
    subject TEXT NOT NULL,
    user_handle BYTEA NOT NULL,
    opaque_state BYTEA NOT NULL,
    state_sha256 BYTEA NOT NULL,
    expires_at_unix_ms BIGINT NOT NULL,
    status SMALLINT NOT NULL DEFAULT 0,
    response_sha256 BYTEA,
    credential_id BYTEA,
    CONSTRAINT webauthn_registration_ceremonies_pk PRIMARY KEY (registration_id),
    CONSTRAINT webauthn_registration_ceremonies_id_length CHECK (octet_length(registration_id) = 16),
    CONSTRAINT webauthn_registration_ceremonies_hash_length CHECK (octet_length(state_sha256) = 32),
    CONSTRAINT webauthn_registration_ceremonies_state_length CHECK (octet_length(opaque_state) BETWEEN 1 AND 65536),
    CONSTRAINT webauthn_registration_ceremonies_handle_length CHECK (octet_length(user_handle) BETWEEN 16 AND 64),
    CONSTRAINT webauthn_registration_ceremonies_expiry CHECK (expires_at_unix_ms > 0),
    CONSTRAINT webauthn_registration_ceremonies_status CHECK (status IN (0, 1)),
    CONSTRAINT webauthn_registration_ceremonies_consumption CHECK (
        (status = 0 AND response_sha256 IS NULL AND credential_id IS NULL)
        OR (status = 1 AND response_sha256 IS NOT NULL
            AND octet_length(response_sha256) = 32 AND credential_id IS NOT NULL)
    ),
    CONSTRAINT webauthn_registration_ceremonies_owner_fk FOREIGN KEY (issuer, subject)
        REFERENCES cyrene_workspace_webauthn.owners (issuer, subject)
);
CREATE INDEX IF NOT EXISTS webauthn_registration_expiry_idx
    ON cyrene_workspace_webauthn.registration_ceremonies (expires_at_unix_ms)
    WHERE status = 0;

CREATE TABLE IF NOT EXISTS cyrene_workspace_webauthn.audit_events (
    sequence BIGSERIAL NOT NULL,
    occurred_at_unix_ms BIGINT NOT NULL,
    issuer TEXT NOT NULL,
    subject TEXT NOT NULL,
    action TEXT NOT NULL,
    correlation_id BYTEA,
    credential_id_sha256 BYTEA,
    previous_counter BIGINT,
    new_counter BIGINT,
    user_verified BOOLEAN,
    backup_eligible BOOLEAN,
    backup_state BOOLEAN,
    reason_code TEXT,
    CONSTRAINT webauthn_audit_events_pk PRIMARY KEY (sequence),
    CONSTRAINT webauthn_audit_events_time CHECK (occurred_at_unix_ms >= 0),
    CONSTRAINT webauthn_audit_events_action CHECK (action IN (
        'AUTHENTICATION_CHALLENGE_ISSUED', 'CREDENTIAL_REGISTRATION_STARTED',
        'CREDENTIAL_REGISTERED', 'ASSERTION_ACCEPTED', 'SECURITY_REJECTED',
        'CREDENTIAL_REVOKED'
    )),
    CONSTRAINT webauthn_audit_events_reason CHECK (reason_code IS NULL OR reason_code IN (
        'STATE_OR_OWNER_MISMATCH', 'CEREMONY_EXPIRED', 'INVALID_ASSERTION',
        'USER_HANDLE_MISMATCH', 'USER_VERIFICATION_MISSING',
        'BACKUP_CREDENTIAL_DISALLOWED', 'SIGNATURE_COUNTER_REPLAY',
        'CREDENTIAL_CHANGED_OR_REVOKED', 'INVALID_REGISTRATION',
        'USER_REQUEST', 'SUSPECTED_CLONING', 'ACCOUNT_RECOVERY', 'POLICY_CHANGE'
    )),
    CONSTRAINT webauthn_audit_events_correlation_length CHECK (
        correlation_id IS NULL OR octet_length(correlation_id) = 16
    ),
    CONSTRAINT webauthn_audit_events_credential_hash_length CHECK (
        credential_id_sha256 IS NULL OR octet_length(credential_id_sha256) = 32
    ),
    CONSTRAINT webauthn_audit_events_counters CHECK (
        (previous_counter IS NULL OR previous_counter BETWEEN 0 AND 4294967295)
        AND (new_counter IS NULL OR new_counter BETWEEN 0 AND 4294967295)
    )
);
CREATE INDEX IF NOT EXISTS webauthn_audit_owner_idx
    ON cyrene_workspace_webauthn.audit_events (issuer, subject, sequence);

CREATE OR REPLACE FUNCTION cyrene_workspace_webauthn.reject_audit_mutation()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'WebAuthn audit events are append-only';
END;
$$;
CREATE TRIGGER webauthn_audit_no_update
    BEFORE UPDATE ON cyrene_workspace_webauthn.audit_events
    FOR EACH ROW EXECUTE FUNCTION cyrene_workspace_webauthn.reject_audit_mutation();
CREATE TRIGGER webauthn_audit_no_delete
    BEFORE DELETE ON cyrene_workspace_webauthn.audit_events
    FOR EACH ROW EXECUTE FUNCTION cyrene_workspace_webauthn.reject_audit_mutation();

REVOKE ALL ON ALL TABLES IN SCHEMA cyrene_workspace_webauthn FROM PUBLIC;
REVOKE ALL ON ALL SEQUENCES IN SCHEMA cyrene_workspace_webauthn FROM PUBLIC;
GRANT SELECT, INSERT ON cyrene_workspace_webauthn.owners TO cyrene_workspace_webauthn_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON cyrene_workspace_webauthn.credentials
    TO cyrene_workspace_webauthn_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON cyrene_workspace_webauthn.authentication_ceremonies
    TO cyrene_workspace_webauthn_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON cyrene_workspace_webauthn.registration_ceremonies
    TO cyrene_workspace_webauthn_app;
GRANT SELECT, INSERT ON cyrene_workspace_webauthn.audit_events TO cyrene_workspace_webauthn_app;
GRANT USAGE, SELECT ON SEQUENCE cyrene_workspace_webauthn.audit_events_sequence_seq
    TO cyrene_workspace_webauthn_app;

ALTER DEFAULT PRIVILEGES IN SCHEMA cyrene_workspace_webauthn REVOKE ALL ON TABLES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES IN SCHEMA cyrene_workspace_webauthn REVOKE ALL ON SEQUENCES FROM PUBLIC;
