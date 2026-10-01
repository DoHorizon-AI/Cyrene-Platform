-- Serialize the WebAuthn approval reservation against Directory membership revoke.
-- The lock ends with the Issuing CAS transaction; it does not span the CA call.

DO $dependencies$
BEGIN
    IF to_regclass('cyrene_workspace_directory.memberships') IS NULL
       OR to_regrole('cyrene_workspace_directory_operator') IS NULL THEN
        RAISE EXCEPTION 'Workspace Directory membership and operator role are required';
    END IF;
    IF NOT has_column_privilege(
        'cyrene_workspace_directory_operator',
        'cyrene_workspace_directory.memberships',
        'provisioned_at',
        'UPDATE'
    ) THEN
        RAISE EXCEPTION 'Workspace Directory migration 0004 must run before device authorization 0008';
    END IF;
END
$dependencies$;

DO $roles$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_roles
        WHERE rolname = 'cyrene_workspace_device_approval_fence_owner'
    ) THEN
        CREATE ROLE cyrene_workspace_device_approval_fence_owner NOLOGIN;
    END IF;
END
$roles$;

ALTER ROLE cyrene_workspace_device_approval_fence_owner
    NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS NOINHERIT;
REVOKE cyrene_workspace_device_approval_fence_owner
    FROM cyrene_workspace_device_authorization_app;

DO $owner_membership_guard$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM pg_auth_members
        WHERE roleid = 'cyrene_workspace_device_approval_fence_owner'::regrole
          AND member <> current_user::regrole
    ) THEN
        RAISE EXCEPTION 'device approval membership fence owner must not have runtime members';
    END IF;
END
$owner_membership_guard$;

DO $owner_membership$
BEGIN
    EXECUTE format(
        'GRANT cyrene_workspace_device_approval_fence_owner TO %I',
        current_user
    );
END
$owner_membership$;

GRANT USAGE ON SCHEMA
    cyrene_workspace_device_authorization,
    cyrene_workspace_directory
    TO cyrene_workspace_device_approval_fence_owner;
GRANT CREATE ON SCHEMA cyrene_workspace_device_authorization
    TO cyrene_workspace_device_approval_fence_owner;

-- PostgreSQL requires UPDATE on at least one column for SELECT ... FOR SHARE.
-- Keep that privilege on a dedicated NOLOGIN function owner, never on the app.
GRANT SELECT (issuer, subject, organization_id, workspace_id),
    UPDATE (provisioned_at)
    ON cyrene_workspace_directory.memberships
    TO cyrene_workspace_device_approval_fence_owner;

SET ROLE cyrene_workspace_device_approval_fence_owner;

CREATE OR REPLACE FUNCTION
    cyrene_workspace_device_authorization.lock_approval_membership(
        p_issuer TEXT,
        p_subject TEXT,
        p_organization_id TEXT,
        p_workspace_id TEXT
    )
RETURNS BOOLEAN
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog
AS $membership_fence$
BEGIN
    PERFORM 1
    FROM cyrene_workspace_directory.memberships
    WHERE issuer = p_issuer
      AND subject = p_subject
      AND organization_id = p_organization_id
      AND workspace_id = p_workspace_id
    FOR SHARE;
    RETURN FOUND;
END;
$membership_fence$;

REVOKE ALL ON FUNCTION
    cyrene_workspace_device_authorization.lock_approval_membership(TEXT, TEXT, TEXT, TEXT)
    FROM PUBLIC, cyrene_workspace_device_authorization_app,
         cyrene_workspace_device_approval_fence_owner;
GRANT EXECUTE ON FUNCTION
    cyrene_workspace_device_authorization.lock_approval_membership(TEXT, TEXT, TEXT, TEXT)
    TO cyrene_workspace_device_authorization_app;
COMMENT ON FUNCTION
    cyrene_workspace_device_authorization.lock_approval_membership(TEXT, TEXT, TEXT, TEXT)
    IS 'Locks one exact Directory membership row FOR SHARE until the caller transaction ends.';

RESET ROLE;

REVOKE CREATE ON SCHEMA cyrene_workspace_device_authorization
    FROM cyrene_workspace_device_approval_fence_owner;

DO $owner_membership_cleanup$
BEGIN
    EXECUTE format(
        'REVOKE cyrene_workspace_device_approval_fence_owner FROM %I',
        current_user
    );
END
$owner_membership_cleanup$;
