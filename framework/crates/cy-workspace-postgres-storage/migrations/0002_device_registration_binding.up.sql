-- ┌──────────────────────────────────────────────────────────────────────┐
-- │ Stable Workspace device identity and registration bindings            │
-- │ Workspace 设备稳定身份与 registration credential 绑定                  │
-- └──────────────────────────────────────────────────────────────────────┘

DO $roles$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'cyrene_workspace_device_registrar') THEN
        CREATE ROLE cyrene_workspace_device_registrar NOLOGIN;
    END IF;
END
$roles$;

ALTER ROLE cyrene_workspace_device_registrar NOLOGIN;

GRANT USAGE ON SCHEMA cyrene_workspace_directory
    TO cyrene_workspace_device_registrar;

CREATE TABLE IF NOT EXISTS cyrene_workspace_directory.workspace_device_identities (
    organization_id TEXT NOT NULL
        CHECK (btrim(organization_id) <> '' AND length(organization_id) <= 256),
    workspace_id TEXT NOT NULL
        CHECK (btrim(workspace_id) <> '' AND length(workspace_id) <= 256),
    device_id TEXT NOT NULL
        CHECK (btrim(device_id) <> '' AND length(device_id) <= 256),
    current_authorization_generation BIGINT NOT NULL
        CHECK (current_authorization_generation > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (organization_id, workspace_id, device_id)
);

CREATE TABLE IF NOT EXISTS cyrene_workspace_directory.device_registration_bindings (
    registration_key_digest BYTEA PRIMARY KEY
        CHECK (octet_length(registration_key_digest) = 32),
    binding_id UUID NOT NULL UNIQUE,
    organization_id TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    device_id TEXT NOT NULL,
    authorization_generation BIGINT NOT NULL
        CHECK (authorization_generation > 0),
    csr_sha256 BYTEA NOT NULL CHECK (octet_length(csr_sha256) = 32),
    spki_sha256 BYTEA NOT NULL CHECK (octet_length(spki_sha256) = 32),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    UNIQUE (organization_id, workspace_id, device_id, authorization_generation),
    FOREIGN KEY (organization_id, workspace_id, device_id)
        REFERENCES cyrene_workspace_directory.workspace_device_identities
            (organization_id, workspace_id, device_id)
        ON DELETE RESTRICT
);

CREATE OR REPLACE FUNCTION cyrene_workspace_directory.reject_device_registration_binding_mutation()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $registration$
BEGIN
    RAISE EXCEPTION 'Workspace device registration bindings are immutable';
END;
$registration$;

DROP TRIGGER IF EXISTS device_registration_bindings_immutable
    ON cyrene_workspace_directory.device_registration_bindings;
CREATE TRIGGER device_registration_bindings_immutable
    BEFORE UPDATE OR DELETE ON cyrene_workspace_directory.device_registration_bindings
    FOR EACH ROW EXECUTE FUNCTION
        cyrene_workspace_directory.reject_device_registration_binding_mutation();

CREATE OR REPLACE FUNCTION cyrene_workspace_directory.require_next_device_authorization_generation()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $generation$
BEGIN
    IF TG_OP = 'INSERT' AND NEW.current_authorization_generation <> 1 THEN
        RAISE EXCEPTION 'Initial Workspace device generation must be one';
    END IF;
    IF TG_OP = 'UPDATE'
       AND NEW.current_authorization_generation <> OLD.current_authorization_generation + 1 THEN
        RAISE EXCEPTION 'Workspace device authorization generation must advance by one';
    END IF;
    RETURN NEW;
END;
$generation$;

CREATE TRIGGER workspace_device_generation_monotonic
    BEFORE INSERT OR UPDATE OF current_authorization_generation
    ON cyrene_workspace_directory.workspace_device_identities
    FOR EACH ROW EXECUTE FUNCTION
        cyrene_workspace_directory.require_next_device_authorization_generation();

REVOKE ALL ON
    cyrene_workspace_directory.workspace_device_identities,
    cyrene_workspace_directory.device_registration_bindings
    FROM PUBLIC;
REVOKE ALL ON
    cyrene_workspace_directory.workspace_device_identities,
    cyrene_workspace_directory.device_registration_bindings
    FROM cyrene_workspace_directory_reader, cyrene_workspace_directory_operator;
REVOKE ALL ON
    cyrene_workspace_directory.workspace_device_identities,
    cyrene_workspace_directory.device_registration_bindings
    FROM cyrene_workspace_device_registrar;
GRANT SELECT
    ON cyrene_workspace_directory.workspace_device_identities
    TO cyrene_workspace_device_registrar;
GRANT INSERT
    (organization_id, workspace_id, device_id, current_authorization_generation)
    ON cyrene_workspace_directory.workspace_device_identities
    TO cyrene_workspace_device_registrar;
GRANT UPDATE (current_authorization_generation, updated_at)
    ON cyrene_workspace_directory.workspace_device_identities
    TO cyrene_workspace_device_registrar;
GRANT SELECT, INSERT
    ON cyrene_workspace_directory.device_registration_bindings
    TO cyrene_workspace_device_registrar;
