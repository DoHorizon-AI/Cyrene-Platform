-- ┌─────────────────────────────────────────────────────────────────────┐
-- │  📄 0001_restricted_device_ca.down.sql                              │
-- │  Schema: cyrene_workspace_device_ca                                  │
-- │  Role: Refuse to remove any persisted CA or CRL state.                │
-- │                                                                     │
-- │  用途：只有 CA 与 CRL 状态为空时才允许移除 schema。                   │
-- └─────────────────────────────────────────────────────────────────────┘
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM cyrene_workspace_device_ca.issued_certificates)
       OR EXISTS (SELECT 1 FROM cyrene_workspace_device_ca.current_crl) THEN
        RAISE EXCEPTION 'refusing to remove non-empty restricted device CA state';
    END IF;
END
$$;

DROP TABLE cyrene_workspace_device_ca.current_crl;
DROP TABLE cyrene_workspace_device_ca.issued_certificates;
DROP SCHEMA cyrene_workspace_device_ca;
-- Keep the NOLOGIN role in place: database operators own its membership lifecycle.
