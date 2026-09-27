-- ┌─────────────────────────────────────────────────────────────────────┐
-- │  📄 0005_user_code_attempt_limiter.down.sql                          │
-- │  Schema: cyrene_workspace_device_authorization                       │
-- │  Role: Removes the shared user-code attempt-window table.            │
-- │                                                                     │
-- │  用途：删除共享 user-code 尝试窗口表。                                  │
-- └─────────────────────────────────────────────────────────────────────┘

DROP TABLE cyrene_workspace_device_authorization.user_code_attempt_windows;
