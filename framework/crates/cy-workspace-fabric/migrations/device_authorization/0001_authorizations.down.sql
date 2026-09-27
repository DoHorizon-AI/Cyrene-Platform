-- This removes authorization state and must only be used for a disposable
-- database. Production recovery/audit records are intentionally not migrated
-- down during normal operation.
DROP SCHEMA IF EXISTS cyrene_workspace_device_authorization CASCADE;
