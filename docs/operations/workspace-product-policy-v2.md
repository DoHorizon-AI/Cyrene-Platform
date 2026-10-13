# Workspace Product v2 policy release and role activation

This page describes the operator steps for releasing Platform-owned Product v2
authorization policy and assigning its Directory roles. The pinned Product
catalog remains the source of operation IDs, request and response scope
selectors, and owner contract bytes. The Platform policy adds only the roles
and exact-workspace requirement approved for those operations.

## Release inputs

Build a release only from merged, full Git commit IDs. Confirm the Exchange
catalog source SHA from the canonical remote after merge. Run
`tooling/workspace-product-contract-bundle/build_v2_bundle.py` against that
commit and the existing pinned commits for the other owners. Preserve those
other owner pins unless a separate owner contract release has been reviewed.
The builder reads commit objects, not branch names or dirty checkout files.

Review the generated owner catalog map, catalog digests, manifest digest, and
the separate policy digest before updating
`tooling/workspace-product-contract-bundle/releases/workspace-product-v2.lock.json`.
Run the dynamic Product contract tests and the ignored release loader smoke
against the generated bundle and proposed lock. Publish the new Platform policy
and matching bundle together; a policy file by itself does not activate new
operation grants.

## Directory role activation

Every approved Exchange operation requires a current `workspace.member` role
for the exact authenticated organization and Workspace. Account linking,
actor-binding management, issue reads and queries, operation status reads, and
read-only issue-query execution, operation status reads, and read-only
reconciliation use that baseline role. Connection and binding administration,
connection health, and job-listing operations require the additional
`workspace.product.command.exchange.integration.configure.v1` role. Issue
comment writes require
`workspace.product.command.exchange.integration.comment.v1`, while issue-state
writes require
`workspace.product.command.exchange.integration.state.v1`. No Exchange grant
uses `TRUSTED_NAVIGATOR_WORKLOAD` as a general write identity.

Assign command roles only through the authoritative Directory administration
process, scoped to the intended Workspace and approved operators. Do not add
these roles by default, infer them from an Exchange account, accept them from an
invocation body, or create synthetic user identities for OAuth callbacks or
webhooks. The request's principal and current roles must come from Platform's
authenticated session and fresh Directory lookup. The signed owner catalog
continues to enforce every required request and response scope selector.

After role changes, check the user's current catalog snapshot in the exact
Workspace. A member without a command role must not see or invoke the associated
write operations; removing a role must take effect on the next authorization
check. Keep read-only operation status and reconciliation separate from
connection administration, comments, and state changes.

## Rollback

Remove the command role from the Directory assignment before reverting a policy
or bundle release. Restore a previously reviewed lock and matching bundle as a
unit, then verify the deployed Platform release reports that exact lock. Never
edit the source pin or lock to make an unmerged owner catalog appear released.

<!-- Chinese Translation / 中文翻译 -->

# Workspace Product v2 policy 发布与 role 激活

本文说明如何发布 Platform 自有的 Product v2 授权 policy，以及如何在
Directory 中分配对应 role。已固定的 Product catalog 仍是 operation ID、请求与
响应 scope selectors、owner contract 字节的来源。Platform policy 只为获批
operation 增加所需 roles 和 exact Workspace 要求。

## 发布输入

仅使用已合并的完整 Git commit ID 构建 release。Exchange 合并后，从 canonical
remote 确认 catalog source SHA。使用
`tooling/workspace-product-contract-bundle/build_v2_bundle.py`，以该 commit 和其他
owner 当前固定的 commits 构建 bundle。除非单独审查了其他 owner 的 contract
release，否则保留其 source pins。Builder 读取 Git commit objects，不读取 branch
名称或 dirty checkout 文件。

更新
`tooling/workspace-product-contract-bundle/releases/workspace-product-v2.lock.json`
前，审查生成的 owner catalog map、catalog digests、manifest digest 和独立 policy
digest。使用生成的 bundle 与候选 lock 运行 dynamic Product contract tests 和被
ignore 的 release loader smoke。必须一起发布新的 Platform policy 与匹配的 bundle；
单独更新 policy 文件不会启用新的 operation grants。

## Directory role 激活

每个获批的 Exchange operation 都要求当前已认证 organization 与 Workspace 中的
`workspace.member` role。Account linking、actor-binding 管理、issue 读取与查询、只读
issue-query 执行、operation status 读取和只读 reconcile 使用此基础 role。连接和 binding 管理、
连接健康状态以及 job 列表读取还需要
`workspace.product.command.exchange.integration.configure.v1`。Issue comment 写入
需要 `workspace.product.command.exchange.integration.comment.v1`；issue 状态写入需要
`workspace.product.command.exchange.integration.state.v1`。Exchange grants 不会把
`TRUSTED_NAVIGATOR_WORKLOAD` 用作通用写入身份。

只能通过权威 Directory 管理流程，为目标 Workspace 内获批的操作员分配 command
roles。不要默认分配这些 roles，不要从 Exchange account 推断权限，不要接受 invocation
body 中提交的 roles，也不要为 OAuth callback 或 webhook 创建模拟用户身份。Principal
和当前 roles 必须来自 Platform 已认证 session 与最新 Directory lookup。已签名 owner
catalog 继续强制执行每个必需的 request 和 response scope selector。

修改 role 后，应在目标 Workspace 检查用户当前 catalog snapshot。只有 member role
而没有 command role 的用户不能看到或调用对应写操作；移除 role 后，下一次授权检查必须
立即生效。Operation status 与 reconcile 保持只读，并与连接管理、comment 和状态修改
权限分开。

## 回滚

回滚 policy 或 bundle 前，先从 Directory assignment 中移除 command role。然后成套恢复
此前经过审查的 lock 和匹配 bundle，并验证已部署 Platform release 报告相同的 lock。
不得修改 source pin 或 lock 来伪装尚未合并的 owner catalog 已经发布。
