DO $owner_membership$
BEGIN
    EXECUTE format(
        'GRANT cyrene_workspace_device_approval_fence_owner TO %I',
        current_user
    );
END
$owner_membership$;

SET ROLE cyrene_workspace_device_approval_fence_owner;

REVOKE ALL ON FUNCTION
    cyrene_workspace_device_authorization.lock_approval_membership(TEXT, TEXT, TEXT, TEXT)
    FROM PUBLIC, cyrene_workspace_device_authorization_app,
         cyrene_workspace_device_approval_fence_owner;
DROP FUNCTION IF EXISTS
    cyrene_workspace_device_authorization.lock_approval_membership(TEXT, TEXT, TEXT, TEXT);

RESET ROLE;

REVOKE SELECT (issuer, subject, organization_id, workspace_id),
    UPDATE (provisioned_at)
    ON cyrene_workspace_directory.memberships
    FROM cyrene_workspace_device_approval_fence_owner;
REVOKE CREATE, USAGE ON SCHEMA cyrene_workspace_device_authorization
    FROM cyrene_workspace_device_approval_fence_owner;
REVOKE USAGE ON SCHEMA cyrene_workspace_directory
    FROM cyrene_workspace_device_approval_fence_owner;

DO $owner_membership_cleanup$
BEGIN
    EXECUTE format(
        'REVOKE cyrene_workspace_device_approval_fence_owner FROM %I',
        current_user
    );
END
$owner_membership_cleanup$;
