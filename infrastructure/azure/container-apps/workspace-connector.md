# Workspace Connector Container App Profile

`workspace-connector.internal.review.yaml` is a review-only template for one
outbound Workspace Connector. Do not apply it or send traffic from it. The
Connector image workflow builds a GHCR-named image locally in Actions but does
not log in to GHCR, publish an image, write Azure resources, or enable
production. The image digest in the template must come from a separately
approved publishing step.

The Connector has no inbound API or probe listener, so the template has no
ingress and no synthetic health/readiness probes. A running process is not
evidence that Relay has approved the device certificate or that Product APIs
are reachable. The host retries Relay sessions with bounded backoff; Product
reachability still needs separate deployment evidence.

## Secret staging and identity domains

ACA Key Vault secret volumes may expose files as symlinks and do not provide
the exact owner/mode policy required by the host. The image entrypoint runs as
root only long enough to copy the fixed input filenames into
`/run/cyrene/workspace-connector` and its `secrets/` directory. It creates both
directories with mode `0700`, writes staged files with mode `0600`, assigns
them to UID/GID `10001`, and then drops privileges before starting the host.
The host reopens and validates the files; a failed copy, invalid manifest,
missing credential, bad CA, or mismatched/expired device identity stops the
process. No secret is placed in an environment variable or build argument.

The TLS files have separate roles:

| Material | Validates |
| --- | --- |
| `relay-server-ca.pem` | Relay's server certificate and configured server name |
| `device-enrollment-cert.pem` + `device-enrollment-key.pem` | Connector's outbound mTLS client identity |
| Relay-side device CA | The Connector client certificate; configured only in the Relay deployment |
| Relay-side durable device registry | Certificate fingerprint, approval state, organization, Workspace, and device binding |

The Connector manifest fixes the Relay endpoint and SNI plus one exact
organization, Workspace, and device identity. The Product endpoint manifest
must scope every configured owner to that same organization and Workspace.
Product bearer credentials are staged only as owner files. Frontend user
tokens and BFF credentials are not mounted.

The current `ProductHttpClient` sends HTTPS requests with scoped bearer
credentials. It has no Product client-certificate or additional private-root
configuration. If an owning Product requires mTLS or a server CA outside the
client's supported root set, this profile remains blocked until a typed Product
TLS configuration seam is implemented. Relay mTLS credentials do not configure
Product HTTP.

## Private networking and deployment gates

Before deployment review, prove that this app can resolve and privately reach
the configured Relay endpoint and every Product endpoint named in its
manifest. The routing must stay within approved private service paths and must
not rely on public Product ingress, proxy variables, or cross-environment ACA
internal DNS assumptions. Keep the Connector with the relevant Relay and
Product services in a supported network layout, or document equivalent VNet,
DNS, routing, and TLS evidence.

Production remains blocked until all of these facts are verified:

1. The Relay trusts the configured device issuer and uses the durable approved
   device registry for every `WorkspaceConnector` session. Missing registry
   configuration must continue to reject every Connector.
2. The exact current device certificate is approved in that registry for the
   manifest's organization, Workspace, and device IDs; the private key matches
   the certificate and has not expired.
3. The mounted Relay CA and exact SNI verify the Relay server. The Relay has a
   separate client-CA trust path for device certificates.
4. The staged Product manifest contains the required exact scopes, and every
   configured Product owner accepts the matching service credential.
5. Relay and Product private DNS and routing work from the chosen ACA
   environment. If Product mTLS or a private trust root is required, add and
   review that explicit adapter seam before deployment.
6. The image is built from the reviewed source SHA, receives an approved
   immutable GHCR digest through a publishing workflow, and passes the
   deployment's provenance and security gates.

The first release permits Relay to read Workspace API payloads; end-to-end
encryption is not a deployment precondition.

## Workspace Connector ACA Profile 中文说明

`workspace-connector.internal.review.yaml` 是单个出站 Workspace Connector 的评审模板，不能
直接应用，也不能据此切换流量。镜像工作流会在 Actions 中构建命名为 GHCR image 的本地镜像，
但不会登录 GHCR、发布镜像、写入 Azure 资源或启用生产。模板中的 image digest 必须来自单独批准的
发布步骤。

Connector 没有入站 API 或 probe listener，因此模板不设置 ingress，也不伪造 health/readiness
probe。进程正在运行不能证明 Relay 已批准设备证书，也不能证明 Product API 可达。host 会以有界退避
重连 Relay；Product 可达性仍需单独部署证据。

ACA Key Vault secret volume 可能把文件呈现为符号链接，且不保证 host 所需 owner/mode。镜像入口只在
启动阶段以 root 身份把固定输入文件复制到 `/run/cyrene/workspace-connector` 及 `secrets/`，创建
`0700` 目录、写入 `0600` 文件、赋予 UID/GID `10001`，然后降权启动 host。host 会重新打开并校验这些
文件；复制失败、manifest 无效、credential 缺失、CA 错误或证书过期/密钥不匹配都会停止进程。secret
不会进入环境变量或 build argument。

Relay server CA 用于验证 Relay server 和精确 server name；设备证书与私钥用于 Connector 出站 mTLS。
Relay 端还必须独立配置设备 CA 和持久设备注册表，校验已批准证书 fingerprint 与 organization、
Workspace、device 绑定。这些 Relay 端配置不能由 Connector 进程证明。

当前 `ProductHttpClient` 通过作用域 bearer credential 发送 HTTPS request，不支持 Product client
certificate 或额外私有 root。如果某 Product 要求 mTLS 或客户端 root 集合之外的 server CA，本 profile
必须先阻塞，直到增加并评审明确的 Product TLS configuration seam。Relay mTLS credential 不会配置
Product HTTP。

部署评审前必须证明此 ACA app 能私有解析并访问 manifest 中的 Relay 与每个 Product endpoint。路由必须
沿批准的私有 service path，不依赖 public Product ingress、proxy 环境变量或跨 ACA environment 内部 DNS
假设。没有持久 registry、当前批准的设备证书、正确 Relay CA/SNI、精确 Product scope/credential、私网
DNS/routing 及不可变镜像发布证据时，不得进入生产。首个版本允许 Relay 读取 Workspace API payload；
不要求端到端加密。
