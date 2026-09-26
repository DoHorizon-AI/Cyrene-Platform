# Product HTTP adapters / Product HTTP 适配器

This directory maps the Workspace's closed Product operation keys to fixed
Product-owned HTTP routes. The shared client requires private per-owner HTTPS
endpoint and bearer credential configuration, rejects redirects, applies
timeouts, and caps JSON request and response bodies at 4 MiB. It does not
persist Product resources or allow callers to select an endpoint, path, or
HTTP method.

Sending the configured bearer value does not prove that a Product server
validates it. The current Catalyst and Echo OpenAPI documents and route
implementations do not establish bearer authentication or user attribution;
production endpoints must therefore terminate at a hosting gateway that
validates the service credential. This adapter does not invent or forward a
Workspace user as a Product actor. Trusted user provenance and Product-local
audit events remain a hosting/contract integration gate. Frontend COMMAND
operations stay denied by Workspace until the explicit Product role policy is
connected.

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
| `catalyst.rs` | Fixed `listDatasets` and `createDataset` route mappings. |
| `echo.rs` | Fixed `getEvaluationSuite` and `createEvaluationSuite` route mappings. |
| `mod.rs` | `ProductInvocationPort` adapter and module composition. |

Suggested reading order: `mod.rs`, `http.rs`, then the owner adapter matching
the operation under review.

建议阅读顺序：先读 `mod.rs`，再读 `http.rs`，最后按待审查操作阅读对应 owner 适配器。
