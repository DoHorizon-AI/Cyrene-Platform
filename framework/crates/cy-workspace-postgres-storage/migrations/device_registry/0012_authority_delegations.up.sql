-- ┌─────────────────────────────────────────────────────────────────────┐
-- │ Authority delegations and explicit invocation authorization source  │
-- │ Authority delegation 与 invocation source 的持久化结构                │
-- └─────────────────────────────────────────────────────────────────────┘

DO $owner_membership$
BEGIN
    EXECUTE format('GRANT cyrene_workspace_device_registry_owner TO %I', current_user);
END
$owner_membership$;

GRANT CREATE ON SCHEMA cyrene_workspace_device_registry
    TO cyrene_workspace_device_registry_owner;
SET ROLE cyrene_workspace_device_registry_owner;

CREATE TABLE cyrene_workspace_device_registry.authority_delegations (
    delegation_id UUID PRIMARY KEY,
    generation BIGINT NOT NULL CHECK (generation > 0),
    organization_id VARCHAR(256) NOT NULL CHECK (btrim(organization_id) <> ''),
    workspace_id VARCHAR(256) NOT NULL CHECK (btrim(workspace_id) <> ''),
    principal_issuer TEXT NOT NULL CHECK (btrim(principal_issuer) <> ''),
    principal_subject TEXT NOT NULL CHECK (btrim(principal_subject) <> ''),
    audience VARCHAR(256) NOT NULL CHECK (btrim(audience) <> ''),
    managed_workload_id VARCHAR(256) NOT NULL CHECK (btrim(managed_workload_id) <> ''),
    operations JSONB NOT NULL CHECK (jsonb_typeof(operations) = 'array'),
    expires_at_unix_ms BIGINT NOT NULL CHECK (expires_at_unix_ms > 0),
    revoked_at_unix_ms BIGINT,
    created_at_unix_ms BIGINT NOT NULL CHECK (created_at_unix_ms > 0),
    CHECK (revoked_at_unix_ms IS NULL OR revoked_at_unix_ms > 0)
);

CREATE INDEX idx_authority_delegations_owner
    ON cyrene_workspace_device_registry.authority_delegations
       (organization_id, workspace_id, principal_issuer, principal_subject, expires_at_unix_ms);

CREATE OR REPLACE FUNCTION
    cyrene_workspace_device_registry.guard_authority_delegation_immutable()
RETURNS TRIGGER
LANGUAGE plpgsql
SET search_path = pg_catalog, cyrene_workspace_device_registry, pg_temp
AS $immutable$
BEGIN
    IF ROW(
        OLD.delegation_id, OLD.organization_id, OLD.workspace_id,
        OLD.principal_issuer, OLD.principal_subject, OLD.audience,
        OLD.managed_workload_id, OLD.operations, OLD.expires_at_unix_ms,
        OLD.created_at_unix_ms
    ) IS DISTINCT FROM ROW(
        NEW.delegation_id, NEW.organization_id, NEW.workspace_id,
        NEW.principal_issuer, NEW.principal_subject, NEW.audience,
        NEW.managed_workload_id, NEW.operations, NEW.expires_at_unix_ms,
        NEW.created_at_unix_ms
    ) THEN
        RAISE EXCEPTION 'authority delegation scope is immutable';
    END IF;
    IF OLD.revoked_at_unix_ms IS NULL THEN
        IF NEW.revoked_at_unix_ms IS NULL AND NEW.generation <> OLD.generation THEN
            RAISE EXCEPTION 'authority delegation generation changes only on revoke';
        END IF;
        IF NEW.revoked_at_unix_ms IS NOT NULL
           AND NEW.generation <> OLD.generation + 1 THEN
            RAISE EXCEPTION 'authority delegation revoke must increment generation once';
        END IF;
    ELSIF NEW.revoked_at_unix_ms IS DISTINCT FROM OLD.revoked_at_unix_ms
       OR NEW.generation <> OLD.generation THEN
        RAISE EXCEPTION 'authority delegation revocation is terminal';
    END IF;
    RETURN NEW;
END
$immutable$;

CREATE TRIGGER authority_delegation_immutable
    BEFORE UPDATE ON cyrene_workspace_device_registry.authority_delegations
    FOR EACH ROW
    EXECUTE FUNCTION cyrene_workspace_device_registry.guard_authority_delegation_immutable();

RESET ROLE;

DO $registrar_membership$
BEGIN
    EXECUTE format(
        'GRANT cyrene_workspace_device_registrar TO %I',
        current_user
    );
END
$registrar_membership$;

GRANT USAGE, CREATE ON SCHEMA cyrene_workspace_device_registry
    TO cyrene_workspace_device_registrar;
SET ROLE cyrene_workspace_device_registrar;

CREATE OR REPLACE FUNCTION
    cyrene_workspace_device_registry.authority_directory_generation_current(
        requested_organization_id TEXT,
        requested_workspace_id TEXT,
        requested_device_id TEXT,
        requested_generation BIGINT
    )
RETURNS BOOLEAN
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, cyrene_workspace_directory, pg_temp
AS $directory_fence$
DECLARE
    current_generation BIGINT;
BEGIN
    IF requested_organization_id IS NULL OR btrim(requested_organization_id) = ''
       OR requested_workspace_id IS NULL OR btrim(requested_workspace_id) = ''
       OR requested_device_id IS NULL OR btrim(requested_device_id) = ''
       OR requested_generation <= 0 THEN
        RETURN FALSE;
    END IF;

    SELECT identity.current_authorization_generation
    INTO current_generation
    FROM cyrene_workspace_directory.workspace_device_identities AS identity
    WHERE identity.organization_id = requested_organization_id
      AND identity.workspace_id = requested_workspace_id
      AND identity.device_id = requested_device_id
    FOR SHARE OF identity;
    IF NOT FOUND THEN
        RETURN FALSE;
    END IF;
    RETURN current_generation = requested_generation;
END
$directory_fence$;

REVOKE ALL ON FUNCTION
    cyrene_workspace_device_registry.authority_directory_generation_current(
        TEXT, TEXT, TEXT, BIGINT
    ) FROM PUBLIC, cyrene_workspace_device_registry_app,
         cyrene_workspace_device_registrar;
GRANT EXECUTE ON FUNCTION
    cyrene_workspace_device_registry.authority_directory_generation_current(
        TEXT, TEXT, TEXT, BIGINT
    ) TO cyrene_workspace_device_registry_owner;

RESET ROLE;
REVOKE USAGE, CREATE ON SCHEMA cyrene_workspace_device_registry
    FROM cyrene_workspace_device_registrar;
DO $registrar_membership_cleanup$
BEGIN
    EXECUTE format(
        'REVOKE cyrene_workspace_device_registrar FROM %I',
        current_user
    );
END
$registrar_membership_cleanup$;

GRANT CREATE ON SCHEMA cyrene_workspace_device_registry
    TO cyrene_workspace_device_registry_owner;
SET ROLE cyrene_workspace_device_registry_owner;

CREATE OR REPLACE FUNCTION
    cyrene_workspace_device_registry.authority_execution_device_current(
        requested_organization_id TEXT,
        requested_workspace_id TEXT,
        requested_device_id TEXT,
        requested_authorization_id BYTEA,
        requested_certificate_sha256 BYTEA,
        requested_generation BIGINT
    )
RETURNS BOOLEAN
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, cyrene_workspace_device_registry, pg_temp
AS $device_fence$
BEGIN
    IF requested_organization_id IS NULL OR btrim(requested_organization_id) = ''
       OR requested_workspace_id IS NULL OR btrim(requested_workspace_id) = ''
       OR requested_device_id IS NULL OR btrim(requested_device_id) = ''
       OR octet_length(requested_authorization_id) <> 16
       OR octet_length(requested_certificate_sha256) <> 32
       OR requested_generation <= 0 THEN
        RETURN FALSE;
    END IF;

    IF NOT cyrene_workspace_device_registry.authority_directory_generation_current(
        requested_organization_id,
        requested_workspace_id,
        requested_device_id,
        requested_generation
    ) THEN
        RETURN FALSE;
    END IF;

    PERFORM certificate.authorization_id
    FROM cyrene_workspace_device_registry.certificate_records AS certificate
    WHERE certificate.organization_id = requested_organization_id
      AND certificate.workspace_id = requested_workspace_id
      AND certificate.device_id = requested_device_id
      AND certificate.authorization_id = requested_authorization_id
      AND certificate.certificate_sha256 = requested_certificate_sha256
      AND certificate.authorization_generation = requested_generation
      AND certificate.state = 'active'
    FOR SHARE OF certificate;
    IF NOT FOUND THEN
        RETURN FALSE;
    END IF;

    RETURN NOT EXISTS (
        SELECT 1
        FROM cyrene_workspace_device_registry.terminal_device_keys AS terminal
        WHERE terminal.organization_id = requested_organization_id
          AND terminal.workspace_id = requested_workspace_id
          AND terminal.device_id = requested_device_id
    );
END
$device_fence$;

REVOKE ALL ON FUNCTION
    cyrene_workspace_device_registry.authority_execution_device_current(
        TEXT, TEXT, TEXT, BYTEA, BYTEA, BIGINT
    ) FROM PUBLIC, cyrene_workspace_device_registry_owner;
GRANT EXECUTE ON FUNCTION
    cyrene_workspace_device_registry.authority_execution_device_current(
        TEXT, TEXT, TEXT, BYTEA, BYTEA, BIGINT
    ) TO cyrene_workspace_device_registry_app;

CREATE OR REPLACE FUNCTION
    cyrene_workspace_device_registry.lock_authority_execution_target_binding(
        requested_organization_id TEXT,
        requested_workspace_id TEXT,
        requested_operation_owner_id TEXT,
        requested_operation_id TEXT,
        requested_target_component TEXT,
        requested_execution_device_id TEXT,
        requested_device_generation BIGINT,
        requested_authorization_id BYTEA,
        requested_certificate_sha256 BYTEA,
        requested_target_manifest_sha256 TEXT,
        requested_bundle_manifest_sha256 TEXT,
        requested_contract_generation BIGINT,
        requested_owner_source_commit TEXT
    )
RETURNS TEXT
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, cyrene_workspace_device_registry, pg_temp
AS $target_fence$
DECLARE
    endpoint_base TEXT;
BEGIN
    SELECT target.endpoint_base
    INTO endpoint_base
    FROM cyrene_workspace_device_registry.authority_execution_target_bindings AS target
    WHERE target.organization_id = requested_organization_id
      AND target.workspace_id = requested_workspace_id
      AND target.operation_owner_id = requested_operation_owner_id
      AND target.operation_id = requested_operation_id
      AND target.target_component = requested_target_component
      AND target.execution_device_id = requested_execution_device_id
      AND target.execution_device_generation = requested_device_generation
      AND target.execution_authorization_id = requested_authorization_id
      AND target.certificate_fingerprint_sha256 = requested_certificate_sha256
      AND encode(target.target_binding_manifest_sha256, 'hex') = requested_target_manifest_sha256
      AND encode(target.bundle_manifest_sha256, 'hex') = requested_bundle_manifest_sha256
      AND target.contract_activation_generation = requested_contract_generation
      AND target.owner_source_commit = requested_owner_source_commit
      AND target.active = TRUE
    FOR SHARE OF target;
    IF NOT FOUND THEN
        RETURN NULL;
    END IF;
    RETURN endpoint_base;
END
$target_fence$;

REVOKE ALL ON FUNCTION
    cyrene_workspace_device_registry.lock_authority_execution_target_binding(
        TEXT, TEXT, TEXT, TEXT, TEXT, TEXT, BIGINT, BYTEA, BYTEA, TEXT,
        TEXT, BIGINT, TEXT
    ) FROM PUBLIC, cyrene_workspace_device_registry_owner;
GRANT EXECUTE ON FUNCTION
    cyrene_workspace_device_registry.lock_authority_execution_target_binding(
        TEXT, TEXT, TEXT, TEXT, TEXT, TEXT, BIGINT, BYTEA, BYTEA, TEXT,
        TEXT, BIGINT, TEXT
    ) TO cyrene_workspace_device_registry_app;

REVOKE ALL ON TABLE cyrene_workspace_device_registry.authority_delegations FROM PUBLIC;
GRANT SELECT, INSERT, UPDATE ON TABLE
    cyrene_workspace_device_registry.authority_delegations
    TO cyrene_workspace_device_registry_app;

ALTER TABLE cyrene_workspace_device_registry.workspace_invocation_outbox
    ADD COLUMN authorization_source VARCHAR(32) NOT NULL DEFAULT 'INTERACTIVE_SESSION',
    ADD COLUMN delegation_id UUID,
    ADD COLUMN delegation_generation BIGINT,
    ADD COLUMN delegation_audience VARCHAR(256);

ALTER TABLE cyrene_workspace_device_registry.workspace_invocation_outbox
    DROP CONSTRAINT workspace_invocation_outbox_v2_complete_check,
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
                AND authorization_id IS NOT NULL AND execution_target_json IS NOT NULL
                AND signing_key_id IS NOT NULL AND target_binding_manifest_sha256 IS NOT NULL
                AND bundle_manifest_sha256 IS NOT NULL AND owner_source_commit IS NOT NULL
                AND credential_issued_at_unix_ms IS NOT NULL AND credential_expires_at_unix_ms IS NOT NULL
                AND stable_idempotency_request_digest_sha256 IS NOT NULL
                AND btrim(organization_id) <> '' AND btrim(workspace_id) <> ''
                AND btrim(principal_issuer) <> '' AND btrim(principal_subject) <> ''
                AND btrim(operation_owner_id) <> '' AND btrim(operation_id) <> ''
                AND btrim(execution_device_id) <> '' AND btrim(target_component) <> ''
                AND device_generation > 0 AND contract_activation_generation > 0
                AND credential_issued_at_unix_ms > 0
                AND credential_expires_at_unix_ms > credential_issued_at_unix_ms
                AND credential_expires_at_unix_ms = expires_at_unix_ms
                AND idempotency_semantics IN (
                    'IDEMPOTENCY_SEMANTICS_NOT_SUPPORTED',
                    'IDEMPOTENCY_SEMANTICS_REQUIRED',
                    'IDEMPOTENCY_SEMANTICS_OPTIONAL'
                )
                AND (
                    (authorization_source = 'INTERACTIVE_SESSION'
                     AND session_id IS NOT NULL AND session_generation > 0
                     AND delegation_id IS NULL AND delegation_generation IS NULL
                     AND delegation_audience IS NULL)
                    OR
                    (authorization_source = 'DELEGATION'
                     AND session_id IS NULL AND session_generation = 0
                     AND delegation_id IS NOT NULL AND delegation_generation > 0
                     AND delegation_audience IS NOT NULL AND btrim(delegation_audience) <> '')
                )
            )
        );

CREATE OR REPLACE FUNCTION
    cyrene_workspace_device_registry.guard_authority_delegation_binding_immutable()
RETURNS TRIGGER
LANGUAGE plpgsql
SET search_path = pg_catalog, cyrene_workspace_device_registry, pg_temp
AS $immutable$
BEGIN
    IF OLD.record_schema_version = 2
       AND ROW(OLD.authorization_source, OLD.delegation_id, OLD.delegation_generation,
               OLD.delegation_audience)
           IS DISTINCT FROM
           ROW(NEW.authorization_source, NEW.delegation_id, NEW.delegation_generation,
               NEW.delegation_audience) THEN
        RAISE EXCEPTION 'approved authority authorization source is immutable';
    END IF;
    RETURN NEW;
END
$immutable$;

CREATE TRIGGER authority_delegation_binding_immutable
    BEFORE UPDATE ON cyrene_workspace_device_registry.workspace_invocation_outbox
    FOR EACH ROW
    EXECUTE FUNCTION cyrene_workspace_device_registry.guard_authority_delegation_binding_immutable();

RESET ROLE;
REVOKE CREATE ON SCHEMA cyrene_workspace_device_registry
    FROM cyrene_workspace_device_registry_owner;

DO $owner_membership_cleanup$
BEGIN
    EXECUTE format('REVOKE cyrene_workspace_device_registry_owner FROM %I', current_user);
END
$owner_membership_cleanup$;
