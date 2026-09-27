# Distributed Workspace Fabric v1

Status: **V1 direct/runtime/artifact reference path implemented; positive Workspace Connector over Relay is not configured**

Scope: identity-based Workspace discovery, transport-neutral connection
descriptors, private `LAN_DIRECT` access, Relay Frontend discovery, and a stable
frontend-to-Workspace API projection.

Non-goals: production IAM, a VPN, direct NAT traversal, a global relay fleet,
multi-region Workspace authority, or a second execution or Artifact system.

状态：**v1 直连、Runtime 与 Artifact 参考链路已实现；正向 Relay Connector 路径等待 trust 接线**。
本文约束基于身份的 Workspace 发现、transport-neutral 连接描述符、直连与 Relay 连接，以及稳定的
frontend-to-Workspace API 投影；不实现生产 IAM、VPN、NAT 打洞、全球 Relay 集群或第二套 Execution/Artifact 系统。

## 1. Boundary and authority / 边界与权威

The Account/Directory plane answers **who may discover which Workspace and
how it may currently be reached**. The Workspace Control Plane owns Workspace
authority and is the stable frontend-facing API for Product projections. Each
Product service retains authority over its domain records behind that API.
Relay is authenticated transport; it keeps only live routing registrations and
cannot reconstruct, mutate, or reconcile an Operation.

A frontend authenticates with organization-scoped user claims before it knows
a Workspace ID. Membership-filtered discovery returns the Workspace identity;
only then does the frontend address a Workspace API request. A credential may
optionally narrow later requests to one Workspace, but discovery never requires
the client to supply the target Workspace in advance.

Account/Directory 平面只回答“某身份可发现哪些 Workspace，以及当前有哪些连接方式”。
Workspace Control Plane 持有 Workspace 权威，并提供面向前端的稳定 Product API 投影；
各 Product 服务仍持有各自领域记录的权威。Relay 只是认证传输；它仅保存当前连接的
临时路由，不能重建、修改或协调 Operation。

| Component | Owns | Must not own |
| --- | --- | --- |
| Identity provider | `UserIdentity` authentication | Workspace, Product, execution, or Artifact state |
| Account/Directory | Organization membership, Workspace membership, enrollment metadata, expiring connection descriptors | Dataset, TrainingRun, ModelVersion, EvaluationRun, Deployment, Artifact contents, or Operation state |
| Workspace Control Plane | Workspace/Product requests and stable API views | User credential issuance or transport routing |
| Relay/Rendezvous | Authenticated live session routing | Workspace state, execution reconciliation, Artifact bytes, or identity derivation |
| Execution Fabric | Existing Node/Runtime/Lease/Fence/Operation execution semantics | Product semantics or user sessions |
| Artifact Plane | Existing `ArtifactRef`, manifests, replicas, plans, tickets, and transfer checkpoints | Workspace membership or Product authority |

This design adds no Lease, Event, Capability, Artifact identity, Runtime, or
Product authority. It consumes `cy-execution-fabric`, `cy-runtime-agent`,
`cy-artifact-transfer`, and the frozen semantic `Identity` contract.

本设计不新增 Lease、Event、Capability、Artifact identity、Runtime 或 Product
authority；它直接消费既有 Execution Fabric、Runtime Agent、Artifact transfer 和
frozen semantic `Identity`。

## 2. Identity model / 身份模型

Three credential domains remain separate:

1. **User identity** is an issuer/subject pair used for login, membership, and
   Workspace discovery.
2. **Node/device identity** enrolls a Workspace connector or Execution Node.
   A Workspace device session is not a user session, and `DeviceEnrollmentRef`
   is not a Fabric `NodeId`.
3. **Workload identity** remains a short-lived Execution Fabric delegation for
   one Runtime generation.

Runtime containers receive neither long-lived user credentials nor the
frontend relay session credential. The development verifier accepts opaque,
pre-registered, expiring session credentials only as a replaceable MVP seam.
Production implementations can replace it with OIDC, OAuth Device
Authorization Grant, enterprise SSO, device approval, and short-lived signed
credentials without changing the Workspace API.

User、Node/device 与 Workload 三类身份互不替代。Runtime 容器不得获得长期用户凭据
或 frontend 会话凭据。开发 verifier 只是可替换 seam；未来接入 OIDC、OAuth Device
Authorization Grant、企业 SSO 与设备审批时无需改变 Workspace API。

No identity is derived from an IP address, port, Docker address, container ID,
Tailscale address, relay session ID, or local filesystem path.

## 3. Workspace connection descriptor / Workspace 连接描述符

`WorkspaceConnectionDescriptor` is an expiring Directory response keyed by
stable `workspace_id` and `organization_id`. It contains ordered connection
candidates, not execution placement:

| Mode | Contract status | v1 implementation |
| --- | --- | --- |
| `LOCAL` | supported | in-process `LocalWorkspaceClient` |
| `LAN_DIRECT` | supported | private mTLS reference fixture; production topology remains deployment configuration |
| `DIRECT` | extension seam | not implemented |
| `OVERLAY` | extension seam | future Tailscale/private-network provider |
| `RELAY` | supported | Frontend discovery; Connector sessions fail closed until trust providers are composed |

Each candidate carries a provider ID, URI, TLS server name, priority, and
opaque routing hint. Those fields are transport inputs only. The descriptor
does not expose a Docker address, Runtime Agent address, Artifact peer, or
local path, and its API is not structurally tied to Relay.

`WorkspaceConnectionDescriptor` 是带过期时间的 Directory 响应，以稳定
Workspace/Organization identity 为键。candidate 中的 provider、URI、TLS server
name、priority 与 opaque routing hint 只属于连接层，不得成为 Workspace/Node
identity，也不得暴露 Docker、Runtime Agent、Artifact Peer 或本地路径。

## 4. Relay-first reference path / Relay-first 参考链路

```text
Remote Navigator/Web/CLI-like client
    | user session: discover Workspace by membership
    v
Account/Directory -> WorkspaceConnectionDescriptor { LOCAL, LAN_DIRECT, RELAY }
    | frontend outbound mTLS + short-lived user session
    v
Relay/Rendezvous (live routing only)
    ^ Workspace connector outbound mTLS + device session
    |
Workspace Control Plane / stable Workspace API
    | canonical Operation + ArtifactRef
    v
existing Execution Fabric -> container Runtime Agent -> fake workload
                               |
                               +-> existing Artifact Transfer Plan/HTTP Range/CAS publish
```

The Workspace connector initiates the connection, so the Workspace host needs
no inbound port and the user enters no host IP, port, Docker address, overlay
address, or container ID. Frontends call only the Workspace API. They never
call Runtime Agent, Docker, Kernel internals, or Artifact local storage.

Workspace Connector 主动建立出站 mTLS，因此 Workspace host 不需要入站端口，用户
也不输入 IP、端口、Docker/Tailscale 地址或 container ID。Frontend 只调用
Workspace API，不直接调用 Runtime Agent、Docker、Kernel 内部接口或 Artifact 本地
存储。

The diagram describes the target Relay route. The current reference host accepts
Frontend discovery and rejects Workspace Connector sessions while peer certificate
validation, current revocation evidence, and an active Registry binding with dispatch
fence are unavailable. The executable proof verifies this denial and uses `LAN_DIRECT`
for its positive Workspace API, Runtime Agent, and Artifact path. The Relay trace names
the Workspace-device peer validator, signed current revocation, current Registry binding
validation, and dispatch fence as `NOT_CONFIGURED`. TLS client-certificate trust remains
configured. This Connector attempt is denied at the first missing application-level
validator gate, so the proof does not claim to exercise later gates independently.

此图描述目标 Relay 路径。当前参考 host 支持 Frontend discovery；在 peer certificate
validation、当前撤销证据、active Registry binding 和 dispatch fence 未接通时，会拒绝
Workspace Connector session。Relay trace 会将 Workspace-device peer validator、签名的当前
撤销证据、当前 Registry binding validation 和 dispatch fence 明确记录为 `NOT_CONFIGURED`。
TLS client-certificate trust 仍然配置。本次 Connector 尝试在首个缺失的应用层 validator
gate 被拒绝，因此 proof 不宣称单独执行了后续 gate。可执行 proof 通过 `LAN_DIRECT` 验收
正向的 Workspace API、Runtime Agent 与 Artifact 链路。

## 5. Relay failure semantics / Relay 故障语义

A relay connection and its `relay_session_id` are disposable transport state.
The following are target failure semantics after the Workspace Connector has
passed configured device authentication. The current reference fixture rejects
the Connector before a session is established and checks that it remains denied
after Relay restart; it does not claim to prove Connector reconnection.

When Relay disappears:

1. the Workspace connector observes stream closure;
2. it reconnects with the same enrolled device identity and a fresh relay
   session;
3. frontend requests may retry through a newly discovered/current candidate;
4. the Workspace Control Plane keeps the same Operation identity, Artifact
   references, and authority instance; Product owners retain their state;
5. Execution Fabric reconciliation continues independently through its
   existing Node/Runtime control stream and Lease/Fence rules.

Relay 重启不会提升 Runtime generation，也不会创建新 Operation、Artifact 或
Workspace authority。Relay 不能把连接丢失解释为执行丢失；执行状态仍由既有
Execution Fabric 证据与 Lease/Fence 协调。

## 6. Reference API and vertical / 参考 API 与纵向证明

The v1 Workspace API intentionally exposes only:

- stable Workspace and semantic Operation identity;
- Product-facing Operation state and progress;
- Artifact URIs and stable resource references;
- an authority-instance marker used by the acceptance fixture to verify that
  private direct access reaches the same Workspace authority.

The `FileWorkspaceApi` and opaque development credentials are acceptance-only
fixtures, not production stores or IAM. The current proof discovers the Workspace
through an authenticated Frontend Relay session, verifies that the Relay denies
the unconfigured Workspace Connector at its application-level device validator,
then sends the start request over private `LAN_DIRECT`. It releases an existing
Execution Fabric assignment, starts the existing Runtime Agent in an ordinary unprivileged container, downloads an
existing digest-addressed Artifact through the existing range transfer, and
reports progress to the same logical Operation. It also confirms that an offline
Relay Connector is not treated as a successful fallback and that direct Workspace
authority remains available after Relay stops.

Positive Workspace Connector traffic through Relay remains unimplemented. It
requires a device certificate issuer and validator, signed current CRL/OCSP
revocation evidence, and an active current Registry binding with a dispatch fence.

v1 API 只暴露稳定 Workspace/Operation identity、面向产品的状态/进度、Artifact URI
与资源引用。`FileWorkspaceApi` 和开发凭据仅用于 acceptance，不是生产状态存储或
IAM。当前 proof 通过已认证的 Frontend Relay session 发现 Workspace，确认 Relay 在应用层
device validator 拒绝尚未配置 trust providers 的 Connector，然后通过私网 `LAN_DIRECT`
发送 start request。Relay trace 会将 Workspace-device peer validator、当前撤销证据、当前
Registry binding validation 和 dispatch fence 记录为 `NOT_CONFIGURED`；TLS client-certificate
trust 仍然配置，proof 不宣称单独执行了后续 gate。它释放现有
Execution Fabric assignment，在普通无特权 container 中启动现有
Runtime Agent，通过现有 range transfer 下载 digest-addressed Artifact，并向同一个
Operation 报告进度；同时验证 Connector 离线时 Relay 不会假装回退成功，以及 Relay 停止后
Workspace 仍可通过直连访问。

Workspace Connector 经 Relay 的正向流量尚未实现。它需要设备证书签发与验证器、已签名且
当前的 CRL/OCSP 撤销证据，以及包含 dispatch fence 的 active current Registry binding。

Run the release evidence:

```bash
bash tooling/ci/check-distributed-workspace-fabric.sh
bash tooling/acceptance/distributed-workspace-fabric/run-workspace-fail-closed-proof.sh
```

The Docker proof is mandatory for release acceptance. A Docker daemon or WSL
transport outage is a blocker, never a skipped or mocked pass.

## 7. Extension seams and non-goals / 扩展 seam 与非目标

`ConnectivityProvider`, `WorkspaceDirectory`, `RelayAuthenticator`, and
`WorkspaceApi` are the deliberate seams for future direct, overlay, QUIC,
cloud-private, OIDC, and durable Workspace implementations. V1 does not add
WireGuard, STUN, ICE, hole punching, a native mesh, a global P2P/CDN scheduler,
or a production global IAM/control plane.

这些 seam 只避免未来锁定，不提前实现 WireGuard、STUN、ICE、NAT hole punching、
native mesh、全球 P2P/CDN scheduler 或生产级全球 IAM/control plane。
---

<!-- Chinese Translation / 中文翻译 -->

# Distributed Workspace Fabric v1

状态：**v1 参考纵向已实现**

范围：基于身份的 Workspace discovery、与 transport 无关的 connection descriptor、\`LOCAL\) 和应用层 \`RELAY\) connectivity，以及稳定的 frontend-to-Workspace API projection。

非目标：生产 IAM、VPN、直接 NAT traversal、全球 Relay fleet、多区域 Workspace authority，或第二套 execution/Artifact 系统。

## 1. 边界与权威

Account/Directory 平面回答：**谁能发现哪些 Workspace，以及当前可以通过什么方式访问它们**。Workspace Control Plane 持有 Workspace 权威，并提供面向前端的稳定 Product API 投影；各 Product 服务仍持有各自领域记录的权威。Relay 是认证传输，只保留活动路由 registration，不能重建、修改或 reconciliation 一个 Operation。

在知道 Workspace ID 之前，frontend 使用组织作用域的 user claim 完成认证。按成员关系过滤的 discovery 会返回 Workspace identity；之后 frontend 才能向 Workspace API 发送请求。credential 可以选择将后续 request 限定到一个 Workspace，但 discovery 不要求 client 预先提供目标 Workspace。

| 组件 | 拥有的职责 | 不得拥有的内容 |
|---|---|---|
| Identity provider | \`UserIdentity\` authentication | Workspace、Product、execution 或 Artifact 状态 |
| Account/Directory | 组织成员关系、Workspace 成员关系、enrollment metadata、有过期时间的 connection descriptor | Dataset、TrainingRun、ModelVersion、EvaluationRun、Deployment、Artifact 内容或 Operation 状态 |
| Workspace Control Plane | Workspace/Product request 与稳定 API view | User credential 签发或 transport 路由 |
| Relay/Rendezvous | 经过认证的实时 session 路由 | Workspace 状态、execution reconciliation、Artifact 字节或 identity 推导 |
| Execution Fabric | 既有 Node/Runtime/Lease/Fence/Operation 执行语义 | Product 语义或 user session |
| Artifact Plane | 既有 \`ArtifactRef\`、manifest、replica、plan、ticket 和 transfer checkpoint | Workspace 成员关系或 Product authority |

本设计不新增 Lease、Event、Capability、Artifact identity、Runtime 或 Product authority。它消费现有的 \`cy-execution-fabric\`、\`cy-runtime-agent\)、\`cy-artifact-transfer\) 和冻结的 semantic \`Identity\` contract。

## 2. 身份模型

三类 credential domain 彼此独立：

1. **User identity** 是 issuer/subject pair，用于登录、成员关系和 Workspace discovery。
2. **Node/device identity** 用于注册 Workspace connector 或 Execution Node。Workspace device session 不是 user session；\`DeviceEnrollmentRef\) 也不是 Fabric \`NodeId\)。
3. **Workload identity** 仍是 Execution Fabric 对单个 Runtime generation 的短时委派。

Runtime container 不接收长期 user credential，也不接收 frontend Relay session credential。开发用 verifier 只将不透明、预先登记、会过期的 session credential 作为可替换的 MVP seam。生产实现可以替换为 OIDC、OAuth Device Authorization Grant、企业 SSO、设备审批和短期签名 credential，无需修改 Workspace API。

任何 identity 都不能从 IP address、port、Docker address、container ID、Tailscale address、Relay session ID 或本地文件系统路径推导。

## 3. Workspace Connection Descriptor

\`WorkspaceConnectionDescriptor\` 是会过期的 Directory response，以稳定的 \`workspace_id\) 和 \`organization_id\) 为 key。它包含按顺序排列的 connection candidate，不表示 execution placement：

| 模式 | 契约状态 | v1 实现 |
|---|---|---|
| \`LOCAL\` | 支持 | 进程内 \`LocalWorkspaceClient\` |
| \`LAN_DIRECT\` | 支持 | 私网 mTLS 参考 fixture；生产网络拓扑仍由部署配置提供 |
| \`DIRECT\` | 扩展 seam | 未实现 |
| \`OVERLAY\` | 扩展 seam | 未来的 Tailscale/private-network provider |
| \`RELAY\` | 支持 | Frontend discovery 可用；Connector trust providers 接通前保持 fail-closed |

每个 candidate 包含 provider ID、URI、TLS server name、priority 和不透明 routing hint。这些字段只是 transport 输入。Descriptor 不暴露 Docker address、Runtime Agent address、Artifact peer 或本地路径；它的 API 结构也不绑定到 Relay。

## 4. Relay-first 参考路径

\`\`\`text
Remote Navigator/Web/CLI-like client
    | user session：根据成员关系发现 Workspace
    v
Account/Directory -> WorkspaceConnectionDescriptor { LOCAL, LAN_DIRECT, RELAY }
    | frontend 出站 mTLS + 短时 user session
    v
Relay/Rendezvous（仅实时路由）
    ^ Workspace connector 出站 mTLS + device session
    |
Workspace Control Plane / stable Workspace API
    | canonical Operation + ArtifactRef
    v
现有 Execution Fabric -> container Runtime Agent -> fake workload
                              |
                              +-> 现有 Artifact Transfer Plan/HTTP Range/CAS publish
\`\`\`

Workspace connector 主动建立连接，因此 Workspace host 无需开放入站端口；用户也不需要输入 host IP、port、Docker address、overlay address 或 container ID。Frontend 只调用 Workspace API，不调用 Runtime Agent、Docker、Kernel 内部接口或 Artifact 本地存储。

## 5. Relay 故障语义

以下是 Workspace Connector 通过已配置的 device authentication 后的目标故障语义。当前
reference fixture 会在建立 Connector session 前拒绝连接，并确认 Relay 重启后仍然拒绝；
它不宣称证明了 Connector 重连。

Relay connection 及其 \`relay_session_id\) 是可丢弃的 transport state。Relay 消失后：

1. Workspace connector 观察到 stream 已关闭；
2. connector 使用相同的已注册 device identity 和新的 Relay session 重新连接；
3. frontend request 可以通过重新发现或当前有效的 candidate 重试；
4. Workspace Control Plane 保留相同的 Operation identity、Artifact 引用和 authority instance；各 Product owner 保留其状态；
5. Execution Fabric 通过原有 Node/Runtime control stream 和 Lease/Fence 规则独立继续 reconciliation。

Relay 重启不会提升 Runtime generation，也不会创建新的 Operation、Artifact 或 Workspace authority。Relay 不能把连接丢失解释为执行丢失；执行状态仍由既有 Execution Fabric 证据和 Lease/Fence 协调。

## 6. 参考 API 与纵向证明

v1 Workspace API 仅公开：

- 稳定的 Workspace 和 semantic Operation identity；
- 面向 Product 的 Operation 状态和进度；
- Artifact URI 和稳定 resource reference；
- authority-instance 标记；acceptance fixture 用它验证私网直连仍访问同一 Workspace authority。

\`FileWorkspaceApi\` 和不透明开发 credential 仅用于 acceptance fixture，不是生产 store 或 IAM。当前 proof 通过已认证的 Frontend Relay session 发现 Workspace，确认 Relay 会拒绝尚未配置 Workspace trust providers 的 Connector，然后通过私网 \`LAN_DIRECT\` 发送 start request。它释放现有 Execution Fabric assignment，在普通无特权 container 中启动现有 Runtime Agent，通过现有 range transfer 下载 digest-addressed Artifact，并向同一个 Operation 报告进度；同时验证 Connector 离线时 Relay 不会假装回退成功，以及 Relay 停止后 Workspace 仍可通过直连访问。

Workspace Connector 经 Relay 的正向流量尚未实现。它需要设备证书签发与验证器、已签名且当前的 CRL/OCSP 撤销证据，以及包含 dispatch fence 的 active current Registry binding。

运行发布证据：

\`\`\`bash
bash tooling/ci/check-distributed-workspace-fabric.sh
bash tooling/acceptance/distributed-workspace-fabric/run-workspace-fail-closed-proof.sh
\`\`\`

发布验收必须执行 Docker proof。Docker daemon 或 WSL transport 故障是 blocker，不能跳过或 mock pass。

## 7. 扩展 seam 与非目标

\`ConnectivityProvider\`、\`WorkspaceDirectory\`、\`RelayAuthenticator\) 和 \`WorkspaceApi\` 是为未来 direct、overlay、QUIC、cloud-private、OIDC 和持久化 Workspace 实现预留的 seam。v1 不增加 WireGuard、STUN、ICE、hole punching、native mesh、全球 P2P/CDN scheduler 或生产级全球 IAM/control plane。

这些 seam 只是避免未来被某种实现锁定，并不代表现在实现了 WireGuard、STUN、ICE、NAT hole punching、native mesh、全球 P2P/CDN scheduler 或生产级全球 IAM/control plane。
