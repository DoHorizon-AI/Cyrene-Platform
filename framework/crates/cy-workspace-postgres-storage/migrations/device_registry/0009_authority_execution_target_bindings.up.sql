-- ┌─────────────────────────────────────────────────────────────────────┐
-- │ Operator-managed Product operation to execution target bindings      │
-- │ 运维管理的 Product operation 到执行目标绑定                           │
-- └─────────────────────────────────────────────────────────────────────┘

DO $owner_membership$
BEGIN
    EXECUTE format(
        'GRANT cyrene_workspace_device_registry_owner TO %I',
        current_user
    );
END
$owner_membership$;

DO $admin_role$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_roles WHERE rolname = 'cyrene_workspace_execution_target_admin'
    ) THEN
        CREATE ROLE cyrene_workspace_execution_target_admin NOLOGIN;
    END IF;
END
$admin_role$;

ALTER ROLE cyrene_workspace_execution_target_admin
    NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS NOINHERIT;
REVOKE cyrene_workspace_execution_target_admin FROM cyrene_workspace_device_registry_app;

-- The function owner has read access to certificate records but the new target
-- binding table also needs a foreign key to that primary key.
GRANT REFERENCES ON TABLE
    cyrene_workspace_device_registry.certificate_records
    TO cyrene_workspace_device_registry_owner;

SET ROLE cyrene_workspace_device_registry_owner;

CREATE TABLE IF NOT EXISTS
    cyrene_workspace_device_registry.authority_execution_target_bindings (
    organization_id VARCHAR(256) NOT NULL,
    workspace_id VARCHAR(256) NOT NULL,
    operation_owner_id VARCHAR(128) NOT NULL,
    operation_id VARCHAR(128) NOT NULL,
    target_component VARCHAR(128) NOT NULL,
    execution_device_id VARCHAR(256) NOT NULL,
    execution_device_generation BIGINT NOT NULL CHECK (execution_device_generation > 0),
    execution_authorization_id BYTEA NOT NULL CHECK (octet_length(execution_authorization_id) = 16),
    certificate_fingerprint_sha256 BYTEA NOT NULL
        CHECK (octet_length(certificate_fingerprint_sha256) = 32),
    target_binding_manifest_sha256 BYTEA NOT NULL
        CHECK (octet_length(target_binding_manifest_sha256) = 32),
    bundle_manifest_sha256 BYTEA NOT NULL
        CHECK (octet_length(bundle_manifest_sha256) = 32),
    contract_activation_generation BIGINT NOT NULL CHECK (contract_activation_generation > 0),
    owner_source_commit VARCHAR(64) NOT NULL
        CHECK (owner_source_commit ~ '^[0-9a-f]{40}([0-9a-f]{24})?$'),
    endpoint_base TEXT NOT NULL CHECK (btrim(endpoint_base) <> ''),
    active BOOLEAN NOT NULL DEFAULT TRUE,
    configured_at_unix_ms BIGINT NOT NULL CHECK (configured_at_unix_ms > 0),
    configured_by TEXT NOT NULL CHECK (btrim(configured_by) <> ''),
    configuration_reason TEXT NOT NULL CHECK (btrim(configuration_reason) <> ''),
    PRIMARY KEY (
        organization_id,
        workspace_id,
        operation_owner_id,
        operation_id
    ),
    CONSTRAINT authority_execution_target_certificate_fk
        FOREIGN KEY (execution_authorization_id)
        REFERENCES cyrene_workspace_device_registry.certificate_records (authorization_id)
        ON DELETE RESTRICT
);

CREATE INDEX IF NOT EXISTS authority_execution_target_device_idx
    ON cyrene_workspace_device_registry.authority_execution_target_bindings (
        organization_id,
        workspace_id,
        execution_device_id,
        execution_device_generation
    )
    WHERE active;

CREATE TABLE IF NOT EXISTS
    cyrene_workspace_device_registry.authority_execution_target_binding_events (
    event_id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    organization_id VARCHAR(256) NOT NULL,
    workspace_id VARCHAR(256) NOT NULL,
    operation_owner_id VARCHAR(128) NOT NULL,
    operation_id VARCHAR(128) NOT NULL,
    target_component VARCHAR(128) NOT NULL,
    execution_device_id VARCHAR(256) NOT NULL,
    execution_device_generation BIGINT NOT NULL,
    execution_authorization_id BYTEA NOT NULL,
    certificate_fingerprint_sha256 BYTEA NOT NULL,
    target_binding_manifest_sha256 BYTEA NOT NULL,
    bundle_manifest_sha256 BYTEA NOT NULL,
    contract_activation_generation BIGINT NOT NULL,
    owner_source_commit VARCHAR(64) NOT NULL,
    endpoint_base TEXT NOT NULL,
    active BOOLEAN NOT NULL,
    configured_at_unix_ms BIGINT NOT NULL,
    configured_by TEXT NOT NULL,
    configuration_reason TEXT NOT NULL
);

CREATE OR REPLACE FUNCTION
    cyrene_workspace_device_registry.upsert_authority_execution_target_binding(
        requested_organization_id TEXT,
        requested_workspace_id TEXT,
        requested_operation_owner_id TEXT,
        requested_operation_id TEXT,
        requested_target_component TEXT,
        requested_execution_device_id TEXT,
        requested_execution_device_generation BIGINT,
        requested_execution_authorization_id BYTEA,
        requested_certificate_fingerprint_sha256 BYTEA,
        requested_target_binding_manifest_sha256 BYTEA,
        requested_bundle_manifest_sha256 BYTEA,
        requested_contract_activation_generation BIGINT,
        requested_owner_source_commit TEXT,
        requested_endpoint_base TEXT,
        requested_configured_by TEXT,
        requested_configuration_reason TEXT,
        requested_configured_at_unix_ms BIGINT
    )
RETURNS VOID
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, cyrene_workspace_device_registry, cyrene_workspace_directory, pg_temp
AS $upsert$
DECLARE
    current_generation BIGINT;
BEGIN
    IF btrim(requested_organization_id) = ''
       OR btrim(requested_workspace_id) = ''
       OR btrim(requested_operation_owner_id) = ''
       OR btrim(requested_operation_id) = ''
       OR btrim(requested_target_component) = ''
       OR btrim(requested_execution_device_id) = ''
       OR octet_length(requested_execution_authorization_id) <> 16
       OR octet_length(requested_certificate_fingerprint_sha256) <> 32
       OR octet_length(requested_target_binding_manifest_sha256) <> 32
       OR octet_length(requested_bundle_manifest_sha256) <> 32
       OR requested_execution_device_generation <= 0
       OR requested_contract_activation_generation <= 0
       OR requested_owner_source_commit !~ '^[0-9a-f]{40}([0-9a-f]{24})?$'
       OR requested_configured_at_unix_ms <= 0
       OR btrim(requested_configured_by) = ''
       OR btrim(requested_configuration_reason) = ''
       OR requested_endpoint_base !~ '^https://[^/@?#]+(/[^?#]*)?$' THEN
        RAISE EXCEPTION 'invalid execution target binding';
    END IF;

    SELECT identity.current_authorization_generation
    INTO current_generation
    FROM cyrene_workspace_directory.workspace_device_identities AS identity
    JOIN cyrene_workspace_device_registry.certificate_records AS certificate
      ON certificate.organization_id = identity.organization_id
     AND certificate.workspace_id = identity.workspace_id
     AND certificate.device_id = identity.device_id
     AND certificate.authorization_generation = identity.current_authorization_generation
    WHERE identity.organization_id = requested_organization_id
      AND identity.workspace_id = requested_workspace_id
      AND identity.device_id = requested_execution_device_id
      AND identity.current_authorization_generation = requested_execution_device_generation
      AND certificate.authorization_id = requested_execution_authorization_id
      AND certificate.certificate_sha256 = requested_certificate_fingerprint_sha256
      AND certificate.state = 'active'
    FOR SHARE OF identity, certificate;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'execution device binding is not current and active';
    END IF;

    INSERT INTO cyrene_workspace_device_registry.authority_execution_target_bindings (
        organization_id, workspace_id, operation_owner_id, operation_id,
        target_component, execution_device_id, execution_device_generation,
        execution_authorization_id, certificate_fingerprint_sha256,
        target_binding_manifest_sha256, bundle_manifest_sha256,
        contract_activation_generation, owner_source_commit,
        endpoint_base, active, configured_at_unix_ms, configured_by,
        configuration_reason
    ) VALUES (
        requested_organization_id, requested_workspace_id,
        requested_operation_owner_id, requested_operation_id,
        requested_target_component, requested_execution_device_id,
        requested_execution_device_generation, requested_execution_authorization_id,
        requested_certificate_fingerprint_sha256, requested_target_binding_manifest_sha256,
        requested_bundle_manifest_sha256, requested_contract_activation_generation,
        requested_owner_source_commit, requested_endpoint_base,
        TRUE, requested_configured_at_unix_ms, requested_configured_by,
        requested_configuration_reason
    )
    ON CONFLICT (organization_id, workspace_id, operation_owner_id, operation_id)
    DO UPDATE SET
        target_component = EXCLUDED.target_component,
        execution_device_id = EXCLUDED.execution_device_id,
        execution_device_generation = EXCLUDED.execution_device_generation,
        execution_authorization_id = EXCLUDED.execution_authorization_id,
        certificate_fingerprint_sha256 = EXCLUDED.certificate_fingerprint_sha256,
        target_binding_manifest_sha256 = EXCLUDED.target_binding_manifest_sha256,
        bundle_manifest_sha256 = EXCLUDED.bundle_manifest_sha256,
        contract_activation_generation = EXCLUDED.contract_activation_generation,
        owner_source_commit = EXCLUDED.owner_source_commit,
        endpoint_base = EXCLUDED.endpoint_base,
        active = TRUE,
        configured_at_unix_ms = EXCLUDED.configured_at_unix_ms,
        configured_by = EXCLUDED.configured_by,
        configuration_reason = EXCLUDED.configuration_reason;

    INSERT INTO cyrene_workspace_device_registry.authority_execution_target_binding_events (
        organization_id, workspace_id, operation_owner_id, operation_id,
        target_component, execution_device_id, execution_device_generation,
        execution_authorization_id, certificate_fingerprint_sha256,
        target_binding_manifest_sha256, bundle_manifest_sha256,
        contract_activation_generation, owner_source_commit,
        endpoint_base, active, configured_at_unix_ms, configured_by,
        configuration_reason
    ) VALUES (
        requested_organization_id, requested_workspace_id,
        requested_operation_owner_id, requested_operation_id,
        requested_target_component, requested_execution_device_id,
        requested_execution_device_generation, requested_execution_authorization_id,
        requested_certificate_fingerprint_sha256, requested_target_binding_manifest_sha256,
        requested_bundle_manifest_sha256, requested_contract_activation_generation,
        requested_owner_source_commit, requested_endpoint_base,
        TRUE, requested_configured_at_unix_ms, requested_configured_by,
        requested_configuration_reason
    );
END;
$upsert$;

REVOKE ALL ON TABLE
    cyrene_workspace_device_registry.authority_execution_target_binding_events
    FROM PUBLIC, cyrene_workspace_device_registry_app,
         cyrene_workspace_execution_target_admin;
REVOKE ALL ON FUNCTION
    cyrene_workspace_device_registry.upsert_authority_execution_target_binding(
        TEXT, TEXT, TEXT, TEXT, TEXT, TEXT, BIGINT, BYTEA, BYTEA, BYTEA,
        BYTEA, BIGINT, TEXT, TEXT, TEXT, TEXT, BIGINT
    ) FROM PUBLIC, cyrene_workspace_device_registry_app,
             cyrene_workspace_device_registry_owner;
GRANT EXECUTE ON FUNCTION
    cyrene_workspace_device_registry.upsert_authority_execution_target_binding(
        TEXT, TEXT, TEXT, TEXT, TEXT, TEXT, BIGINT, BYTEA, BYTEA, BYTEA,
        BYTEA, BIGINT, TEXT, TEXT, TEXT, TEXT, BIGINT
    ) TO cyrene_workspace_execution_target_admin;

REVOKE ALL ON TABLE
    cyrene_workspace_device_registry.authority_execution_target_bindings FROM PUBLIC;
GRANT SELECT ON TABLE
    cyrene_workspace_device_registry.authority_execution_target_bindings
    TO cyrene_workspace_device_registry_app;

COMMENT ON TABLE
    cyrene_workspace_device_registry.authority_execution_target_bindings IS
    'Trusted operator-provisioned mapping from Product operation to an approved execution device and endpoint base. The runtime app is read-only.';

RESET ROLE;

DO $owner_membership_cleanup$
BEGIN
    EXECUTE format(
        'REVOKE cyrene_workspace_device_registry_owner FROM %I',
        current_user
    );
END
$owner_membership_cleanup$;
