-- Reversing this fence while records exist would discard their Directory identity.
DO $migration$
BEGIN
    IF EXISTS (
        SELECT 1 FROM cyrene_workspace_device_authorization.authorizations
    ) THEN
        RAISE EXCEPTION 'cannot remove Directory binding snapshots while authorizations exist';
    END IF;
END
$migration$;

DROP INDEX cyrene_workspace_device_authorization.authorizations_directory_generation_idx;
DROP INDEX cyrene_workspace_device_authorization.authorizations_delivery_certificate_expiry_scan_idx;
DROP INDEX cyrene_workspace_device_authorization.authorizations_registration_binding_unique;

ALTER TABLE cyrene_workspace_device_authorization.authorizations
    DROP COLUMN authorization_generation,
    DROP COLUMN device_id,
    DROP COLUMN registration_binding_id;

REVOKE cyrene_workspace_device_registrar
    FROM cyrene_workspace_device_authorization_app;
