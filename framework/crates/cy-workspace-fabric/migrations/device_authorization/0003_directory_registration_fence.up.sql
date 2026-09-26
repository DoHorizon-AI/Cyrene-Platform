-- Persist the exact Directory identity generation on every authorization row.
-- This migration intentionally requires the Directory binding schema first.
DO $migration$
BEGIN
    IF to_regclass('cyrene_workspace_directory.workspace_device_identities') IS NULL
       OR to_regclass('cyrene_workspace_directory.device_registration_bindings') IS NULL THEN
        RAISE EXCEPTION 'Workspace Directory registration schema must be migrated first';
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_roles WHERE rolname = 'cyrene_workspace_device_registrar'
    ) THEN
        RAISE EXCEPTION 'Workspace Directory registrar role is unavailable';
    END IF;
    IF EXISTS (
        SELECT 1 FROM cyrene_workspace_device_authorization.authorizations
    ) THEN
        RAISE EXCEPTION 'cannot bind legacy device authorizations without Directory snapshots';
    END IF;
END
$migration$;

GRANT cyrene_workspace_device_registrar
    TO cyrene_workspace_device_authorization_app;

ALTER TABLE cyrene_workspace_device_authorization.authorizations
    ADD COLUMN registration_binding_id BYTEA NOT NULL
        CHECK (octet_length(registration_binding_id) = 16),
    ADD COLUMN device_id TEXT NOT NULL
        CHECK (btrim(device_id) <> '' AND length(device_id) <= 256),
    ADD COLUMN authorization_generation BIGINT NOT NULL
        CHECK (authorization_generation > 0);

CREATE UNIQUE INDEX authorizations_registration_binding_unique
    ON cyrene_workspace_device_authorization.authorizations (registration_binding_id);

CREATE INDEX authorizations_directory_generation_idx
    ON cyrene_workspace_device_authorization.authorizations
        (organization_id, workspace_id, device_id, authorization_generation, state_kind);

CREATE INDEX authorizations_delivery_certificate_expiry_scan_idx
    ON cyrene_workspace_device_authorization.authorizations (
        ((convert_from(state_payload, 'UTF8')::jsonb #>>
            '{state,certificate,not_after_unix_ms}')::NUMERIC), id
    )
    WHERE state_kind = 'delivery_pending';

COMMENT ON COLUMN cyrene_workspace_device_authorization.authorizations.registration_binding_id IS
    'Opaque ID of the immutable Directory registration binding used for this authorization.';
COMMENT ON COLUMN cyrene_workspace_device_authorization.authorizations.authorization_generation IS
    'Directory-owned current device generation; registered CAS validates it under identity-row lock.';
