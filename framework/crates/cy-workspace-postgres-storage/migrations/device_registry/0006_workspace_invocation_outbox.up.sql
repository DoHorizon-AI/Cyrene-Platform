-- Persistent invocation outbox and final dispatch fence queue for Workspace Authority.
-- Authority 控制面的持久 Outbox 队列与最终派发栅栏表。

DO $owner_membership$
BEGIN
    EXECUTE format(
        'GRANT cyrene_workspace_device_registry_owner TO %I',
        current_user
    );
END
$owner_membership$;

SET ROLE cyrene_workspace_device_registry_owner;

CREATE TABLE IF NOT EXISTS cyrene_workspace_device_registry.workspace_invocation_outbox (
    invocation_id VARCHAR(128) PRIMARY KEY,
    workspace_id VARCHAR(128) NOT NULL,
    idempotency_key VARCHAR(256),
    request_digest_sha256 VARCHAR(64) NOT NULL,
    target_component VARCHAR(128) NOT NULL,
    resolved_target_url TEXT NOT NULL,
    raw_invocation_json BYTEA NOT NULL,
    device_generation BIGINT NOT NULL,
    session_generation BIGINT NOT NULL,
    contract_activation_generation BIGINT NOT NULL,
    status VARCHAR(32) NOT NULL DEFAULT 'PENDING',
    claimed_by_connector_id VARCHAR(128),
    claimed_at_unix_ms BIGINT,
    delivery_receipt BYTEA,
    delivery_acknowledged_at_unix_ms BIGINT,
    outcome_status VARCHAR(32),
    response_status_code INTEGER,
    response_json_body BYTEA,
    response_content_type VARCHAR(128),
    error_message TEXT,
    created_at_unix_ms BIGINT NOT NULL,
    updated_at_unix_ms BIGINT NOT NULL,
    expires_at_unix_ms BIGINT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_workspace_invocation_outbox_queue 
    ON cyrene_workspace_device_registry.workspace_invocation_outbox (workspace_id, status, created_at_unix_ms);

CREATE INDEX IF NOT EXISTS idx_workspace_invocation_outbox_idempotency 
    ON cyrene_workspace_device_registry.workspace_invocation_outbox (workspace_id, idempotency_key)
    WHERE idempotency_key IS NOT NULL;

GRANT SELECT, INSERT, UPDATE, DELETE ON cyrene_workspace_device_registry.workspace_invocation_outbox
    TO cyrene_workspace_device_registry_app;

GRANT SELECT ON cyrene_workspace_device_registry.workspace_invocation_outbox
    TO cyrene_workspace_device_registry_relay_reader;

RESET ROLE;

DO $owner_membership_cleanup$
BEGIN
    EXECUTE format(
        'REVOKE cyrene_workspace_device_registry_owner FROM %I',
        current_user
    );
END
$owner_membership_cleanup$;
