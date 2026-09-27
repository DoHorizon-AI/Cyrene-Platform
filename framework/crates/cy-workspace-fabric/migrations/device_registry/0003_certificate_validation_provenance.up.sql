-- Require validator provenance from authorization state v5 before staging or activating
-- a Workspace device certificate. Legacy live records are held for explicit review.
-- Workspace 设备证书必须带 authorization v5 的验证来源证明；旧活跃记录须先盘点。

DO $dependencies$
BEGIN
    IF to_regclass('cyrene_workspace_device_registry.certificate_records') IS NULL
       OR to_regclass('cyrene_workspace_device_authorization.authorizations') IS NULL
       OR to_regrole('cyrene_workspace_device_registry_owner') IS NULL THEN
        RAISE EXCEPTION 'device registry and authorization migrations must run before registry 0003';
    END IF;
END
$dependencies$;

DO $owner_membership$
BEGIN
    EXECUTE format('GRANT cyrene_workspace_device_registry_owner TO %I', current_user);
END
$owner_membership$;

GRANT CREATE ON SCHEMA cyrene_workspace_device_registry
    TO cyrene_workspace_device_registry_owner;
SET ROLE cyrene_workspace_device_registry_owner;

-- Authorization v5 binds validator provenance to the immutable certificate snapshot.
CREATE OR REPLACE FUNCTION cyrene_workspace_device_registry.certificate_validation_checked_at(
    authorization_payload JSONB
)
RETURNS BIGINT
LANGUAGE plpgsql
IMMUTABLE
STRICT
SET search_path = pg_catalog, cyrene_workspace_device_registry, pg_temp
AS $checked_at$
DECLARE
    raw_checked_at TEXT;
BEGIN
    IF jsonb_typeof(
        authorization_payload #> '{state,certificate_validation,checked_at_unix_ms}'
    ) IS DISTINCT FROM 'number' THEN
        RETURN NULL;
    END IF;
    raw_checked_at :=
        authorization_payload #>> '{state,certificate_validation,checked_at_unix_ms}';
    IF raw_checked_at !~ '^[1-9][0-9]{0,15}$' THEN
        RETURN NULL;
    END IF;
    RETURN raw_checked_at::BIGINT;
END;
$checked_at$;

CREATE OR REPLACE FUNCTION cyrene_workspace_device_registry.certificate_validation_matches(
    authorization_payload JSONB,
    expected_certificate_sha256 BYTEA,
    expected_registration_binding_id UUID
)
RETURNS BOOLEAN
LANGUAGE plpgsql
IMMUTABLE
STRICT
SET search_path = pg_catalog, cyrene_workspace_device_registry, pg_temp
AS $validation_matches$
BEGIN
    IF authorization_payload -> 'format_version' IS DISTINCT FROM to_jsonb(5)
       OR (
            authorization_payload #>> '{state,kind}' IS DISTINCT FROM 'delivery_pending'
            AND authorization_payload #>> '{state,kind}' IS DISTINCT FROM 'delivered'
          )
       OR authorization_payload #> '{state,certificate_validation,validation_version}'
            IS DISTINCT FROM to_jsonb(1)
       OR authorization_payload #> '{state,certificate_validation,certificate_sha256}'
            IS DISTINCT FROM
                cyrene_workspace_device_registry.bytes_to_json_array(
                    expected_certificate_sha256
                )
       OR authorization_payload #> '{state,certificate_validation,registration_binding_id}'
            IS DISTINCT FROM
                cyrene_workspace_device_registry.bytes_to_json_array(
                    uuid_send(expected_registration_binding_id)
                )
       OR cyrene_workspace_device_registry.certificate_validation_checked_at(
            authorization_payload
          ) IS NULL THEN
        RETURN FALSE;
    END IF;
    RETURN TRUE;
END;
$validation_matches$;

CREATE OR REPLACE FUNCTION cyrene_workspace_device_registry.certificate_validation_is_fresh(
    authorization_payload JSONB,
    expected_certificate_sha256 BYTEA,
    expected_registration_binding_id UUID,
    reference_time_unix_ms BIGINT
)
RETURNS BOOLEAN
LANGUAGE plpgsql
IMMUTABLE
STRICT
SET search_path = pg_catalog, cyrene_workspace_device_registry, pg_temp
AS $validation_fresh$
DECLARE
    checked_at_unix_ms BIGINT;
BEGIN
    IF reference_time_unix_ms <= 0
       OR cyrene_workspace_device_registry.certificate_validation_matches(
            authorization_payload,
            expected_certificate_sha256,
            expected_registration_binding_id
          ) IS DISTINCT FROM TRUE THEN
        RETURN FALSE;
    END IF;
    checked_at_unix_ms :=
        cyrene_workspace_device_registry.certificate_validation_checked_at(
            authorization_payload
        );
    RETURN checked_at_unix_ms <= reference_time_unix_ms
       AND reference_time_unix_ms - checked_at_unix_ms <= 30000;
END;
$validation_fresh$;

DO $legacy_live_rows$
DECLARE
    blocked_count BIGINT;
BEGIN
    SELECT count(*) INTO blocked_count
    FROM cyrene_workspace_device_registry.certificate_records AS r
    LEFT JOIN cyrene_workspace_device_authorization.authorizations AS a
        ON a.id = r.authorization_id
    LEFT JOIN LATERAL (
        SELECT convert_from(a.state_payload, 'UTF8')::JSONB AS payload
    ) AS decoded ON TRUE
    WHERE r.state IN ('active', 'pending_ack')
      AND (
          a.id IS NULL
          OR a.registration_binding_id
                IS DISTINCT FROM uuid_send(r.registration_binding_id)
          OR a.organization_id IS DISTINCT FROM r.organization_id
          OR a.workspace_id IS DISTINCT FROM r.workspace_id
          OR a.device_id IS DISTINCT FROM r.device_id
          OR a.authorization_generation IS DISTINCT FROM r.authorization_generation
          OR a.csr_sha256 IS DISTINCT FROM r.csr_sha256
          OR a.spki_sha256 IS DISTINCT FROM r.spki_sha256
          OR cyrene_workspace_device_registry.certificate_validation_matches(
                decoded.payload, r.certificate_sha256, r.registration_binding_id
             ) IS DISTINCT FROM TRUE
          OR (
              r.state = 'active'
              AND (
                  a.state_kind IS DISTINCT FROM 'delivered'
                  OR decoded.payload #>> '{state,kind}' IS DISTINCT FROM 'delivered'
              )
          )
          OR (
              r.state = 'pending_ack'
              AND NOT (
                  (a.state_kind = 'delivery_pending'
                   AND decoded.payload #>> '{state,kind}' = 'delivery_pending')
                  OR
                  (a.state_kind = 'delivered'
                   AND decoded.payload #>> '{state,kind}' = 'delivered')
              )
          )
      );
    IF blocked_count > 0 THEN
        RAISE EXCEPTION
            'cannot apply device registry validation gate while legacy live rows remain'
            USING DETAIL = blocked_count::TEXT ||
                ' active or pending_ack row(s) lack matching v5 certificate validation provenance; audit and disposition them during a controlled migration window';
    END IF;
END
$legacy_live_rows$;

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
    validation_now_unix_ms BIGINT;
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
        validation_now_unix_ms := floor(extract(epoch FROM clock_timestamp()) * 1000)::BIGINT;
        IF payload -> 'format_version' IS DISTINCT FROM to_jsonb(5)
           OR certificate_validation_matches(
                  payload, NEW.certificate_sha256, NEW.registration_binding_id
              ) IS DISTINCT FROM TRUE
           OR certificate_validation_is_fresh(
                  payload, NEW.certificate_sha256, NEW.registration_binding_id,
                  validation_now_unix_ms
              ) IS DISTINCT FROM TRUE
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
        certificate := payload #> '{state,certificate}';
        validation_now_unix_ms := floor(extract(epoch FROM clock_timestamp()) * 1000)::BIGINT;
        IF payload -> 'format_version' IS DISTINCT FROM to_jsonb(5)
           OR certificate_validation_matches(
                  payload, NEW.certificate_sha256, NEW.registration_binding_id
              ) IS DISTINCT FROM TRUE
           OR certificate_validation_checked_at(payload) IS NULL
           OR certificate_validation_checked_at(payload) > validation_now_unix_ms
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
    certificate JSONB;
    acknowledged_at BIGINT;
    validation_checked_at BIGINT;
    now_unix_ms BIGINT;
    previous_state TEXT;
    identity_found BOOLEAN;
    auth_found BOOLEAN;
    acknowledged_at_text TEXT;
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

    SELECT registration_binding_id, organization_id, workspace_id, device_id,
           approval_id, authorization_generation, csr_sha256, spki_sha256,
           state_kind, state_payload
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

    IF auth_row.state_payload IS NULL
       OR octet_length(auth_row.state_payload) > 33554432 THEN
        UPDATE cyrene_workspace_device_registry.certificate_records
        SET state = 'ineligible'
        WHERE authorization_id = requested_authorization_id;
        RETURN 'ineligible';
    END IF;

    payload := convert_from(auth_row.state_payload, 'UTF8')::JSONB;
    receipt := payload #> '{state,receipt}';
    certificate := payload #> '{state,certificate}';
    now_unix_ms := floor(extract(epoch FROM clock_timestamp()) * 1000)::BIGINT;
    validation_checked_at :=
        cyrene_workspace_device_registry.certificate_validation_checked_at(payload);

    IF auth_row.registration_binding_id
            IS DISTINCT FROM uuid_send(registry_row.registration_binding_id)
       OR auth_row.organization_id IS DISTINCT FROM registry_row.organization_id
       OR auth_row.workspace_id IS DISTINCT FROM registry_row.workspace_id
       OR auth_row.device_id IS DISTINCT FROM registry_row.device_id
       OR auth_row.authorization_generation
            IS DISTINCT FROM registry_row.authorization_generation
       OR auth_row.csr_sha256 IS DISTINCT FROM registry_row.csr_sha256
       OR auth_row.spki_sha256 IS DISTINCT FROM registry_row.spki_sha256
       OR auth_row.approval_id IS NULL
       OR payload -> 'format_version' IS DISTINCT FROM to_jsonb(5)
       OR payload #>> '{state,kind}' IS DISTINCT FROM 'delivered'
       OR payload #> '{state,approval_id}' IS DISTINCT FROM
            cyrene_workspace_device_registry.bytes_to_json_array(auth_row.approval_id)
       OR cyrene_workspace_device_registry.certificate_validation_matches(
              payload,
              registry_row.certificate_sha256,
              registry_row.registration_binding_id
          ) IS DISTINCT FROM TRUE
       OR validation_checked_at IS NULL
       OR validation_checked_at > now_unix_ms
       OR certificate -> 'registration_binding_id' IS DISTINCT FROM
            cyrene_workspace_device_registry.bytes_to_json_array(
                uuid_send(registry_row.registration_binding_id)
            )
       OR certificate #> '{device_key,organization_id}' IS DISTINCT FROM
            to_jsonb(registry_row.organization_id)
       OR certificate #> '{device_key,workspace_id}' IS DISTINCT FROM
            to_jsonb(registry_row.workspace_id)
       OR certificate #> '{device_key,device_id}' IS DISTINCT FROM
            to_jsonb(registry_row.device_id)
       OR certificate #> '{scope,organization_id}' IS DISTINCT FROM
            to_jsonb(registry_row.organization_id)
       OR certificate #> '{scope,workspace_id}' IS DISTINCT FROM
            to_jsonb(registry_row.workspace_id)
       OR certificate #> '{authorization_generation}' IS DISTINCT FROM
            to_jsonb(registry_row.authorization_generation)
       OR certificate -> 'spki_sha256' IS DISTINCT FROM
            cyrene_workspace_device_registry.bytes_to_json_array(registry_row.spki_sha256)
       OR certificate -> 'serial_number' IS DISTINCT FROM
            cyrene_workspace_device_registry.bytes_to_json_array(registry_row.serial_number)
       OR certificate -> 'not_after_unix_ms' IS DISTINCT FROM
            to_jsonb(registry_row.not_after_unix_ms)
       OR receipt -> 'authorization_id' IS DISTINCT FROM
            cyrene_workspace_device_registry.bytes_to_json_array(
                registry_row.authorization_id
            )
       OR receipt -> 'delivery_id' IS DISTINCT FROM
            cyrene_workspace_device_registry.bytes_to_json_array(registry_row.delivery_id)
       OR receipt -> 'device_id' IS DISTINCT FROM to_jsonb(registry_row.device_id)
       OR receipt -> 'authorization_generation' IS DISTINCT FROM
            to_jsonb(registry_row.authorization_generation)
       OR receipt -> 'certificate_sha256' IS DISTINCT FROM
            cyrene_workspace_device_registry.bytes_to_json_array(
                registry_row.certificate_sha256
            )
       OR receipt -> 'csr_sha256' IS DISTINCT FROM
            cyrene_workspace_device_registry.bytes_to_json_array(registry_row.csr_sha256)
       OR receipt -> 'csr_spki_sha256' IS DISTINCT FROM
            cyrene_workspace_device_registry.bytes_to_json_array(registry_row.spki_sha256) THEN
        UPDATE cyrene_workspace_device_registry.certificate_records
        SET state = 'ineligible'
        WHERE authorization_id = requested_authorization_id;
        RETURN 'ineligible';
    END IF;

    acknowledged_at_text := receipt ->> 'acknowledged_at_unix_ms';
    IF jsonb_typeof(receipt -> 'acknowledged_at_unix_ms') IS DISTINCT FROM 'number'
       OR acknowledged_at_text !~ '^[1-9][0-9]{0,15}$' THEN
        UPDATE cyrene_workspace_device_registry.certificate_records
        SET state = 'ineligible'
        WHERE authorization_id = requested_authorization_id;
        RETURN 'ineligible';
    END IF;
    acknowledged_at := acknowledged_at_text::BIGINT;
    IF acknowledged_at >= registry_row.delivery_deadline_unix_ms
       OR acknowledged_at > now_unix_ms
       OR acknowledged_at >= registry_row.not_after_unix_ms
       OR registry_row.not_after_unix_ms <= now_unix_ms THEN
        IF registry_row.not_after_unix_ms <= now_unix_ms THEN
            UPDATE cyrene_workspace_device_registry.certificate_records
            SET state = 'expired'
            WHERE authorization_id = requested_authorization_id;
        ELSE
            UPDATE cyrene_workspace_device_registry.certificate_records
            SET state = 'ineligible'
            WHERE authorization_id = requested_authorization_id;
        END IF;
        RETURN 'ineligible';
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
    certificate JSONB;
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
    certificate := payload #> '{state,certificate}';
    now_unix_ms := floor(extract(epoch FROM clock_timestamp()) * 1000)::BIGINT;
    IF payload -> 'format_version' IS DISTINCT FROM to_jsonb(5)
       OR cyrene_workspace_device_registry.certificate_validation_matches(
              payload, certificate_row.certificate_sha256,
              certificate_row.registration_binding_id
          ) IS DISTINCT FROM TRUE
       OR cyrene_workspace_device_registry.certificate_validation_checked_at(payload) IS NULL
       OR cyrene_workspace_device_registry.certificate_validation_checked_at(payload) > now_unix_ms
       OR certificate -> 'registration_binding_id' IS DISTINCT FROM
            cyrene_workspace_device_registry.bytes_to_json_array(
                uuid_send(certificate_row.registration_binding_id)
            )
       OR certificate #> '{device_key,organization_id}' IS DISTINCT FROM
            to_jsonb(certificate_row.organization_id)
       OR certificate #> '{device_key,workspace_id}' IS DISTINCT FROM
            to_jsonb(certificate_row.workspace_id)
       OR certificate #> '{device_key,device_id}' IS DISTINCT FROM
            to_jsonb(certificate_row.device_id)
       OR certificate #> '{scope,organization_id}' IS DISTINCT FROM
            to_jsonb(certificate_row.organization_id)
       OR certificate #> '{scope,workspace_id}' IS DISTINCT FROM
            to_jsonb(certificate_row.workspace_id)
       OR certificate #> '{authorization_generation}' IS DISTINCT FROM
            to_jsonb(certificate_row.authorization_generation)
       OR certificate -> 'spki_sha256' IS DISTINCT FROM
            cyrene_workspace_device_registry.bytes_to_json_array(certificate_row.spki_sha256)
       OR certificate -> 'serial_number' IS DISTINCT FROM
            cyrene_workspace_device_registry.bytes_to_json_array(certificate_row.serial_number)
       OR certificate -> 'not_after_unix_ms' IS DISTINCT FROM
            to_jsonb(certificate_row.not_after_unix_ms)
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

REVOKE ALL ON FUNCTION
    cyrene_workspace_device_registry.certificate_validation_checked_at(JSONB),
    cyrene_workspace_device_registry.certificate_validation_matches(JSONB, BYTEA, UUID),
    cyrene_workspace_device_registry.certificate_validation_is_fresh(JSONB, BYTEA, UUID, BIGINT)
    FROM PUBLIC, cyrene_workspace_device_registry_app;
GRANT EXECUTE ON FUNCTION
    cyrene_workspace_device_registry.certificate_validation_checked_at(JSONB),
    cyrene_workspace_device_registry.certificate_validation_matches(JSONB, BYTEA, UUID),
    cyrene_workspace_device_registry.certificate_validation_is_fresh(JSONB, BYTEA, UUID, BIGINT)
    TO cyrene_workspace_device_registry_owner;

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
