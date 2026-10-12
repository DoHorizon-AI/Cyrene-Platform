# Workspace Web BFF v1 TCK

`scenarios.tsv` is the language-neutral acceptance matrix for the Web BFF
contract. It covers verified Azure AD access-token claims and scope, the
server-side identity-to-organization mapping, member-scoped discovery, closed
Product operation resolution, owner HTTP-method/path/body mapping, command
CSRF, W3C trace propagation, bounded JSON/errors, safe resource references, and
fail-closed deployment behavior. It also covers the signed Web-to-Relay
Frontend handoff and the separate BFF workload mTLS identity. Browser device
approval coverage checks the canonical private begin/complete/deny DTOs and
status codes, verified-session digest continuity, current Directory role checks,
exact Origin and CSRF on all three commands, and that device start/poll/ACK stay
private. Product invocation uses a POST envelope for both READ and COMMAND
kinds; kind and upstream method come from the closed projection manifest and
owner OpenAPI document.

`scenarios.tsv` 是 Web BFF 契约的跨语言验收矩阵，覆盖已验证的 Azure AD access token claim/scope、服务端 identity-to-organization
映射、member-scoped discovery、封闭 Product routing、
CSRF（包括同一 session 并发刷新稳定性）、W3C trace 传播、有界 JSON/error、安全资源引用以及 fail-closed 部署行为。Product invocation 对 READ 和 COMMAND 类型
统一使用 POST envelope；类型和上游 method 来自 closed projection manifest 与 owner OpenAPI 文档。浏览器设备审批场景检查规范私有 begin/complete/deny
DTO 与状态码、verified-session digest 连续性、当前 Directory role、三个 command 的 exact Origin 和 CSRF，以及设备 start/poll/ACK 保持私有。
矩阵还覆盖签名 Web→Relay Frontend handoff
及单独的 BFF workload mTLS identity。

The `operation_enum_parity` scenario reads the canonical
`WorkspaceProductApiOperation` enum and
`contracts/tck/distributed-workspace-fabric/v1/product-projections.tsv` from the
same integrated Platform revision. It passes only when every nonzero enum key
has exactly one manifest row and every row has one accepted owner OpenAPI
operation and matching `READ`/`COMMAND` kind.

`operation_enum_parity` 场景从同一 Platform 集成版本读取规范
`WorkspaceProductApiOperation` enum 和
`contracts/tck/distributed-workspace-fabric/v1/product-projections.tsv`。只有每个非零 enum key 恰好对应一行
manifest，且每行都对应一个已接受的 owner OpenAPI operation 及匹配的 `READ`/`COMMAND` 类型时才通过。

The matrix defines required evidence; it is not evidence that a production
runtime is deployed or has passed. The Platform app crate now supplies an
injectable router, but production provider composition, ingress isolation, and
the complete TCK remain deployment gates.

The delegation scenarios cover user-owned create/revoke routes, exact operation
and resource scopes, current Directory and policy checks at every execution
phase, configured Exchange mTLS trust, signed certificate-bound feature
negotiation, seven-day grant expiry, short execution credentials, explicit
interactive/delegated outbox schema branches, and fail-closed revocation of
queued work. Product execution validation also checks the certificate-derived
owner/target/audience profile and re-locks the original Connector target fence.
Delegation read-back returns the exact managed workload ID and is self-only
under the current user bearer. Invocation
status is a persisted observation scoped by the Product workload profile and
never acts as execution authorization. Empty Product profiles disable only the
new Product workload RPC and do not block legacy Authority startup.
Catalog operation IDs are filtered by the current Directory roles on each
snapshot request, so a member never receives global command/admin grants.
Passing source or mock-only scenarios does not count as production workload,
database, Directory, or policy evidence.

此矩阵定义所需证据，不代表生产 runtime 已部署或通过。Platform app crate 已提供可注入 router，但生产 provider 接线、
ingress 网络隔离与完整 TCK 仍是部署门槛。

delegation 场景覆盖用户创建和撤销路由、精确 operation/resource scope、每个执行阶段重新检查 Directory 与 policy、配置的 Exchange mTLS
信任、绑定证书的签名 feature negotiation、七天授权期限、短时执行凭据、interactive/delegated outbox schema 分支，以及队列任务撤销后的
fail-closed 行为。仅源码或 mock 场景通过不构成生产 workload、数据库、Directory 或 policy 的运行证据。
Product 执行验证还会检查由证书推导的 owner/target/audience profile，并重新锁定原 Connector target fence。
delegation 读取会返回精确 managed workload ID，且只允许当前 bearer 用户读取自己的 grant。调用状态读取是由 Product workload profile 限定的持久观察，不构成执行授权。Product
profile 为空时只禁用新的 Product workload RPC，不阻止旧版 Authority 启动。
每次获取 catalog 时都会使用最新 Directory role 过滤 operation ID，不会把全局 command/admin grant 返回给普通成员。
