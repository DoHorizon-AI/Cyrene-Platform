-- Persist Workspace-device revocation independently of currently issued certificates.
-- A terminal key fences recovery paths that complete after the original CA sweep.

DO $transactional_migration_guard$
BEGIN
    IF pg_catalog.current_setting('transaction_isolation') <> 'read committed' THEN
        RAISE EXCEPTION USING
            ERRCODE = 'PZ002',
            MESSAGE = 'device registry 0011 requires READ COMMITTED';
    END IF;
END
$transactional_migration_guard$;

DO $dependencies$
BEGIN
    IF to_regclass('cyrene_workspace_device_registry.certificate_records') IS NULL
       OR to_regrole('cyrene_workspace_device_registry_owner') IS NULL THEN
        RAISE EXCEPTION 'device registry tables and owner role must exist before migration 0011';
    END IF;
END
$dependencies$;

DO $owner_membership$
BEGIN
    EXECUTE format(
        'GRANT cyrene_workspace_device_registry_owner TO %I',
        current_user
    );
END
$owner_membership$;

-- Exclude old writers while adding the database-side guard and replacing secured entrypoints.
LOCK TABLE cyrene_workspace_device_registry.certificate_records
    IN SHARE ROW EXCLUSIVE MODE;

CREATE TABLE cyrene_workspace_device_registry.terminal_device_keys (
    organization_id TEXT NOT NULL
        CHECK (btrim(organization_id) <> '' AND length(organization_id) <= 256),
    workspace_id TEXT NOT NULL
        CHECK (btrim(workspace_id) <> '' AND length(workspace_id) <= 256),
    device_id TEXT NOT NULL
        CHECK (btrim(device_id) <> '' AND length(device_id) <= 256),
    revoked_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (organization_id, workspace_id, device_id)
);
REVOKE ALL ON cyrene_workspace_device_registry.terminal_device_keys FROM PUBLIC;
REVOKE ALL ON cyrene_workspace_device_registry.terminal_device_keys
    FROM cyrene_workspace_device_registry_app,
         cyrene_workspace_device_registry_relay_reader,
         cyrene_workspace_device_registry_owner;
GRANT SELECT ON cyrene_workspace_device_registry.terminal_device_keys
    TO cyrene_workspace_device_registry_app,
       cyrene_workspace_device_registry_owner;
GRANT INSERT ON cyrene_workspace_device_registry.terminal_device_keys
    TO cyrene_workspace_device_registry_owner;

SET ROLE cyrene_workspace_device_registry_owner;

CREATE FUNCTION cyrene_workspace_device_registry.lock_workspace_device_key(
    requested_organization_id TEXT,
    requested_workspace_id TEXT,
    requested_device_id TEXT
)
RETURNS VOID
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, cyrene_workspace_device_registry, pg_temp
AS $lock_key$
DECLARE
    lock_identity TEXT;
BEGIN
    IF requested_organization_id IS NULL
       OR btrim(requested_organization_id) = ''
       OR length(requested_organization_id) > 256
       OR requested_workspace_id IS NULL
       OR btrim(requested_workspace_id) = ''
       OR length(requested_workspace_id) > 256
       OR requested_device_id IS NULL
       OR btrim(requested_device_id) = ''
       OR length(requested_device_id) > 256 THEN
        RAISE EXCEPTION 'Workspace device key is invalid';
    END IF;

    -- Hex plus byte lengths makes the complete triple unambiguous before hashing.
    lock_identity := 'cyrene-workspace-device-key:v1:'
        || octet_length(convert_to(requested_organization_id, 'UTF8'))::TEXT || ':'
        || encode(convert_to(requested_organization_id, 'UTF8'), 'hex') || ':'
        || octet_length(convert_to(requested_workspace_id, 'UTF8'))::TEXT || ':'
        || encode(convert_to(requested_workspace_id, 'UTF8'), 'hex') || ':'
        || octet_length(convert_to(requested_device_id, 'UTF8'))::TEXT || ':'
        || encode(convert_to(requested_device_id, 'UTF8'), 'hex');
    PERFORM pg_advisory_xact_lock(hashtextextended(lock_identity, 0));
END;
$lock_key$;

CREATE FUNCTION cyrene_workspace_device_registry.reject_terminal_device_key_write()
RETURNS TRIGGER
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, cyrene_workspace_device_registry, pg_temp
AS $terminal_guard$
BEGIN
    IF NEW.state IN ('pending_ack', 'active') THEN
        PERFORM cyrene_workspace_device_registry.lock_workspace_device_key(
            NEW.organization_id, NEW.workspace_id, NEW.device_id
        );
        IF EXISTS (
            SELECT 1
            FROM cyrene_workspace_device_registry.terminal_device_keys
            WHERE organization_id = NEW.organization_id
              AND workspace_id = NEW.workspace_id
              AND device_id = NEW.device_id
        ) THEN
            RAISE EXCEPTION 'Workspace device authorization is terminally revoked'
                USING ERRCODE = '23514';
        END IF;
    END IF;
    RETURN NEW;
END;
$terminal_guard$;

-- The registry owner role owns the SECURITY DEFINER function, while the migration role
-- attaches it to the certificate table it owns.
RESET ROLE;
CREATE TRIGGER certificate_records_terminal_device_key_guard
    BEFORE INSERT OR UPDATE OF organization_id, workspace_id, device_id, state
    ON cyrene_workspace_device_registry.certificate_records
    FOR EACH ROW
    EXECUTE FUNCTION cyrene_workspace_device_registry.reject_terminal_device_key_write();
SET ROLE cyrene_workspace_device_registry_owner;

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
BEGIN
    PERFORM cyrene_workspace_device_registry.lock_workspace_device_key(
        requested_organization_id, requested_workspace_id, requested_device_id
    );

    IF EXISTS (
        SELECT 1
        FROM cyrene_workspace_device_registry.terminal_device_keys
        WHERE organization_id = requested_organization_id
          AND workspace_id = requested_workspace_id
          AND device_id = requested_device_id
    ) THEN
        UPDATE cyrene_workspace_device_registry.certificate_records
        SET state = 'revoked'
        WHERE organization_id = requested_organization_id
          AND workspace_id = requested_workspace_id
          AND device_id = requested_device_id
          AND state IN ('pending_ack', 'active');
        RETURN TRUE;
    END IF;

    PERFORM 1
    FROM cyrene_workspace_directory.workspace_device_identities
    WHERE organization_id = requested_organization_id
      AND workspace_id = requested_workspace_id
      AND device_id = requested_device_id
    FOR SHARE;
    IF NOT FOUND THEN
        RETURN FALSE;
    END IF;

    INSERT INTO cyrene_workspace_device_registry.terminal_device_keys (
        organization_id, workspace_id, device_id
    )
    VALUES (
        requested_organization_id, requested_workspace_id, requested_device_id
    )
    ON CONFLICT (organization_id, workspace_id, device_id) DO NOTHING;

    UPDATE cyrene_workspace_device_registry.certificate_records
    SET state = 'revoked'
    WHERE organization_id = requested_organization_id
      AND workspace_id = requested_workspace_id
      AND device_id = requested_device_id
      AND state IN ('pending_ack', 'active');
    RETURN TRUE;
END;
$revoke$;

ALTER FUNCTION cyrene_workspace_device_registry.activate_acknowledged_delivery(BYTEA)
    RENAME TO activate_acknowledged_delivery_without_terminal_key_guard;
REVOKE ALL ON FUNCTION
    cyrene_workspace_device_registry.activate_acknowledged_delivery_without_terminal_key_guard(BYTEA)
    FROM PUBLIC, cyrene_workspace_device_registry_app,
         cyrene_workspace_device_registry_relay_reader,
         cyrene_workspace_device_registry_owner;
GRANT EXECUTE ON FUNCTION
    cyrene_workspace_device_registry.activate_acknowledged_delivery_without_terminal_key_guard(BYTEA)
    TO cyrene_workspace_device_registry_owner;

CREATE FUNCTION cyrene_workspace_device_registry.activate_acknowledged_delivery(
    requested_authorization_id BYTEA
)
RETURNS TEXT
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, cyrene_workspace_device_registry, pg_temp
AS $activate$
DECLARE
    candidate RECORD;
BEGIN
    IF octet_length(requested_authorization_id) <> 16 THEN
        RETURN 'not_staged';
    END IF;

    SELECT organization_id, workspace_id, device_id
    INTO candidate
    FROM cyrene_workspace_device_registry.certificate_records
    WHERE authorization_id = requested_authorization_id;
    IF NOT FOUND THEN
        RETURN 'not_staged';
    END IF;

    PERFORM cyrene_workspace_device_registry.lock_workspace_device_key(
        candidate.organization_id, candidate.workspace_id, candidate.device_id
    );
    IF EXISTS (
        SELECT 1
        FROM cyrene_workspace_device_registry.terminal_device_keys
        WHERE organization_id = candidate.organization_id
          AND workspace_id = candidate.workspace_id
          AND device_id = candidate.device_id
    ) THEN
        UPDATE cyrene_workspace_device_registry.certificate_records
        SET state = 'revoked'
        WHERE authorization_id = requested_authorization_id
          AND state IN ('pending_ack', 'active');
        RETURN 'ineligible';
    END IF;

    RETURN cyrene_workspace_device_registry
        .activate_acknowledged_delivery_without_terminal_key_guard(
            requested_authorization_id
        );
END;
$activate$;

ALTER FUNCTION cyrene_workspace_device_registry.relay_dispatch_fence(
    TEXT, TEXT, TEXT, UUID, BIGINT, BYTEA, BYTEA, BYTEA, BYTEA, BIGINT
) RENAME TO relay_dispatch_fence_without_terminal_key_guard;
REVOKE ALL ON FUNCTION
    cyrene_workspace_device_registry.relay_dispatch_fence_without_terminal_key_guard(
        TEXT, TEXT, TEXT, UUID, BIGINT, BYTEA, BYTEA, BYTEA, BYTEA, BIGINT
    )
    FROM PUBLIC, cyrene_workspace_device_registry_app,
         cyrene_workspace_device_registry_relay_reader,
         cyrene_workspace_device_registry_owner;
GRANT EXECUTE ON FUNCTION
    cyrene_workspace_device_registry.relay_dispatch_fence_without_terminal_key_guard(
        TEXT, TEXT, TEXT, UUID, BIGINT, BYTEA, BYTEA, BYTEA, BYTEA, BIGINT
    ) TO cyrene_workspace_device_registry_owner;

CREATE FUNCTION cyrene_workspace_device_registry.relay_dispatch_fence(
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
AS $dispatch_fence$
BEGIN
    PERFORM cyrene_workspace_device_registry.lock_workspace_device_key(
        expected_organization_id, expected_workspace_id, expected_device_id
    );
    IF EXISTS (
        SELECT 1
        FROM cyrene_workspace_device_registry.terminal_device_keys
        WHERE organization_id = expected_organization_id
          AND workspace_id = expected_workspace_id
          AND device_id = expected_device_id
    ) THEN
        RETURN FALSE;
    END IF;

    RETURN cyrene_workspace_device_registry
        .relay_dispatch_fence_without_terminal_key_guard(
            expected_organization_id,
            expected_workspace_id,
            expected_device_id,
            expected_registration_binding_id,
            expected_authorization_generation,
            expected_certificate_sha256,
            expected_csr_sha256,
            expected_spki_sha256,
            expected_serial_number,
            expected_not_after_unix_ms
        );
END;
$dispatch_fence$;

REVOKE ALL ON FUNCTION
    cyrene_workspace_device_registry.lock_workspace_device_key(TEXT, TEXT, TEXT),
    cyrene_workspace_device_registry.reject_terminal_device_key_write(),
    cyrene_workspace_device_registry.activate_acknowledged_delivery(BYTEA),
    cyrene_workspace_device_registry.relay_dispatch_fence(
        TEXT, TEXT, TEXT, UUID, BIGINT, BYTEA, BYTEA, BYTEA, BYTEA, BIGINT
    )
    FROM PUBLIC, cyrene_workspace_device_registry_app,
         cyrene_workspace_device_registry_relay_reader,
         cyrene_workspace_device_registry_owner;
GRANT EXECUTE ON FUNCTION
    cyrene_workspace_device_registry.lock_workspace_device_key(TEXT, TEXT, TEXT),
    cyrene_workspace_device_registry.activate_acknowledged_delivery(BYTEA)
    TO cyrene_workspace_device_registry_app,
       cyrene_workspace_device_registry_owner;
GRANT EXECUTE ON FUNCTION
    cyrene_workspace_device_registry.relay_dispatch_fence(
        TEXT, TEXT, TEXT, UUID, BIGINT, BYTEA, BYTEA, BYTEA, BYTEA, BIGINT
    ) TO cyrene_workspace_device_registry_app,
         cyrene_workspace_device_registry_relay_reader,
         cyrene_workspace_device_registry_owner;

RESET ROLE;
DO $owner_membership_cleanup$
BEGIN
    EXECUTE format(
        'REVOKE cyrene_workspace_device_registry_owner FROM %I',
        current_user
    );
END
$owner_membership_cleanup$;
