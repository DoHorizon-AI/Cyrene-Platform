DO $owner_membership$
BEGIN
    EXECUTE format('GRANT cyrene_workspace_device_registry_owner TO %I', current_user);
END
$owner_membership$;

SET ROLE cyrene_workspace_device_registry_owner;

DO $rollback_guard$
BEGIN
    IF EXISTS (
        SELECT 1 FROM cyrene_workspace_device_registry.workspace_invocation_outbox
        WHERE authorization_source = 'DELEGATION'
    ) THEN
        RAISE EXCEPTION 'delegated invocation rows prevent this rollback';
    END IF;
END
$rollback_guard$;

DROP FUNCTION IF EXISTS
    cyrene_workspace_device_registry.authority_execution_device_current(
        TEXT, TEXT, TEXT, BYTEA, BYTEA, BIGINT
    );
DROP FUNCTION IF EXISTS
    cyrene_workspace_device_registry.lock_authority_execution_target_binding(
        TEXT, TEXT, TEXT, TEXT, TEXT, TEXT, BIGINT, BYTEA, BYTEA, TEXT,
        TEXT, BIGINT, TEXT
    );

DROP TRIGGER IF EXISTS authority_delegation_binding_immutable
    ON cyrene_workspace_device_registry.workspace_invocation_outbox;
DROP FUNCTION IF EXISTS
    cyrene_workspace_device_registry.guard_authority_delegation_binding_immutable();

ALTER TABLE cyrene_workspace_device_registry.workspace_invocation_outbox
    DROP CONSTRAINT workspace_invocation_outbox_v2_complete_check,
    DROP COLUMN authorization_source,
    DROP COLUMN delegation_id,
    DROP COLUMN delegation_generation,
    DROP COLUMN delegation_audience;

ALTER TABLE cyrene_workspace_device_registry.workspace_invocation_outbox
    ADD CONSTRAINT workspace_invocation_outbox_v2_complete_check
        CHECK (
            record_schema_version <> 2
            OR (
                organization_id IS NOT NULL AND principal_issuer IS NOT NULL
                AND principal_subject IS NOT NULL AND operation_owner_id IS NOT NULL
                AND operation_id IS NOT NULL AND scope IS NOT NULL AND resource_id IS NOT NULL
                AND product_invocation_proto IS NOT NULL AND approved_envelope_proto IS NOT NULL
                AND execution_credential_proto IS NOT NULL AND invocation_digest_sha256 IS NOT NULL
                AND idempotency_semantics IS NOT NULL AND idempotency_scope_sha256 IS NOT NULL
                AND execution_device_id IS NOT NULL AND certificate_fingerprint_sha256 IS NOT NULL
                AND authorization_id IS NOT NULL AND session_id IS NOT NULL
                AND execution_target_json IS NOT NULL AND signing_key_id IS NOT NULL
                AND target_binding_manifest_sha256 IS NOT NULL AND bundle_manifest_sha256 IS NOT NULL
                AND owner_source_commit IS NOT NULL AND credential_issued_at_unix_ms IS NOT NULL
                AND credential_expires_at_unix_ms IS NOT NULL
                AND stable_idempotency_request_digest_sha256 IS NOT NULL
                AND btrim(organization_id) <> '' AND btrim(workspace_id) <> ''
                AND btrim(principal_issuer) <> '' AND btrim(principal_subject) <> ''
                AND btrim(operation_owner_id) <> '' AND btrim(operation_id) <> ''
                AND btrim(execution_device_id) <> '' AND btrim(target_component) <> ''
                AND device_generation > 0 AND session_generation > 0
                AND contract_activation_generation > 0 AND credential_issued_at_unix_ms > 0
                AND credential_expires_at_unix_ms > credential_issued_at_unix_ms
                AND credential_expires_at_unix_ms = expires_at_unix_ms
                AND idempotency_semantics IN (
                    'IDEMPOTENCY_SEMANTICS_NOT_SUPPORTED',
                    'IDEMPOTENCY_SEMANTICS_REQUIRED',
                    'IDEMPOTENCY_SEMANTICS_OPTIONAL'
                )
            )
        );

DROP TABLE cyrene_workspace_device_registry.authority_delegations;
DROP FUNCTION IF EXISTS
    cyrene_workspace_device_registry.guard_authority_delegation_immutable();

RESET ROLE;

DO $registrar_membership$
BEGIN
    EXECUTE format('GRANT cyrene_workspace_device_registrar TO %I', current_user);
END
$registrar_membership$;
GRANT USAGE ON SCHEMA cyrene_workspace_device_registry
    TO cyrene_workspace_device_registrar;
SET ROLE cyrene_workspace_device_registrar;
DROP FUNCTION IF EXISTS
    cyrene_workspace_device_registry.authority_directory_generation_current(
        TEXT, TEXT, TEXT, BIGINT
    );
RESET ROLE;
REVOKE USAGE ON SCHEMA cyrene_workspace_device_registry
    FROM cyrene_workspace_device_registrar;
DO $registrar_membership_cleanup$
BEGIN
    EXECUTE format('REVOKE cyrene_workspace_device_registrar FROM %I', current_user);
END
$registrar_membership_cleanup$;
REVOKE CREATE ON SCHEMA cyrene_workspace_device_registry
    FROM cyrene_workspace_device_registry_owner;

DO $owner_membership_cleanup$
BEGIN
    EXECUTE format('REVOKE cyrene_workspace_device_registry_owner FROM %I', current_user);
END
$owner_membership_cleanup$;
