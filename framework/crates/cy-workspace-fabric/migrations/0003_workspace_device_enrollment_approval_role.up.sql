-- ┌─────────────────────────────────────────────────────────────────────┐
-- │  📄 0003_workspace_device_enrollment_approval_role.up.sql           │
-- │  Schema: cyrene_workspace_directory                                  │
-- │  Role: Permit the audited Directory operator to grant approval scope.│
-- │                                                                      │
-- │  用途：允许受审计的 Directory operator 授予设备审批 scope。             │
-- └─────────────────────────────────────────────────────────────────────┘

-- ── Replace the deployed allowlists without rewriting migration 0001 ──
-- 阶段一：替换已部署 allowlist，不改写 migration 0001
DO $migration$
DECLARE
    constraint_name TEXT;
    role_constraint_count INTEGER := 0;
    audit_constraint_count INTEGER := 0;
BEGIN
    IF to_regclass('cyrene_workspace_directory.roles') IS NULL
        OR to_regclass('cyrene_workspace_directory.audit_events') IS NULL THEN
        RAISE EXCEPTION 'Workspace Directory schema is not initialized';
    END IF;

    FOR constraint_name IN
        SELECT constraint_row.conname
        FROM pg_catalog.pg_constraint AS constraint_row
        WHERE constraint_row.conrelid =
                  'cyrene_workspace_directory.roles'::regclass
          AND constraint_row.contype = 'c'
          AND pg_catalog.pg_get_constraintdef(constraint_row.oid) LIKE '%role_name%'
          AND pg_catalog.pg_get_constraintdef(constraint_row.oid)
                  LIKE '%workspace.product.command.catalyst.create_dataset.v1%'
    LOOP
        EXECUTE format(
            'ALTER TABLE cyrene_workspace_directory.roles DROP CONSTRAINT %I',
            constraint_name
        );
        role_constraint_count := role_constraint_count + 1;
    END LOOP;

    IF role_constraint_count <> 1 THEN
        RAISE EXCEPTION 'Expected one deployed Directory role allowlist constraint';
    END IF;

    ALTER TABLE cyrene_workspace_directory.roles
        ADD CONSTRAINT workspace_directory_roles_supported_role CHECK (
            role_name IN (
                'workspace.product.command.catalyst.create_dataset.v1',
                'workspace.product.command.yield.start_training_run.v1',
                'workspace.product.command.reactor.create_model_import.v1',
                'workspace.product.command.exchange.create_route_draft.v1',
                'workspace.product.command.echo.create_evaluation_suite.v1',
                'workspace.device.enrollment.approve.v1'
            )
        );

    FOR constraint_name IN
        SELECT constraint_row.conname
        FROM pg_catalog.pg_constraint AS constraint_row
        WHERE constraint_row.conrelid =
                  'cyrene_workspace_directory.audit_events'::regclass
          AND constraint_row.contype = 'c'
          AND pg_catalog.pg_get_constraintdef(constraint_row.oid)
                  LIKE '%affected_roles%'
          AND pg_catalog.pg_get_constraintdef(constraint_row.oid)
                  LIKE '%workspace.product.command.catalyst.create_dataset.v1%'
    LOOP
        EXECUTE format(
            'ALTER TABLE cyrene_workspace_directory.audit_events DROP CONSTRAINT %I',
            constraint_name
        );
        audit_constraint_count := audit_constraint_count + 1;
    END LOOP;

    IF audit_constraint_count <> 1 THEN
        RAISE EXCEPTION 'Expected one deployed Directory audit-role allowlist constraint';
    END IF;

    ALTER TABLE cyrene_workspace_directory.audit_events
        ADD CONSTRAINT workspace_directory_audit_supported_roles CHECK (
            affected_roles <@ ARRAY[
                'workspace.product.command.catalyst.create_dataset.v1',
                'workspace.product.command.yield.start_training_run.v1',
                'workspace.product.command.reactor.create_model_import.v1',
                'workspace.product.command.exchange.create_route_draft.v1',
                'workspace.product.command.echo.create_evaluation_suite.v1',
                'workspace.device.enrollment.approve.v1'
            ]::TEXT[]
        );
END
$migration$;
