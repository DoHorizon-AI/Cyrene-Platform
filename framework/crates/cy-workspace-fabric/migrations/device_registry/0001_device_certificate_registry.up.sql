-- Registry metadata is inert until the device has durably acknowledged the
-- exact certificate and the Directory generation is still current.
-- PostgreSQL 注册表记录在设备持久 ACK 且 Directory 代次仍有效前始终不可活动。

DO $dependencies$
BEGIN
    IF to_regclass('cyrene_workspace_device_authorization.authorizations') IS NULL
       OR to_regclass('cyrene_workspace_directory.device_registration_bindings') IS NULL
       OR to_regclass('cyrene_workspace_directory.workspace_device_identities') IS NULL THEN
        RAISE EXCEPTION 'Directory and device authorization schemas must be migrated first';
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = 'cyrene_workspace_device_authorization'
          AND table_name = 'authorizations'
          AND column_name = 'registration_binding_id'
    ) OR NOT EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = 'cyrene_workspace_device_authorization'
          AND table_name = 'authorizations'
          AND column_name = 'authorization_generation'
    ) OR NOT EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = 'cyrene_workspace_device_authorization'
          AND table_name = 'authorizations'
          AND column_name = 'state_payload'
    ) THEN
        RAISE EXCEPTION 'device authorization V2 binding and payload columns are required';
    END IF;
END
$dependencies$;

DO $roles$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_roles WHERE rolname = 'cyrene_workspace_device_registry_app'
    ) THEN
        CREATE ROLE cyrene_workspace_device_registry_app NOLOGIN;
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_roles WHERE rolname = 'cyrene_workspace_device_registry_owner'
    ) THEN
        CREATE ROLE cyrene_workspace_device_registry_owner NOLOGIN;
    END IF;
END
$roles$;

ALTER ROLE cyrene_workspace_device_registry_app
    NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS NOINHERIT;
ALTER ROLE cyrene_workspace_device_registry_owner
    NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS NOINHERIT;
REVOKE cyrene_workspace_device_registry_owner
    FROM cyrene_workspace_device_registry_app;

DO $owner_membership_guard$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM pg_auth_members
        WHERE roleid = 'cyrene_workspace_device_registry_owner'::regrole
          AND member <> current_user::regrole
    ) THEN
        RAISE EXCEPTION 'device registry function-owner role must not have runtime members';
    END IF;
END
$owner_membership_guard$;

DO $owner_membership$
BEGIN
    EXECUTE format('GRANT cyrene_workspace_device_registry_owner TO %I', current_user);
END
$owner_membership$;

CREATE SCHEMA IF NOT EXISTS cyrene_workspace_device_registry;
REVOKE ALL ON SCHEMA cyrene_workspace_device_registry FROM PUBLIC;
REVOKE ALL ON SCHEMA
    cyrene_workspace_device_registry,
    cyrene_workspace_device_authorization,
    cyrene_workspace_directory
    FROM cyrene_workspace_device_registry_app,
         cyrene_workspace_device_registry_owner;
GRANT USAGE ON SCHEMA cyrene_workspace_device_registry,
    cyrene_workspace_device_authorization,
    cyrene_workspace_directory
    TO cyrene_workspace_device_registry_app;
GRANT USAGE ON SCHEMA cyrene_workspace_device_registry,
    cyrene_workspace_device_authorization,
    cyrene_workspace_directory
    TO cyrene_workspace_device_registry_owner;
GRANT CREATE ON SCHEMA cyrene_workspace_device_registry
    TO cyrene_workspace_device_registry_owner;

CREATE TABLE cyrene_workspace_device_registry.certificate_records (
    authorization_id BYTEA PRIMARY KEY
        CHECK (octet_length(authorization_id) = 16),
    registration_binding_id UUID NOT NULL,
    organization_id TEXT NOT NULL
        CHECK (btrim(organization_id) <> '' AND length(organization_id) <= 256),
    workspace_id TEXT NOT NULL
        CHECK (btrim(workspace_id) <> '' AND length(workspace_id) <= 256),
    device_id TEXT NOT NULL
        CHECK (btrim(device_id) <> '' AND length(device_id) <= 256),
    authorization_generation BIGINT NOT NULL CHECK (authorization_generation > 0),
    delivery_id BYTEA NOT NULL CHECK (octet_length(delivery_id) = 16),
    certificate_sha256 BYTEA NOT NULL CHECK (octet_length(certificate_sha256) = 32),
    csr_sha256 BYTEA NOT NULL CHECK (octet_length(csr_sha256) = 32),
    spki_sha256 BYTEA NOT NULL CHECK (octet_length(spki_sha256) = 32),
    serial_number BYTEA NOT NULL
        CHECK (octet_length(serial_number) BETWEEN 1 AND 256),
    not_after_unix_ms BIGINT NOT NULL CHECK (not_after_unix_ms > 0),
    delivery_deadline_unix_ms BIGINT NOT NULL CHECK (delivery_deadline_unix_ms > 0),
    state TEXT NOT NULL CHECK (
        state IN ('pending_ack', 'active', 'revoked', 'expired', 'stale', 'ineligible')
    ),
    acknowledged_at_unix_ms BIGINT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    activated_at TIMESTAMPTZ,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT certificate_records_ack_state CHECK (
        state <> 'active' OR acknowledged_at_unix_ms IS NOT NULL
    ),
    CONSTRAINT certificate_records_inert_state CHECK (
        state <> 'pending_ack' OR acknowledged_at_unix_ms IS NULL
    ),
    CONSTRAINT certificate_records_binding_fk FOREIGN KEY (registration_binding_id)
        REFERENCES cyrene_workspace_directory.device_registration_bindings (binding_id)
        ON DELETE RESTRICT,
    CONSTRAINT certificate_records_authorization_fk FOREIGN KEY (authorization_id)
        REFERENCES cyrene_workspace_device_authorization.authorizations (id)
        ON DELETE RESTRICT,
    CONSTRAINT certificate_records_one_generation UNIQUE (
        organization_id, workspace_id, device_id, authorization_generation
    ),
    CONSTRAINT certificate_records_fingerprint_unique UNIQUE (certificate_sha256)
);

CREATE INDEX certificate_records_pending_ack_idx
    ON cyrene_workspace_device_registry.certificate_records (created_at, authorization_id)
    WHERE state = 'pending_ack';
CREATE INDEX certificate_records_active_device_idx
    ON cyrene_workspace_device_registry.certificate_records
        (organization_id, workspace_id, device_id, authorization_generation)
    WHERE state = 'active';

CREATE OR REPLACE FUNCTION cyrene_workspace_device_registry.bytes_to_json_array(value BYTEA)
RETURNS JSONB
LANGUAGE plpgsql
IMMUTABLE
STRICT
SET search_path = pg_catalog, cyrene_workspace_device_registry, pg_temp
AS $bytes$
DECLARE
    result JSONB := '[]'::JSONB;
    offset_value INTEGER;
BEGIN
    FOR offset_value IN 0..octet_length(value) - 1 LOOP
        result := result || to_jsonb(get_byte(value, offset_value));
    END LOOP;
    RETURN result;
END;
$bytes$;

CREATE OR REPLACE FUNCTION cyrene_workspace_device_registry.guard_certificate_record()
RETURNS TRIGGER
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, cyrene_workspace_device_registry, pg_temp
AS $guard$
DECLARE
    auth_row RECORD;
    identity_generation BIGINT;
    binding_row RECORD;
    payload JSONB;
    receipt JSONB;
    certificate JSONB;
    ack_time BIGINT;
BEGIN
    IF TG_OP = 'INSERT' THEN
        IF NEW.state <> 'pending_ack' OR NEW.acknowledged_at_unix_ms IS NOT NULL
           OR NEW.activated_at IS NOT NULL THEN
            RAISE EXCEPTION 'new Workspace device certificates must remain pending ACK';
        END IF;

        SELECT current_authorization_generation INTO identity_generation
        FROM cyrene_workspace_directory.workspace_device_identities
        WHERE organization_id = NEW.organization_id
          AND workspace_id = NEW.workspace_id
          AND device_id = NEW.device_id
        FOR SHARE;
        IF identity_generation IS DISTINCT FROM NEW.authorization_generation THEN
            RAISE EXCEPTION 'Workspace device certificate generation is stale';
        END IF;

        SELECT organization_id, workspace_id, device_id, authorization_generation,
               csr_sha256, spki_sha256
        INTO binding_row
        FROM cyrene_workspace_directory.device_registration_bindings
        WHERE binding_id = NEW.registration_binding_id;
        IF NOT FOUND THEN
            RAISE EXCEPTION 'Workspace device certificate Directory binding is unavailable';
        END IF;
        IF binding_row.organization_id IS DISTINCT FROM NEW.organization_id
           OR binding_row.workspace_id IS DISTINCT FROM NEW.workspace_id
           OR binding_row.device_id IS DISTINCT FROM NEW.device_id
           OR binding_row.authorization_generation IS DISTINCT FROM NEW.authorization_generation
           OR binding_row.csr_sha256 IS DISTINCT FROM NEW.csr_sha256
           OR binding_row.spki_sha256 IS DISTINCT FROM NEW.spki_sha256 THEN
            RAISE EXCEPTION 'Workspace device certificate Directory binding mismatch';
        END IF;

        SELECT id, organization_id, workspace_id, registration_binding_id, device_id,
               approval_id,
               authorization_generation, csr_sha256, spki_sha256, state_kind,
               state_deadline_unix_ms, state_payload
        INTO auth_row
        FROM cyrene_workspace_device_authorization.authorizations
        WHERE id = NEW.authorization_id
        FOR SHARE;
        IF NOT FOUND THEN
            RAISE EXCEPTION 'Workspace device authorization is unavailable';
        END IF;
        IF auth_row.state_kind IS DISTINCT FROM 'delivery_pending'
           OR auth_row.organization_id IS DISTINCT FROM NEW.organization_id
           OR auth_row.workspace_id IS DISTINCT FROM NEW.workspace_id
           OR auth_row.registration_binding_id IS DISTINCT FROM uuid_send(NEW.registration_binding_id)
           OR auth_row.device_id IS DISTINCT FROM NEW.device_id
           OR auth_row.authorization_generation IS DISTINCT FROM NEW.authorization_generation
           OR auth_row.csr_sha256 IS DISTINCT FROM NEW.csr_sha256
           OR auth_row.spki_sha256 IS DISTINCT FROM NEW.spki_sha256 THEN
            RAISE EXCEPTION 'durable Workspace device delivery is unavailable';
        END IF;

        payload := convert_from(auth_row.state_payload, 'UTF8')::JSONB;
        certificate := payload #> '{state,certificate}';
        IF payload -> 'format_version' IS DISTINCT FROM to_jsonb(2)
           OR payload #>> '{state,kind}' IS DISTINCT FROM 'delivery_pending'
           OR payload #> '{state,approval_id}' IS DISTINCT FROM
                cyrene_workspace_device_registry.bytes_to_json_array(auth_row.approval_id)
           OR payload #> '{state,delivery_id}' IS DISTINCT FROM
                cyrene_workspace_device_registry.bytes_to_json_array(NEW.delivery_id)
           OR payload #> '{state,certificate_sha256}' IS DISTINCT FROM
                cyrene_workspace_device_registry.bytes_to_json_array(NEW.certificate_sha256)
           OR payload #> '{state,delivery_deadline_unix_ms}'
                IS DISTINCT FROM to_jsonb(NEW.delivery_deadline_unix_ms)
           OR auth_row.state_deadline_unix_ms IS DISTINCT FROM NEW.delivery_deadline_unix_ms
           OR certificate -> 'registration_binding_id' IS DISTINCT FROM
                cyrene_workspace_device_registry.bytes_to_json_array(uuid_send(NEW.registration_binding_id))
           OR certificate #> '{device_key,organization_id}' IS DISTINCT FROM to_jsonb(NEW.organization_id)
           OR certificate #> '{device_key,workspace_id}' IS DISTINCT FROM to_jsonb(NEW.workspace_id)
           OR certificate #> '{device_key,device_id}' IS DISTINCT FROM to_jsonb(NEW.device_id)
           OR certificate #> '{scope,organization_id}' IS DISTINCT FROM to_jsonb(NEW.organization_id)
           OR certificate #> '{scope,workspace_id}' IS DISTINCT FROM to_jsonb(NEW.workspace_id)
           OR certificate #> '{authorization_generation}'
                IS DISTINCT FROM to_jsonb(NEW.authorization_generation)
           OR certificate -> 'spki_sha256' IS DISTINCT FROM
                cyrene_workspace_device_registry.bytes_to_json_array(NEW.spki_sha256)
           OR certificate -> 'serial_number' IS DISTINCT FROM
                cyrene_workspace_device_registry.bytes_to_json_array(NEW.serial_number)
           OR certificate -> 'not_after_unix_ms'
                IS DISTINCT FROM to_jsonb(NEW.not_after_unix_ms)
           OR NEW.delivery_deadline_unix_ms <= floor(extract(epoch FROM clock_timestamp()) * 1000)
           OR NEW.not_after_unix_ms <= floor(extract(epoch FROM clock_timestamp()) * 1000) THEN
            RAISE EXCEPTION 'Workspace device delivery snapshot does not match authorization';
        END IF;
        RETURN NEW;
    END IF;

    IF ROW(
        NEW.authorization_id,
        NEW.registration_binding_id,
        NEW.organization_id,
        NEW.workspace_id,
        NEW.device_id,
        NEW.authorization_generation,
        NEW.delivery_id,
        NEW.certificate_sha256,
        NEW.csr_sha256,
        NEW.spki_sha256,
        NEW.serial_number,
        NEW.not_after_unix_ms,
        NEW.delivery_deadline_unix_ms
    ) IS DISTINCT FROM ROW(
        OLD.authorization_id,
        OLD.registration_binding_id,
        OLD.organization_id,
        OLD.workspace_id,
        OLD.device_id,
        OLD.authorization_generation,
        OLD.delivery_id,
        OLD.certificate_sha256,
        OLD.csr_sha256,
        OLD.spki_sha256,
        OLD.serial_number,
        OLD.not_after_unix_ms,
        OLD.delivery_deadline_unix_ms
    ) THEN
        RAISE EXCEPTION 'Workspace device certificate identity is immutable';
    END IF;

    IF NEW.state = OLD.state AND NEW.state <> 'active' THEN
        RETURN NEW;
    END IF;
    IF NEW.state <> OLD.state AND (
       OLD.state IN ('revoked', 'expired', 'stale', 'ineligible')
       OR (OLD.state = 'active' AND NEW.state NOT IN
           ('revoked', 'stale', 'expired', 'ineligible'))
       OR (OLD.state = 'pending_ack' AND NEW.state NOT IN
           ('active', 'revoked', 'expired', 'stale', 'ineligible'))) THEN
        RAISE EXCEPTION 'invalid Workspace device certificate state transition';
    END IF;

    IF NEW.state = 'active' THEN
        -- Lock order matches the authorization registration adapter:
        -- Directory identity, authorization, then registry record.
        SELECT current_authorization_generation INTO identity_generation
        FROM cyrene_workspace_directory.workspace_device_identities
        WHERE organization_id = NEW.organization_id
          AND workspace_id = NEW.workspace_id
          AND device_id = NEW.device_id
        FOR SHARE;
        IF identity_generation IS DISTINCT FROM NEW.authorization_generation THEN
            RAISE EXCEPTION 'Workspace device certificate generation is stale';
        END IF;

        SELECT binding_id, device_id, authorization_generation,
               organization_id, workspace_id, csr_sha256, spki_sha256
        INTO binding_row
        FROM cyrene_workspace_directory.device_registration_bindings
        WHERE binding_id = NEW.registration_binding_id;
        IF NOT FOUND THEN
            RAISE EXCEPTION 'Workspace device certificate Directory binding is unavailable';
        END IF;
        IF binding_row.organization_id IS DISTINCT FROM NEW.organization_id
           OR binding_row.workspace_id IS DISTINCT FROM NEW.workspace_id
           OR binding_row.device_id IS DISTINCT FROM NEW.device_id
           OR binding_row.authorization_generation IS DISTINCT FROM NEW.authorization_generation
           OR binding_row.csr_sha256 IS DISTINCT FROM NEW.csr_sha256
           OR binding_row.spki_sha256 IS DISTINCT FROM NEW.spki_sha256 THEN
            RAISE EXCEPTION 'Workspace device certificate Directory binding mismatch';
        END IF;

        SELECT id, organization_id, workspace_id, registration_binding_id, device_id,
               approval_id,
               authorization_generation, csr_sha256, spki_sha256, state_kind,
               state_payload
        INTO auth_row
        FROM cyrene_workspace_device_authorization.authorizations
        WHERE id = NEW.authorization_id
        FOR SHARE;
        IF NOT FOUND THEN
            RAISE EXCEPTION 'Workspace device authorization is unavailable';
        END IF;
        IF auth_row.state_kind IS DISTINCT FROM 'delivered'
           OR auth_row.organization_id IS DISTINCT FROM NEW.organization_id
           OR auth_row.workspace_id IS DISTINCT FROM NEW.workspace_id
           OR auth_row.registration_binding_id IS DISTINCT FROM uuid_send(NEW.registration_binding_id)
           OR auth_row.device_id IS DISTINCT FROM NEW.device_id
           OR auth_row.authorization_generation IS DISTINCT FROM NEW.authorization_generation
           OR auth_row.csr_sha256 IS DISTINCT FROM NEW.csr_sha256
           OR auth_row.spki_sha256 IS DISTINCT FROM NEW.spki_sha256 THEN
            RAISE EXCEPTION 'durable Workspace device delivery ACK is unavailable';
        END IF;

        payload := convert_from(auth_row.state_payload, 'UTF8')::JSONB;
        receipt := payload #> '{state,receipt}';
        IF payload -> 'format_version' IS DISTINCT FROM to_jsonb(2)
           OR payload #>> '{state,kind}' IS DISTINCT FROM 'delivered'
           OR payload #> '{state,approval_id}' IS DISTINCT FROM
                cyrene_workspace_device_registry.bytes_to_json_array(auth_row.approval_id)
           OR receipt -> 'authorization_id' IS DISTINCT FROM
                cyrene_workspace_device_registry.bytes_to_json_array(NEW.authorization_id)
           OR receipt -> 'delivery_id' IS DISTINCT FROM
                cyrene_workspace_device_registry.bytes_to_json_array(NEW.delivery_id)
           OR receipt -> 'device_id' IS DISTINCT FROM to_jsonb(NEW.device_id)
           OR receipt -> 'authorization_generation'
                IS DISTINCT FROM to_jsonb(NEW.authorization_generation)
           OR receipt -> 'certificate_sha256' IS DISTINCT FROM
                cyrene_workspace_device_registry.bytes_to_json_array(NEW.certificate_sha256)
           OR receipt -> 'csr_sha256' IS DISTINCT FROM
                cyrene_workspace_device_registry.bytes_to_json_array(NEW.csr_sha256)
           OR receipt -> 'csr_spki_sha256' IS DISTINCT FROM
                cyrene_workspace_device_registry.bytes_to_json_array(NEW.spki_sha256) THEN
            RAISE EXCEPTION 'durable Workspace device delivery ACK does not match registry';
        END IF;

        ack_time := (receipt ->> 'acknowledged_at_unix_ms')::BIGINT;
        IF ack_time IS NULL
           OR receipt -> 'acknowledged_at_unix_ms' IS DISTINCT FROM to_jsonb(ack_time)
           OR ack_time >= NEW.delivery_deadline_unix_ms
           OR ack_time > floor(extract(epoch FROM clock_timestamp()) * 1000)::BIGINT
           OR ack_time >= NEW.not_after_unix_ms
           OR NEW.not_after_unix_ms <= floor(extract(epoch FROM clock_timestamp()) * 1000)
           OR NEW.acknowledged_at_unix_ms IS DISTINCT FROM ack_time THEN
            RAISE EXCEPTION 'Workspace device certificate ACK or validity window expired';
        END IF;
        IF NEW.activated_at IS NULL THEN
            NEW.activated_at := clock_timestamp();
        END IF;
    END IF;

    NEW.updated_at := clock_timestamp();
    RETURN NEW;
END;
$guard$;

CREATE TRIGGER certificate_records_guard
    BEFORE INSERT OR UPDATE ON cyrene_workspace_device_registry.certificate_records
    FOR EACH ROW EXECUTE FUNCTION
        cyrene_workspace_device_registry.guard_certificate_record();

CREATE OR REPLACE FUNCTION cyrene_workspace_device_registry.activate_acknowledged_delivery(
    requested_authorization_id BYTEA
)
RETURNS TEXT
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, cyrene_workspace_device_registry, pg_temp
AS $activate$
DECLARE
    candidate RECORD;
    directory_generation BIGINT;
    auth_row RECORD;
    registry_row RECORD;
    payload JSONB;
    receipt JSONB;
    acknowledged_at BIGINT;
    now_unix_ms BIGINT;
    previous_state TEXT;
    identity_found BOOLEAN;
    auth_found BOOLEAN;
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

    -- Lock order is Directory identity -> authorization -> registry row.
    SELECT current_authorization_generation
    INTO directory_generation
    FROM cyrene_workspace_directory.workspace_device_identities
    WHERE organization_id = candidate.organization_id
      AND workspace_id = candidate.workspace_id
      AND device_id = candidate.device_id
    FOR SHARE;
    identity_found := FOUND;

    SELECT approval_id, state_kind, state_payload
    INTO auth_row
    FROM cyrene_workspace_device_authorization.authorizations
    WHERE id = requested_authorization_id
    FOR SHARE;
    auth_found := FOUND;

    SELECT * INTO registry_row
    FROM cyrene_workspace_device_registry.certificate_records
    WHERE authorization_id = requested_authorization_id
    FOR UPDATE;
    IF NOT FOUND THEN
        RETURN 'not_staged';
    END IF;
    previous_state := registry_row.state;
    IF previous_state IN ('revoked', 'expired', 'stale', 'ineligible') THEN
        RETURN 'ineligible';
    END IF;

    IF NOT identity_found
       OR directory_generation IS DISTINCT FROM registry_row.authorization_generation THEN
        UPDATE cyrene_workspace_device_registry.certificate_records
        SET state = 'stale'
        WHERE authorization_id = requested_authorization_id;
        RETURN 'ineligible';
    END IF;
    IF NOT auth_found THEN
        UPDATE cyrene_workspace_device_registry.certificate_records
        SET state = 'ineligible'
        WHERE authorization_id = requested_authorization_id;
        RETURN 'ineligible';
    END IF;
    IF auth_row.state_kind IS DISTINCT FROM 'delivered' THEN
        IF auth_row.state_kind = 'delivery_expired' THEN
            UPDATE cyrene_workspace_device_registry.certificate_records
            SET state = 'expired'
            WHERE authorization_id = requested_authorization_id;
            RETURN 'ineligible';
        END IF;
        IF auth_row.state_kind IN (
            'retirement_pending', 'issuance_failed', 'denied', 'consumed', 'expired'
        ) THEN
            UPDATE cyrene_workspace_device_registry.certificate_records
            SET state = 'ineligible'
            WHERE authorization_id = requested_authorization_id;
            RETURN 'ineligible';
        END IF;
        RETURN 'awaiting_acknowledgement';
    END IF;

    payload := convert_from(auth_row.state_payload, 'UTF8')::JSONB;
    receipt := payload #> '{state,receipt}';
    IF payload -> 'format_version' IS DISTINCT FROM to_jsonb(2)
       OR payload #>> '{state,kind}' IS DISTINCT FROM 'delivered' THEN
        RAISE EXCEPTION 'unsupported Workspace device authorization payload';
    END IF;
    acknowledged_at := (receipt ->> 'acknowledged_at_unix_ms')::BIGINT;
    now_unix_ms := floor(extract(epoch FROM clock_timestamp()) * 1000)::BIGINT;
    IF acknowledged_at IS NULL
       OR acknowledged_at >= registry_row.delivery_deadline_unix_ms
       OR acknowledged_at > now_unix_ms
       OR acknowledged_at >= registry_row.not_after_unix_ms
       OR registry_row.not_after_unix_ms <= now_unix_ms THEN
        IF registry_row.not_after_unix_ms <= now_unix_ms THEN
            UPDATE cyrene_workspace_device_registry.certificate_records
            SET state = 'expired'
            WHERE authorization_id = requested_authorization_id;
            RETURN 'ineligible';
        END IF;
        RAISE EXCEPTION 'Workspace device ACK is outside its valid window';
    END IF;

    UPDATE cyrene_workspace_device_registry.certificate_records
    SET state = 'active', acknowledged_at_unix_ms = acknowledged_at
    WHERE authorization_id = requested_authorization_id;
    IF previous_state = 'active' THEN
        RETURN 'already_active';
    END IF;
    RETURN 'activated';
END;
$activate$;

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
    FOR SHARE;
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

REVOKE ALL ON cyrene_workspace_device_registry.certificate_records FROM PUBLIC;
REVOKE ALL ON FUNCTION
    cyrene_workspace_device_registry.bytes_to_json_array(BYTEA) FROM PUBLIC;
REVOKE ALL ON FUNCTION
    cyrene_workspace_device_registry.guard_certificate_record() FROM PUBLIC;
REVOKE ALL ON FUNCTION
    cyrene_workspace_device_registry.activate_acknowledged_delivery(BYTEA) FROM PUBLIC;
REVOKE ALL ON FUNCTION
    cyrene_workspace_device_registry.revoke_workspace_device(TEXT, TEXT, TEXT) FROM PUBLIC;
REVOKE ALL ON FUNCTION
    cyrene_workspace_device_registry.bytes_to_json_array(BYTEA),
    cyrene_workspace_device_registry.guard_certificate_record(),
    cyrene_workspace_device_registry.activate_acknowledged_delivery(BYTEA),
    cyrene_workspace_device_registry.revoke_workspace_device(TEXT, TEXT, TEXT)
    FROM cyrene_workspace_device_registry_app;
REVOKE ALL ON
    cyrene_workspace_device_authorization.authorizations,
    cyrene_workspace_directory.device_registration_bindings,
    cyrene_workspace_directory.workspace_device_identities
    FROM cyrene_workspace_device_registry_app,
         cyrene_workspace_device_registry_owner;
REVOKE UPDATE (state_kind)
    ON cyrene_workspace_device_authorization.authorizations
    FROM cyrene_workspace_device_registry_app,
         cyrene_workspace_device_registry_owner;
REVOKE UPDATE (current_authorization_generation)
    ON cyrene_workspace_directory.workspace_device_identities
    FROM cyrene_workspace_device_registry_app,
         cyrene_workspace_device_registry_owner;
REVOKE SELECT (
    id, registration_binding_id, organization_id, workspace_id, device_id,
    approval_id, authorization_generation, csr_sha256, spki_sha256, state_kind,
    state_deadline_unix_ms, state_payload
) ON cyrene_workspace_device_authorization.authorizations
    FROM cyrene_workspace_device_registry_app,
         cyrene_workspace_device_registry_owner;
REVOKE SELECT (
    binding_id, organization_id, workspace_id, device_id,
    authorization_generation, csr_sha256, spki_sha256
) ON cyrene_workspace_directory.device_registration_bindings
    FROM cyrene_workspace_device_registry_app,
         cyrene_workspace_device_registry_owner;
REVOKE SELECT (
    organization_id, workspace_id, device_id, current_authorization_generation
) ON cyrene_workspace_directory.workspace_device_identities
    FROM cyrene_workspace_device_registry_app,
         cyrene_workspace_device_registry_owner;
REVOKE ALL ON cyrene_workspace_device_registry.certificate_records
    FROM cyrene_workspace_device_registry_app,
         cyrene_workspace_device_registry_owner;
GRANT SELECT
    ON cyrene_workspace_device_registry.certificate_records
    TO cyrene_workspace_device_registry_app;
GRANT INSERT (
    authorization_id, registration_binding_id, organization_id, workspace_id, device_id,
    authorization_generation, delivery_id, certificate_sha256, csr_sha256, spki_sha256,
    serial_number, not_after_unix_ms, delivery_deadline_unix_ms, state
) ON cyrene_workspace_device_registry.certificate_records
    TO cyrene_workspace_device_registry_app;
GRANT EXECUTE ON FUNCTION
    cyrene_workspace_device_registry.bytes_to_json_array(BYTEA)
    TO cyrene_workspace_device_registry_owner;
GRANT SELECT (
    id, registration_binding_id, organization_id, workspace_id, device_id,
    approval_id, authorization_generation, csr_sha256, spki_sha256, state_kind,
    state_deadline_unix_ms, state_payload
) ON cyrene_workspace_device_authorization.authorizations
    TO cyrene_workspace_device_registry_app;
GRANT SELECT (
    binding_id, organization_id, workspace_id, device_id,
    authorization_generation, csr_sha256, spki_sha256
) ON cyrene_workspace_directory.device_registration_bindings
    TO cyrene_workspace_device_registry_app;
GRANT SELECT (
    organization_id, workspace_id, device_id, current_authorization_generation
) ON cyrene_workspace_directory.workspace_device_identities
    TO cyrene_workspace_device_registry_app;
GRANT EXECUTE ON FUNCTION
    cyrene_workspace_device_registry.activate_acknowledged_delivery(BYTEA)
    TO cyrene_workspace_device_registry_app;
GRANT EXECUTE ON FUNCTION
    cyrene_workspace_device_registry.revoke_workspace_device(TEXT, TEXT, TEXT)
    TO cyrene_workspace_device_registry_app;

GRANT SELECT (
    id, registration_binding_id, organization_id, workspace_id, device_id,
    approval_id, authorization_generation, csr_sha256, spki_sha256, state_kind,
    state_deadline_unix_ms, state_payload
) ON cyrene_workspace_device_authorization.authorizations
    TO cyrene_workspace_device_registry_owner;
GRANT UPDATE (state_kind)
    ON cyrene_workspace_device_authorization.authorizations
    TO cyrene_workspace_device_registry_owner;
GRANT SELECT (
    binding_id, organization_id, workspace_id, device_id,
    authorization_generation, csr_sha256, spki_sha256
) ON cyrene_workspace_directory.device_registration_bindings
    TO cyrene_workspace_device_registry_owner;
GRANT SELECT (
    organization_id, workspace_id, device_id, current_authorization_generation
) ON cyrene_workspace_directory.workspace_device_identities
    TO cyrene_workspace_device_registry_owner;
GRANT UPDATE (current_authorization_generation)
    ON cyrene_workspace_directory.workspace_device_identities
    TO cyrene_workspace_device_registry_owner;
GRANT SELECT
    ON cyrene_workspace_device_registry.certificate_records
    TO cyrene_workspace_device_registry_owner;
GRANT UPDATE (state, acknowledged_at_unix_ms, activated_at, updated_at)
    ON cyrene_workspace_device_registry.certificate_records
    TO cyrene_workspace_device_registry_owner;

ALTER FUNCTION cyrene_workspace_device_registry.bytes_to_json_array(BYTEA)
    OWNER TO cyrene_workspace_device_registry_owner;
ALTER FUNCTION cyrene_workspace_device_registry.guard_certificate_record()
    OWNER TO cyrene_workspace_device_registry_owner;
ALTER FUNCTION cyrene_workspace_device_registry.activate_acknowledged_delivery(BYTEA)
    OWNER TO cyrene_workspace_device_registry_owner;
ALTER FUNCTION cyrene_workspace_device_registry.revoke_workspace_device(TEXT, TEXT, TEXT)
    OWNER TO cyrene_workspace_device_registry_owner;
REVOKE CREATE ON SCHEMA cyrene_workspace_device_registry
    FROM cyrene_workspace_device_registry_owner;

DO $owner_membership_cleanup$
BEGIN
    EXECUTE format('REVOKE cyrene_workspace_device_registry_owner FROM %I', current_user);
END
$owner_membership_cleanup$;

COMMENT ON TABLE cyrene_workspace_device_registry.certificate_records IS
    'Workspace device certificate snapshots; only exact durable Delivered ACK and current Directory generation can activate a row.';
COMMENT ON COLUMN cyrene_workspace_device_registry.certificate_records.state IS
    'pending_ack is inert; active requires durable matching ACK and current Directory generation; terminal states never reactivate.';
