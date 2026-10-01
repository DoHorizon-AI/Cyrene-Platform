REVOKE EXECUTE ON FUNCTION cyrene_workspace_device_registry.relay_dispatch_fence(
    TEXT, TEXT, TEXT, UUID, BIGINT, BYTEA, BYTEA, BYTEA, BYTEA, BIGINT
) FROM cyrene_workspace_device_registry_relay_reader;
REVOKE SELECT (
    authorization_id, registration_binding_id, organization_id, workspace_id, device_id,
    authorization_generation, delivery_id, certificate_sha256, csr_sha256, spki_sha256,
    serial_number, not_after_unix_ms, delivery_deadline_unix_ms, state,
    acknowledged_at_unix_ms
) ON cyrene_workspace_device_registry.certificate_records
    FROM cyrene_workspace_device_registry_relay_reader;
REVOKE SELECT (
    id, registration_binding_id, organization_id, workspace_id, device_id,
    approval_id, authorization_generation, csr_sha256, spki_sha256, state_kind,
    state_deadline_unix_ms, state_payload
) ON cyrene_workspace_device_authorization.authorizations
    FROM cyrene_workspace_device_registry_relay_reader;
REVOKE SELECT (
    binding_id, organization_id, workspace_id, device_id,
    authorization_generation, csr_sha256, spki_sha256
) ON cyrene_workspace_directory.device_registration_bindings
    FROM cyrene_workspace_device_registry_relay_reader;
REVOKE SELECT (
    organization_id, workspace_id, device_id, current_authorization_generation
) ON cyrene_workspace_directory.workspace_device_identities
    FROM cyrene_workspace_device_registry_relay_reader;
REVOKE USAGE ON SCHEMA
    cyrene_workspace_device_registry,
    cyrene_workspace_device_authorization,
    cyrene_workspace_directory
    FROM cyrene_workspace_device_registry_relay_reader;

-- Rollback expects operators to revoke externally managed LOGIN memberships first.
DROP ROLE cyrene_workspace_device_registry_relay_reader;
