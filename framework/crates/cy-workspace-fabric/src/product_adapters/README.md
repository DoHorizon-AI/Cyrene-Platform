# Product HTTP adapters / Product HTTP 适配器

This directory maps the Workspace's closed Product operation keys to fixed
Product-owned HTTP routes. The shared client requires private per-owner HTTPS
endpoint and bearer credential configuration, rejects redirects, applies
timeouts, and caps JSON request and response bodies at 4 MiB. It does not
persist Product resources or allow callers to select an endpoint, path, or
HTTP method.

Sending the configured bearer value does not prove that a Product server
validates it. Current Catalyst and Echo routes do not establish bearer
authentication or user attribution. Yield's current routes do not validate an
inbound bearer, and Reactor's legacy ControlBearer path is fail-open when
`credential_file` is absent. Production adapters must use a dedicated private
`/internal/workspace/v1/...` alias at each Product server that validates its
own configured service credential. Do not enable this aggregate as a production
adapter until those owner routes and trusted caller audit provenance are wired.
The Workspace adapter does not invent or forward a user as a Product actor.
The role policy grants only the exact configured user commands; Navigator event
append remains denied until a trusted Harness writer handoff exists.

本目录将 Workspace 封闭的 Product 操作键映射到固定的 Product HTTP 路由。公共 client
要求服务端私有配置每个 owner 的 HTTPS 端点与 bearer credential，禁用重定向，设置超时，
并将 JSON 请求与响应限制为 4 MiB。它不持久化 Product 资源，也不允许调用方选择端点、
路径或 HTTP method。

发送配置的 bearer 值不能证明 Product 服务会校验它。当前 Catalyst 与 Echo OpenAPI
 文档和路由实现均未建立 bearer 认证或用户归属；因此生产端点必须指向会校验服务凭据
 的 hosting gateway。此适配器不会伪造或转发 Workspace 用户作为 Product actor。可信用户
 来源与 Product 本地审计事件仍是 hosting/契约集成门槛。Workspace 接通显式 Product
 role policy 前，Frontend COMMAND 操作继续拒绝。

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

发送配置的 bearer 值不能证明 Product 服务会校验它。当前 Catalyst 与 Echo 路由没有建立
bearer 认证或用户归属；Yield 当前路由不校验入站 bearer；Reactor 的旧 ControlBearer 路径在
`credential_file` 未配置时 fail-open。生产 adapter 必须使用每个 Product 服务独立的
`/internal/workspace/v1/...` 私有别名，并由 owner 自行校验对应的 service credential。接通这些
私有路由与可信调用者审计来源前，不得将此 aggregate 启用为生产 adapter。Workspace adapter 不会
伪造或转发用户作为 Product actor。角色策略只授予精确配置的用户命令；可信 Harness writer handoff
接入前，Navigator event append 继续拒绝。
