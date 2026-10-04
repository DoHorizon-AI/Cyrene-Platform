DO $owner_membership$
BEGIN
    EXECUTE format(
        'GRANT cyrene_workspace_device_registry_owner TO %I',
        current_user
    );
END
$owner_membership$;

SET ROLE cyrene_workspace_device_registry_owner;

DROP FUNCTION IF EXISTS
    cyrene_workspace_device_registry.upsert_authority_execution_target_binding(
        TEXT, TEXT, TEXT, TEXT, TEXT, TEXT, BIGINT, BYTEA, BYTEA, BYTEA,
        BYTEA, BIGINT, TEXT, TEXT, TEXT, TEXT, BIGINT
    );
DROP TABLE IF EXISTS
    cyrene_workspace_device_registry.authority_execution_target_binding_events;

DROP TABLE IF EXISTS
    cyrene_workspace_device_registry.authority_execution_target_bindings;

RESET ROLE;

REVOKE REFERENCES ON TABLE
    cyrene_workspace_device_registry.certificate_records
    FROM cyrene_workspace_device_registry_owner;

DO $owner_membership_cleanup$
BEGIN
    EXECUTE format(
        'REVOKE cyrene_workspace_device_registry_owner FROM %I',
        current_user
    );
END
$owner_membership_cleanup$;
