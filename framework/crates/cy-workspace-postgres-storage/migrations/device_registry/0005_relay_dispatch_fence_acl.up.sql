-- Keep the dispatch fence callable only by the Registry app and read-only Relay role.
-- Call privileges remain a migration invariant after any Registry upgrade.

DO $owner_membership$
BEGIN
    EXECUTE format(
        'GRANT cyrene_workspace_device_registry_owner TO %I',
        current_user
    );
END
$owner_membership$;

SET ROLE cyrene_workspace_device_registry_owner;

REVOKE ALL ON FUNCTION cyrene_workspace_device_registry.relay_dispatch_fence(
    TEXT, TEXT, TEXT, UUID, BIGINT, BYTEA, BYTEA, BYTEA, BYTEA, BIGINT
) FROM PUBLIC, cyrene_workspace_device_registry_app,
         cyrene_workspace_device_registry_owner;
GRANT EXECUTE ON FUNCTION cyrene_workspace_device_registry.relay_dispatch_fence(
    TEXT, TEXT, TEXT, UUID, BIGINT, BYTEA, BYTEA, BYTEA, BYTEA, BIGINT
) TO cyrene_workspace_device_registry_app;

RESET ROLE;

DO $owner_membership_cleanup$
BEGIN
    EXECUTE format(
        'REVOKE cyrene_workspace_device_registry_owner FROM %I',
        current_user
    );
END
$owner_membership_cleanup$;
