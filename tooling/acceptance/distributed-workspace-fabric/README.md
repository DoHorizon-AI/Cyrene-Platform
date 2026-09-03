# Distributed Workspace Fabric acceptance

This fixture proves the relay-first Workspace path with one real unprivileged
Docker container. The remote reference frontend discovers a Workspace by user
identity, selects a transport-neutral `RELAY` candidate, and reaches the
Workspace API through a TLS relay. The Workspace connector, Runtime Agent, and
fake workload run without published container ports.

The Runtime Agent consumes the existing Execution Fabric control contract and
Artifact transfer implementation. The frontend receives only canonical
Operation and Artifact references; it never receives Docker, Runtime Agent, or
local filesystem access.

本 fixture 使用一个真实无特权 Docker 容器证明 relay-first Workspace 纵向链路。
远程参考前端按用户身份发现 Workspace，选择 transport-neutral `RELAY` candidate，
并通过 TLS Relay 访问 Workspace API。Workspace Connector、Runtime Agent 与 fake
workload 均不发布容器端口。

Runtime Agent 继续消费既有 Execution Fabric 控制 Contract 与 Artifact transfer
实现。前端只看到 canonical Operation/Artifact reference，不接触 Docker、Runtime
Agent 或本地文件系统。

Run:

```bash
bash tooling/acceptance/distributed-workspace-fabric/run-relay-proof.sh
```
