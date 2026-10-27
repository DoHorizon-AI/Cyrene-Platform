-- ┌─────────────────────────────────────────────────────────────────────┐
-- │  📄 0007_workspace_office_task_writer_role.down.sql                 │
-- │  Schema: cyrene_workspace_directory                                  │
-- │  Role: Remove the task writer role after all use is cleared.        │
-- │                                                                      │
-- │  用途：清除全部使用后移除 task writer role。                           │
-- └─────────────────────────────────────────────────────────────────────┘

-- ── Refuse rollback while durable grants or audit events use the role ──
-- 阶段一：存在持久授权或审计事件时拒绝回退
DO $migration$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM cyrene_workspace_directory.roles
        WHERE role_name = 'navigator.office.task_writer.v1'
    ) OR EXISTS (
        SELECT 1
        FROM cyrene_workspace_directory.audit_events
        WHERE affected_roles @> ARRAY['navigator.office.task_writer.v1']::TEXT[]
    ) THEN
        RAISE EXCEPTION 'Cannot remove office task writer role while grants or audit events remain';
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
                'workspace.device.enrollment.approve.v1',
                'navigator.connector.operator.v1',
                'navigator.automation.viewer.v1'
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
                'workspace.device.enrollment.approve.v1',
                'navigator.connector.operator.v1',
                'navigator.automation.viewer.v1'
            ]::TEXT[]
        );
END
$migration$;
