-- ┌──────────────────────────────────────────────────────────────────────┐
-- │ Workspace Directory membership, role, descriptor, and audit schema   │
-- │ Workspace 成员关系、角色、描述符与审计表结构                           │
-- └──────────────────────────────────────────────────────────────────────┘

DO $roles$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'cyrene_workspace_directory_reader') THEN
        CREATE ROLE cyrene_workspace_directory_reader NOLOGIN;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'cyrene_workspace_directory_operator') THEN
        CREATE ROLE cyrene_workspace_directory_operator NOLOGIN;
    END IF;
END
$roles$;

ALTER ROLE cyrene_workspace_directory_reader NOLOGIN;
ALTER ROLE cyrene_workspace_directory_operator NOLOGIN;

CREATE SCHEMA IF NOT EXISTS cyrene_workspace_directory;
REVOKE ALL ON SCHEMA cyrene_workspace_directory FROM PUBLIC;
GRANT USAGE ON SCHEMA cyrene_workspace_directory
    TO cyrene_workspace_directory_reader, cyrene_workspace_directory_operator;

CREATE TABLE IF NOT EXISTS cyrene_workspace_directory.memberships (
    issuer TEXT NOT NULL CHECK (btrim(issuer) <> '' AND length(issuer) <= 2048),
    subject TEXT NOT NULL CHECK (btrim(subject) <> '' AND length(subject) <= 2048),
    organization_id TEXT NOT NULL CHECK (btrim(organization_id) <> '' AND length(organization_id) <= 256),
    workspace_id TEXT NOT NULL CHECK (btrim(workspace_id) <> '' AND length(workspace_id) <= 256),
    provisioned_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (issuer, subject, organization_id, workspace_id)
);

CREATE INDEX IF NOT EXISTS memberships_identity_organization_idx
    ON cyrene_workspace_directory.memberships (issuer, subject, organization_id);

CREATE TABLE IF NOT EXISTS cyrene_workspace_directory.roles (
    issuer TEXT NOT NULL,
    subject TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    role_name TEXT NOT NULL CHECK (
        role_name IN (
            'workspace.product.command.catalyst.create_dataset.v1',
            'workspace.product.command.yield.start_training_run.v1',
            'workspace.product.command.reactor.create_model_import.v1',
            'workspace.product.command.exchange.create_route_draft.v1',
            'workspace.product.command.echo.create_evaluation_suite.v1'
        )
    ),
    provisioned_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (issuer, subject, organization_id, workspace_id, role_name),
    FOREIGN KEY (issuer, subject, organization_id, workspace_id)
        REFERENCES cyrene_workspace_directory.memberships (issuer, subject, organization_id, workspace_id)
        ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS cyrene_workspace_directory.descriptors (
    organization_id TEXT NOT NULL CHECK (btrim(organization_id) <> '' AND length(organization_id) <= 256),
    workspace_id TEXT NOT NULL CHECK (btrim(workspace_id) <> '' AND length(workspace_id) <= 256),
    descriptor_proto BYTEA NOT NULL CHECK (octet_length(descriptor_proto) BETWEEN 1 AND 1048576),
    updated_by TEXT NOT NULL CHECK (btrim(updated_by) <> '' AND length(updated_by) <= 256),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (organization_id, workspace_id)
);

CREATE TABLE IF NOT EXISTS cyrene_workspace_directory.audit_events (
    change_id UUID NOT NULL,
    event_ordinal SMALLINT NOT NULL CHECK (event_ordinal >= 0),
    actor_id TEXT NOT NULL CHECK (btrim(actor_id) <> '' AND length(actor_id) <= 256),
    action TEXT NOT NULL CHECK (
        action IN (
            'membership.granted',
            'membership.revoked',
            'role.granted',
            'role.revoked',
            'descriptor.published',
            'descriptor.revoked'
        )
    ),
    issuer TEXT,
    subject TEXT,
    CHECK ((issuer IS NULL AND subject IS NULL) OR (issuer IS NOT NULL AND subject IS NOT NULL)),
    CHECK (issuer IS NULL OR (btrim(issuer) <> '' AND length(issuer) <= 2048)),
    CHECK (subject IS NULL OR (btrim(subject) <> '' AND length(subject) <= 2048)),
    organization_id TEXT NOT NULL CHECK (btrim(organization_id) <> '' AND length(organization_id) <= 256),
    workspace_id TEXT NOT NULL CHECK (btrim(workspace_id) <> '' AND length(workspace_id) <= 256),
    affected_roles TEXT[] NOT NULL DEFAULT ARRAY[]::TEXT[],
    CHECK (affected_roles <@ ARRAY[
        'workspace.product.command.catalyst.create_dataset.v1',
        'workspace.product.command.yield.start_training_run.v1',
        'workspace.product.command.reactor.create_model_import.v1',
        'workspace.product.command.exchange.create_route_draft.v1',
        'workspace.product.command.echo.create_evaluation_suite.v1'
    ]::TEXT[]),
    reason TEXT NOT NULL CHECK (btrim(reason) <> '' AND length(reason) <= 2000),
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (change_id, event_ordinal)
);

CREATE INDEX IF NOT EXISTS audit_events_scope_time_idx
    ON cyrene_workspace_directory.audit_events (organization_id, workspace_id, occurred_at DESC);

CREATE OR REPLACE FUNCTION cyrene_workspace_directory.reject_audit_mutation()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $audit$
BEGIN
    RAISE EXCEPTION 'Workspace Directory audit events are append-only';
END;
$audit$;

DROP TRIGGER IF EXISTS audit_events_immutable ON cyrene_workspace_directory.audit_events;
CREATE TRIGGER audit_events_immutable
    BEFORE UPDATE OR DELETE ON cyrene_workspace_directory.audit_events
    FOR EACH ROW EXECUTE FUNCTION cyrene_workspace_directory.reject_audit_mutation();

REVOKE ALL ON ALL TABLES IN SCHEMA cyrene_workspace_directory FROM PUBLIC;
GRANT SELECT ON
    cyrene_workspace_directory.memberships,
    cyrene_workspace_directory.roles,
    cyrene_workspace_directory.descriptors
    TO cyrene_workspace_directory_reader;
GRANT SELECT ON
    cyrene_workspace_directory.memberships,
    cyrene_workspace_directory.roles,
    cyrene_workspace_directory.descriptors,
    cyrene_workspace_directory.audit_events
    TO cyrene_workspace_directory_operator;
GRANT INSERT, DELETE ON
    cyrene_workspace_directory.memberships,
    cyrene_workspace_directory.roles
    TO cyrene_workspace_directory_operator;
GRANT INSERT, UPDATE, DELETE ON
    cyrene_workspace_directory.descriptors
    TO cyrene_workspace_directory_operator;
GRANT INSERT ON cyrene_workspace_directory.audit_events
    TO cyrene_workspace_directory_operator;

ALTER DEFAULT PRIVILEGES IN SCHEMA cyrene_workspace_directory
    REVOKE ALL ON TABLES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES IN SCHEMA cyrene_workspace_directory
    GRANT SELECT ON TABLES TO cyrene_workspace_directory_reader;
ALTER DEFAULT PRIVILEGES IN SCHEMA cyrene_workspace_directory
    GRANT SELECT ON TABLES TO cyrene_workspace_directory_operator;
