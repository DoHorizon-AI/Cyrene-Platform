# Workspace Connector Host

`cy-workspace-connector-host` is the production process entrypoint for one
Workspace Connector identity. It opens an outbound mTLS Relay session, sends a
server-constructed `WorkspaceConnector` `RelayHello`, and serves the existing
`WorkspaceApi` over that stream. It creates no inbound listener and does not
accept command-line, environment, or request-supplied endpoints, paths, device
identity, or credentials.

This host is buildable code, not a production readiness claim. It has no health
or readiness endpoint. Relay can read the forwarded Workspace API payload in
this release; end-to-end encryption is not a deployment precondition.

## Fixed private files

The process reads only these paths:

| Path | Required policy | Purpose |
| --- | --- | --- |
| `/run/cyrene/workspace-connector` | Real directory, service-owned, mode `0700` | Root for private manifests |
| `connector.json` | Regular file, same owner, mode `0600` | Relay endpoint, expected TLS server name, and exact organization/Workspace/device scope |
| `product-endpoints.json` | Regular file, same owner, no group/world write; deploy as `0600` | Scoped Product owner endpoints and secret basenames |
| `secrets/` | Real directory, same owner, mode `0700` | Relay TLS files and Product credentials |
| `secrets/relay-server-ca.pem` | Regular file, same owner, mode `0600` | Trust roots for the Relay server certificate |
| `secrets/device-enrollment-cert.pem` | Regular file, same owner, mode `0600` | The enrolled Workspace device client certificate |
| `secrets/device-enrollment-key.pem` | Regular file, same owner, mode `0600` | Private key matching the enrolled device certificate |
| `secrets/<credentialFile>` | Regular file, same owner, mode `0600` | One Product owner bearer value named by the Product manifest |

Every path component is fixed in the binary. The host rejects symlinks,
non-regular files, unexpected owners or modes, oversized values, malformed
PEM, an expired/not-yet-valid device certificate, and a certificate/private
key mismatch. The Connector manifest denies unknown JSON fields and accepts
only HTTPS Relay endpoints without URL credentials, query strings, fragments,
or non-root paths. The device CA is a separate trust domain from the Relay
server CA: Relay uses its device CA to authenticate this client certificate,
while this host uses `relay-server-ca.pem` to authenticate Relay.

Example `connector.json`:

```json
{
  "version": 1,
  "relayEndpoint": "https://workspace-relay.internal:8443",
  "relayServerName": "workspace-relay.internal",
  "organizationId": "org-production",
  "workspaceId": "workspace-production",
  "deviceId": "device-production"
}
```

The Product manifest uses the existing strict endpoint loader. Every endpoint
entry must match the Connector's exact organization and Workspace scope.
`credentialFile` is a basename in `secrets/`; paths, duplicate scope entries,
reused bearer secrets, non-HTTPS URLs, and unknown fields fail startup.

```json
{
  "version": 1,
  "endpoints": [
    {
      "owner": "CATALYST",
      "organizationId": "org-production",
      "workspaceId": "workspace-production",
      "baseUrl": "https://catalyst.private.internal/",
      "credentialFile": "catalyst-token"
    }
  ]
}
```

The bearer file contains one printable ASCII token without a trailing newline.
Only owners present in this manifest are routable. A request for an owner with
no configured endpoint scope fails with the fixed unavailable response; no
public endpoint or alternate credential is selected.

ACA Key Vault volume entries can be symlinks or have permissions that do not
meet this policy. Copy each needed value into the service-owned private tree,
then verify ownership, mode, file type, and contents before starting the host.
The process must never hand the mounted volume paths directly to the loader.

## Identity, request handling, and failure behavior

The host constructs `RelayHello` with role `WorkspaceConnector`, no user, an
empty session credential, and the exact organization, Workspace, and device
IDs from `connector.json`. Relay authenticates the presented device
certificate against its approved device registry and compares the registered
certificate fingerprint and scope with the Hello. The Connector cannot prove
that Relay has a live registry; a Relay with no configured durable registry
must reject the connection. Relay-side registry startup, database health, and
the approved device record are separate deployment gates.

Product API requests use `WorkspaceControlPlane` with
`ProductHttpApiAdapter`. The adapter routes only typed Product operations to
the private owner endpoint and credential selected by the manifest. The
separate product-neutral `StartOperation` and `GetOperation` dispatcher
remains unavailable until its Product authority integration is implemented.
Frontend bearer tokens and BFF workload credentials are not loaded or
forwarded by this host.

Missing private files, invalid TLS material, or a Product manifest with a
different organization or Workspace scope stop startup. A Relay rejection or
session loss causes reconnect attempts with a bounded 1-to-30-second backoff.
The process does not report readiness while reconnecting and exposes no
health listener that could be mistaken for readiness. Shutdown flushes logs.

Before production traffic, deployment review must verify the private Relay
route, exact Relay server CA/name, the Relay's device CA and durable registry
configuration, the active certificate's approved fingerprint/scope, private
reachability to every configured Product owner, and the Product owners'
credential validation. This template does not claim those live facts.

The build-only image workflow and review-only ACA packaging gates are in
[`workspace-connector.md`](../../../infrastructure/azure/container-apps/workspace-connector.md).

## Workspace Connector Host 中文说明

`cy-workspace-connector-host` 是单个 Workspace Connector 身份的进程入口。它通过
mTLS 主动连接 Relay，在进程内部构造 `WorkspaceConnector` `RelayHello`，并通过现有
Relay stream 提供 `WorkspaceApi`。它不创建入站 listener，也不接受命令行、环境变量或
请求传入的 endpoint、路径、device identity 或 credential。

该 host 是可构建代码，不代表生产就绪。它没有 health 或 readiness endpoint。本版本
Relay 可以读取转发的 Workspace API payload；部署不以端到端加密为前置条件。

## 固定私有文件

进程只读取上表所列的固定路径。目录和文件必须由服务身份拥有；Connector manifest、
Relay CA、设备证书和设备私钥使用 `0600`，`secrets/` 使用 `0700`。host 拒绝符号链接、
非普通文件、owner/mode 不匹配、超限内容、格式错误的 PEM、尚未生效或已过期的设备证书，
以及与私钥不匹配的设备证书。Connector manifest 拒绝未知 JSON 字段，只接受无 URL
credential、query、fragment 或非根路径的 HTTPS Relay endpoint。

设备 CA 与 Relay server CA 属于不同信任域：Relay 使用设备 CA 验证客户端设备证书；此
host 使用 `relay-server-ca.pem` 验证 Relay server。部署时不得把二者混作一个 bundle。

Product manifest 的每条 endpoint 必须匹配 Connector 的精确 organization 和 Workspace
scope。`credentialFile` 只能是 `secrets/` 内的 basename。不存在 scope 的 owner 返回固定的
不可用响应，不会选择 public endpoint 或替代 credential。bearer 文件不含末尾换行符。

ACA Key Vault volume 项可能是符号链接，也可能具有不符合策略的权限。启动 host 前，应先把
需要的内容复制到服务身份私有目录，并验证 owner、权限、文件类型和内容；不能把挂载卷路径
直接交给 loader。

## 身份、请求处理与失败行为

host 从 `connector.json` 精确读取 organization、Workspace 和 device ID，并自行创建 role 为
`WorkspaceConnector`、不含 user、session credential 为空的 `RelayHello`。Relay 必须通过已批准
设备注册表验证客户端证书 fingerprint 与 scope。Connector 无法证明 Relay 是否连着实时注册表；
未配置持久注册表的 Relay 必须拒绝连接。Relay 注册表启动、数据库状态及已批准设备记录都是
独立部署 gate。

Product API request 由绑定 `ProductHttpApiAdapter` 的 `WorkspaceControlPlane` 处理。adapter
只会把类型化 Product 操作发往 manifest 选定的私有 owner endpoint 和 credential。独立的
`StartOperation` / `GetOperation` dispatcher 在 Product authority 集成完成前保持不可用。此 host
不加载或转发 Frontend bearer token 与 BFF workload credential。

缺少私有文件、TLS 材料无效或 Product manifest scope 与当前 Workspace 不一致时，进程启动失败。
Relay 拒绝或 session 中断时，进程以 1 至 30 秒有界退避重连。重连期间不报告 readiness，也不开放
可能被误认为 readiness 的 health listener。关闭时会刷新日志。

生产流量前，部署评审必须验证私有 Relay 路由、精确 Relay server CA/name、Relay 的设备 CA 与持久
注册表配置、当前证书对应的批准 fingerprint/scope、至各 Product owner 的私网可达性，以及 Product
owner 的 credential 校验。模板不声称这些线上事实已通过。

仅构建镜像的 workflow 与 ACA 评审模板 gate 见
[`workspace-connector.md`](../../../infrastructure/azure/container-apps/workspace-connector.md)。
