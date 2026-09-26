# Product HTTP adapters / Product HTTP 适配器

This directory maps the Workspace's closed Product operation keys to fixed
Product-owned HTTP routes. The shared client requires private HTTPS endpoint
and bearer credential configuration for each owner, organization, and
Workspace. It resolves endpoints from the authenticated caller context and
fails closed on an organization or Workspace mismatch. It rejects redirects,
applies timeouts, and caps JSON request and response bodies at 4 MiB. It does
not persist Product resources or allow callers to select an endpoint, path, or
HTTP method.

The dispatcher enables Catalyst, Yield, Echo, Exchange, and Reactor through
fixed owner routes and caller-scoped endpoint credentials. Catalyst, Yield,
Echo, and Reactor use `/internal/workspace/v1/...`; Exchange uses protected
`/api/v1/workspace/...` aliases. Reactor permits only GET and POST
`/internal/workspace/v1/model-imports`; its owner checks the configured
workspace-to-serving-binding grant for POST and the adapter preserves an owner
403 response. Legacy Reactor routes are rejected. Navigator operations return
a fixed unavailable error regardless of configured endpoints, and Navigator
append remains denied until a trusted Harness writer handoff exists. The
Workspace adapter does not invent or forward a user as a Product actor, and the
role policy grants only the exact configured user commands. Production
composition and trusted caller audit provenance are still incomplete, so this
aggregate is not yet a production-ready path.

本目录将 Workspace 封闭的 Product 操作键映射到固定的 Product HTTP 路由。公共 client
要求服务端按 owner、organization 与 Workspace 私有配置 HTTPS 端点及 bearer credential。
resolver 根据可信调用者上下文精确匹配范围，organization 或 Workspace 不匹配时 fail closed。
它禁用重定向、设置超时，并将 JSON 请求与响应限制为 4 MiB；不会持久化 Product 资源，
也不允许调用方选择端点、路径或 HTTP method。

| File | Responsibility |
| --- | --- |
| `http.rs` | Private endpoint resolution, injected transport, bounded HTTP and JSON handling. |
| `catalyst.rs` | Fixed Catalyst dataset route mappings. |
| `yield_api.rs` | Fixed Yield draft and run route mappings. |
| `reactor.rs` | Fixed Reactor model import route mappings. |
| `exchange.rs` | Fixed Exchange gateway route mappings. |
| `echo.rs` | Fixed Echo evaluation suite route mappings. |
| `navigator.rs` | Fixed Navigator read routes; event append remains denied. |
| `mod.rs` | Six-owner `ProductInvocationPort` router and module composition. |

Suggested reading order: `mod.rs`, `http.rs`, then the owner adapter matching
the operation under review.

建议阅读顺序：先读 `mod.rs`，再读 `http.rs`，最后按待审查操作阅读对应 owner 适配器。

当前 dispatcher 通过固定 owner 路由与调用者范围 endpoint credential 启用 Catalyst、Yield、Echo、Exchange
和 Reactor。Catalyst、Yield、Echo 与 Reactor 使用 `/internal/workspace/v1/...`；Exchange 使用受保护的
`/api/v1/workspace/...` 别名。Reactor 仅允许 GET 与 POST
`/internal/workspace/v1/model-imports`；owner 会检查 POST 对应的 workspace-to-serving-binding grant，适配器原样
保留 owner 的 403 响应，并拒绝旧 Reactor 路由。即使配置了 endpoint，Navigator 操作仍返回固定 unavailable。
可信 Harness writer handoff 接入前，Navigator append 也继续拒绝。Workspace adapter 不会伪造或转发用户作为
Product actor，角色策略只授予精确配置的用户命令。生产组合与可信调用者审计来源仍未完成，因此此 aggregate 尚不是
可用于生产的路径。
