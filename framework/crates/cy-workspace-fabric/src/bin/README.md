# Workspace Fabric fixtures | Workspace Fabric 验收程序

`cy-workspace-fabric-fixture` supplies four acceptance-only roles: `relay`,
`connector`, `frontend-start`, and `frontend-observe`. Production deployments
must provide durable Workspace state and a real identity provider; they must
not reuse the file-backed operation view or development session tokens.

`cy-workspace-fabric-fixture` 提供四个仅用于验收的角色：`relay`、`connector`、
`frontend-start` 与 `frontend-observe`。生产部署必须提供持久 Workspace 状态和真实
Identity Provider，不得复用文件驱动的 Operation view 或开发 session token。

The full executable path is owned by
`tooling/acceptance/distributed-workspace-fabric/run-relay-proof.sh`.

完整可执行路径由 `tooling/acceptance/distributed-workspace-fabric/run-relay-proof.sh`
统一编排。
