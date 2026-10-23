-- ┌─────────────────────────────────────────────────────────────────────┐
-- │ Revert the Workspace Directory connector operator role allowlist    │
-- │ 回滚 Workspace Directory Connector operator role allowlist          │
-- └─────────────────────────────────────────────────────────────────────┘

DO $migration$
BEGIN
    IF EXISTS (
        SELECT 1 FROM cyrene_workspace_directory.roles
        WHERE role_name = 'navigator.connector.operator.v1'
    ) OR EXISTS (
        SELECT 1 FROM cyrene_workspace_directory.audit_events
        WHERE affected_roles @> ARRAY['navigator.connector.operator.v1']::TEXT[]
    ) THEN
        RAISE EXCEPTION 'Connector operator role assignments or audit events still exist';
    END IF;

    ALTER TABLE cyrene_workspace_directory.roles
        DROP CONSTRAINT IF EXISTS workspace_directory_roles_supported_role;
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

    ALTER TABLE cyrene_workspace_directory.audit_events
        DROP CONSTRAINT IF EXISTS workspace_directory_audit_supported_roles;
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
