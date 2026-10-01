# Workspace Relay Host runtime notes | Workspace Relay Host 运行说明

The Relay Host, Connector Host, and Sidecar are independent Cargo packages.
This document keeps their runtime boundary notes together; the corresponding
package READMEs own their build commands and component manifests.

Relay Host、Connector Host 与 Sidecar 已拆为独立 Cargo package。本文件集中保留运行边界说明；各组件的 README 负责其构建命令与 package manifest。

## `cy-workspace-relay-host`

`cy-workspace-relay-host` is the non-fixture process entrypoint for
`WorkspaceRelay`. Startup requires `CYRENE_WORKSPACE_DIRECTORY_DATABASE_URL`
for the read-only PostgreSQL Directory role. Connections require TLS with
certificate and hostname verification; include `sslrootcert` in the URL when
the database uses a private CA. Missing configuration, connection failure, or
invalid Directory data fails closed. The host has no file-backed fallback.

The host can authenticate the Frontend role only when all five BFF trust
settings are present: `CYRENE_WORKSPACE_RELAY_BFF_CLIENT_CA_BUNDLE`,
`CYRENE_WORKSPACE_RELAY_BFF_CERT_ALLOWLIST`,
`CYRENE_WORKSPACE_RELAY_WEB_HANDOFF_ISSUER`,
`CYRENE_WORKSPACE_RELAY_WEB_HANDOFF_AUDIENCE`, and
`CYRENE_WORKSPACE_RELAY_WEB_HANDOFF_PUBLIC_KEY_BASE64URL`. It checks the same
RPC's ACA-overwritten XFCC against a dedicated BFF CA, exact subject and active
leaf fingerprint pin before checking the signed, short-lived handoff. The
browser's AAD access token is not a Relay credential. Partial BFF configuration
fails startup; if absent, Frontend authentication denies every request.

WorkspaceConnector authentication is disabled in this host. The host does not
load a device CA or accept a device registry until the durable registry adapter
is composed. A device CA mount by itself does not enable Connector access.
Directory administration, DeviceAuthorization, CA issuance, WebAuthn and
Product private access are also not composed.

Product HTTP is performed by `ProductHttpApiAdapter` on the WorkspaceConnector
path. A configured Frontend Relay session does not create that Product path;
Connector authentication is denied here, so no Product request can currently
reach the owning service through this host.

`GET /healthz` returns HTTP 200 while the process is alive. `GET /readyz` runs a
bounded read-only Directory database probe and reports only fixed dependency
booleans. It returns HTTP 503 until Directory administration, the durable
Workspace device registry, DeviceAuthorization, certificate issuance, WebAuthn,
Product private access, and deployment topology are verified. The host cannot
infer live ACA settings or private-network reachability from environment
variables, so topology remains an external gate. Database diagnostics and
credentials are not included in probe output.

For local use, supply the trusted database URL through the environment before
starting the host:

```bash
export CYRENE_WORKSPACE_DIRECTORY_DATABASE_URL='postgresql://<reader>@<host>/<database>?sslmode=verify-full&sslrootcert=/path/to/postgres-roots.pem'
cargo run --locked -p cy-workspace-relay-host
```

The default listeners are `127.0.0.1:8080` for gRPC and `127.0.0.1:8081` for
health probes. A non-loopback Relay bind requires
`CYRENE_WORKSPACE_RELAY_ACA_INGRESS_ASSERTION=client-certificate-required`; a
configured Frontend requires this assertion even with a loopback bind. It is
an operator assertion, not evidence of live ACA configuration. A non-loopback
health bind also requires
`CYRENE_WORKSPACE_RELAY_HEALTH_BIND_ASSERTION=probe-only-not-ingress`.

ACA must use a private ingress (`external: false`), HTTP/2 on port 8080,
`allowInsecure: false`, `clientCertificateMode: require`, and must block direct
connections that could bypass Envoy. Bind the listeners to the container
interface (for example `0.0.0.0:8080` and `0.0.0.0:8081`), keep port 8081 out
of additional ingress mappings, and configure startup/liveness probes to
`8081/healthz` and readiness to `8081/readyz`. ACA requires a client
certificate at the application ingress before RelayHello reveals the role; the
Relay then selects the independent BFF trust path for Frontend. XFCC is trusted
only if ingress overwrites caller-supplied values and direct bypass is blocked.
If those properties cannot be demonstrated, use a separate private Relay
Frontend service rather than relying on forwarded headers.

Mount the PostgreSQL CA at the path named by `sslrootcert` in the secret-backed
Directory URL. Mount the BFF CA and its JSON certificate allowlist at the paths
used by the two BFF path variables. The public handoff verification key is
configured separately. A future device CA mount may be prepared by deployment
infrastructure, but it is unused here until a durable device-registry-backed
Connector adapter is implemented.

The BFF handoff is a bearer credential with a maximum 60-second lifetime. It
can be replayed until expiry and is not holder-bound to the BFF certificate or
backed by replay-state storage. Every Frontend RPC still requires the separately
verified BFF workload certificate. Keep the BFF signing key outside Relay and
never log either credential.

ACA deployment notes and the current cross-environment network gates are in
[`workspace-relay.md`](https://github.com/DoHorizon-AI/Cyrene-Client/blob/07a851624193f22c524b14c81f382e770fd63e0e/infrastructure/azure/container-apps/workspace-relay.md). The YAML remains a
review-only template; this host is not deployed or ready for production traffic.

---

## `cy-workspace-relay-host` 中文说明

`cy-workspace-relay-host` 是 `WorkspaceRelay` 的非 fixture 进程入口。启动必须提供
`CYRENE_WORKSPACE_DIRECTORY_DATABASE_URL`，使用 PostgreSQL Directory 只读角色。连接必须启用证书和主机名校验；数据库使用私有 CA 时，应在 URL 中配置
`sslrootcert`。缺少配置、无法连接或 Directory 数据无效时均 fail closed；host 不再回退到文件存储。

只有以下五项 BFF 信任配置全部提供时，host 才会认证 Frontend：
`CYRENE_WORKSPACE_RELAY_BFF_CLIENT_CA_BUNDLE`、
`CYRENE_WORKSPACE_RELAY_BFF_CERT_ALLOWLIST`、
`CYRENE_WORKSPACE_RELAY_WEB_HANDOFF_ISSUER`、
`CYRENE_WORKSPACE_RELAY_WEB_HANDOFF_AUDIENCE` 和
`CYRENE_WORKSPACE_RELAY_WEB_HANDOFF_PUBLIC_KEY_BASE64URL`。host 会先在同一 RPC 上校验 ACA 覆盖写入的 XFCC：证书链来自独立 BFF CA，subject 精确匹配，叶证书指纹命中未撤销 pin；之后才校验短时签名 handoff。浏览器 AAD access token 不是 Relay credential。部分配置会使启动失败；未配置时所有 Frontend 请求均被拒绝。

此 host 禁用 WorkspaceConnector 认证。持久设备注册表 adapter 接通前，host 不加载 device CA，也不接受 Connector；仅挂载 device CA 不会启用访问。Directory 管理、DeviceAuthorization、CA 签发、WebAuthn 和 Product 私有访问也尚未组合。Product HTTP 由 WorkspaceConnector 路径上的 `ProductHttpApiAdapter` 发起；Frontend Relay session 配置不会自动产生该通路。此 host 拒绝 Connector 身份，因此当前没有请求能经它到达 Product owner。

进程存活时 `GET /healthz` 返回 HTTP 200。`GET /readyz` 会在有界时间内执行只读 Directory 数据库探测，并只报告固定依赖布尔值。Directory 管理、持久 Workspace 设备注册表、DeviceAuthorization、证书签发、WebAuthn、Product 私有访问和部署拓扑全部验证前，响应保持 HTTP 503。host 无法根据环境变量推断 ACA 实际设置或私网连通性，因此拓扑仍是外部 gate。探针不包含数据库诊断信息或 credential。

本地启动前，通过环境变量提供受信数据库 URL：

```bash
export CYRENE_WORKSPACE_DIRECTORY_DATABASE_URL='postgresql://<reader>@<host>/<database>?sslmode=verify-full&sslrootcert=/path/to/postgres-roots.pem'
cargo run --locked -p cy-workspace-relay-host
```

默认 gRPC listener 为 `127.0.0.1:8080`，健康探针 listener 为
`127.0.0.1:8081`。Relay 使用非 loopback bind 时必须设置
`CYRENE_WORKSPACE_RELAY_ACA_INGRESS_ASSERTION=client-certificate-required`；配置 Frontend 时，即使 loopback bind 也必须设置。该变量只是运维声明，不是 ACA 实际配置证据。健康 listener 使用非 loopback bind 时，还须设置
`CYRENE_WORKSPACE_RELAY_HEALTH_BIND_ASSERTION=probe-only-not-ingress`。

ACA 必须使用私有 ingress（`external: false`），在 8080 使用 HTTP/2，设置
`allowInsecure: false` 与 `clientCertificateMode: require`，并阻止绕过 Envoy 的直连。listener 绑定容器网卡（例如 `0.0.0.0:8080` 和
`0.0.0.0:8081`），8081 不得出现在额外 ingress mapping 中；启动/存活 probe 使用
`8081/healthz`，就绪 probe 使用 `8081/readyz`。ACA 应用级 ingress 会在 RelayHello 揭示角色前先要求客户端证书；Relay 再为 Frontend 选择独立 BFF 信任路径。只有能证明 ingress 覆盖调用方提交的 XFCC 并阻止直连绕过时才可信任 XFCC。若无法证明，应拆分为独立私有 Relay Frontend service，不依赖转发 header。

PostgreSQL CA 挂载路径必须与 secret-backed Directory URL 的 `sslrootcert` 一致。BFF CA 和 JSON certificate allowlist 分别挂载到两个 BFF path 变量指定的位置；handoff 公钥单独配置。部署基础设施可以提前准备 device CA mount，但持久设备注册表 Connector adapter 实现前，此 host 不使用它。

BFF handoff 是最长 60 秒的 bearer credential，到期前可重放，不绑定 BFF 证书，也没有 replay-state storage。每个 Frontend RPC 仍必须单独通过 BFF workload 证书校验。BFF 签名私钥必须留在 Relay 之外，任何 credential 都不得写日志。

ACA 部署说明和当前跨环境网络 gate 见
[`workspace-relay.md`](https://github.com/DoHorizon-AI/Cyrene-Client/blob/07a851624193f22c524b14c81f382e770fd63e0e/infrastructure/azure/container-apps/workspace-relay.md)。YAML 仍是仅供 review 的模板；host 未部署，也未达到生产流量就绪条件。

## `cy-workspace-sidecar`

`cy-workspace-sidecar` is a separate loopback client bridge for non-Rust
consumers. It requires an externally issued credential bundle and a local
bearer token; it does not use fixture credentials or the development verifier.
Its gRPC contract is `cyrene.workspace.local.v1.WorkspaceSidecarService`.
Build or run it independently with `cargo build --locked -p cy-workspace-sidecar`
or `cargo run --locked -p cy-workspace-sidecar`. See its package README and the
fabric crate README for protected file and deployment constraints.

## `cy-workspace-sidecar` 本地代理

`cy-workspace-sidecar` 为非 Rust client 提供独立的 loopback 代理。它要求外部签发的
credential bundle 与本地 bearer token，不使用 fixture credential 或 development
verifier。gRPC 合同是 `cyrene.workspace.local.v1.WorkspaceSidecarService`。可通过
`cargo build --locked -p cy-workspace-sidecar` 独立构建，或通过
`cargo run --locked -p cy-workspace-sidecar` 启动。受保护文件格式与部署边界见其 package
README 和 fabric crate README。

The full executable path is owned by
`tooling/acceptance/distributed-workspace-fabric/run-relay-proof.sh`.

完整可执行路径由 `tooling/acceptance/distributed-workspace-fabric/run-relay-proof.sh`
统一编排。
