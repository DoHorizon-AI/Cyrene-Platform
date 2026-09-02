# Distributed Workspace Fabric v1

Status: **V1 REFERENCE VERTICAL IMPLEMENTED**

Scope: identity-based Workspace discovery, transport-neutral connection
descriptors, `LOCAL` and application-level `RELAY` connectivity, and a stable
frontend-to-Workspace API projection.

Non-goals: production IAM, a VPN, direct NAT traversal, a global relay fleet,
multi-region Workspace authority, or a second execution or Artifact system.

状态：**v1 reference vertical 已实现**。本文约束基于身份的 Workspace 发现、
transport-neutral 连接描述符、`LOCAL`/应用层 `RELAY` 连接，以及稳定的
frontend-to-Workspace API 投影；不实现生产 IAM、VPN、NAT 打洞、全球 Relay 集群或
第二套 Execution/Artifact 系统。

## 1. Boundary and authority / 边界与权威

The Account/Directory plane answers **who may discover which Workspace and
how it may currently be reached**. The Workspace Control Plane remains the
only owner of Workspace and Product state. Relay is authenticated transport;
it keeps only live routing registrations and cannot reconstruct, mutate, or
reconcile an Operation.

A frontend authenticates with organization-scoped user claims before it knows
a Workspace ID. Membership-filtered discovery returns the Workspace identity;
only then does the frontend address a Workspace API request. A credential may
optionally narrow later requests to one Workspace, but discovery never requires
the client to supply the target Workspace in advance.

Account/Directory 平面只回答“某身份可发现哪些 Workspace，以及当前有哪些连接方式”。
Workspace Control Plane 仍是 Workspace/Product 状态的唯一权威。Relay 只是认证传输；
它仅保存当前连接的临时路由，不能重建、修改或协调 Operation。

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
| `LAN_DIRECT` | extension seam | not implemented |
| `DIRECT` | extension seam | not implemented |
| `OVERLAY` | extension seam | future Tailscale/private-network provider |
| `RELAY` | supported | outbound mTLS application relay |

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
Account/Directory -> WorkspaceConnectionDescriptor { LOCAL, RELAY }
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

## 5. Relay failure semantics / Relay 故障语义

A relay connection and its `relay_session_id` are disposable transport state.
When Relay disappears:

1. the Workspace connector observes stream closure;
2. it reconnects with the same enrolled device identity and a fresh relay
   session;
3. frontend requests may retry through a newly discovered/current candidate;
4. the Workspace Control Plane keeps the same Operation identity, Product
   state, Artifact references, and authority instance;
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
- an authority-instance marker used by the acceptance fixture to prove Relay
  restart did not replace Workspace authority.

The `FileWorkspaceApi` and opaque development credentials are acceptance-only
fixtures, not production stores or IAM. The real vertical sends a remote start
request through Relay, releases an existing Execution Fabric assignment,
starts the existing Runtime Agent in an ordinary unprivileged container,
downloads an existing digest-addressed Artifact through the existing range
transfer, and reports progress to the same logical Operation. It then breaks
and restarts Relay and proves the same Workspace authority and Operation remain
observable.

v1 API 只暴露稳定 Workspace/Operation identity、面向产品的状态/进度、Artifact URI
与资源引用。`FileWorkspaceApi` 和开发凭据仅用于 acceptance，不是生产状态存储或
IAM。

Run the release evidence:

```bash
bash tooling/ci/check-distributed-workspace-fabric.sh
bash tooling/acceptance/distributed-workspace-fabric/run-relay-proof.sh
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
