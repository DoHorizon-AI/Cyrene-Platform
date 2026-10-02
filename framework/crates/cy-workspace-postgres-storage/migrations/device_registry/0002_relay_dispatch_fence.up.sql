-- ┌─────────────────────────────────────────────────────────────────────┐
-- │  Migration: device_registry/0002_relay_dispatch_fence                │
-- │  Role: Fence final Relay queue admission against Registry revocation. │
-- │                                                                     │
-- │  迁移职责：以 Registry identity 行锁串行化最终 Relay 入队与撤销。       │
-- └─────────────────────────────────────────────────────────────────────┘

DO $owner_membership$
BEGIN
    EXECUTE format(
        'GRANT cyrene_workspace_device_registry_owner TO %I',
        current_user
    );
END
$owner_membership$;

GRANT CREATE ON SCHEMA cyrene_workspace_device_registry
    TO cyrene_workspace_device_registry_owner;
SET ROLE cyrene_workspace_device_registry_owner;

CREATE OR REPLACE FUNCTION cyrene_workspace_device_registry.relay_dispatch_fence(
    expected_organization_id TEXT,
    expected_workspace_id TEXT,
    expected_device_id TEXT,
    expected_registration_binding_id UUID,
    expected_authorization_generation BIGINT,
    expected_certificate_sha256 BYTEA,
    expected_csr_sha256 BYTEA,
    expected_spki_sha256 BYTEA,
    expected_serial_number BYTEA,
    expected_not_after_unix_ms BIGINT
)
RETURNS BOOLEAN
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, cyrene_workspace_device_registry, pg_temp
AS $fence$
DECLARE
    directory_generation BIGINT;
    candidate RECORD;
    authorization_row RECORD;
    certificate_row RECORD;
    binding_row RECORD;
    payload JSONB;
    receipt JSONB;
    acknowledged_at BIGINT;
    now_unix_ms BIGINT;
BEGIN
    IF expected_organization_id IS NULL
       OR btrim(expected_organization_id) = ''
       OR expected_workspace_id IS NULL
       OR btrim(expected_workspace_id) = ''
       OR expected_device_id IS NULL
       OR btrim(expected_device_id) = ''
       OR expected_registration_binding_id IS NULL
       OR expected_authorization_generation <= 0
       OR octet_length(expected_certificate_sha256) <> 32
       OR octet_length(expected_csr_sha256) <> 32
       OR octet_length(expected_spki_sha256) <> 32
       OR octet_length(expected_serial_number) NOT BETWEEN 1 AND 256
       OR expected_not_after_unix_ms <= 0 THEN
        RETURN FALSE;
    END IF;

    -- Lock order is Directory identity, authorization, then certificate record.
    SELECT current_authorization_generation
    INTO directory_generation
    FROM cyrene_workspace_directory.workspace_device_identities
    WHERE organization_id = expected_organization_id
      AND workspace_id = expected_workspace_id
      AND device_id = expected_device_id
    FOR SHARE;
    IF NOT FOUND THEN
        RETURN FALSE;
    END IF;
    IF directory_generation IS DISTINCT FROM expected_authorization_generation THEN
        RETURN FALSE;
    END IF;

    SELECT authorization_id
    INTO candidate
    FROM cyrene_workspace_device_registry.certificate_records
    WHERE organization_id = expected_organization_id
      AND workspace_id = expected_workspace_id
      AND device_id = expected_device_id
      AND certificate_sha256 = expected_certificate_sha256;
    IF NOT FOUND THEN
        RETURN FALSE;
    END IF;

    SELECT id, registration_binding_id, organization_id, workspace_id, device_id,
           approval_id, authorization_generation, csr_sha256, spki_sha256,
           state_kind, state_payload
    INTO authorization_row
    FROM cyrene_workspace_device_authorization.authorizations
    WHERE id = candidate.authorization_id
    FOR SHARE;
    IF NOT FOUND THEN
        RETURN FALSE;
    END IF;

    SELECT authorization_id, registration_binding_id, organization_id, workspace_id,
           device_id, authorization_generation, delivery_id, certificate_sha256,
           csr_sha256, spki_sha256, serial_number, not_after_unix_ms,
           delivery_deadline_unix_ms, state, acknowledged_at_unix_ms
    INTO certificate_row
    FROM cyrene_workspace_device_registry.certificate_records
    WHERE authorization_id = candidate.authorization_id
    FOR SHARE;
    IF NOT FOUND THEN
        RETURN FALSE;
    END IF;

    IF certificate_row.state IS DISTINCT FROM 'active'
       OR certificate_row.organization_id IS DISTINCT FROM expected_organization_id
       OR certificate_row.workspace_id IS DISTINCT FROM expected_workspace_id
       OR certificate_row.device_id IS DISTINCT FROM expected_device_id
       OR certificate_row.registration_binding_id
            IS DISTINCT FROM expected_registration_binding_id
       OR certificate_row.authorization_generation
            IS DISTINCT FROM expected_authorization_generation
       OR certificate_row.certificate_sha256
            IS DISTINCT FROM expected_certificate_sha256
       OR certificate_row.csr_sha256 IS DISTINCT FROM expected_csr_sha256
       OR certificate_row.spki_sha256 IS DISTINCT FROM expected_spki_sha256
       OR certificate_row.serial_number IS DISTINCT FROM expected_serial_number
       OR certificate_row.not_after_unix_ms IS DISTINCT FROM expected_not_after_unix_ms THEN
        RETURN FALSE;
    END IF;

    SELECT binding_id, organization_id, workspace_id, device_id,
           authorization_generation, csr_sha256, spki_sha256
    INTO binding_row
    FROM cyrene_workspace_directory.device_registration_bindings
    WHERE binding_id = certificate_row.registration_binding_id;
    IF NOT FOUND THEN
        RETURN FALSE;
    END IF;
    IF binding_row.organization_id IS DISTINCT FROM expected_organization_id
       OR binding_row.workspace_id IS DISTINCT FROM expected_workspace_id
       OR binding_row.device_id IS DISTINCT FROM expected_device_id
       OR binding_row.authorization_generation
            IS DISTINCT FROM expected_authorization_generation
       OR binding_row.csr_sha256 IS DISTINCT FROM expected_csr_sha256
       OR binding_row.spki_sha256 IS DISTINCT FROM expected_spki_sha256 THEN
        RETURN FALSE;
    END IF;

    IF authorization_row.state_kind IS DISTINCT FROM 'delivered'
       OR authorization_row.registration_binding_id
            IS DISTINCT FROM uuid_send(expected_registration_binding_id)
       OR authorization_row.organization_id IS DISTINCT FROM expected_organization_id
       OR authorization_row.workspace_id IS DISTINCT FROM expected_workspace_id
       OR authorization_row.device_id IS DISTINCT FROM expected_device_id
       OR authorization_row.authorization_generation
            IS DISTINCT FROM expected_authorization_generation
       OR authorization_row.csr_sha256 IS DISTINCT FROM expected_csr_sha256
       OR authorization_row.spki_sha256 IS DISTINCT FROM expected_spki_sha256
       OR authorization_row.approval_id IS NULL
       OR authorization_row.state_payload IS NULL
       OR octet_length(authorization_row.state_payload) > 33554432 THEN
        RETURN FALSE;
    END IF;

    payload := convert_from(authorization_row.state_payload, 'UTF8')::JSONB;
    receipt := payload #> '{state,receipt}';
    IF payload -> 'format_version' IS DISTINCT FROM to_jsonb(2)
       OR payload #>> '{state,kind}' IS DISTINCT FROM 'delivered'
       OR payload #> '{state,approval_id}' IS DISTINCT FROM
            cyrene_workspace_device_registry.bytes_to_json_array(
                authorization_row.approval_id
            )
       OR receipt -> 'authorization_id' IS DISTINCT FROM
            cyrene_workspace_device_registry.bytes_to_json_array(
                certificate_row.authorization_id
            )
       OR receipt -> 'delivery_id' IS DISTINCT FROM
            cyrene_workspace_device_registry.bytes_to_json_array(
                certificate_row.delivery_id
            )
       OR receipt -> 'device_id' IS DISTINCT FROM to_jsonb(expected_device_id)
       OR receipt -> 'authorization_generation' IS DISTINCT FROM
            to_jsonb(expected_authorization_generation)
       OR receipt -> 'certificate_sha256' IS DISTINCT FROM
            cyrene_workspace_device_registry.bytes_to_json_array(
                certificate_row.certificate_sha256
            )
       OR receipt -> 'csr_sha256' IS DISTINCT FROM
            cyrene_workspace_device_registry.bytes_to_json_array(
                certificate_row.csr_sha256
            )
       OR receipt -> 'csr_spki_sha256' IS DISTINCT FROM
            cyrene_workspace_device_registry.bytes_to_json_array(
                certificate_row.spki_sha256
            ) THEN
        RETURN FALSE;
    END IF;

    acknowledged_at := (receipt ->> 'acknowledged_at_unix_ms')::BIGINT;
    now_unix_ms := floor(extract(epoch FROM clock_timestamp()) * 1000)::BIGINT;
    IF acknowledged_at IS NULL
       OR receipt -> 'acknowledged_at_unix_ms' IS DISTINCT FROM to_jsonb(acknowledged_at)
       OR acknowledged_at >= certificate_row.delivery_deadline_unix_ms
       OR acknowledged_at > now_unix_ms
       OR acknowledged_at >= certificate_row.not_after_unix_ms
       OR certificate_row.acknowledged_at_unix_ms IS DISTINCT FROM acknowledged_at
       OR certificate_row.not_after_unix_ms <= now_unix_ms THEN
        RETURN FALSE;
    END IF;

    RETURN TRUE;
END;
$fence$;

CREATE OR REPLACE FUNCTION cyrene_workspace_device_registry.revoke_workspace_device(
    requested_organization_id TEXT,
    requested_workspace_id TEXT,
    requested_device_id TEXT
)
RETURNS BOOLEAN
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, cyrene_workspace_device_registry, pg_temp
AS $revoke$
DECLARE
    directory_generation BIGINT;
BEGIN
    SELECT current_authorization_generation
    INTO directory_generation
    FROM cyrene_workspace_directory.workspace_device_identities
    WHERE organization_id = requested_organization_id
      AND workspace_id = requested_workspace_id
      AND device_id = requested_device_id
    FOR NO KEY UPDATE;
    IF NOT FOUND THEN
        RETURN FALSE;
    END IF;

    UPDATE cyrene_workspace_device_registry.certificate_records
    SET state = 'revoked'
    WHERE organization_id = requested_organization_id
      AND workspace_id = requested_workspace_id
      AND device_id = requested_device_id
      AND state IN ('pending_ack', 'active');
    RETURN TRUE;
END;
$revoke$;

REVOKE ALL ON FUNCTION cyrene_workspace_device_registry.relay_dispatch_fence(
    TEXT, TEXT, TEXT, UUID, BIGINT, BYTEA, BYTEA, BYTEA, BYTEA, BIGINT
) FROM PUBLIC, cyrene_workspace_device_registry_app,
         cyrene_workspace_device_registry_owner;
GRANT EXECUTE ON FUNCTION cyrene_workspace_device_registry.relay_dispatch_fence(
    TEXT, TEXT, TEXT, UUID, BIGINT, BYTEA, BYTEA, BYTEA, BYTEA, BIGINT
) TO cyrene_workspace_device_registry_app;

REVOKE CREATE ON SCHEMA cyrene_workspace_device_registry
    FROM cyrene_workspace_device_registry_owner;
RESET ROLE;

DO $owner_membership_cleanup$
BEGIN
    EXECUTE format(
        'REVOKE cyrene_workspace_device_registry_owner FROM %I',
        current_user
    );
END
$owner_membership_cleanup$;
