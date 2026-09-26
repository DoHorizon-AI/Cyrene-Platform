# Workspace Fabric executables | Workspace Fabric 可执行程序

`cy-workspace-relay-host` is the non-fixture process entrypoint for
`WorkspaceRelay`. It opens the private file-backed Workspace Directory, emits
structured logs, serves `/healthz` and `/readyz` on its health listener, and
handles SIGINT/SIGTERM with graceful shutdown. Frontend sessions are denied by
default; the optional Frontend path requires both a pinned BFF workload
certificate and a signed short-lived handoff. Workspace connector certificate
checks are disabled by default; the explicit ACA mode validates XFCC against a
separate device CA and the file-backed device registry. `/healthz` reports
process liveness; `/readyz` remains HTTP 503 while production Directory,
authorization, WebAuthn, and Product access dependencies are missing.

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
automatic target-port detection when both listeners are active. Use private
ingress (`external: false`) and prevent direct connections to the app target
port so every Relay request reaches ACA Envoy first. For example, the ACA
template needs this ingress and per-container probe configuration:

```yaml
configuration:
  ingress:
    external: false
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
        # Optional Frontend authentication; this CA is separate from the device CA.
        - name: CYRENE_WORKSPACE_RELAY_BFF_CLIENT_CA_BUNDLE
          value: /etc/cyrene/bff-ca/roots.pem
        - name: CYRENE_WORKSPACE_RELAY_BFF_CERT_ALLOWLIST
          value: /etc/cyrene/bff-ca/allowlist.json
        - name: CYRENE_WORKSPACE_RELAY_WEB_HANDOFF_ISSUER
          value: https://workspace-web-bff.internal
        - name: CYRENE_WORKSPACE_RELAY_WEB_HANDOFF_AUDIENCE
          value: cyrene-workspace-relay
        - name: CYRENE_WORKSPACE_RELAY_WEB_HANDOFF_PUBLIC_KEY_BASE64URL
          value: <base64url-no-padding-ed25519-public-key>
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
the mounted device CA, and applies the approved-device registry checks. This
mode is safe only when the deployed ACA ingress requires client certificates
and overwrites caller-supplied XFCC; the environment assertion does not prove
that configuration. ACA's client-certificate mode is app-wide: it requires and
forwards a client certificate before RelayHello reveals the participant role.
The Relay then selects trust by role on the same RPC: WorkspaceConnector uses
only the device CA and registry; Frontend uses only the BFF CA, exact subject,
certificate fingerprint allowlist, and signed handoff verifier. Caller-supplied
organization, roles, URL, or unsigned identity headers do not establish a
Frontend principal.

The optional Frontend path is enabled only when all five variables are present:
`CYRENE_WORKSPACE_RELAY_BFF_CLIENT_CA_BUNDLE`,
`CYRENE_WORKSPACE_RELAY_BFF_CERT_ALLOWLIST`,
`CYRENE_WORKSPACE_RELAY_WEB_HANDOFF_ISSUER`,
`CYRENE_WORKSPACE_RELAY_WEB_HANDOFF_AUDIENCE`, and
`CYRENE_WORKSPACE_RELAY_WEB_HANDOFF_PUBLIC_KEY_BASE64URL`. Partial configuration
fails startup. The key must be the canonical, unpadded base64url encoding of a
32-byte Ed25519 public key. The allowlist is a JSON array such as:

```json
[
  {
    "sha256Fingerprint": "<64-hex-character-leaf-DER-sha256>",
    "subject": "CN=Cyrene Web BFF Workload",
    "revoked": false
  }
]
```

Every Frontend Relay RPC must carry exactly one ACA-overwritten XFCC value whose
chain validates under the BFF-only CA, whose subject and leaf fingerprint match
an active allowlist entry, and whose leaf is current and has client-auth EKU.
`revoked: true`, an absent pin, malformed XFCC, or a certificate mismatch denies
the RPC before the signed handoff is checked. The handoff verifier accepts only
its configured issuer/audience and the signed `v1` credential; the browser's AAD
access token is not a Relay credential. Keep the BFF signing key outside Relay
and never log either credential. Handoff credentials are bearer tokens: they
are replayable until expiry, with a maximum lifetime of 60 seconds, and are not
cryptographically bound to a BFF certificate or backed by replay-state storage.
The independently checked BFF certificate is still required for each Frontend
RPC. Pin additions, revocations, and CA changes take effect after host restart.
The environment assertion is an operator
setting, not proof of ACA configuration. If ACA cannot guarantee ingress-only
reachability and XFCC overwrite, use a separate private Relay frontend service
instead of trusting the forwarded header.

The Relay remains unready until durable Directory membership and administration,
device authorization, CA issuance, WebAuthn, Product private credentials, and
live ingress identity are connected and verified.

`cy-workspace-relay-host` 是 `WorkspaceRelay` 的非 fixture 进程入口。它打开私有文件式
Workspace Directory，以结构化格式写日志，在健康监听端口提供 `/healthz` 与 `/readyz`，并在收到
SIGINT/SIGTERM 后优雅停止。Frontend 默认拒绝；可选 Frontend 路径必须同时验证固定 BFF workload
证书和短时签名 handoff。Workspace Connector 证书校验默认关闭；显式 ACA 模式会使用独立设备 CA 与文件式注册表校验
XFCC。`/healthz` 表示进程存活；生产 Directory、授权、WebAuthn 与 Product 访问依赖未接通时 `/readyz` 保持 HTTP 503。

host 默认仅绑定 loopback。Relay 使用非 loopback bind 时，必须设置
`CYRENE_WORKSPACE_RELAY_ACA_INGRESS_ASSERTION=client-certificate-required`，并设置
`CYRENE_WORKSPACE_RELAY_ACA_CLIENT_CA_BUNDLE` 指向运维管理的客户端 CA bundle；两者同时设置才会启用
ACA XFCC 校验，loopback 也可显式启用。这只是运维声明，不会对照 Azure 实际资源配置校验。健康监听使用非 loopback bind 时，还必须设置
`CYRENE_WORKSPACE_RELAY_HEALTH_BIND_ASSERTION=probe-only-not-ingress`；该 listener 只响应存活和就绪
探针，且不得映射为 ACA ingress 或 `additionalPortMappings` target。使用 ACA 时，两个 listener 都要绑定容器网卡（例如
`0.0.0.0:8080` 与 `0.0.0.0:8081`）；loopback 默认值仅用于本地运行。必须显式将 ingress
target port 设为 `8080`，transport 设为 HTTP/2，`allowInsecure: false`，并设置
`clientCertificateMode: require`。两个 listener 同时运行时不要依赖自动 target-port 检测。
使用私有 ingress（`external: false`），并阻止直连应用 target port，确保 Relay request 一律先经过 ACA Envoy。
在容器上显式配置启动与存活 HTTP probe：`port: 8081`、`path: /healthz`；就绪 HTTP probe 使用
`port: 8081`、`path: /readyz`。ACA 默认 probe 检查 ingress port；若不显式配置，它们会检查 gRPC
listener 而不是健康端点。当前 `/readyz` 返回 503，因此 ACA 会正确地将该 revision 保持为未就绪并且
不向它发送 ingress 流量。该脚手架可用于部署配置验证，但不能服务生产流量。ACA Envoy 在入口终止客户端 TLS；host 在该私有边界之后监听明文协议。
显式配置上述两项后，host 会读取 XFCC、用挂载的 CA bundle 校验证书链并查询设备注册表；该路径依赖真实 ACA
ingress 强制客户端证书并覆盖客户端提交的 XFCC，环境变量声明不是部署证据。生产就绪前仍须接通持久设备授权、CA
签发、WebAuthn、用户身份、Product 私有凭据并验证 ingress 配置。

ACA 的 client-certificate mode 是应用级配置：RelayHello 揭示角色之前，入口会要求并转发客户端证书。随后 Relay
在同一 RPC 内按角色选择信任路径：WorkspaceConnector 只走设备 CA 与注册表；Frontend 只走独立 BFF CA、精确 subject、
证书指纹 allowlist 与签名 handoff verifier。请求提交的 organization、role、URL 或未签名身份 header 都不构成 Frontend 主体。

只有同时设置以下五项才启用可选 Frontend 路径：`CYRENE_WORKSPACE_RELAY_BFF_CLIENT_CA_BUNDLE`、
`CYRENE_WORKSPACE_RELAY_BFF_CERT_ALLOWLIST`、`CYRENE_WORKSPACE_RELAY_WEB_HANDOFF_ISSUER`、
`CYRENE_WORKSPACE_RELAY_WEB_HANDOFF_AUDIENCE` 与
`CYRENE_WORKSPACE_RELAY_WEB_HANDOFF_PUBLIC_KEY_BASE64URL`。部分配置会使 host 启动失败。公钥必须是 32 字节 Ed25519
公钥的规范无填充 base64url 编码。allowlist 是 JSON array，例如：

```json
[
  {
    "sha256Fingerprint": "<64 位十六进制叶证书 DER SHA-256>",
    "subject": "CN=Cyrene Web BFF Workload",
    "revoked": false
  }
]
```

每个 Frontend Relay RPC 必须携带一个 ACA 覆盖写入的 XFCC 值；证书链必须由 BFF 专用 CA 验证，subject 与叶证书指纹必须
命中未撤销 allowlist，证书须在有效期内且包含 client-auth EKU。`revoked: true`、缺少 pin、错误 XFCC 或证书不匹配都会在
检查签名 handoff 前拒绝 RPC。handoff verifier 只接受固定 issuer/audience 与签名 `v1` credential；浏览器 AAD access token
不是 Relay credential。BFF 签名密钥不得交给 Relay，也不得记录任何 credential。环境变量只是运维配置声明，不证明 ACA 真实设置。
handoff credential 是 bearer token：在到期前可重放，最长有效期为 60 秒，不会以 BFF 证书做密码学绑定，也没有重放状态存储。
每个 Frontend RPC 仍必须单独通过 BFF 证书验证。新增或撤销 pin、替换 CA 要在 host 重启后生效。
若无法确保只允许经 ACA ingress 访问且由 ACA 覆盖 XFCC，应拆分为独立私有 Frontend Relay service，不得信任转发 header。

持久 Directory membership 与管理、设备授权、CA 签发、WebAuthn、Product 私有凭据和 live ingress identity 接通并验证之前，Relay 仍保持未就绪。

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
