# Distributed Workspace Fabric acceptance

This fixture proves Relay and private direct Workspace paths with one real unprivileged
Docker container. The remote reference frontend discovers a Workspace by user
identity, selects a transport-neutral `RELAY` candidate, and reaches the
Workspace API through a TLS relay. The Workspace connector, Runtime Agent, and
fake workload run without published container ports.

The Runtime Agent consumes the existing Execution Fabric control contract and
Artifact transfer implementation. The frontend receives only canonical
Operation and Artifact references; it never receives Docker, Runtime Agent, or
local filesystem access.

The same fixture also exposes a private mTLS Workspace endpoint inside the
container without publishing a Docker port. It proves fallback when a direct
candidate is unreachable, then discovers the real private candidate, stops the
Relay, and reads the same Workspace authority over `LAN_DIRECT`. This is a
development credential fixture; it does not prove production device enrollment.

本 fixture 使用一个真实无特权 Docker 容器证明 Relay 与私网直连 Workspace 纵向链路。
远程参考前端按用户身份发现 Workspace，选择 transport-neutral `RELAY` candidate，
并通过 TLS Relay 访问 Workspace API。Workspace Connector、Runtime Agent 与 fake
workload 均不发布容器端口。

同一 fixture 还在容器私网监听 mTLS Workspace 端点，不发布 Docker 端口。它先验证
直连候选不可达时回退到 Relay，再发现真实私网候选、关闭 Relay，确认前端通过
`LAN_DIRECT` 仍能读取同一个 Workspace authority。这里使用开发凭证，尚未证明
生产设备注册流程。

Runtime Agent 继续消费既有 Execution Fabric 控制 Contract 与 Artifact transfer
实现。前端只看到 canonical Operation/Artifact reference，不接触 Docker、Runtime
Agent 或本地文件系统。

Run:

```bash
bash tooling/acceptance/distributed-workspace-fabric/run-relay-proof.sh
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
bash tooling/acceptance/distributed-workspace-fabric/run-relay-proof.sh
```
