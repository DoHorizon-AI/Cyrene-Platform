# Workspace Fabric executables | Workspace Fabric 可执行程序

`cy-workspace-relay-host` is the non-fixture process entrypoint for
`WorkspaceRelay`. It opens the private file-backed Workspace Directory, emits
structured logs, serves `/healthz` and `/readyz` on its health listener, and
handles SIGINT/SIGTERM with graceful shutdown. Frontend user sessions remain
denied. Workspace connector certificate checks are disabled by default; the
explicit ACA mode validates XFCC against a configured client CA and the
file-backed device registry. `/healthz` reports process liveness; `/readyz`
remains HTTP 503 while production identity, authorization, and Product access
dependencies are missing.

The host defaults to loopback binds. A non-loopback Relay bind requires
`CYRENE_WORKSPACE_RELAY_ACA_INGRESS_ASSERTION=client-certificate-required` and
`CYRENE_WORKSPACE_RELAY_ACA_CLIENT_CA_BUNDLE` pointing to an operator-managed
client CA bundle. Setting both variables opts into ACA XFCC validation, even
on loopback. The assertion is not checked against live Azure resources. A
non-loopback health bind also requires
`CYRENE_WORKSPACE_RELAY_HEALTH_BIND_ASSERTION=probe-only-not-ingress`; that
listener exposes only the liveness and readiness responses and must not be
mapped as ACA ingress or an `additionalPortMappings` target. In ACA, both
listeners must bind to the container interface (for example
`0.0.0.0:8080` and `0.0.0.0:8081`); the loopback defaults are for local use.
Set the ingress target port explicitly to `8080` with HTTP/2 transport,
`allowInsecure: false`, and `clientCertificateMode: require`. Do not rely on
automatic target-port detection when both listeners are active. For example,
the ACA template needs this ingress and per-container probe configuration:

```yaml
configuration:
  ingress:
    targetPort: 8080
    transport: http2
    allowInsecure: false
    clientCertificateMode: require
    # Do not add 8081 under additionalPortMappings.
template:
  containers:
    - name: workspace-relay
      env:
        - name: CYRENE_WORKSPACE_RELAY_BIND
          value: 0.0.0.0:8080
        - name: CYRENE_WORKSPACE_RELAY_HEALTH_BIND
          value: 0.0.0.0:8081
        - name: CYRENE_WORKSPACE_RELAY_ACA_INGRESS_ASSERTION
          value: client-certificate-required
        - name: CYRENE_WORKSPACE_RELAY_ACA_CLIENT_CA_BUNDLE
          value: /etc/cyrene/device-ca/roots.pem
        - name: CYRENE_WORKSPACE_RELAY_HEALTH_BIND_ASSERTION
          value: probe-only-not-ingress
        - name: CYRENE_WORKSPACE_RELAY_DIRECTORY
          value: /var/lib/cyrene/workspace-relay
      probes:
        - type: Startup
          httpGet:
            path: /healthz
            port: 8081
        - type: Liveness
          httpGet:
            path: /healthz
            port: 8081
        - type: Readiness
          httpGet:
            path: /readyz
            port: 8081
```

ACA's default probes target the ingress port; without these explicit probes
they would check the gRPC listener instead of the health endpoints. `/readyz`
returns 503 today, so ACA correctly keeps this revision unready and sends it
no ingress traffic. This scaffold can therefore be started for deployment
configuration validation, but it cannot serve production traffic. Mount private
persistent storage at `/var/lib/cyrene/workspace-relay` and keep one active
Directory owner; the host's file lock is exclusive.
ACA terminates client TLS at Envoy. When the two explicit settings above are
present, the host reads `X-Forwarded-Client-Cert`, validates its chain against
the mounted CA bundle, and applies the approved-device registry checks. This
mode is safe only when the deployed ACA ingress requires client certificates
and overwrites caller-supplied XFCC; the environment assertion does not prove
that configuration. The Relay remains unready until user identity, durable
device authorization, CA issuance, WebAuthn, Directory administration, Product
private credentials, and live ingress identity are connected and verified.

`cy-workspace-relay-host` 是 `WorkspaceRelay` 的非 fixture 进程入口。它打开私有文件式
Workspace Directory，以结构化格式写日志，在健康监听端口提供 `/healthz` 与 `/readyz`，并在收到
SIGINT/SIGTERM 后优雅停止。生产设备 IAM verifier 和目录管理路径接通前，它会拒绝所有 Relay
session。`/healthz` 表示进程存活；缺少生产门槛时 `/readyz` 保持 HTTP 503。

host 默认仅绑定 loopback。Relay 使用非 loopback bind 时，必须设置
`CYRENE_WORKSPACE_RELAY_ACA_INGRESS_ASSERTION=client-certificate-required`，并设置
`CYRENE_WORKSPACE_RELAY_ACA_CLIENT_CA_BUNDLE` 指向运维管理的客户端 CA bundle；两者同时设置才会启用
ACA XFCC 校验，loopback 也可显式启用。这只是运维声明，不会对照 Azure 实际资源配置校验。健康监听使用非 loopback bind 时，还必须设置
`CYRENE_WORKSPACE_RELAY_HEALTH_BIND_ASSERTION=probe-only-not-ingress`；该 listener 只响应存活和就绪
探针，且不得映射为 ACA ingress 或 `additionalPortMappings` target。使用 ACA 时，两个 listener 都要绑定容器网卡（例如
`0.0.0.0:8080` 与 `0.0.0.0:8081`）；loopback 默认值仅用于本地运行。必须显式将 ingress
target port 设为 `8080`，transport 设为 HTTP/2，`allowInsecure: false`，并设置
`clientCertificateMode: require`。两个 listener 同时运行时不要依赖自动 target-port 检测。
在容器上显式配置启动与存活 HTTP probe：`port: 8081`、`path: /healthz`；就绪 HTTP probe 使用
`port: 8081`、`path: /readyz`。ACA 默认 probe 检查 ingress port；若不显式配置，它们会检查 gRPC
listener 而不是健康端点。当前 `/readyz` 返回 503，因此 ACA 会正确地将该 revision 保持为未就绪并且
不向它发送 ingress 流量。该脚手架可用于部署配置验证，但不能服务生产流量。ACA Envoy 在入口终止客户端 TLS；host 在该私有边界之后监听明文协议。
显式配置上述两项后，host 会读取 XFCC、用挂载的 CA bundle 校验证书链并查询设备注册表；该路径依赖真实 ACA
ingress 强制客户端证书并覆盖客户端提交的 XFCC，环境变量声明不是部署证据。生产就绪前仍须接通持久设备授权、CA
签发、WebAuthn、用户身份、Product 私有凭据并验证 ingress 配置。

The ACA ingress requirements follow Microsoft's [client certificate
authorization](https://learn.microsoft.com/en-us/azure/container-apps/client-certificate-authorization),
[ingress configuration](https://learn.microsoft.com/en-us/azure/container-apps/ingress-how-to),
and [health probes](https://learn.microsoft.com/en-us/azure/container-apps/health-probes)
documentation. The environment assertions are not deployment evidence.

`cy-workspace-fabric-fixture` supplies four acceptance-only roles: `relay`,
`connector`, `frontend-start`, and `frontend-observe`. Production deployments
must provide durable Workspace state and a real identity provider; they must
not reuse the file-backed operation view or development session tokens.

`cy-workspace-fabric-fixture` 提供四个仅用于验收的角色：`relay`、`connector`、
`frontend-start` 与 `frontend-observe`。生产部署必须提供持久 Workspace 状态和真实
Identity Provider，不得复用文件驱动的 Operation view 或开发 session token。

## `cy-workspace-sidecar`

`cy-workspace-sidecar` is a separate loopback client bridge for non-Rust
consumers. It requires an externally issued credential bundle and a local
bearer token; it does not use fixture credentials or the development verifier.
Its gRPC contract is `cyrene.workspace.local.v1.WorkspaceSidecarService`.
See the crate README for the protected file format and deployment constraints.

## `cy-workspace-sidecar` 本地代理

`cy-workspace-sidecar` 为非 Rust client 提供独立的 loopback 代理。它要求外部签发的
credential bundle 与本地 bearer token，不使用 fixture credential 或 development
verifier。gRPC 合同是 `cyrene.workspace.local.v1.WorkspaceSidecarService`。受保护文件格式
与部署边界见 crate README。

The full executable path is owned by
`tooling/acceptance/distributed-workspace-fabric/run-relay-proof.sh`.

完整可执行路径由 `tooling/acceptance/distributed-workspace-fabric/run-relay-proof.sh`
统一编排。
