-- ┌─────────────────────────────────────────────────────────────────────┐
-- │ Durable, scoped Workspace Authority invocation records              │
-- │ 持久化完整、按范围绑定的 Workspace Authority 调用记录                   │
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

REVOKE DELETE ON TABLE
    cyrene_workspace_device_registry.workspace_invocation_outbox
    FROM cyrene_workspace_device_registry_app;

ALTER TABLE cyrene_workspace_device_registry.workspace_invocation_outbox
    ADD COLUMN IF NOT EXISTS record_schema_version SMALLINT NOT NULL DEFAULT 1,
    ADD COLUMN IF NOT EXISTS organization_id VARCHAR(256),
    ADD COLUMN IF NOT EXISTS principal_issuer TEXT,
    ADD COLUMN IF NOT EXISTS principal_subject TEXT,
    ADD COLUMN IF NOT EXISTS operation_owner_id VARCHAR(128),
    ADD COLUMN IF NOT EXISTS operation_id VARCHAR(128),
    ADD COLUMN IF NOT EXISTS scope TEXT,
    ADD COLUMN IF NOT EXISTS resource_id TEXT,
    ADD COLUMN IF NOT EXISTS product_invocation_proto BYTEA,
    ADD COLUMN IF NOT EXISTS approved_envelope_proto BYTEA,
    ADD COLUMN IF NOT EXISTS execution_credential_proto BYTEA,
    ADD COLUMN IF NOT EXISTS invocation_digest_sha256 VARCHAR(64),
    ADD COLUMN IF NOT EXISTS idempotency_semantics VARCHAR(32),
    ADD COLUMN IF NOT EXISTS idempotency_scope_sha256 VARCHAR(64),
    ADD COLUMN IF NOT EXISTS execution_device_id VARCHAR(256),
    ADD COLUMN IF NOT EXISTS certificate_fingerprint_sha256 VARCHAR(64),
    ADD COLUMN IF NOT EXISTS authorization_id BYTEA,
    ADD COLUMN IF NOT EXISTS session_id UUID,
    ADD COLUMN IF NOT EXISTS execution_target_json JSONB,
    ADD COLUMN IF NOT EXISTS signing_key_id VARCHAR(128),
    ADD COLUMN IF NOT EXISTS target_binding_manifest_sha256 VARCHAR(64),
    ADD COLUMN IF NOT EXISTS bundle_manifest_sha256 VARCHAR(64),
    ADD COLUMN IF NOT EXISTS owner_source_commit VARCHAR(64),
    ADD COLUMN IF NOT EXISTS credential_issued_at_unix_ms BIGINT,
    ADD COLUMN IF NOT EXISTS credential_expires_at_unix_ms BIGINT,
    ADD COLUMN IF NOT EXISTS claim_credential_digest_sha256 VARCHAR(64),
    ADD COLUMN IF NOT EXISTS claim_lease_expires_at_unix_ms BIGINT,
    ADD COLUMN IF NOT EXISTS claim_count BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS result_digest_sha256 VARCHAR(64),
    ADD COLUMN IF NOT EXISTS result_receipt BYTEA,
    ADD COLUMN IF NOT EXISTS stable_idempotency_request_digest_sha256 VARCHAR(64),
    ADD COLUMN IF NOT EXISTS error_code VARCHAR(128);

-- Legacy rows are retained for audit. V2 claim queries filter on
-- record_schema_version = 2, so incomplete records cannot be replayed.

ALTER TABLE cyrene_workspace_device_registry.workspace_invocation_outbox
    ADD CONSTRAINT workspace_invocation_outbox_schema_version_check
        CHECK (record_schema_version IN (1, 2)),
    ADD CONSTRAINT workspace_invocation_outbox_v2_complete_check
        CHECK (
            record_schema_version <> 2
            OR (
                organization_id IS NOT NULL
                AND principal_issuer IS NOT NULL
                AND principal_subject IS NOT NULL
                AND operation_owner_id IS NOT NULL
                AND operation_id IS NOT NULL
                AND scope IS NOT NULL
                AND resource_id IS NOT NULL
                AND product_invocation_proto IS NOT NULL
                AND approved_envelope_proto IS NOT NULL
                AND execution_credential_proto IS NOT NULL
                AND invocation_digest_sha256 IS NOT NULL
                AND idempotency_semantics IS NOT NULL
                AND idempotency_scope_sha256 IS NOT NULL
                AND execution_device_id IS NOT NULL
                AND certificate_fingerprint_sha256 IS NOT NULL
                AND authorization_id IS NOT NULL
                AND session_id IS NOT NULL
                AND execution_target_json IS NOT NULL
                AND signing_key_id IS NOT NULL
                AND target_binding_manifest_sha256 IS NOT NULL
                AND bundle_manifest_sha256 IS NOT NULL
                AND owner_source_commit IS NOT NULL
                AND credential_issued_at_unix_ms IS NOT NULL
                AND credential_expires_at_unix_ms IS NOT NULL
                AND stable_idempotency_request_digest_sha256 IS NOT NULL
                AND btrim(organization_id) <> ''
                AND btrim(workspace_id) <> ''
                AND btrim(principal_issuer) <> ''
                AND btrim(principal_subject) <> ''
                AND btrim(operation_owner_id) <> ''
                AND btrim(operation_id) <> ''
                AND btrim(execution_device_id) <> ''
                AND btrim(target_component) <> ''
                AND device_generation > 0
                AND session_generation > 0
                AND contract_activation_generation > 0
                AND credential_issued_at_unix_ms > 0
                AND credential_expires_at_unix_ms > credential_issued_at_unix_ms
                AND credential_expires_at_unix_ms = expires_at_unix_ms
                AND idempotency_semantics IN (
                    'IDEMPOTENCY_SEMANTICS_NOT_SUPPORTED',
                    'IDEMPOTENCY_SEMANTICS_REQUIRED',
                    'IDEMPOTENCY_SEMANTICS_OPTIONAL'
                )
            )
        ),
    ADD CONSTRAINT workspace_invocation_outbox_v2_authorization_id_length_check
        CHECK (authorization_id IS NULL OR octet_length(authorization_id) = 16),
    ADD CONSTRAINT workspace_invocation_outbox_v2_digest_shape_check
        CHECK (
            (idempotency_scope_sha256 IS NULL OR idempotency_scope_sha256 ~ '^[0-9a-f]{64}$')
            AND (request_digest_sha256 IS NULL OR request_digest_sha256 ~ '^[0-9a-f]{64}$')
            AND (certificate_fingerprint_sha256 IS NULL OR certificate_fingerprint_sha256 ~ '^[0-9a-f]{64}$')
            AND (invocation_digest_sha256 IS NULL OR invocation_digest_sha256 ~ '^[0-9a-f]{64}$')
            AND (target_binding_manifest_sha256 IS NULL OR target_binding_manifest_sha256 ~ '^[0-9a-f]{64}$')
            AND (bundle_manifest_sha256 IS NULL OR bundle_manifest_sha256 ~ '^[0-9a-f]{64}$')
            AND (claim_credential_digest_sha256 IS NULL OR claim_credential_digest_sha256 ~ '^[0-9a-f]{64}$')
            AND (stable_idempotency_request_digest_sha256 IS NULL OR stable_idempotency_request_digest_sha256 ~ '^[0-9a-f]{64}$')
            AND (result_digest_sha256 IS NULL OR result_digest_sha256 ~ '^[0-9a-f]{64}$')
        );

-- PostgreSQL arbitrates concurrent callers. The scope digest is a canonical
-- hash of organization/workspace/principal/owner/operation/scope/resource.
CREATE UNIQUE INDEX IF NOT EXISTS idx_workspace_invocation_outbox_v2_idempotency
    ON cyrene_workspace_device_registry.workspace_invocation_outbox (
        organization_id,
        workspace_id,
        idempotency_scope_sha256,
        idempotency_key
    )
    WHERE record_schema_version = 2 AND idempotency_key IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_workspace_invocation_outbox_v2_device_generation
    ON cyrene_workspace_device_registry.workspace_invocation_outbox (
        organization_id,
        workspace_id,
        execution_device_id,
        device_generation,
        status
    )
    WHERE record_schema_version = 2;

COMMENT ON COLUMN cyrene_workspace_device_registry.workspace_invocation_outbox.product_invocation_proto IS
    'Complete deterministic ProductApiInvocationV2 protobuf bytes; immutable after approval.';
COMMENT ON COLUMN cyrene_workspace_device_registry.workspace_invocation_outbox.approved_envelope_proto IS
    'Canonical deterministic CanonicalInvocationEnvelope bytes, excluding credential digest/signature; request_digest_sha256 is its SHA-256.';
COMMENT ON COLUMN cyrene_workspace_device_registry.workspace_invocation_outbox.execution_credential_proto IS
    'Exact complete signed authority execution credential returned for this immutable invocation.';
COMMENT ON COLUMN cyrene_workspace_device_registry.workspace_invocation_outbox.execution_target_json IS
    'Authority-resolved immutable execution target; never reconstructed from request body.';
COMMENT ON COLUMN cyrene_workspace_device_registry.workspace_invocation_outbox.idempotency_scope_sha256 IS
    'SHA-256 of canonical organization/workspace/principal/operation/scope/resource binding.';
COMMENT ON COLUMN cyrene_workspace_device_registry.workspace_invocation_outbox.stable_idempotency_request_digest_sha256 IS
    'Stable SHA-256 of domain-separated deterministic ProductApiInvocationV2 bytes plus idempotency scope, excluding random invocation id and timestamps.';

CREATE OR REPLACE FUNCTION
    cyrene_workspace_device_registry.guard_authority_outbox_v2_immutable()
RETURNS TRIGGER
LANGUAGE plpgsql
SET search_path = pg_catalog, cyrene_workspace_device_registry, pg_temp
AS $immutable$
BEGIN
    IF OLD.record_schema_version = 2 AND ROW(
        OLD.invocation_id,
        OLD.organization_id,
        OLD.workspace_id,
        OLD.principal_issuer,
        OLD.principal_subject,
        OLD.operation_owner_id,
        OLD.operation_id,
        OLD.scope,
        OLD.resource_id,
        OLD.product_invocation_proto,
        OLD.approved_envelope_proto,
        OLD.execution_credential_proto,
        OLD.invocation_digest_sha256,
        OLD.idempotency_key,
        OLD.idempotency_semantics,
        OLD.idempotency_scope_sha256,
        OLD.request_digest_sha256,
        OLD.execution_device_id,
        OLD.device_generation,
        OLD.certificate_fingerprint_sha256,
        OLD.authorization_id,
        OLD.session_id,
        OLD.session_generation,
        OLD.contract_activation_generation,
        OLD.target_component,
        OLD.execution_target_json,
        OLD.resolved_target_url,
        OLD.signing_key_id,
        OLD.target_binding_manifest_sha256,
        OLD.bundle_manifest_sha256,
        OLD.owner_source_commit,
        OLD.credential_issued_at_unix_ms,
        OLD.credential_expires_at_unix_ms,
        OLD.expires_at_unix_ms,
        OLD.claim_credential_digest_sha256,
        OLD.stable_idempotency_request_digest_sha256,
        OLD.record_schema_version
    ) IS DISTINCT FROM ROW(
        NEW.invocation_id,
        NEW.organization_id,
        NEW.workspace_id,
        NEW.principal_issuer,
        NEW.principal_subject,
        NEW.operation_owner_id,
        NEW.operation_id,
        NEW.scope,
        NEW.resource_id,
        NEW.product_invocation_proto,
        NEW.approved_envelope_proto,
        NEW.execution_credential_proto,
        NEW.invocation_digest_sha256,
        NEW.idempotency_key,
        NEW.idempotency_semantics,
        NEW.idempotency_scope_sha256,
        NEW.request_digest_sha256,
        NEW.execution_device_id,
        NEW.device_generation,
        NEW.certificate_fingerprint_sha256,
        NEW.authorization_id,
        NEW.session_id,
        NEW.session_generation,
        NEW.contract_activation_generation,
        NEW.target_component,
        NEW.execution_target_json,
        NEW.resolved_target_url,
        NEW.signing_key_id,
        NEW.target_binding_manifest_sha256,
        NEW.bundle_manifest_sha256,
        NEW.owner_source_commit,
        NEW.credential_issued_at_unix_ms,
        NEW.credential_expires_at_unix_ms,
        NEW.expires_at_unix_ms,
        NEW.claim_credential_digest_sha256,
        NEW.stable_idempotency_request_digest_sha256,
        NEW.record_schema_version
    ) THEN
        RAISE EXCEPTION 'approved authority invocation fields are immutable';
    END IF;

    IF OLD.record_schema_version = 2
       AND OLD.status IS DISTINCT FROM NEW.status
       AND NOT (
            (OLD.status = 'PENDING' AND NEW.status IN (
                'CLAIMED', 'CANCELLED', 'FAILED', 'UNKNOWN_RESULT'
            ))
            OR (OLD.status = 'CLAIMED' AND NEW.status IN (
                'PENDING', 'ACKNOWLEDGED', 'FAILED', 'UNKNOWN_RESULT'
            ))
            OR (OLD.status = 'ACKNOWLEDGED' AND NEW.status IN (
                'PENDING', 'SUCCEEDED', 'FAILED', 'UNKNOWN_RESULT'
            ))
       ) THEN
        RAISE EXCEPTION 'invalid authority invocation state transition';
    END IF;

    IF OLD.delivery_receipt IS NOT NULL
       AND OLD.delivery_receipt IS DISTINCT FROM NEW.delivery_receipt THEN
        RAISE EXCEPTION 'authority delivery receipt is immutable once acknowledged';
    END IF;

    IF OLD.result_digest_sha256 IS NOT NULL
       AND ROW(
           OLD.result_digest_sha256,
           OLD.result_receipt,
           OLD.outcome_status,
           OLD.response_status_code,
           OLD.response_json_body,
           OLD.response_content_type,
           OLD.error_code,
           OLD.error_message
       ) IS DISTINCT FROM ROW(
           NEW.result_digest_sha256,
           NEW.result_receipt,
           NEW.outcome_status,
           NEW.response_status_code,
           NEW.response_json_body,
           NEW.response_content_type,
           NEW.error_code,
           NEW.error_message
       ) THEN
        RAISE EXCEPTION 'authority invocation result is immutable once submitted';
    END IF;
    RETURN NEW;
END;
$immutable$;

DROP TRIGGER IF EXISTS authority_outbox_v2_immutable
    ON cyrene_workspace_device_registry.workspace_invocation_outbox;
CREATE TRIGGER authority_outbox_v2_immutable
    BEFORE UPDATE ON cyrene_workspace_device_registry.workspace_invocation_outbox
    FOR EACH ROW
    EXECUTE FUNCTION cyrene_workspace_device_registry.guard_authority_outbox_v2_immutable();

REVOKE ALL ON FUNCTION
    cyrene_workspace_device_registry.guard_authority_outbox_v2_immutable()
    FROM PUBLIC, cyrene_workspace_device_registry_app;

RESET ROLE;

DO $owner_membership_cleanup$
BEGIN
    EXECUTE format(
        'REVOKE cyrene_workspace_device_registry_owner FROM %I',
        current_user
    );
END
$owner_membership_cleanup$;
