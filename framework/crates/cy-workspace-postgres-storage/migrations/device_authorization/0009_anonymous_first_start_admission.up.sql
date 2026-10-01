-- Admit anonymous first-generation registrations through one PostgreSQL-owned
-- window counter and active-pending cap. No quota is seeded here: an operator
-- must insert a policy row before generation-one registered starts can pass.

DO $dependencies$
BEGIN
    IF to_regclass('cyrene_workspace_device_authorization.authorizations') IS NULL
       OR to_regclass('cyrene_workspace_directory.workspace_device_identities') IS NULL
       OR to_regclass('cyrene_workspace_directory.device_registration_bindings') IS NULL
       OR to_regrole('cyrene_workspace_device_authorization_app') IS NULL THEN
        RAISE EXCEPTION 'device authorization and Directory registration migrations must run before 0009';
    END IF;

    IF NOT has_column_privilege(
        'cyrene_workspace_device_authorization_app',
        'cyrene_workspace_device_authorization.authorizations',
        'authorization_generation',
        'INSERT'
    ) OR NOT has_column_privilege(
        'cyrene_workspace_device_authorization_app',
        'cyrene_workspace_device_authorization.authorizations',
        'registration_key_digest',
        'INSERT'
    ) OR NOT has_column_privilege(
        'cyrene_workspace_device_authorization_app',
        'cyrene_workspace_device_authorization.authorizations',
        'device_code_generation',
        'INSERT'
    ) THEN
        RAISE EXCEPTION 'device authorization migration 0004 must run before device authorization 0009';
    END IF;
END
$dependencies$;

DO $roles$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_roles
        WHERE rolname = 'cyrene_workspace_anonymous_start_admission_owner'
    ) THEN
        CREATE ROLE cyrene_workspace_anonymous_start_admission_owner NOLOGIN;
    END IF;
END
$roles$;

ALTER ROLE cyrene_workspace_anonymous_start_admission_owner
    NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS NOINHERIT;
REVOKE cyrene_workspace_anonymous_start_admission_owner
    FROM cyrene_workspace_device_authorization_app;

DO $owner_membership_guard$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM pg_auth_members
        WHERE roleid = 'cyrene_workspace_anonymous_start_admission_owner'::regrole
          AND member <> current_user::regrole
    ) THEN
        RAISE EXCEPTION 'anonymous first-start admission owner must not have runtime members';
    END IF;
END
$owner_membership_guard$;

DO $owner_membership$
BEGIN
    EXECUTE format(
        'GRANT cyrene_workspace_anonymous_start_admission_owner TO %I',
        current_user
    );
END
$owner_membership$;

GRANT USAGE ON SCHEMA
    cyrene_workspace_device_authorization,
    cyrene_workspace_directory
    TO cyrene_workspace_anonymous_start_admission_owner;
GRANT CREATE ON SCHEMA cyrene_workspace_device_authorization
    TO cyrene_workspace_anonymous_start_admission_owner;
GRANT SELECT (
    registration_key_digest,
    authorization_generation,
    expires_at_unix_ms,
    state_kind
)
    ON cyrene_workspace_device_authorization.authorizations
    TO cyrene_workspace_anonymous_start_admission_owner;
GRANT SELECT (
    registration_key_digest,
    binding_id,
    organization_id,
    workspace_id,
    device_id,
    authorization_generation
)
    ON cyrene_workspace_directory.device_registration_bindings
    TO cyrene_workspace_anonymous_start_admission_owner;
GRANT SELECT (
    organization_id,
    workspace_id,
    device_id,
    current_authorization_generation
)
    ON cyrene_workspace_directory.workspace_device_identities
    TO cyrene_workspace_anonymous_start_admission_owner;
-- PostgreSQL requires UPDATE on at least one column for SELECT ... FOR SHARE.
-- This dedicated NOLOGIN owner never changes the Directory identity itself.
GRANT UPDATE (updated_at)
    ON cyrene_workspace_directory.workspace_device_identities
    TO cyrene_workspace_anonymous_start_admission_owner;
GRANT TRIGGER ON cyrene_workspace_device_authorization.authorizations
    TO cyrene_workspace_anonymous_start_admission_owner;

CREATE INDEX authorizations_anonymous_first_start_active_idx
    ON cyrene_workspace_device_authorization.authorizations (expires_at_unix_ms)
    WHERE authorization_generation = 1
      AND state_kind IN (
          'pending', 'awaiting_webauthn', 'verifying_webauthn', 'issuing',
          'delivery_pending'
      );

SET ROLE cyrene_workspace_anonymous_start_admission_owner;

CREATE TABLE cyrene_workspace_device_authorization.anonymous_first_start_admission_policy (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton IS TRUE),
    write_window_ms BIGINT NOT NULL CHECK (write_window_ms > 0),
    maximum_first_starts_per_window BIGINT NOT NULL
        CHECK (maximum_first_starts_per_window >= 0),
    maximum_pending_active BIGINT NOT NULL
        CHECK (maximum_pending_active >= 0)
);

CREATE TABLE cyrene_workspace_device_authorization.anonymous_first_start_admission_state (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton IS TRUE),
    write_window_ms BIGINT CHECK (write_window_ms IS NULL OR write_window_ms > 0),
    window_number BIGINT CHECK (window_number IS NULL OR window_number >= 0),
    accepted_first_starts BIGINT NOT NULL CHECK (accepted_first_starts >= 0),
    CHECK ((write_window_ms IS NULL) = (window_number IS NULL))
);

-- The state row is shared by every application replica. The absent policy row
-- deliberately keeps admission closed until an administrator provisions it.
INSERT INTO cyrene_workspace_device_authorization.anonymous_first_start_admission_state
    (singleton, write_window_ms, window_number, accepted_first_starts)
VALUES (TRUE, NULL, NULL, 0);

CREATE FUNCTION cyrene_workspace_device_authorization.admit_anonymous_first_start()
RETURNS TRIGGER
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $admission$
DECLARE
    v_policy RECORD;
    v_state RECORD;
    v_now_ms BIGINT;
    v_window_number BIGINT;
    v_accepted_starts BIGINT;
    v_pending_active BIGINT;
BEGIN
    -- A different transaction isolation level would keep a stale COUNT snapshot
    -- after waiting on the singleton row. Refuse such calls before admission.
    IF pg_catalog.current_setting('transaction_isolation') <> 'read committed' THEN
        RAISE EXCEPTION USING
            ERRCODE = 'PZ002',
            MESSAGE = 'anonymous first-start admission requires READ COMMITTED';
    END IF;

    IF NEW.registration_key_digest IS NULL
       OR NEW.authorization_generation <> 1 THEN
        RETURN NEW;
    END IF;
    IF NEW.device_code_generation <> 1 THEN
        RAISE EXCEPTION USING
            ERRCODE = 'PZ002',
            MESSAGE = 'generation-one anonymous starts must use the initial device-code generation';
    END IF;

    SELECT write_window_ms,
           maximum_first_starts_per_window,
           maximum_pending_active
    INTO v_policy
    FROM cyrene_workspace_device_authorization.anonymous_first_start_admission_policy
    WHERE singleton IS TRUE
    FOR SHARE;

    IF NOT FOUND THEN
        RAISE EXCEPTION USING
            ERRCODE = 'PZ002',
            MESSAGE = 'anonymous first-start admission policy is not configured';
    END IF;

    -- Directory binds the registration key to its immutable generation-one
    -- identity in the same transaction that reaches this authorization INSERT.
    PERFORM 1
    FROM cyrene_workspace_directory.device_registration_bindings AS registration_binding
    JOIN cyrene_workspace_directory.workspace_device_identities AS device_identity
      ON device_identity.organization_id = registration_binding.organization_id
     AND device_identity.workspace_id = registration_binding.workspace_id
     AND device_identity.device_id = registration_binding.device_id
    WHERE registration_binding.registration_key_digest = NEW.registration_key_digest
      AND pg_catalog.uuid_send(registration_binding.binding_id) = NEW.registration_binding_id
      AND registration_binding.organization_id = NEW.organization_id
      AND registration_binding.workspace_id = NEW.workspace_id
      AND registration_binding.device_id = NEW.device_id
      AND registration_binding.authorization_generation = 1
      AND device_identity.current_authorization_generation = 1
    FOR SHARE OF device_identity;

    IF NOT FOUND THEN
        RAISE EXCEPTION USING
            ERRCODE = 'PZ002',
            MESSAGE = 'anonymous first-start registration binding is unavailable';
    END IF;

    -- All first-generation starts lock this singleton. Under READ COMMITTED,
    -- the following COUNT is a new command snapshot, so it sees a prior
    -- transaction's authorization row after that transaction releases the lock.
    SELECT write_window_ms, window_number, accepted_first_starts
    INTO v_state
    FROM cyrene_workspace_device_authorization.anonymous_first_start_admission_state
    WHERE singleton IS TRUE
    FOR UPDATE;

    IF NOT FOUND THEN
        RAISE EXCEPTION USING
            ERRCODE = 'PZ002',
            MESSAGE = 'anonymous first-start admission state is unavailable';
    END IF;

    v_now_ms := FLOOR(EXTRACT(EPOCH FROM pg_catalog.clock_timestamp()) * 1000)::BIGINT;
    IF v_now_ms < 0 THEN
        RAISE EXCEPTION USING
            ERRCODE = 'PZ002',
            MESSAGE = 'database time is outside the supported admission range';
    END IF;
    v_window_number := v_now_ms / v_policy.write_window_ms;

    IF v_state.write_window_ms IS NOT DISTINCT FROM v_policy.write_window_ms
       AND v_state.window_number IS NOT DISTINCT FROM v_window_number THEN
        v_accepted_starts := v_state.accepted_first_starts;
    ELSE
        v_accepted_starts := 0;
    END IF;

    IF v_accepted_starts >= v_policy.maximum_first_starts_per_window THEN
        RAISE EXCEPTION USING
            ERRCODE = 'PZ001',
            MESSAGE = 'anonymous first-start write quota exceeded';
    END IF;

    SELECT COUNT(*)
    INTO v_pending_active
    FROM cyrene_workspace_device_authorization.authorizations AS auth_row
    WHERE auth_row.authorization_generation = 1
      AND auth_row.expires_at_unix_ms > v_now_ms
      AND auth_row.state_kind IN (
          'pending', 'awaiting_webauthn', 'verifying_webauthn', 'issuing',
          'delivery_pending'
      );

    IF v_pending_active >= v_policy.maximum_pending_active THEN
        RAISE EXCEPTION USING
            ERRCODE = 'PZ001',
            MESSAGE = 'anonymous first-start active pending cap reached';
    END IF;

    UPDATE cyrene_workspace_device_authorization.anonymous_first_start_admission_state
    SET write_window_ms = v_policy.write_window_ms,
        window_number = v_window_number,
        accepted_first_starts = v_accepted_starts + 1
    WHERE singleton IS TRUE;

    RETURN NEW;
END;
$admission$;

COMMENT ON TABLE
    cyrene_workspace_device_authorization.anonymous_first_start_admission_policy
    IS 'DBA bootstrap: insert exactly one row with write_window_ms (positive database-time window), maximum_first_starts_per_window, and maximum_pending_active. Zero limits disable new starts. No row is seeded; generation-one registered starts fail closed until a privileged administrator provisions the policy. The application role has no access.';
COMMENT ON COLUMN
    cyrene_workspace_device_authorization.anonymous_first_start_admission_policy.write_window_ms
    IS 'Positive fixed-window duration in milliseconds, anchored to the PostgreSQL Unix epoch clock.';
COMMENT ON COLUMN
    cyrene_workspace_device_authorization.anonymous_first_start_admission_policy.maximum_first_starts_per_window
    IS 'Administrator-selected maximum admitted generation-one registrations in each fixed window; zero disables starts.';
COMMENT ON COLUMN
    cyrene_workspace_device_authorization.anonymous_first_start_admission_policy.maximum_pending_active
    IS 'Administrator-selected maximum unexpired active generation-one authorizations; zero disables starts.';
COMMENT ON TABLE
    cyrene_workspace_device_authorization.anonymous_first_start_admission_state
    IS 'Private singleton transaction counter shared by all PostgreSQL registration replicas; active rows leave the pending cap automatically when their authorization TTL expires.';
COMMENT ON FUNCTION
    cyrene_workspace_device_authorization.admit_anonymous_first_start()
    IS 'Security-definer BEFORE INSERT admission for immutable Directory-bound anonymous generation-one registrations. Uses database time and requires READ COMMITTED.';

REVOKE ALL ON TABLE
    cyrene_workspace_device_authorization.anonymous_first_start_admission_policy,
    cyrene_workspace_device_authorization.anonymous_first_start_admission_state
    FROM PUBLIC, cyrene_workspace_device_authorization_app;
REVOKE ALL ON FUNCTION
    cyrene_workspace_device_authorization.admit_anonymous_first_start()
    FROM PUBLIC, cyrene_workspace_device_authorization_app;

CREATE TRIGGER anonymous_first_start_admission
    BEFORE INSERT ON cyrene_workspace_device_authorization.authorizations
    FOR EACH ROW
    WHEN (
        NEW.registration_key_digest IS NOT NULL
        AND NEW.authorization_generation = 1
    )
    EXECUTE FUNCTION
        cyrene_workspace_device_authorization.admit_anonymous_first_start();

RESET ROLE;

REVOKE CREATE ON SCHEMA cyrene_workspace_device_authorization
    FROM cyrene_workspace_anonymous_start_admission_owner;
REVOKE TRIGGER ON cyrene_workspace_device_authorization.authorizations
    FROM cyrene_workspace_anonymous_start_admission_owner;

DO $owner_membership_cleanup$
BEGIN
    EXECUTE format(
        'REVOKE cyrene_workspace_anonymous_start_admission_owner FROM %I',
        current_user
    );
END
$owner_membership_cleanup$;
