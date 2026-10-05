DO $terminal_history_guard$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM cyrene_workspace_device_registry.terminal_device_keys
    ) THEN
        RAISE EXCEPTION
            'terminal Workspace device revocations cannot be removed by migration rollback';
    END IF;
END
$terminal_history_guard$;

DO $owner_membership$
BEGIN
    EXECUTE format(
        'GRANT cyrene_workspace_device_registry_owner TO %I',
        current_user
    );
END
$owner_membership$;

LOCK TABLE cyrene_workspace_device_registry.certificate_records
    IN SHARE ROW EXCLUSIVE MODE;
SET ROLE cyrene_workspace_device_registry_owner;

RESET ROLE;
DROP TRIGGER certificate_records_terminal_device_key_guard
    ON cyrene_workspace_device_registry.certificate_records;
SET ROLE cyrene_workspace_device_registry_owner;
DROP FUNCTION cyrene_workspace_device_registry.reject_terminal_device_key_write();

DROP FUNCTION cyrene_workspace_device_registry.activate_acknowledged_delivery(BYTEA);
ALTER FUNCTION
    cyrene_workspace_device_registry.activate_acknowledged_delivery_without_terminal_key_guard(BYTEA)
    RENAME TO activate_acknowledged_delivery;
GRANT EXECUTE ON FUNCTION
    cyrene_workspace_device_registry.activate_acknowledged_delivery(BYTEA)
    TO cyrene_workspace_device_registry_app;

DROP FUNCTION cyrene_workspace_device_registry.relay_dispatch_fence(
    TEXT, TEXT, TEXT, UUID, BIGINT, BYTEA, BYTEA, BYTEA, BYTEA, BIGINT
);
ALTER FUNCTION
    cyrene_workspace_device_registry.relay_dispatch_fence_without_terminal_key_guard(
        TEXT, TEXT, TEXT, UUID, BIGINT, BYTEA, BYTEA, BYTEA, BYTEA, BIGINT
    ) RENAME TO relay_dispatch_fence;
GRANT EXECUTE ON FUNCTION cyrene_workspace_device_registry.relay_dispatch_fence(
    TEXT, TEXT, TEXT, UUID, BIGINT, BYTEA, BYTEA, BYTEA, BYTEA, BIGINT
) TO cyrene_workspace_device_registry_app,
     cyrene_workspace_device_registry_relay_reader;

CREATE OR REPLACE FUNCTION cyrene_workspace_device_registry.revoke_workspace_device(
    requested_organization_id TEXT,
    requested_workspace_id TEXT,
    requested_device_id TEXT
)
RETURNS BOOLEAN
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, cyrene_workspace_device_registry, pg_temp
AS $revoke$
BEGIN
    PERFORM 1
    FROM cyrene_workspace_directory.workspace_device_identities
    WHERE organization_id = requested_organization_id
      AND workspace_id = requested_workspace_id
      AND device_id = requested_device_id
    FOR SHARE;
    IF NOT FOUND THEN
        RETURN FALSE;
    END IF;

    UPDATE cyrene_workspace_device_registry.certificate_records
    SET state = 'revoked'
    WHERE organization_id = requested_organization_id
      AND workspace_id = requested_workspace_id
      AND device_id = requested_device_id
      AND state IN ('pending_ack', 'active');
    RETURN TRUE;
END;
$revoke$;

DROP FUNCTION cyrene_workspace_device_registry.lock_workspace_device_key(TEXT, TEXT, TEXT);
RESET ROLE;
DROP TABLE cyrene_workspace_device_registry.terminal_device_keys;

DO $owner_membership_cleanup$
BEGIN
    EXECUTE format(
        'REVOKE cyrene_workspace_device_registry_owner FROM %I',
        current_user
    );
END
$owner_membership_cleanup$;
