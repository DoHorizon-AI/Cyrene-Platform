DROP TRIGGER IF EXISTS anonymous_first_start_admission
    ON cyrene_workspace_device_authorization.authorizations;
DROP INDEX IF EXISTS
    cyrene_workspace_device_authorization.authorizations_anonymous_first_start_active_idx;

REVOKE TRIGGER ON cyrene_workspace_device_authorization.authorizations
    FROM cyrene_workspace_anonymous_start_admission_owner;

DO $owner_membership$
BEGIN
    EXECUTE format(
        'GRANT cyrene_workspace_anonymous_start_admission_owner TO %I',
        current_user
    );
END
$owner_membership$;

SET ROLE cyrene_workspace_anonymous_start_admission_owner;

DROP FUNCTION cyrene_workspace_device_authorization.admit_anonymous_first_start();
DROP TABLE cyrene_workspace_device_authorization.anonymous_first_start_admission_state;
DROP TABLE cyrene_workspace_device_authorization.anonymous_first_start_admission_policy;

RESET ROLE;

REVOKE UPDATE (updated_at)
    ON cyrene_workspace_directory.workspace_device_identities
    FROM cyrene_workspace_anonymous_start_admission_owner;
REVOKE SELECT (
    organization_id,
    workspace_id,
    device_id,
    current_authorization_generation
)
    ON cyrene_workspace_directory.workspace_device_identities
    FROM cyrene_workspace_anonymous_start_admission_owner;
REVOKE SELECT (
    registration_key_digest,
    binding_id,
    organization_id,
    workspace_id,
    device_id,
    authorization_generation
)
    ON cyrene_workspace_directory.device_registration_bindings
    FROM cyrene_workspace_anonymous_start_admission_owner;
REVOKE SELECT (
    registration_key_digest,
    authorization_generation,
    expires_at_unix_ms,
    state_kind
)
    ON cyrene_workspace_device_authorization.authorizations
    FROM cyrene_workspace_anonymous_start_admission_owner;
REVOKE CREATE ON SCHEMA cyrene_workspace_device_authorization
    FROM cyrene_workspace_anonymous_start_admission_owner;
REVOKE USAGE ON SCHEMA
    cyrene_workspace_device_authorization,
    cyrene_workspace_directory
    FROM cyrene_workspace_anonymous_start_admission_owner;

DO $owner_membership_cleanup$
BEGIN
    EXECUTE format(
        'REVOKE cyrene_workspace_anonymous_start_admission_owner FROM %I',
        current_user
    );
END
$owner_membership_cleanup$;

DROP ROLE cyrene_workspace_anonymous_start_admission_owner;
