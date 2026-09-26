# Product HTTP adapters / Product HTTP 适配器

This directory maps the Workspace's closed Product operation keys to fixed
Product-owned HTTP routes. The shared client requires private HTTPS endpoint
and bearer credential configuration for each owner, organization, and
Workspace. It resolves endpoints from the authenticated caller context and
fails closed on an organization or Workspace mismatch. It rejects redirects,
applies timeouts, and caps JSON request and response bodies at 4 MiB. It does
not persist Product resources or allow callers to select an endpoint, path, or
HTTP method.

The dispatcher currently enables Catalyst, Yield, Echo, and Exchange through
fixed owner routes and caller-scoped endpoint credentials. Catalyst, Yield, and
Echo use `/internal/workspace/v1/...`; Exchange uses the protected
`/api/v1/workspace/...` aliases and rejects its legacy global paths. Reactor and
Navigator operations return a fixed unavailable error regardless of configured
endpoints until their private routes are ready. Navigator append also remains
denied until a trusted Harness writer handoff exists. The Workspace adapter
does not invent or forward a user as a Product actor, and the role policy grants
only the exact configured user commands. Production composition and trusted
caller audit provenance are still incomplete, so this aggregate is not yet a
production-ready path.

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

当前 dispatcher 仅通过固定 owner 路由与调用者范围 endpoint credential 启用 Catalyst、Yield、Echo
和 Exchange。Catalyst、Yield 与 Echo 使用 `/internal/workspace/v1/...`；Exchange 使用受保护的
`/api/v1/workspace/...` 别名，并拒绝旧全局路径。即使配置了 endpoint，Reactor 与 Navigator 的操作仍返回固定
unavailable，直到各自私有路由就绪。可信 Harness writer handoff 接入前，Navigator append 也继续拒绝。
Workspace adapter 不会伪造或转发用户作为 Product actor，角色策略只授予精确配置的用户命令。生产组合与可信调用者
审计来源仍未完成，因此此 aggregate 尚不是可用于生产的路径。
