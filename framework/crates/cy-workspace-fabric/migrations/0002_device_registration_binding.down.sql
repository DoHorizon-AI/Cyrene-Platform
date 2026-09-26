-- ┌──────────────────────────────────────────────────────────────────────┐
-- │ Remove device registration binding data                              │
-- │ 删除设备 registration binding 数据                                    │
-- └──────────────────────────────────────────────────────────────────────┘

DROP TABLE IF EXISTS cyrene_workspace_directory.device_registration_bindings;
DROP TABLE IF EXISTS cyrene_workspace_directory.workspace_device_identities;
DROP FUNCTION IF EXISTS cyrene_workspace_directory.reject_device_registration_binding_mutation();
DROP FUNCTION IF EXISTS cyrene_workspace_directory.require_next_device_authorization_generation();
