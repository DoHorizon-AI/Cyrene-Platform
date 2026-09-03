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
reconciliation, generation fencing, policy-first Peer selection, scoped ticket
issuance, and resumable publication. The Docker command is mandatory for release acceptance. A Docker daemon outage
is reported as a blocker; it is never converted into a skipped or mocked pass.

发布验收必须执行 Docker 命令。Docker daemon 故障只能报告为 blocker，不能被
转换成 skip 或 mock pass。
