# Distributed Workspace Fabric v1 TCK

This TCK freezes the authority boundaries and observable outcomes of
identity-based Workspace discovery, transport-neutral connection selection,
direct-first private access with Relay fallback. `scenarios.tsv` is the language-neutral matrix;
Rust unit tests and the real Docker relay acceptance are executable adapters.

本 TCK 冻结基于身份的 Workspace 发现、transport-neutral 连接选择、私网直连优先与 Relay 回退的
远程访问的权威边界与可观察结果。`scenarios.tsv` 是跨语言矩阵，Rust 单元测试与
真实 Docker Relay acceptance 是其可执行 adapter。

Run:

```bash
bash tooling/ci/check-distributed-workspace-fabric.sh
bash tooling/acceptance/distributed-workspace-fabric/run-relay-proof.sh
```

The Docker proof is mandatory for release acceptance. A Docker/WSL transport
outage is reported as a blocker and is never converted into a skip or fake
pass.

发布验收必须执行 Docker proof。Docker/WSL transport 故障只能报告为 blocker，
不得转换为 skip 或 fake pass。
---

<!-- Chinese Translation / 中文翻译 -->

# Distributed Workspace Fabric v1 TCK

此 TCK 冻结基于身份的 Workspace 发现、与 transport 无关的连接选择、私网直连优先和 Relay 回退的 authority 边界与可观测结果。`scenarios.tsv` 是跨语言矩阵；Rust 单元测试和真实 Docker 验收是可执行 adapter。

运行：

```bash
bash tooling/ci/check-distributed-workspace-fabric.sh
bash tooling/acceptance/distributed-workspace-fabric/run-relay-proof.sh
```

发布验收必须执行 Docker proof。Docker/WSL transport 故障只能报告 blocker，不得转换成 skip 或 fake pass。
