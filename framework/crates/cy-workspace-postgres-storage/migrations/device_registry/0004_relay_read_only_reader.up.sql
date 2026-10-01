-- Relay receives a separate read-only grant set. Deployment provisions a LOGIN role
-- and grants it membership in this fixed NOLOGIN group role.

DO $roles$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_roles
        WHERE rolname = 'cyrene_workspace_device_registry_relay_reader'
    ) THEN
        CREATE ROLE cyrene_workspace_device_registry_relay_reader NOLOGIN;
    END IF;
END
$roles$;

ALTER ROLE cyrene_workspace_device_registry_relay_reader
    NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS NOINHERIT;

REVOKE ALL ON SCHEMA
    cyrene_workspace_device_registry,
    cyrene_workspace_device_authorization,
    cyrene_workspace_directory
    FROM cyrene_workspace_device_registry_relay_reader;
GRANT USAGE ON SCHEMA
    cyrene_workspace_device_registry,
    cyrene_workspace_device_authorization,
    cyrene_workspace_directory
    TO cyrene_workspace_device_registry_relay_reader;

GRANT SELECT (
    authorization_id, registration_binding_id, organization_id, workspace_id, device_id,
    authorization_generation, delivery_id, certificate_sha256, csr_sha256, spki_sha256,
    serial_number, not_after_unix_ms, delivery_deadline_unix_ms, state,
    acknowledged_at_unix_ms
) ON cyrene_workspace_device_registry.certificate_records
    TO cyrene_workspace_device_registry_relay_reader;
GRANT SELECT (
    id, registration_binding_id, organization_id, workspace_id, device_id,
    approval_id, authorization_generation, csr_sha256, spki_sha256, state_kind,
    state_deadline_unix_ms, state_payload
) ON cyrene_workspace_device_authorization.authorizations
    TO cyrene_workspace_device_registry_relay_reader;
GRANT SELECT (
    binding_id, organization_id, workspace_id, device_id,
    authorization_generation, csr_sha256, spki_sha256
) ON cyrene_workspace_directory.device_registration_bindings
    TO cyrene_workspace_device_registry_relay_reader;
GRANT SELECT (
    organization_id, workspace_id, device_id, current_authorization_generation
) ON cyrene_workspace_directory.workspace_device_identities
    TO cyrene_workspace_device_registry_relay_reader;
GRANT EXECUTE ON FUNCTION cyrene_workspace_device_registry.relay_dispatch_fence(
    TEXT, TEXT, TEXT, UUID, BIGINT, BYTEA, BYTEA, BYTEA, BYTEA, BIGINT
) TO cyrene_workspace_device_registry_relay_reader;
