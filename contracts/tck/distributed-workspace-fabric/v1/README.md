# Distributed Workspace Fabric v1 TCK

This TCK freezes the authority boundaries and observable outcomes of
identity-based Workspace discovery, transport-neutral connection selection,
and relay-first remote access. `scenarios.tsv` is the language-neutral matrix;
Rust unit tests and the real Docker relay acceptance are executable adapters.

本 TCK 冻结基于身份的 Workspace 发现、transport-neutral 连接选择和 relay-first
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
