-- Roll back the v2 record shape while retaining the original outbox columns.

DO $owner_membership$
BEGIN
    EXECUTE format(
        'GRANT cyrene_workspace_device_registry_owner TO %I',
        current_user
    );
END
$owner_membership$;

SET ROLE cyrene_workspace_device_registry_owner;

DROP TRIGGER IF EXISTS authority_outbox_v2_immutable
    ON cyrene_workspace_device_registry.workspace_invocation_outbox;
DROP FUNCTION IF EXISTS
    cyrene_workspace_device_registry.guard_authority_outbox_v2_immutable();

DROP INDEX IF EXISTS cyrene_workspace_device_registry.idx_workspace_invocation_outbox_v2_device_generation;
DROP INDEX IF EXISTS cyrene_workspace_device_registry.idx_workspace_invocation_outbox_v2_idempotency;

ALTER TABLE cyrene_workspace_device_registry.workspace_invocation_outbox
    DROP CONSTRAINT IF EXISTS workspace_invocation_outbox_v2_digest_shape_check,
    DROP CONSTRAINT IF EXISTS workspace_invocation_outbox_v2_authorization_id_length_check,
    DROP CONSTRAINT IF EXISTS workspace_invocation_outbox_v2_complete_check,
    DROP CONSTRAINT IF EXISTS workspace_invocation_outbox_schema_version_check,
    DROP COLUMN IF EXISTS result_digest_sha256,
    DROP COLUMN IF EXISTS result_receipt,
    DROP COLUMN IF EXISTS stable_idempotency_request_digest_sha256,
    DROP COLUMN IF EXISTS error_code,
    DROP COLUMN IF EXISTS claim_count,
    DROP COLUMN IF EXISTS claim_lease_expires_at_unix_ms,
    DROP COLUMN IF EXISTS claim_credential_digest_sha256,
    DROP COLUMN IF EXISTS credential_expires_at_unix_ms,
    DROP COLUMN IF EXISTS credential_issued_at_unix_ms,
    DROP COLUMN IF EXISTS signing_key_id,
    DROP COLUMN IF EXISTS target_binding_manifest_sha256,
    DROP COLUMN IF EXISTS bundle_manifest_sha256,
    DROP COLUMN IF EXISTS owner_source_commit,
    DROP COLUMN IF EXISTS execution_target_json,
    DROP COLUMN IF EXISTS session_id,
    DROP COLUMN IF EXISTS authorization_id,
    DROP COLUMN IF EXISTS certificate_fingerprint_sha256,
    DROP COLUMN IF EXISTS execution_device_id,
    DROP COLUMN IF EXISTS idempotency_scope_sha256,
    DROP COLUMN IF EXISTS idempotency_semantics,
    DROP COLUMN IF EXISTS product_invocation_proto,
    DROP COLUMN IF EXISTS execution_credential_proto,
    DROP COLUMN IF EXISTS approved_envelope_proto,
    DROP COLUMN IF EXISTS invocation_digest_sha256,
    DROP COLUMN IF EXISTS resource_id,
    DROP COLUMN IF EXISTS scope,
    DROP COLUMN IF EXISTS operation_id,
    DROP COLUMN IF EXISTS operation_owner_id,
    DROP COLUMN IF EXISTS principal_subject,
    DROP COLUMN IF EXISTS principal_issuer,
    DROP COLUMN IF EXISTS organization_id,
    DROP COLUMN IF EXISTS record_schema_version;

RESET ROLE;

GRANT DELETE ON TABLE
    cyrene_workspace_device_registry.workspace_invocation_outbox
    TO cyrene_workspace_device_registry_app;

DO $owner_membership_cleanup$
BEGIN
    EXECUTE format(
        'REVOKE cyrene_workspace_device_registry_owner FROM %I',
        current_user
    );
END
$owner_membership_cleanup$;
