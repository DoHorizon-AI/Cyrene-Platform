# Distributed Workspace Fabric acceptance

This fixture uses one real unprivileged Docker container. The reference frontend
discovers the Workspace through an authenticated Relay session. The Relay rejects
the Workspace Connector with `WORKSPACE_DEVICE_PEER_CERTIFICATE_VALIDATION_NOT_CONFIGURED`.
TLS client-certificate trust remains configured; the missing provider is the
Relay's application-level Workspace-device certificate validator. The proof
requires that denial and records positive Connector-over-Relay E2E as unconfigured.
The frontend then uses the private mTLS `LAN_DIRECT` endpoint for Workspace API
operations.

The Relay startup trace must name the Workspace-device peer validator, signed
current revocation, current Registry binding validation, and the dispatch fence
as `NOT_CONFIGURED`. Authentication is denied at the first missing application
validator gate; this proof does not claim to exercise later gates independently.

The Runtime Agent consumes the existing Execution Fabric control contract and
Artifact transfer implementation. The frontend receives only canonical
Operation and Artifact references; it never receives Docker, Runtime Agent, or
local filesystem access.

The container runs the Runtime Agent and fake workload without published ports.
The direct proof starts and observes the same Workspace Operation, reuses the
existing Execution Fabric and Artifact transfer implementations, and confirms
that a request cannot fall back to an offline Relay Connector. It also verifies
that the direct endpoint remains usable after Relay stops and that Relay keeps
rejecting the Connector after restart.

Positive Connector traffic through Relay is not implemented. The Platform has no
configured device CA issuer or signed revocation adapter, and no acceptance
composition currently establishes an active Registry binding and dispatch fence.
The fixture uses development credentials and does not claim production device
enrollment or positive Relay Connector support.

本 fixture 使用一个真实无特权 Docker 容器。参考前端通过已认证的 Relay session 发现
Workspace。Relay 因未配置应用层 Workspace-device certificate validator 而拒绝
Workspace Connector，proof 会要求这一拒绝并明确记录 Connector 经 Relay 的正向 E2E 尚未配置。
TLS client-certificate trust 仍然配置；缺少的是应用层验证器。之后 frontend 通过私网
mTLS `LAN_DIRECT` endpoint 访问 Workspace API。

Relay startup trace 必须明确记录 Workspace-device peer validator、签名的当前撤销证据、当前
Registry binding validation 和 dispatch fence 均为 `NOT_CONFIGURED`。认证会在首个缺失的
应用层验证 gate 被拒绝；本 proof 不宣称单独执行了后续 gate。

容器中的 Runtime Agent 与 fake workload 不发布端口。直连 proof 启动并观察同一个
Workspace Operation，复用现有 Execution Fabric 和 Artifact transfer，并确认 Relay
Connector 离线时请求不会回退通过 Relay。proof 还会验证 Relay 停止后直连 endpoint
仍可用，以及 Relay 重启后继续拒绝该 Connector。

Connector 经 Relay 的正向流量尚未实现。Platform 尚无已配置的设备 CA issuer 或签名撤销
adapter，现有 acceptance 也没有建立 active Registry binding 和 dispatch fence 的接线。
fixture 使用开发凭证，不宣称生产设备注册或正向 Relay Connector 支持。

Runtime Agent 继续消费既有 Execution Fabric 控制 Contract 与 Artifact transfer
实现。前端只看到 canonical Operation/Artifact reference，不接触 Docker、Runtime
Agent 或本地文件系统。

Run:

```bash
bash tooling/acceptance/distributed-workspace-fabric/run-workspace-fail-closed-proof.sh
```
---

<!-- Chinese Translation / 中文翻译 -->

# Distributed Workspace Fabric Acceptance

此 fixture 使用一个真实、无特权的 Docker container，验证 Relay 与私网直连 Workspace 路径。远端参考前端根据用户身份发现 Workspace；Relay 路径完成操作，直连路径在 Relay 停止后仍能读取同一 Workspace API。Workspace connector、Runtime Agent 和 fake workload 运行时不会发布 container port。

Runtime Agent 使用现有 Execution Fabric control contract 和 Artifact transfer 实现。前端只接收规范 Operation 和 Artifact 引用，不会获得 Docker、Runtime Agent 或本地文件系统访问权。

此 fixture 使用一个真实无特权 Docker 容器证明 Relay 与私网直连 Workspace 路径。远程参考前端按用户身份发现 Workspace，优先尝试私网直连，失败时回退 Relay；关闭 Relay 后仍能通过直连读取。Workspace Connector、Runtime Agent 与 fake workload 均不发布容器端口。

Runtime Agent 继续使用既有 Execution Fabric control contract 和 Artifact transfer 实现。前端只看到规范 Operation/Artifact 引用，不接触 Docker、Runtime Agent 或本地文件系统。

运行：

```bash
bash tooling/acceptance/distributed-workspace-fabric/run-workspace-fail-closed-proof.sh
```
