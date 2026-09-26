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

此矩阵定义所需证据，不代表生产 runtime 已部署或通过。Platform app crate 已提供可注入 router，但生产 provider 接线、
ingress 网络隔离与完整 TCK 仍是部署门槛。
