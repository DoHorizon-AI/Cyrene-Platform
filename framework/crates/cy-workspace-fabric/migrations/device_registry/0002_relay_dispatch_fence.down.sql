-- ┌─────────────────────────────────────────────────────────────────────┐
-- │  Rollback: device_registry/0002_relay_dispatch_fence                │
-- │  Role: Remove the final Relay admission fence and restore V1 revoke.  │
-- │                                                                     │
-- │  回滚职责：移除派发 guard，并恢复原 Registry 撤销锁模式。             │
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
DECLARE
    directory_generation BIGINT;
BEGIN
    SELECT current_authorization_generation
    INTO directory_generation
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

DROP FUNCTION cyrene_workspace_device_registry.relay_dispatch_fence(
    TEXT, TEXT, TEXT, UUID, BIGINT, BYTEA, BYTEA, BYTEA, BYTEA, BIGINT
);

RESET ROLE;

DO $owner_membership_cleanup$
BEGIN
    EXECUTE format(
        'REVOKE cyrene_workspace_device_registry_owner FROM %I',
        current_user
    );
END
$owner_membership_cleanup$;
