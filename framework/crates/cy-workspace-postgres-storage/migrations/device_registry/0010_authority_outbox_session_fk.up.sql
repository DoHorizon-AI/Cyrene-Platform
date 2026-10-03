DO $owner_membership$
BEGIN
    EXECUTE format(
        'GRANT cyrene_workspace_device_registry_owner TO %I',
        current_user
    );
END
$owner_membership$;

SET ROLE cyrene_workspace_device_registry_owner;

ALTER TABLE cyrene_workspace_device_registry.workspace_invocation_outbox
    ADD CONSTRAINT workspace_invocation_outbox_session_fk
    FOREIGN KEY (session_id)
    REFERENCES cyrene_workspace_device_registry.authority_web_sessions (session_id)
    ON DELETE RESTRICT;

RESET ROLE;

DO $owner_membership_cleanup$
BEGIN
    EXECUTE format(
        'REVOKE cyrene_workspace_device_registry_owner FROM %I',
        current_user
    );
END
$owner_membership_cleanup$;
