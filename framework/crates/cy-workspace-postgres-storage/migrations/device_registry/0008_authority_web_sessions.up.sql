-- ┌─────────────────────────────────────────────────────────────────────┐
-- │ Durable web bearer sessions for Authority credentials               │
-- │ Authority credential 的持久 Web bearer session                       │
-- └─────────────────────────────────────────────────────────────────────┘

DO $owner_membership$
BEGIN
    EXECUTE format(
        'GRANT cyrene_workspace_device_registry_owner TO %I',
        current_user
    );
END
$owner_membership$;

SET ROLE cyrene_workspace_device_registry_owner;

CREATE SEQUENCE IF NOT EXISTS
    cyrene_workspace_device_registry.authority_web_session_generation_seq
    AS BIGINT MINVALUE 1 NO CYCLE;

CREATE TABLE IF NOT EXISTS cyrene_workspace_device_registry.authority_web_sessions (
    session_id UUID PRIMARY KEY,
    organization_id VARCHAR(256) NOT NULL,
    workspace_id VARCHAR(256) NOT NULL,
    principal_issuer TEXT NOT NULL,
    principal_subject TEXT NOT NULL,
    bearer_token_sha256 BYTEA NOT NULL
        CHECK (octet_length(bearer_token_sha256) = 32),
    session_generation BIGINT NOT NULL UNIQUE
        DEFAULT nextval('cyrene_workspace_device_registry.authority_web_session_generation_seq'),
    issued_at_unix_ms BIGINT NOT NULL CHECK (issued_at_unix_ms > 0),
    expires_at_unix_ms BIGINT NOT NULL CHECK (expires_at_unix_ms > issued_at_unix_ms),
    revoked_at_unix_ms BIGINT,
    created_at_unix_ms BIGINT NOT NULL,
    last_seen_at_unix_ms BIGINT NOT NULL,
    CONSTRAINT authority_web_sessions_subject_scope_unique UNIQUE (
        organization_id,
        workspace_id,
        principal_issuer,
        principal_subject,
        bearer_token_sha256
    ),
    CONSTRAINT authority_web_sessions_revocation_check CHECK (
        revoked_at_unix_ms IS NULL OR revoked_at_unix_ms >= issued_at_unix_ms
    )
);

CREATE INDEX IF NOT EXISTS authority_web_sessions_active_subject_idx
    ON cyrene_workspace_device_registry.authority_web_sessions (
        organization_id,
        workspace_id,
        principal_issuer,
        principal_subject,
        session_generation
    )
    WHERE revoked_at_unix_ms IS NULL;

REVOKE ALL ON TABLE cyrene_workspace_device_registry.authority_web_sessions FROM PUBLIC;
GRANT SELECT, INSERT
    ON cyrene_workspace_device_registry.authority_web_sessions
    TO cyrene_workspace_device_registry_app;
GRANT UPDATE (last_seen_at_unix_ms, revoked_at_unix_ms)
    ON cyrene_workspace_device_registry.authority_web_sessions
    TO cyrene_workspace_device_registry_app;
REVOKE ALL ON SEQUENCE
    cyrene_workspace_device_registry.authority_web_session_generation_seq FROM PUBLIC;
GRANT USAGE, SELECT
    ON SEQUENCE cyrene_workspace_device_registry.authority_web_session_generation_seq
    TO cyrene_workspace_device_registry_app;

COMMENT ON COLUMN cyrene_workspace_device_registry.authority_web_sessions.bearer_token_sha256 IS
    'SHA-256 of a verified bearer token; raw access tokens are never persisted.';
COMMENT ON COLUMN cyrene_workspace_device_registry.authority_web_sessions.session_generation IS
    'Database-allocated durable generation; never supplied by a caller.';

RESET ROLE;

DO $owner_membership_cleanup$
BEGIN
    EXECUTE format(
        'REVOKE cyrene_workspace_device_registry_owner FROM %I',
        current_user
    );
END
$owner_membership_cleanup$;
