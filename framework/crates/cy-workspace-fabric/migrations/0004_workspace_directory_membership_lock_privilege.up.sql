-- ┌─────────────────────────────────────────────────────────────────────┐
-- │  📄 0004_workspace_directory_membership_lock_privilege.up.sql       │
-- │  Schema: cyrene_workspace_directory                                  │
-- │  Role: Permit the operator to lock membership rows.                 │
-- │                                                                      │
-- │  用途：允许 Directory operator 锁定成员行。                            │
-- └─────────────────────────────────────────────────────────────────────┘

-- ── Grant the column-level UPDATE privilege required by SELECT FOR UPDATE ──
-- 阶段一：授予 SELECT FOR UPDATE 所需的列级 UPDATE 权限
GRANT UPDATE (provisioned_at)
    ON cyrene_workspace_directory.memberships
    TO cyrene_workspace_directory_operator;
