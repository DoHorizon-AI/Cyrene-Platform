-- ┌─────────────────────────────────────────────────────────────────────┐
-- │  📄 0005_user_code_attempt_limiter.up.sql                            │
-- │  Schema: cyrene_workspace_device_authorization                       │
-- │  Role: Stores bounded, shared user-code attempt windows.             │
-- │                                                                     │
-- │  用途：持久化有界共享 user-code 尝试窗口，不存原始 IP 或验证码。          │
-- └─────────────────────────────────────────────────────────────────────┘

CREATE TABLE cyrene_workspace_device_authorization.user_code_attempt_windows (
    abuse_key BYTEA NOT NULL,
    window_started_at_unix_ms BIGINT NOT NULL,
    attempts BIGINT NOT NULL,
    window_duration_ms BIGINT NOT NULL,
    maximum_attempts BIGINT NOT NULL,
    CONSTRAINT user_code_attempt_windows_pk PRIMARY KEY (abuse_key),
    CONSTRAINT user_code_attempt_windows_key_length CHECK (octet_length(abuse_key) = 32),
    CONSTRAINT user_code_attempt_windows_started CHECK (window_started_at_unix_ms >= 0),
    CONSTRAINT user_code_attempt_windows_attempts CHECK (
        attempts > 0 AND attempts <= maximum_attempts
    ),
    CONSTRAINT user_code_attempt_windows_duration CHECK (window_duration_ms > 0),
    CONSTRAINT user_code_attempt_windows_maximum CHECK (
        maximum_attempts BETWEEN 1 AND 4294967295
    )
);

COMMENT ON TABLE cyrene_workspace_device_authorization.user_code_attempt_windows IS
    'Bounded attempt windows keyed only by a caller-derived privacy-preserving abuse digest.';
COMMENT ON COLUMN cyrene_workspace_device_authorization.user_code_attempt_windows.abuse_key IS
    '32-byte server-derived digest; raw IP, pre-auth session, and user code are never stored.';
COMMENT ON COLUMN cyrene_workspace_device_authorization.user_code_attempt_windows.window_duration_ms IS
    'Fixed-window duration owned by this digest; cleanup uses this persisted value.';
COMMENT ON COLUMN cyrene_workspace_device_authorization.user_code_attempt_windows.maximum_attempts IS
    'Attempt limit owned by this digest; a request with a different policy fails closed.';

REVOKE ALL ON TABLE
    cyrene_workspace_device_authorization.user_code_attempt_windows FROM PUBLIC;
GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE
    cyrene_workspace_device_authorization.user_code_attempt_windows
    TO cyrene_workspace_device_authorization_app;
