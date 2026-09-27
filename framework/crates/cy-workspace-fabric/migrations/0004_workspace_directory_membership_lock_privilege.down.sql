-- ┌─────────────────────────────────────────────────────────────────────┐
-- │  📄 0004_workspace_directory_membership_lock_privilege.down.sql     │
-- │  Schema: cyrene_workspace_directory                                  │
-- │  Role: Revoke the operator's membership row-lock privilege.          │
-- │                                                                      │
-- │  用途：撤销 Directory operator 的成员行锁权限。                        │
-- └─────────────────────────────────────────────────────────────────────┘

-- ── Revoke the column-level UPDATE privilege ──────────────────────────
-- 阶段一：撤销列级 UPDATE 权限
REVOKE UPDATE (provisioned_at)
    ON cyrene_workspace_directory.memberships
    FROM cyrene_workspace_directory_operator;
