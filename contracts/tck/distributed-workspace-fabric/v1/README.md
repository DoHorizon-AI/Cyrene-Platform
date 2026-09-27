# Distributed Workspace Fabric v1 TCK

This TCK freezes the authority boundaries and observable outcomes of
identity-based Workspace discovery, transport-neutral connection selection,
direct-first private access and Relay fallback semantics. `scenarios.tsv` is the language-neutral matrix.
The executable Docker proof covers Relay discovery, fail-closed Connector admission,
private direct access, Runtime Agent, and Artifact transfer. Positive Workspace Connector
traffic through Relay remains unimplemented until certificate validation, revocation,
and current Registry binding are composed.

The proof records the Workspace-device peer validator, signed current revocation, current
Registry binding validation, and the dispatch fence as `NOT_CONFIGURED`. TLS client
certificate trust remains configured. The observed Connector denial occurs at the first
missing application-level validator gate; later gates are not exercised independently.

本 TCK 冻结基于身份的 Workspace 发现、transport-neutral 连接选择、私网直连优先与 Relay 回退的
远程访问的权威边界与可观察结果。`scenarios.tsv` 是跨语言矩阵。Docker proof 会验证
Relay discovery、Connector fail-closed admission、私网直连、Runtime Agent 与 Artifact transfer。
Workspace Connector 经 Relay 传输的正向路径仍未实现，需先组合证书验证、撤销检查和当前 Registry binding。
Proof 会记录 Workspace-device peer validator、签名的当前撤销证据、当前 Registry binding
validation 和 dispatch fence 均为 `NOT_CONFIGURED`。TLS client-certificate trust 仍然配置。
观察到的 Connector 拒绝发生在首个缺失的应用层 validator gate；后续 gate 不会被单独执行。

Run:

```bash
bash tooling/ci/check-distributed-workspace-fabric.sh
bash tooling/acceptance/distributed-workspace-fabric/run-workspace-fail-closed-proof.sh
```

The Docker proof is mandatory for release acceptance. A Docker/WSL transport
outage is reported as a blocker and is never converted into a skip or fake
pass.

## Phase 3 Product API projection

`WorkspaceProductApiRequest` admits a closed set of wire operations. The
`product-projections.tsv` matrix binds each wire operation to its owning Product,
the Product's public OpenAPI `operationId`, and its `READ` or `COMMAND` kind.
The Workspace Control Plane must reject an unknown operation or any owner/kind
mismatch before routing. The request has no caller-selected URL, path, or HTTP
method. Request JSON is valid `application/json`; response media types are
`application/json` or `application/problem+json`. Each JSON body is limited to
4 MiB. The opaque body and Product status code pass through unchanged, including
201/202 and the owner-issued resource identity in the response body. The
owner's idempotency and replay rules remain authoritative; HTTP `Location` is
not projected. Product services keep their domain records and command effects.

The first five Products each have one read and one command. Navigator includes
its `observeWorkspaceSnapshot` query plus the Harness persistence `get_session`
and `append_events` operations. The persistence contract and implementation
make the read membership-authorized and the append an atomic, fenced event-batch
write. Append is permitted only for an authenticated Harness/service writer
through trusted server-side caller context; Frontend callers must receive
`PERMISSION_DENIED` or `UNIMPLEMENTED` until that authentication handoff is
implemented. The `writerToken` must never be exposed to a browser. Its
`writerToken`/epoch fencing and `batchId` idempotency stay owner-validated.
Navigator's observation endpoint remains read-only. Exchange's command creates
a route draft and requires its `Idempotency-Key`; it does not confirm or activate
the route. Product responses must not contain Docker addresses, local filesystem
paths, container IDs, or internal Product `Location` URLs. These TCK rows define
acceptance requirements and do not claim that Product adapters or end-to-end
runs have already passed.

发布验收必须执行 Docker proof。Docker/WSL transport 故障只能报告为 blocker，
不得转换为 skip 或 fake pass。

---

<!-- Chinese Translation / 中文翻译 -->

# Distributed Workspace Fabric v1 TCK

此 TCK 冻结基于身份的 Workspace 发现、与 transport 无关的连接选择、私网直连优先和 Relay 回退语义的 authority 边界与可观察结果。`scenarios.tsv` 是跨语言矩阵。Docker proof 会验证 Relay discovery、Connector fail-closed admission、私网直连、Runtime Agent 与 Artifact transfer。Workspace Connector 经 Relay 传输的正向路径仍未实现，需先接通证书验证、撤销检查和当前 Registry binding。Proof 会记录 Workspace-device peer validator、签名的当前撤销证据、当前 Registry binding validation 和 dispatch fence 均为 `NOT_CONFIGURED`；TLS client-certificate trust 仍然配置。观察到的 Connector 拒绝发生在首个缺失的应用层 validator gate，后续 gate 不会被单独执行。

运行：

```bash
bash tooling/ci/check-distributed-workspace-fabric.sh
bash tooling/acceptance/distributed-workspace-fabric/run-workspace-fail-closed-proof.sh
```

发布验收必须执行 Docker proof。Docker/WSL transport 故障只能报告 blocker，不得转换成 skip 或 fake pass。

## Phase 3 Product API 投影

`WorkspaceProductApiRequest` 只接受封闭的 wire operation 集合。`product-projections.tsv` 将每个 wire operation 绑定到所属 Product、公开 OpenAPI `operationId` 及 `READ` 或 `COMMAND` 类型。Workspace Control Plane 必须在路由前拒绝未知 operation 或 owner/kind 不匹配的请求。请求不接受调用方选择 URL、path 或 HTTP method。请求 JSON 必须是 `application/json`，响应媒体类型仅限 `application/json` 或 `application/problem+json`；每个 JSON body 最大 4 MiB。Product HTTP 状态码和不透明 JSON body 原样保留，包括 201/202 和响应 body 中 owner 签发的资源标识。幂等及重放规则仍由 owner 决定，不投影 HTTP `Location`。领域记录与命令效果仍由 Product 服务持有。

首批五个 Product 各有一个读取和一个命令。Navigator 除 `observeWorkspaceSnapshot` 查询外，还纳入 Harness persistence 的 `get_session` 与 `append_events` 操作。其 persistence 契约和实现要求读操作验证 Workspace membership，append 是原子且受 fencing 保护的事件批次写入。Append 仅允许经可信服务端 caller context 认证的 Harness/service writer；在认证 handoff 尚未实现时，Frontend 调用必须返回 `PERMISSION_DENIED` 或 `UNIMPLEMENTED`。`writerToken` 不得暴露给浏览器，其 token/epoch fencing 和 `batchId` 幂等仍由 owner 验证。Navigator observation endpoint 仍为只读。Exchange 命令只创建路由草稿且必须提供 `Idempotency-Key`，不隐含确认或激活。Product 响应不得包含 Docker 地址、本地文件系统路径、container ID 或内部 Product `Location` URL。这些 TCK 场景定义验收要求，不代表 Product adapter 或端到端运行已经通过。
