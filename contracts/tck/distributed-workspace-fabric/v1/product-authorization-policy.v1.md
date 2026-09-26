# Workspace Product Authorization Policy v1

This contract defines the authorization matrix for operations in
`product-projections.tsv`. It introduces versioned role names but does not
assign those roles to users or enable a command by default.

## Authority inputs

The policy runs only after the server has authenticated the caller, matched the
organization and Workspace scope, and resolved current membership through the
authoritative Directory. The role set must come from that trusted Directory
lookup and authenticated server-side context. Request JSON, `RelayHello`, and
caller-controlled headers cannot supply membership or Product roles. The
`workspace.member` marker is valid only after the Directory confirms the exact
user, organization, and Workspace membership.

Relay participant roles describe transport participation; they do not grant
Product permissions. Devices and workloads without a separately verified
Product authorization profile are denied Product API access.

## Permission matrix

| Verified principal | Operation | Required authority | Result |
| --- | --- | --- | --- |
| Directory user | Any manifest `READ` | `workspace.member` | Allow |
| Directory user | Catalyst `createDataset` | `workspace.member` and `workspace.product.command.catalyst.create_dataset.v1` | Allow |
| Directory user | Yield `start_run_api_v1_training_drafts__draft_id__actions_start_post` | `workspace.member` and `workspace.product.command.yield.start_training_run.v1` | Allow |
| Directory user | Reactor `create_model_import_api_v1_model_imports_post` | `workspace.member` and `workspace.product.command.reactor.create_model_import.v1` | Allow |
| Directory user | Exchange `create_draft_api_v1_gateway_route_drafts_post` | `workspace.member` and `workspace.product.command.exchange.create_route_draft.v1` | Allow |
| Directory user | Echo `createEvaluationSuite` | `workspace.member` and `workspace.product.command.echo.create_evaluation_suite.v1` | Allow |
| Directory user | Navigator `append_events_api_v1_harness_workspaces__workspace_id__sessions__session_id__append_post` | User roles do not grant this operation | Deny |
| Trusted Navigator Harness workload | Navigator `append_events` | Trusted server-side workload handoff scoped to the exact organization and Workspace | Allow |
| Any other principal | Any unmapped operation or command | No matching policy entry | Deny |

Each command role grants only the single command named in this matrix. A member
role alone grants reads and no writes. A command role without the membership
marker is also insufficient. Unknown, absent, or misspelled roles never imply a
permission. There is no generic Product command role and no default role for
all Workspace members.

The Navigator service-writer grant is a verified principal class, not a role
string. A future handoff must authenticate the workload, verify its intended
Navigator Harness identity and audience, and bind it to the exact organization
and Workspace before it can produce this principal class. A Directory role
string, including `navigator.service-writer`, cannot substitute for that proof.
The service-writer principal may append Harness events only; it does not gain
read or other Product command permissions from this policy.

## Provisioning prerequisite

No user receives any of the command roles from this contract automatically.
Before a frontend command can succeed, the Workspace owner must have an
authoritative Directory administration process that assigns the exact v1 role
to the intended user's membership in the exact organization and Workspace.
This contract adds neither role assignments nor a product-facing role
administration endpoint. Deployments must not seed command roles for every
member or accept role values from session/request claims.

Until the Directory has explicitly provisioned a matching command role, the
command remains denied. Until a trusted Navigator workload handoff exists,
`append_events` remains denied to Frontend callers.

<!-- Chinese Translation / 中文翻译 -->

# Workspace Product 授权策略 v1

本合同定义 `product-projections.tsv` 中 operation 的授权矩阵。它引入带
版本的角色名称，但不会把角色分配给用户，也不会默认启用命令。

## 权威输入

只有在服务端认证调用方、核对 organization 与 Workspace scope，并通过权威
Directory 查询当前成员关系之后，才能执行本策略。角色集合必须来自可信
Directory 查询与服务端认证上下文。请求 JSON、`RelayHello` 和调用方可控的
header 均不能提供成员身份或 Product 角色。只有 Directory 确认用户在指定
organization 和 Workspace 中的成员关系后，才能设置 `workspace.member` 标记。

Relay participant role 只描述传输参与类型，不授予 Product 权限。没有独立
验证 Product 授权配置的设备和 workload 均不得访问 Product API。

## 权限矩阵

| 已验证主体 | 操作 | 所需权限 | 结果 |
| --- | --- | --- | --- |
| Directory 用户 | manifest 中的任意 `READ` | `workspace.member` | 允许 |
| Directory 用户 | Catalyst `createDataset` | `workspace.member` 和 `workspace.product.command.catalyst.create_dataset.v1` | 允许 |
| Directory 用户 | Yield `start_run_api_v1_training_drafts__draft_id__actions_start_post` | `workspace.member` 和 `workspace.product.command.yield.start_training_run.v1` | 允许 |
| Directory 用户 | Reactor `create_model_import_api_v1_model_imports_post` | `workspace.member` 和 `workspace.product.command.reactor.create_model_import.v1` | 允许 |
| Directory 用户 | Exchange `create_draft_api_v1_gateway_route_drafts_post` | `workspace.member` 和 `workspace.product.command.exchange.create_route_draft.v1` | 允许 |
| Directory 用户 | Echo `createEvaluationSuite` | `workspace.member` 和 `workspace.product.command.echo.create_evaluation_suite.v1` | 允许 |
| Directory 用户 | Navigator `append_events_api_v1_harness_workspaces__workspace_id__sessions__session_id__append_post` | 用户角色不授予此操作 | 拒绝 |
| 受信 Navigator Harness workload | Navigator `append_events` | 绑定到准确 organization 和 Workspace 的受信服务端 workload handoff | 允许 |
| 其他主体 | 未映射操作或命令 | 不存在匹配的策略条目 | 拒绝 |

每个命令角色只授予矩阵中列出的单个命令。成员角色只授予读取，不授予写入。
没有成员标记时，单独的命令角色也不够。未知、缺失或拼写错误的角色不会隐式
获得权限。本策略没有通用 Product 命令角色，也不会为所有 Workspace 成员默认
分配角色。

Navigator service-writer 权限是一种已验证主体类型，不是角色字符串。未来的
handoff 必须认证 workload，验证其 Navigator Harness 身份与 audience，并将其绑定
到准确的 organization 和 Workspace，之后才能产生该主体类型。Directory 角色
字符串（包括 `navigator.service-writer`）不能替代这项证明。该 service-writer
主体只能追加 Harness event；本策略不会因此授予读取或其他 Product 命令权限。

## 赋权前置条件

本合同不会自动向任何用户赋予命令角色。启用 Frontend 命令前，Workspace owner
必须通过权威 Directory 管理流程，把准确的 v1 角色分配给准确 organization 和
Workspace 下目标用户的成员记录。本合同不添加角色分配，也不添加面向 Product
的角色管理 endpoint。部署不得为所有成员预先分配命令角色，也不得接受 session
或 request claim 中的角色值。

在 Directory 明确配置匹配的命令角色之前，命令始终拒绝。在可信 Navigator
workload handoff 存在之前，`append_events` 始终拒绝 Frontend 调用。
