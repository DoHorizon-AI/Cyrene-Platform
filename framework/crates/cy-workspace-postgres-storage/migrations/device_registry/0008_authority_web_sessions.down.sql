DO $owner_membership$
BEGIN
    EXECUTE format(
        'GRANT cyrene_workspace_device_registry_owner TO %I',
        current_user
    );
END
$owner_membership$;

SET ROLE cyrene_workspace_device_registry_owner;

DROP TABLE IF EXISTS cyrene_workspace_device_registry.authority_web_sessions;
DROP SEQUENCE IF EXISTS cyrene_workspace_device_registry.authority_web_session_generation_seq;

RESET ROLE;

DO $owner_membership_cleanup$
BEGIN
    EXECUTE format(
        'REVOKE cyrene_workspace_device_registry_owner FROM %I',
        current_user
    );
END
$owner_membership_cleanup$;
