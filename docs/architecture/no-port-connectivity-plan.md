# No-Port Connectivity Program Plan

Status: **PROPOSED** — planning document, not an implementation record.

Scope: account and device-code identity, lowest-latency in-cluster connectivity,
relay connectivity where direct is impossible, Workspace API projection, direct
Artifact transfer, and the logging error-path close-out that must land first.

This plan builds on `distributed-workspace-fabric-v1.md`, which is already
status **V1 REFERENCE VERTICAL IMPLEMENTED**. It does not propose a second
connectivity system.

---

## 1. Goal and constraints

**Goal.** Services and clients interconnect without inbound ports and without
anyone typing an address, port, container ID, or overlay IP. Signing in to the
same account is the only step required for two devices to reach each other.

**Constraints agreed for this program:**

1. **Lowest latency in-cluster.** Cluster-internal traffic must not hairpin
   through a relay. Use `LAN_DIRECT` inside the cluster; reserve `RELAY` for
   cases where no direct path exists.
2. **Direct and object-store transfer for large artifacts.** Model checkpoints, datasets,
   and other bulk payloads must prefer direct `LAN_DIRECT` paths over the relay.
   When peers reside in separate private networks across NAT, transfer routes via
   user-provided cloud object storage (Cloudflare R2, Azure Blob, S3, or Hugging Face)
   using pre-signed URLs. If direct connection is unavailable and no cloud storage is configured,
   the system fails closed with an explicit configuration prompt, never routing bulk payloads through the relay.
3. **Frictionless identity over token copying.** Manual bearer token copying is eliminated as the
   primary onboarding mechanism. Headless nodes and server containers enroll by requesting
   membership, which trusted devices (browser/client) approve via out-of-band push and
   biometric verification (WebAuthn / Passkeys / Touch ID / Face ID).
4. **Portless boundaries.** Eliminating ports applies strictly to public external ingress ports
   and public NAT pinholes. Container-internal HTTP `/healthz` endpoints and loopback listeners
   remain active for platform orchestrators (Azure Container Apps, Kubernetes) to run liveness and
   readiness probes.
5. **No backward compatibility.** There are no users yet and the first users are
   the project itself. The existing `CYRENE_*_URL` peer-addressing model is
   deleted, not deprecated-and-kept.

**Non-goals for this program:**
- Multi-region Workspace authority and a global relay fleet. Those stay out until in-cluster and edge paths are proven.
- Offline execution of giant foundational models on portable consumer laptops. Large model training and inference target server-grade GPU nodes, connected cloud runtimes, or high-memory workstations.

---

## 2. Current state (verified)

### 2.1 Inter-service connectivity

Each service binds and exposes its own port; peers learn about each other through
environment variables.

| Fact | Evidence |
| --- | --- |
| Catalyst exposes `8014`; Exchange `8000`; Reactor `19300` + `8000` | respective `Dockerfile` `EXPOSE` lines |
| Peer addresses come from env vars, e.g. `CYRENE_YIELD_URL` | `Cyrene-Catalyst/src/cyrene_catalyst/__main__.py:29` |
| Some service-to-service auth already uses bearer tokens | `Cyrene-Reactor` `/data/credentials/control.token` health probe |
| Client reaches services through a loopback-only local bridge | `Cyrene-Client/tooling/server-control.ts:22` |

### 2.2 Platform fabric (the foundation)

`Cyrene-Platform/framework/crates/cy-workspace-fabric` (1284 lines) implements:

- `directory.rs` — account membership and Workspace discovery (`WorkspaceMembership`).
- `transport.rs` — outbound mTLS relay clients; `RelaySession::discover` / `execute` / `serve_workspace`.
- `relay.rs` — stateless application-level gRPC relay (`WorkspaceRelayService`).
- `auth.rs` — short-lived relay sessions; `SessionPrincipal::WorkspaceDevice { workspace_id, device_id }` and a replaceable `DevelopmentSessionVerifier`.

Supporting facts:

- `ConnectivityMode` already models `LOCAL`, `LAN_DIRECT`, `DIRECT`, `OVERLAY`, `RELAY`
  (`contracts/proto/cyrene/core/v1/node_control.proto:86`), but `validate_route`
  currently admits only `LOCAL` and `RELAY`.
- `WorkspaceConnectionDescriptor` carries **ordered** `WorkspaceConnectionCandidate`
  entries (`contracts/proto/cyrene/workspace/v1/workspace_fabric.proto:51`), which is
  the existing mechanism for "prefer the best path, fall back".
- `RelayHello` already carries a `DeviceEnrollmentRef device`
  (`workspace_fabric.proto:68`).
- **Gap:** nothing consumes the fabric except its own fixture binary. No product
  service is a Workspace connector; there is no production relay host; no real IAM.

### 2.3 Artifact plane

`Cyrene-Platform/sdk/rust/cy-artifact-transfer` already models peer-to-peer
distribution:

- `ArtifactPeer` / `ArtifactPeerKind` — includes `DesktopCache`, `NodeCache`, `CentralSeed`, `RegionalCache`, `ObjectStoreGateway`.
- `ArtifactSourceCandidate { peer, replica }` and `TransferPlan` — candidate-driven planning.
- `TransferTicket` — "Short-lived authorization for Peer-to-Peer part transfer".
- `TransferCheckpoint` — resumable transfer.
- **Gap:** `TransferProtocol` has only `HttpsRangeV1`. Peers still need a
  reachable address, which is the same NAT/port problem as §2.1.

### 2.4 Logging

Structured NDJSON logging and W3C traceparent propagation exist in every Python
service and in the client. The gap is a small set of silent swallows, listed in
Phase 0.

---

## 3. Design

### 3.1 Transport selection and cross-language proxying

Transport is chosen from the descriptor's ordered candidates, not hard-coded:

1. `LAN_DIRECT` — in-cluster, direct address reachable on the cluster network
   (ports remain private to the cluster; nothing is exposed externally).
2. `RELAY` — outbound-only mTLS for devices with no direct path. No inbound ports.

"Number of exposed ports" and "latency" therefore stop competing: portless applies
to the outside, direct applies to the inside.

Key architectural mechanics:
- **Cross-language Rust sidecar proxy.** Python services (Catalyst, Yield, Echo, Exchange)
  and non-Rust runtimes reach the Workspace Fabric through a lightweight local Rust sidecar proxy
  listening on loopback `127.0.0.1`. The sidecar transparently manages mTLS Relay sessions,
  `LAN_DIRECT` resolution, and credential cycling, preventing duplicated protocol implementations in Python.
- **Relay stream backpressure.** The Relay enforces HTTP/2 and gRPC stream window flow control
  and bounded per-stream memory buffers. During long-running LLM SSE generation and chunked token streaming,
  slow consumers trigger upstream backpressure rather than causing memory bloat on the Relay.
- **Azure Container Apps ingress boundary.** ACA HTTP ingress terminates client TLS at Envoy and
  forwards client certificate information in `X-Forwarded-Client-Cert` when certificate mode is
  enabled. The production Relay must require client certificates at ACA ingress, trust that header
  only from the ACA proxy, verify its enrolled certificate fingerprint and authorized user or
  device session,
  and reconnect streams across ACA's HTTP request timeout. The reference Relay's in-process mTLS
  listener is not an ACA deployment entrypoint. See [ACA client certificate authentication](https://learn.microsoft.com/en-us/azure/container-apps/client-certificate-authorization)
  and [ACA ingress](https://learn.microsoft.com/en-us/azure/container-apps/ingress-overview).

### 3.2 Artifact transfer and cloud storage gateway fallback

The fabric supplies identity, addressability, and authorization; the artifact
plane supplies planning, resume, and integrity.

- The Directory returns artifact **source candidates** whose reachability is
  described by the same connection-descriptor mechanism.
- When `LAN_DIRECT` is available, peers transfer bulk files directly via high-speed HTTPS Range or TCP.
- **Cross-NAT cloud storage fallback (`ObjectStoreGateway`).** When peers reside behind separate NATs
  with no direct route, Cyrene avoids brittle P2P hole-punching. Instead, transfers seamlessly route
  through user-provided cloud object storage (Cloudflare R2, Azure Blob, AWS S3, or Hugging Face) using
  temporary pre-signed URLs.
- **Fail-closed discipline.** If direct connection is unreachable across NAT and no cloud object store
  is configured, the transfer fails closed immediately with an explicit actionable message ("Please connect
  an Object Storage such as Cloudflare R2 or Azure Blob to enable cross-network model transfer"),
  never falling back to routing gigabyte payloads through the lightweight Relay.
- `TransferTicket` authorizes peer-to-peer or gateway part transfer so a peer can serve
  without holding long-lived user credentials.

### 3.3 Identity and out-of-band biometric approval

One device identity serves both connectivity and the modular installer:

- Device enrollment (`DeviceEnrollmentRef`) already exists in the relay protocol.
- **Eliminating token friction via out-of-band approval.** Rather than requiring users to manually copy
  and paste static bearer tokens into cloud containers:
  1. An unattended server or container node sends an enrollment request to the Directory upon startup.
  2. The Directory pushes an approval notification to the user's active trusted device (e.g. WebAuthn / Passkey prompt in the Client Web Console, or mobile notification).
  3. The user approves via biometric authentication (Touch ID, Face ID, Windows Hello).
  4. The Directory issues a short-lived mTLS session certificate to the waiting node.
- The installer consumes the exact same device and Workspace identity facts.

**The two must not grow separate enrolment or credential systems.**

### 3.4 Workspace API projection

The projection today exposes only `StartWorkspaceOperation` and
`GetWorkspaceOperation` (`workspace_fabric.proto:95`). Extending it to the six
product surfaces is the largest single work item in this plan and is delivered
incrementally, one minimal slice per service.

The Web Client's final product path is the Workspace API for every projected
surface. Direct service endpoints remain internal implementation paths.

The request path is fixed: the Web Client calls the stable Workspace API;
the connection descriptor selects `LAN_DIRECT` or `RELAY` to reach the same
Workspace Control Plane. That control plane owns Workspace authority and
projects Product APIs; each Product service retains authority over its own
domain records. Directory supplies identity-scoped discovery and connection
candidates; Relay only carries authenticated traffic. The current
Web-to-Product proxy routes are an interim deployment path, not the Phase 3
Client contract.

---

## 4. Phases

### Phase 0 — Logging error-path close-out

Independent; lands before any connectivity work.

| Location | Current state | Action |
| --- | --- | --- |
| `Cyrene-Reactor/runtime/core/src/cy_exec/core/server.py:133,353` | `except Exception: pass` | Emit a structured error with trace context, or narrow to a named exception with a stated reason |
| `Cyrene-Yield/training/core/src/cy_exec/training/product_service.py:773` | `except KeyError: pass` | Log or return an explicit default with a warning |
| `Cyrene-Yield/training/core/src/cy_exec/training/product_store.py:364` | `except (ValueError, TypeError): pass` | Same |
| `Cyrene-Echo/src/cyrene_echo/engine.py:323` | `except ValueError: return None` | Record the cause before returning the fallback |
| `Cyrene-Exchange/src/cyrene_exchange/gateway.py:602` | auth resolution failure → `principal = None`, cause lost | Log the cause (never the token), then raise `invalid credentials` |

Structural fix: Exchange's `_observe_rejected` depends on
`self._lifecycle_observer`; when it is `None`, rejections are silent. Make the
observer mandatory at startup (fail closed) so missing observability cannot
produce silence.

Acceptance:

1. A CI gate per repository fails on bare `except ...: pass` / silent `return None`
   outside an explicit allow-list.
2. A fault injected into each repaired path produces a log line whose `trace_id`
   matches the originating request.
3. A deep failure is traceable to the boundary log by `trace_id`.

### Phase 1 — Relay service, LAN_DIRECT, and sidecar proxy

Goal: a deployable relay with backpressure, the direct in-cluster path, and sidecar proxy for non-Rust services.

Work:

- Extract a runnable relay entrypoint from `WorkspaceRelay` (protocol unchanged); TLS, health, logging.
- Implement stream flow control backpressure and bounded memory queues on the Relay for SSE / streaming LLM responses.
- Implement a lightweight local Rust sidecar proxy on loopback `127.0.0.1` for Python services (Catalyst, Yield, Echo, Exchange) to interface with the Workspace Fabric.
- Add `LanDirectConnectivityProvider` and admit `LAN_DIRECT` in `validate_route`.
- Add a non-relay direct transport path, and emit a `LAN_DIRECT` candidate ahead of `RELAY` in descriptors.
- Persist the Directory: `InMemoryWorkspaceDirectory` → a real membership/device store.
- Replace `DevelopmentSessionVerifier` with real device credentials.
- Preserve `traceparent` across the relay hop.

Acceptance:

- `tooling/acceptance/distributed-workspace-fabric/run-relay-proof.sh` passes against the deployed relay.
- A relay restart does not change Workspace authority or Operation identity.
- In-cluster requests resolve a `LAN_DIRECT` candidate and do not transit the relay.
- Streaming responses under artificial client slowdown trigger upstream backpressure without inflating Relay memory.

### Phase 2 — Account login, device code, and out-of-band biometric approval

Goal: RFC 8628 OAuth 2.0 Device Authorization Grant with out-of-band push/biometric approval, plus cloud object store fallback.

Work:

- Device-code & out-of-band push flow: unattended nodes request membership; Directory alerts trusted client devices.
- WebAuthn / Passkey / biometric approval integration in the Client Web Console (Touch ID, Face ID, Windows Hello).
- Membership feeds discovery so same-account devices find each other.
- Cloud object storage credentials binding: configure optional Cloudflare R2, Azure Blob, AWS S3, or Hugging Face repository credentials.
- Cross-NAT artifact transfer engine: generate pre-signed upload/download URLs when direct LAN path is missing; fail closed if unconfigured.
- Keep User / Node-device / Workload identities separate; runtime containers never receive long-lived user credentials.

Acceptance:

- A new headless device enrolls through out-of-band biometric approval without any manual token copying.
- A second device on the same account discovers the target Workspace; a different account cannot (negative case required).
- Cross-NAT large artifact transfer uses configured cloud object storage; unconfigured cross-NAT transfers fail closed with clear diagnostics.
- Credential expiry re-authenticates without leaking credentials to logs.

### Phase 3 — Workspace API projection (long pole)

Goal: project the six product surfaces through the stable Workspace API.

Approach: one minimal slice per service first (a read plus an operation), then
widen. Do not attempt every endpoint up front.

Work:

- Proto definitions and TCK extension under `contracts/`.
- Implement the Workspace Control Plane as the Workspace authority and
  `WorkspaceApi` handler behind both `LAN_DIRECT` and `RELAY` connections.
  Product domain records remain with their owning services.
- Client changes to reach products only through the Workspace API.

Acceptance:

- Each service completes one projected operation end-to-end; TCK passes.
- The client no longer connects to any product port directly.
- Responses expose no Docker address, local path, or container ID.

### Phase 4 — Remove ports and the old addressing model

Work:

- Replace `CYRENE_*_URL` peer addressing with Workspace identity routing (delete, do not deprecate).
- Drop public `EXPOSE` lines in Dockerfiles and disable external ingress ports; keep container-internal HTTP `/healthz` endpoints and loopback listeners for platform orchestrator probes.
- Update deployment and documentation.

Acceptance:

- The full workflow (Catalyst → Yield → Echo → Reactor → Exchange → Navigator) runs with no published product ports.
- No `CYRENE_*_URL` peer configuration remains.
- Orchestrator health probes (`/healthz`) continue functioning cleanly inside container environments.

### Phase 5 — Hardening

Relay HA and fleet topology, progressive end-to-end encryption, relay observability,
rollback rehearsal, and load testing of direct and cloud-storage artifact transfer.

---

## 5. Decisions required

1. **End-to-end encryption. Resolved for the first release:** the self-hosted
   Relay and control plane may read projected payloads. Use authenticated TLS
   in transit and restrict access to authorized operators. Add end-to-end
   encryption incrementally in a later hardening phase.
2. **Cross-NAT bulk transfer gateway.** Resolved: Use user-provided Cloud Object Storage
   (Cloudflare R2, Azure Blob, AWS S3, Hugging Face) via temporary pre-signed URLs when `LAN_DIRECT`
   is unavailable, with strict fail-closed diagnostics if unconfigured.
3. **Headless enrollment approval.** Resolved: Discard static bearer token copying; use
   out-of-band notification and WebAuthn/Passkey biometric approval on trusted client devices.
4. **Cross-language service connectivity.** Resolved: Provide a lightweight local Rust sidecar
   proxy daemon listening on loopback `127.0.0.1` for Python services.
5. **Projection granularity. Resolved for the Client boundary:** the Web Client
   reaches products only through the Workspace API. Select each service's
   projected operations during Phase 3; other operations remain internal.

## 6. Risks

- **Phase 3 is the main risk.** A large contract surface plus a change to how the
  client calls products. Mitigate with one minimal slice per service.
- **Identity coupling.** If the fabric and the modular installer build separate
  enrolment paths, both will need rework. Land the shared device identity once
  (Phase 2) and have the installer consume it.
- **Bulk-path regression.** Routing large transfers through the relay would be a
  silent performance and cost failure. The acceptance criteria in Phase 1 and
  Phase 3 must assert the direct path is actually used, not merely available.

---

<!-- Chinese Translation / 中文翻译 -->

# 无端口互联计划

状态：**提案中** —— 规划文档，不是实施记录。

范围：账号与设备码身份、集群内最低延迟连接、无法直连时的 relay 连接、Workspace
API 投影、Artifact 直连传输，以及必须最先完成的日志错误路径收口。

本计划建立在 `distributed-workspace-fabric-v1.md` 之上，该文档状态已是
**v1 参考纵向已实现**。本计划不新建第二套连接系统。

---

## 1. 目标与约束

**目标。** 服务与客户端互联不需要入站端口，也不需要任何人输入地址、端口、容器 ID
或 overlay IP。登入同一账号是两个设备能互相访问的唯一必要步骤。

**本计划已确认的约束：**

1. **集群内最低延迟。** 集群内流量不得绕经 relay。集群内使用 `LAN_DIRECT`，
   仅在确实没有直连路径时才使用 `RELAY`。
2. **大文件直连与对象存储后备传输。** 模型 checkpoint、数据集等批量载荷必须优先走直连
   `LAN_DIRECT` 路径而非 relay。当对端位于跨 NAT 的不同私有网络时，传输通过用户自备的
   云对象存储（Cloudflare R2、Azure Blob、S3 或 Hugging Face）使用临时预签名 URL 中转。
   若直连不可达且用户未配置云存储，系统立即实施 Fail-closed（断开并阻断），弹出明确提示
   引导用户配置，绝不将大体积文件绕经轻量 relay。
3. **消除手动 Token 摩擦，采用受信任设备带外审批。** 彻底消除手动复制粘贴静态 Bearer Token 作为
   首选纳管方案。无头节点（Headless node）与服务端容器在启动时发起纳管申请，由用户的
   受信任设备（网页控制台 / 手机客户端）接收推送，并通过生物识别（WebAuthn / Passkeys /
   Touch ID / Face ID）进行带外审批与授权。
4. **无端口边界界定。** “无端口”严格针对公网外部入站端口与公网 NAT 穿透。容器内部 HTTP
   `/healthz` 健康检查端点与回环监听保持开启，供平台编排器（Azure Container Apps、
   Kubernetes）执行存活和就绪探测。
5. **不做向后兼容。** 目前没有用户，第一批用户就是项目自己。既有
   `CYRENE_*_URL` 对端寻址模型直接删除，而不是标记废弃后保留。

**本计划的非目标：**
- 多区域 Workspace 权威与全球 relay 集群。在集群内与边缘路径验证完成前不纳入。
- 便携式消费级笔记本离线运行巨大基础模型。大模型训练与推理针对服务器级 GPU 节点、云端连接的运行时或高显存工作站，不把离线轻薄本运行巨大模型纳入本阶段目标。

---

## 2. 现状（已核实）

### 2.1 服务间连接

各服务各自绑定并暴露端口；对端通过环境变量互相得知。

| 事实 | 证据 |
| --- | --- |
| Catalyst 暴露 `8014`；Exchange `8000`；Reactor `19300` + `8000` | 各自 `Dockerfile` 的 `EXPOSE` |
| 对端地址来自环境变量，如 `CYRENE_YIELD_URL` | `Cyrene-Catalyst/src/cyrene_catalyst/__main__.py:29` |
| 部分服务间鉴权已使用 bearer token | `Cyrene-Reactor` 的 `/data/credentials/control.token` 健康探针 |
| 客户端通过仅限 loopback 的本地桥接访问服务 | `Cyrene-Client/tooling/server-control.ts:22` |

### 2.2 Platform fabric（基础）

`Cyrene-Platform/framework/crates/cy-workspace-fabric`（1284 行）已实现：

- `directory.rs` —— 账号成员关系与 Workspace 发现（`WorkspaceMembership`）。
- `transport.rs` —— 出站 mTLS relay 客户端；`RelaySession::discover` / `execute` / `serve_workspace`。
- `relay.rs` —— 无状态应用层 gRPC 中继（`WorkspaceRelayService`）。
- `auth.rs` —— 短期 relay 会话；`SessionPrincipal::WorkspaceDevice { workspace_id, device_id }` 与可替换的 `DevelopmentSessionVerifier`。

相关事实：

- `ConnectivityMode` 已建模 `LOCAL`、`LAN_DIRECT`、`DIRECT`、`OVERLAY`、`RELAY`
  （`contracts/proto/cyrene/core/v1/node_control.proto:86`），但 `validate_route`
  当前只放行 `LOCAL` 与 `RELAY`。
- `WorkspaceConnectionDescriptor` 携带**有序**的 `WorkspaceConnectionCandidate`
  （`contracts/proto/cyrene/workspace/v1/workspace_fabric.proto:51`），这正是
  “优先最优路径、失败回退”的既有机制。
- `RelayHello` 已携带 `DeviceEnrollmentRef device`
  （`workspace_fabric.proto:68`）。
- **缺口：** 除自身 fixture 二进制外没有任何消费者。没有产品服务是 Workspace
  connector；没有生产 relay 宿主；没有真实 IAM。

### 2.3 Artifact 平面

`Cyrene-Platform/sdk/rust/cy-artifact-transfer` 已建模点对点分发：

- `ArtifactPeer` / `ArtifactPeerKind` —— 含 `DesktopCache`、`NodeCache`、`CentralSeed`、`RegionalCache`、`ObjectStoreGateway`。
- `ArtifactSourceCandidate { peer, replica }` 与 `TransferPlan` —— 候选驱动的规划。
- `TransferTicket` —— “点对点分片传输的短期授权”。
- `TransferCheckpoint` —— 可续传。
- **缺口：** `TransferProtocol` 只有 `HttpsRangeV1`。peer 仍需可达地址，这与 §2.1 是同一个 NAT/端口问题。

### 2.4 日志

各 Python 服务与客户端都已有结构化 NDJSON 日志与 W3C traceparent 传播。缺口是一小
组静默吞异常，见 Phase 0。

---

## 3. 设计

### 3.1 传输选择与跨语言代理

传输由描述符的有序候选决定，而非硬编码：

1. `LAN_DIRECT` —— 集群内，直连集群网络内可达地址（端口保持集群私有，不对外暴露）。
2. `RELAY` —— 无直连路径的设备走出站 mTLS，不开放入站端口。

因此“暴露端口数”与“延迟”不再互相冲突：对外无端口，对内走直连。

关键架构机制：
- **跨语言 Rust Sidecar 代理。** Python 服务（Catalyst、Yield、Echo、Exchange）及非 Rust
  运行时通过监听在本地回环 `127.0.0.1` 上的轻量级 Rust Sidecar 代理与 Workspace Fabric 通信。
  Sidecar 透明接管 mTLS Relay 会话维护、`LAN_DIRECT` 路由解析以及凭据轮换，避免在 Python 中重复实现底层网络与加密协议。
- **Relay 流控背压机制。** Relay 实施 HTTP/2 和 gRPC 流控窗口（Flow control window）与每个流的有界内存缓冲。
  在长耗时 LLM SSE 生成和分块 Token 流式传输期间，慢速消费端会向上游触发背压阻尼，绝不导致 Relay 内存膨胀。
- **Azure Container Apps 入口边界。** ACA HTTP ingress 在 Envoy 终止客户端 TLS；开启客户端证书模式后，
  应用通过 `X-Forwarded-Client-Cert` 接收证书信息。生产 Relay 必须在 ACA 入口要求客户端证书、
  仅信任来自 ACA 代理的该请求头、校验已登记的证书指纹及用户或设备会话权限，并在 ACA HTTP 请求超时后
  重连流。参考 Relay 的进程内 mTLS 监听器不能原样作为 ACA 部署入口。参见
  [ACA 客户端证书认证](https://learn.microsoft.com/en-us/azure/container-apps/client-certificate-authorization)
  和 [ACA 入口说明](https://learn.microsoft.com/en-us/azure/container-apps/ingress-overview)。

### 3.2 复用同一 fabric 的 Artifact 传输与云存储网关后备

fabric 提供身份、可达性与授权；artifact 平面提供规划、续传与完整性。

- Directory 返回的 artifact **来源候选**，其可达性由同一套连接描述符机制表达。
- 当 `LAN_DIRECT` 可用时，对端间直接通过高速 HTTPS Range 或 TCP 进行直连传输。
- **跨 NAT 云存储后备（`ObjectStoreGateway`）。** 当对端位于不同 NAT 之后且缺乏直连路径时，Cyrene
  避免脆弱易断的 P2P 打洞，而是无缝回退至用户自备的云对象存储（Cloudflare R2、Azure Blob、AWS S3 或 Hugging Face），使用临时预签名 URL 进行中转。
- **Fail-closed 严格约束。** 若跨 NAT 无法直连且未配置云对象存储，传输立即 Fail-closed 并给出清晰可操作的诊断提示
  （“请绑定 Cloudflare R2 或 Azure Blob 等对象存储以启用跨网络模型传输”），绝不将 GB 级模型载荷降级回退至轻量 Relay。
- `TransferTicket` 授权点对点分片或网关传输，使 peer 无需持有长期用户凭证即可提供服务。

### 3.3 身份与带外生物识别审批

一套设备身份同时服务连接与模块化安装器：

- relay 协议中已有设备注册（`DeviceEnrollmentRef`）。
- **通过带外审批消除 Token 摩擦。** 不再要求用户手动复制粘贴静态 Bearer Token 到云端容器：
  1. 无人值守的服务器或容器节点启动时向 Directory 发送纳管申请。
  2. Directory 向用户活跃的受信任设备（如 Client 网页控制台的 WebAuthn / Passkey 提示，或手机推送）发送审批通知。
  3. 用户使用生物识别（Touch ID、Face ID、Windows Hello）确认审批。
  4. Directory 向等待中的节点颁发短期 mTLS 会话证书。
- 安装器消费完全相同的设备与 Workspace 身份事实。

**两者不得各自生长出独立的注册或凭证体系。**

### 3.4 Workspace API 投影

当前投影只暴露 `StartWorkspaceOperation` 与 `GetWorkspaceOperation`
（`workspace_fabric.proto:95`）。扩展到六个产品面是本计划最大的单项工作，按每个
服务一个最小切片增量交付。

Web Client 最终仅通过 Workspace API 访问各产品投影；服务直连端点仍作为内部实现路径。

请求路径已经确定：Web Client 调用稳定的 Workspace API；连接描述符选择 `LAN_DIRECT`
或 `RELAY`，两条传输路径均到达同一个 Workspace Control Plane。该控制层持有
Workspace 权威并提供 Product API 投影；各 Product 服务继续持有各自领域记录的权威。
Directory 只提供按身份过滤的发现结果和连接候选，Relay 只承载认证后的流量。
当前 Web 到 Product 的代理路由属于过渡部署路径，不是 Phase 3 的 Client 契约。

---

## 4. 阶段

### Phase 0 —— 日志错误路径收口

独立进行；先于任何连接改造落地。

| 位置 | 现状 | 处理 |
| --- | --- | --- |
| `Cyrene-Reactor/runtime/core/src/cy_exec/core/server.py:133,353` | `except Exception: pass` | 产出带 trace 上下文的结构化错误，或收窄为具名异常并说明理由 |
| `Cyrene-Yield/training/core/src/cy_exec/training/product_service.py:773` | `except KeyError: pass` | 记录日志，或返回显式默认值并 warn |
| `Cyrene-Yield/training/core/src/cy_exec/training/product_store.py:364` | `except (ValueError, TypeError): pass` | 同上 |
| `Cyrene-Echo/src/cyrene_echo/engine.py:323` | `except ValueError: return None` | 返回 fallback 前记录原因 |
| `Cyrene-Exchange/src/cyrene_exchange/gateway.py:602` | 鉴权解析失败 → `principal = None`，原因丢失 | 记录原因（绝不记录 token），再抛 `invalid credentials` |

结构性修复：Exchange 的 `_observe_rejected` 依赖 `self._lifecycle_observer`；为
`None` 时拒绝路径静默。改为启动期强制要求 observer（fail-closed），使观测缺失
不会产生静默。

验收：

1. 各仓库 CI 门禁在显式白名单之外遇到裸 `except ...: pass` / 静默 `return None` 即失败。
2. 在每条被修路径注入故障，产出日志的 `trace_id` 与原始请求一致。
3. 深层失败能通过 `trace_id` 追溯到边界日志。

### Phase 1 —— Relay 服务、LAN_DIRECT 与 Sidecar 代理

目标：具备背压流控的可部署 Relay、集群内直连路径，以及非 Rust 服务的 Sidecar 代理。

工作：

- 从 `WorkspaceRelay` 抽出可运行入口（协议不变）；TLS、健康检查、日志。
- 在 Relay 上针对 SSE / 流式 LLM 响应实现流控背压与有界内存队列。
- 在本地回环 `127.0.0.1` 上为 Python 服务（Catalyst、Yield、Echo、Exchange）实现轻量 Rust Sidecar 代理，对接 Workspace Fabric。
- 新增 `LanDirectConnectivityProvider`，在 `validate_route` 放行 `LAN_DIRECT`。
- 增加非 relay 的直连传输路径，并在描述符中把 `LAN_DIRECT` 候选排在 `RELAY` 之前。
- Directory 持久化：`InMemoryWorkspaceDirectory` → 真实的成员/设备存储。
- 用真实设备凭证替换 `DevelopmentSessionVerifier`。
- relay 跳转保持 `traceparent` 不断链。

验收：

- `tooling/acceptance/distributed-workspace-fabric/run-relay-proof.sh` 对已部署 relay 通过。
- relay 重启不改变 Workspace 权威或 Operation 身份。
- 集群内请求解析到 `LAN_DIRECT` 候选，不经 relay。
- 在客户端人为限速下的流式响应会向上游触发背压阻尼，Relay 内存不膨胀。

### Phase 2 —— 账号登录、设备码与带外生物识别审批

目标：RFC 8628 OAuth 2.0 设备授权流与带外推送/生物识别审批，加上云对象存储后备。

工作：

- 设备码与带外推送流程：无人值守节点请求纳管；Directory 提醒受信任客户端设备。
- 客户端 Web 控制台集成 WebAuthn / Passkey / 生物识别审批（Touch ID、Face ID、Windows Hello）。
- 成员关系驱动发现，使同账号设备互相找到。
- 云对象存储凭证绑定：支持配置 Cloudflare R2、Azure Blob、AWS S3 或 Hugging Face 仓库凭据。
- 跨 NAT Artifact 传输引擎：直连不可达时生成预签名上传/下载 URL；未配置时 Fail-closed。
- 保持 User / Node-device / Workload 三类身份分离；runtime 容器绝不获得长期用户凭证。

验收：

- 新的无头设备通过带外生物识别审批完成纳管，无需任何手动 Token 复制。
- 同账号第二台设备发现目标 Workspace；异账号无法发现（必须测负向用例）。
- 跨 NAT 的大文件传输使用已配置的云对象存储；未配置云存储时 Fail-closed 并给出清晰诊断。
- 凭证过期可重新认证且不泄露到日志。

### Phase 3 —— Workspace API 投影（最长杆）

目标：把六个产品面经由稳定 Workspace API 投影。

做法：每个服务先做一个最小切片（一个读 + 一个操作），再逐步扩大。不要一开始就
覆盖全部端点。

工作：

- `contracts/` 下的 proto 定义与 TCK 扩展。
- 实现 Workspace Control Plane，由它持有 Workspace 权威并处理 `WorkspaceApi`；
  `LAN_DIRECT` 和 `RELAY` 均连接到同一个控制层。Product 领域记录仍由各自服务持有。
- 客户端改为只经 Workspace API 访问产品。

验收：

- 每个服务端到端完成一个投影操作；TCK 通过。
- 客户端不再直连任何产品端口。
- 响应不暴露 Docker 地址、本地路径或容器 ID。

### Phase 4 —— 下端口并移除旧寻址模型

工作：

- 对端寻址从 `CYRENE_*_URL` 改为 Workspace 身份路由（删除，不标废弃）。
- 去掉 Dockerfile 中的公共 `EXPOSE` 行并禁用外部入站端口；保留容器内 HTTP `/healthz` 端点与回环监听供平台编排器探针使用。
- 更新部署与文档。

验收：

- 完整工作流（Catalyst → Yield → Echo → Reactor → Exchange → Navigator）在无发布产品端口下运行。
- 不再残留任何 `CYRENE_*_URL` 对端配置。
- 编排器健康检查探针（`/healthz`）在容器环境内部持续正常工作。

### Phase 5 —— 加固

Relay HA 与集群拓扑、逐步实施端到端加密、Relay 可观测性、回滚演练、直连与云存储 Artifact 传输的负载测试。

---

## 5. 需要决定的事项

1. **端到端加密。首版已决议：**允许自建 Relay 和控制面读取投影载荷；传输使用经身份验证的 TLS，
   并限定授权运维人员访问。后续加固阶段逐步加入端到端加密。
2. **跨 NAT 批量传输网关。** 【已决议】：在 `LAN_DIRECT` 不可用时，通过临时预签名 URL 使用用户自备的云对象存储（Cloudflare R2、Azure Blob、AWS S3、Hugging Face）；未配置时严格执行 Fail-closed 并输出诊断信息。
3. **无头节点纳管审批。** 【已决议】：弃用静态 Bearer Token 复制；在受信任客户端设备上通过带外推送与 WebAuthn/Passkey 生物识别进行审批。
4. **跨语言服务互联。** 【已决议】：在本地回环 `127.0.0.1` 上为 Python 服务提供轻量级 Rust Sidecar 代理守护进程。
5. **投影粒度。Client 边界已决议：**Web Client 仅通过 Workspace API 访问产品。
   每个服务具体投影哪些操作，在 Phase 3 逐服务决定；其余操作保持内部使用。

## 6. 风险

- **Phase 3 是主要风险。** 契约面大，且改变客户端调用产品的方式。用每服务一个最小
  切片控制。
- **身份耦合。** 若 fabric 与模块化安装器各自建设注册路径，两者都会返工。在 Phase 2
  一次落定共享设备身份，由安装器消费。
- **批量路径退化。** 让大文件走 relay 会成为静默的性能与成本失败。Phase 1 与 Phase 3
  的验收标准必须断言直连路径**确实被使用**，而不只是"可用"。
