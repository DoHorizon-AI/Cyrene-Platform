-- ┌─────────────────────────────────────────────────────────────────────┐
-- │ Workspace WebAuthn HTTP session-binding persistence                 │
-- │ Creates an isolated schema and least-privilege runtime role.         │
-- │                                                                     │
-- │ Workspace WebAuthn HTTP session binding 持久化。                         │
-- └─────────────────────────────────────────────────────────────────────┘

DO $roles$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_roles WHERE rolname = 'cyrene_workspace_webauthn_http_binding_app'
    ) THEN
        CREATE ROLE cyrene_workspace_webauthn_http_binding_app NOLOGIN;
    END IF;
END
$roles$;

ALTER ROLE cyrene_workspace_webauthn_http_binding_app
    NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;

CREATE SCHEMA IF NOT EXISTS cyrene_workspace_webauthn_http_binding;
REVOKE ALL ON SCHEMA cyrene_workspace_webauthn_http_binding FROM PUBLIC;
GRANT USAGE ON SCHEMA cyrene_workspace_webauthn_http_binding
    TO cyrene_workspace_webauthn_http_binding_app;

CREATE TABLE IF NOT EXISTS cyrene_workspace_webauthn_http_binding.session_bindings (
    purpose SMALLINT NOT NULL,
    ceremony_id BYTEA NOT NULL,
    owner_issuer TEXT NOT NULL,
    owner_subject TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    session_binding_hmac BYTEA NOT NULL,
    expires_at_unix_ms BIGINT NOT NULL,
    finish_status SMALLINT NOT NULL DEFAULT 0,
    finish_response_sha256 BYTEA,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    finish_reserved_at TIMESTAMPTZ,
    finished_at TIMESTAMPTZ,
    CONSTRAINT webauthn_http_binding_pk PRIMARY KEY (purpose, ceremony_id),
    CONSTRAINT webauthn_http_binding_purpose CHECK (purpose IN (0, 1)),
    CONSTRAINT webauthn_http_binding_ceremony_id_length CHECK (octet_length(ceremony_id) = 16),
    CONSTRAINT webauthn_http_binding_owner_length CHECK (
        btrim(owner_issuer) <> '' AND btrim(owner_subject) <> ''
        AND length(owner_issuer) <= 4096 AND length(owner_subject) <= 4096
    ),
    CONSTRAINT webauthn_http_binding_scope_length CHECK (
        btrim(organization_id) <> '' AND length(organization_id) <= 4096
        AND btrim(workspace_id) <> '' AND length(workspace_id) <= 128
    ),
    CONSTRAINT webauthn_http_binding_session_digest_length CHECK (
        octet_length(session_binding_hmac) = 32
    ),
    CONSTRAINT webauthn_http_binding_expiry CHECK (expires_at_unix_ms > 0),
    CONSTRAINT webauthn_http_binding_finish_status CHECK (finish_status IN (0, 1, 2)),
    CONSTRAINT webauthn_http_binding_finish_state CHECK (
        (finish_status = 0 AND finish_response_sha256 IS NULL
            AND finish_reserved_at IS NULL AND finished_at IS NULL)
        OR (finish_status = 1 AND finish_response_sha256 IS NOT NULL
            AND octet_length(finish_response_sha256) = 32
            AND finish_reserved_at IS NOT NULL AND finished_at IS NULL)
        OR (finish_status = 2 AND finish_response_sha256 IS NOT NULL
            AND octet_length(finish_response_sha256) = 32
            AND finish_reserved_at IS NOT NULL AND finished_at IS NOT NULL)
    )
);
CREATE INDEX IF NOT EXISTS webauthn_http_binding_expiry_idx
    ON cyrene_workspace_webauthn_http_binding.session_bindings (expires_at_unix_ms);

CREATE TABLE IF NOT EXISTS cyrene_workspace_webauthn_http_binding.session_binding_audit (
    sequence BIGSERIAL NOT NULL,
    purpose SMALLINT NOT NULL,
    ceremony_id BYTEA NOT NULL,
    event TEXT NOT NULL,
    owner_issuer TEXT NOT NULL,
    owner_subject TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    response_sha256 BYTEA,
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT webauthn_http_binding_audit_pk PRIMARY KEY (sequence),
    CONSTRAINT webauthn_http_binding_audit_purpose CHECK (purpose IN (0, 1)),
    CONSTRAINT webauthn_http_binding_audit_ceremony_id_length CHECK (
        octet_length(ceremony_id) = 16
    ),
    CONSTRAINT webauthn_http_binding_audit_event CHECK (
        event IN ('CEREMONY_BOUND', 'FINISH_RESERVED', 'FINISH_COMPLETED')
    ),
    CONSTRAINT webauthn_http_binding_audit_owner_length CHECK (
        btrim(owner_issuer) <> '' AND btrim(owner_subject) <> ''
    ),
    CONSTRAINT webauthn_http_binding_audit_scope_length CHECK (
        btrim(organization_id) <> '' AND btrim(workspace_id) <> ''
    ),
    CONSTRAINT webauthn_http_binding_audit_digest_length CHECK (
        response_sha256 IS NULL OR octet_length(response_sha256) = 32
    )
);
CREATE INDEX IF NOT EXISTS webauthn_http_binding_audit_scope_idx
    ON cyrene_workspace_webauthn_http_binding.session_binding_audit
       (owner_issuer, owner_subject, sequence);

CREATE OR REPLACE FUNCTION cyrene_workspace_webauthn_http_binding.guard_session_binding_update()
RETURNS trigger
LANGUAGE plpgsql
SET search_path = pg_catalog, cyrene_workspace_webauthn_http_binding
AS $guard$
BEGIN
    IF NEW.purpose IS DISTINCT FROM OLD.purpose
        OR NEW.ceremony_id IS DISTINCT FROM OLD.ceremony_id
        OR NEW.owner_issuer IS DISTINCT FROM OLD.owner_issuer
        OR NEW.owner_subject IS DISTINCT FROM OLD.owner_subject
        OR NEW.organization_id IS DISTINCT FROM OLD.organization_id
        OR NEW.workspace_id IS DISTINCT FROM OLD.workspace_id
        OR NEW.session_binding_hmac IS DISTINCT FROM OLD.session_binding_hmac
        OR NEW.expires_at_unix_ms IS DISTINCT FROM OLD.expires_at_unix_ms
        OR NEW.created_at IS DISTINCT FROM OLD.created_at THEN
        RAISE EXCEPTION 'WebAuthn HTTP session binding fields are immutable';
    END IF;

    IF OLD.finish_status = 0 AND NEW.finish_status = 1
        AND OLD.finish_response_sha256 IS NULL
        AND NEW.finish_response_sha256 IS NOT NULL
        AND octet_length(NEW.finish_response_sha256) = 32
        AND NEW.finished_at IS NULL THEN
        NEW.finish_reserved_at := clock_timestamp();
        NEW.finished_at := NULL;
        RETURN NEW;
    END IF;

    IF OLD.finish_status = 1 AND NEW.finish_status = 2
        AND NEW.finish_response_sha256 IS NOT DISTINCT FROM OLD.finish_response_sha256
        AND OLD.finish_reserved_at IS NOT NULL
        AND NEW.finished_at IS NULL THEN
        NEW.finish_reserved_at := OLD.finish_reserved_at;
        NEW.finished_at := clock_timestamp();
        RETURN NEW;
    END IF;

    RAISE EXCEPTION 'WebAuthn HTTP session finish transition is invalid';
END;
$guard$;

CREATE OR REPLACE FUNCTION cyrene_workspace_webauthn_http_binding.audit_session_binding_change()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, cyrene_workspace_webauthn_http_binding
AS $audit$
DECLARE
    audit_event TEXT;
BEGIN
    IF TG_OP = 'INSERT' THEN
        audit_event := 'CEREMONY_BOUND';
    ELSIF OLD.finish_status = 0 AND NEW.finish_status = 1 THEN
        audit_event := 'FINISH_RESERVED';
    ELSIF OLD.finish_status = 1 AND NEW.finish_status = 2 THEN
        audit_event := 'FINISH_COMPLETED';
    ELSE
        RAISE EXCEPTION 'WebAuthn HTTP session audit transition is invalid';
    END IF;

    INSERT INTO cyrene_workspace_webauthn_http_binding.session_binding_audit (
        purpose, ceremony_id, event, owner_issuer, owner_subject,
        organization_id, workspace_id, response_sha256
    ) VALUES (
        NEW.purpose, NEW.ceremony_id, audit_event, NEW.owner_issuer, NEW.owner_subject,
        NEW.organization_id, NEW.workspace_id, NEW.finish_response_sha256
    );
    RETURN NULL;
END;
$audit$;

CREATE OR REPLACE FUNCTION cyrene_workspace_webauthn_http_binding.reject_session_audit_mutation()
RETURNS trigger
LANGUAGE plpgsql
SET search_path = pg_catalog, cyrene_workspace_webauthn_http_binding
AS $immutable$
BEGIN
    RAISE EXCEPTION 'WebAuthn HTTP session-binding audit rows are append-only';
END;
$immutable$;

REVOKE ALL ON FUNCTION
    cyrene_workspace_webauthn_http_binding.guard_session_binding_update() FROM PUBLIC;
REVOKE ALL ON FUNCTION
    cyrene_workspace_webauthn_http_binding.audit_session_binding_change() FROM PUBLIC;
REVOKE ALL ON FUNCTION
    cyrene_workspace_webauthn_http_binding.reject_session_audit_mutation() FROM PUBLIC;

CREATE TRIGGER webauthn_http_binding_guard_update
    BEFORE UPDATE ON cyrene_workspace_webauthn_http_binding.session_bindings
    FOR EACH ROW EXECUTE FUNCTION
        cyrene_workspace_webauthn_http_binding.guard_session_binding_update();
CREATE TRIGGER webauthn_http_binding_audit_insert
    AFTER INSERT ON cyrene_workspace_webauthn_http_binding.session_bindings
    FOR EACH ROW EXECUTE FUNCTION
        cyrene_workspace_webauthn_http_binding.audit_session_binding_change();
CREATE TRIGGER webauthn_http_binding_audit_update
    AFTER UPDATE ON cyrene_workspace_webauthn_http_binding.session_bindings
    FOR EACH ROW EXECUTE FUNCTION
        cyrene_workspace_webauthn_http_binding.audit_session_binding_change();
CREATE TRIGGER webauthn_http_binding_audit_no_update
    BEFORE UPDATE ON cyrene_workspace_webauthn_http_binding.session_binding_audit
    FOR EACH ROW EXECUTE FUNCTION
        cyrene_workspace_webauthn_http_binding.reject_session_audit_mutation();
CREATE TRIGGER webauthn_http_binding_audit_no_delete
    BEFORE DELETE ON cyrene_workspace_webauthn_http_binding.session_binding_audit
    FOR EACH ROW EXECUTE FUNCTION
        cyrene_workspace_webauthn_http_binding.reject_session_audit_mutation();

REVOKE ALL ON ALL TABLES IN SCHEMA cyrene_workspace_webauthn_http_binding FROM PUBLIC;
REVOKE ALL ON ALL SEQUENCES IN SCHEMA cyrene_workspace_webauthn_http_binding FROM PUBLIC;
GRANT SELECT ON cyrene_workspace_webauthn_http_binding.session_bindings
    TO cyrene_workspace_webauthn_http_binding_app;
GRANT INSERT (
    purpose, ceremony_id, owner_issuer, owner_subject, organization_id,
    workspace_id, session_binding_hmac, expires_at_unix_ms
) ON cyrene_workspace_webauthn_http_binding.session_bindings
    TO cyrene_workspace_webauthn_http_binding_app;
GRANT UPDATE (finish_status, finish_response_sha256)
    ON cyrene_workspace_webauthn_http_binding.session_bindings
    TO cyrene_workspace_webauthn_http_binding_app;
ALTER DEFAULT PRIVILEGES IN SCHEMA cyrene_workspace_webauthn_http_binding
    REVOKE ALL ON TABLES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES IN SCHEMA cyrene_workspace_webauthn_http_binding
    REVOKE ALL ON SEQUENCES FROM PUBLIC;
