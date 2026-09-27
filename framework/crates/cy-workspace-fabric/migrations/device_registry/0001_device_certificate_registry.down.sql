DO $rollback$
BEGIN
    IF EXISTS (
        SELECT 1 FROM cyrene_workspace_device_registry.certificate_records
    ) THEN
        RAISE EXCEPTION 'cannot remove Workspace device certificate registry records';
    END IF;
END
$rollback$;

DO $owner_membership$
BEGIN
    EXECUTE format('GRANT cyrene_workspace_device_registry_owner TO %I', current_user);
END
$owner_membership$;

DROP TRIGGER IF EXISTS certificate_records_guard
    ON cyrene_workspace_device_registry.certificate_records;
DROP TABLE cyrene_workspace_device_registry.certificate_records;

SET ROLE cyrene_workspace_device_registry_owner;
DROP FUNCTION IF EXISTS cyrene_workspace_device_registry.guard_certificate_record();
DROP FUNCTION IF EXISTS
    cyrene_workspace_device_registry.activate_acknowledged_delivery(BYTEA);
DROP FUNCTION IF EXISTS
    cyrene_workspace_device_registry.revoke_workspace_device(TEXT, TEXT, TEXT);
DROP FUNCTION IF EXISTS cyrene_workspace_device_registry.bytes_to_json_array(BYTEA);
RESET ROLE;

DROP SCHEMA cyrene_workspace_device_registry;

REVOKE SELECT ON
    cyrene_workspace_device_authorization.authorizations,
    cyrene_workspace_directory.device_registration_bindings,
    cyrene_workspace_directory.workspace_device_identities
    FROM cyrene_workspace_device_registry_app;
REVOKE SELECT (
    id, registration_binding_id, organization_id, workspace_id, device_id,
    approval_id, authorization_generation, csr_sha256, spki_sha256, state_kind,
    state_deadline_unix_ms, state_payload
) ON cyrene_workspace_device_authorization.authorizations
    FROM cyrene_workspace_device_registry_app,
         cyrene_workspace_device_registry_owner;
REVOKE SELECT (
    binding_id, organization_id, workspace_id, device_id,
    authorization_generation, csr_sha256, spki_sha256
) ON cyrene_workspace_directory.device_registration_bindings
    FROM cyrene_workspace_device_registry_app,
         cyrene_workspace_device_registry_owner;
REVOKE SELECT (
    organization_id, workspace_id, device_id, current_authorization_generation
) ON cyrene_workspace_directory.workspace_device_identities
    FROM cyrene_workspace_device_registry_app,
         cyrene_workspace_device_registry_owner;
REVOKE USAGE ON SCHEMA
    cyrene_workspace_device_authorization,
    cyrene_workspace_directory
    FROM cyrene_workspace_device_registry_app;

REVOKE UPDATE (state_kind)
    ON cyrene_workspace_device_authorization.authorizations
    FROM cyrene_workspace_device_registry_owner;
REVOKE SELECT
    ON cyrene_workspace_directory.device_registration_bindings
    FROM cyrene_workspace_device_registry_owner;
REVOKE UPDATE (current_authorization_generation)
    ON cyrene_workspace_directory.workspace_device_identities
    FROM cyrene_workspace_device_registry_owner;
REVOKE USAGE ON SCHEMA
    cyrene_workspace_device_authorization,
    cyrene_workspace_directory
    FROM cyrene_workspace_device_registry_owner;
DO $owner_membership_cleanup$
BEGIN
    EXECUTE format('REVOKE cyrene_workspace_device_registry_owner FROM %I', current_user);
END
$owner_membership_cleanup$;
