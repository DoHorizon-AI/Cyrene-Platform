# Distributed Execution Fabric v1 TCK

This TCK freezes the product-neutral Execution Attachment projection layered on
the Kernel Semantic Contract. `scenarios.tsv` is the language-neutral matrix;
the Rust contract tests and real Docker acceptance are its executable adapters.

本 TCK 冻结建立在 Kernel Semantic Contract 之上的产品无关 Execution
Attachment 投影。`scenarios.tsv` 是跨语言矩阵，Rust contract tests 与真实
Docker acceptance 是其可执行适配器。

Run:

```bash
bash tooling/ci/check-distributed-execution-fabric.sh
bash tooling/acceptance/distributed-execution-fabric/run-container-proof.sh
```

The matrix covers Node persistence, restart capability, four-evidence Runtime
reconciliation, generation fencing, Welcome/credential ordering, canonical
Lease acquisition and rollback, post-start assignment acknowledgement,
policy-first Peer selection, verified scoped tickets, and resumable publication.
The Docker command is mandatory for release acceptance. A Docker daemon outage
is reported as a blocker; it is never converted into a skipped or mocked pass.

发布验收必须执行 Docker 命令。Docker daemon 故障只能报告为 blocker，不能被
转换成 skip 或 mock pass。
---

<!-- Chinese Translation / 中文翻译 -->

# Distributed Execution Fabric v1 TCK

此 TCK 冻结建立在 Kernel Semantic Contract 之上的、与 Product 无关的 Execution Attachment 投影。`scenarios.tsv` 是跨语言矩阵；Rust contract test 和真实 Docker acceptance 是其可执行 adapter。

运行：

```bash
bash tooling/ci/check-distributed-execution-fabric.sh
bash tooling/acceptance/distributed-execution-fabric/run-container-proof.sh
```

该矩阵覆盖 Node persistence、restart capability、四类证据的 Runtime reconciliation、generation fencing、Welcome/credential 顺序、规范 Lease 获取与 rollback、启动后的 assignment acknowledgement、policy-first Peer 选择、验证过的 scoped ticket，以及可恢复的 publication。发布验收必须执行 Docker 命令。Docker daemon 不可用时只能报告 blocker，不能转换成 skip 或 mock pass。
