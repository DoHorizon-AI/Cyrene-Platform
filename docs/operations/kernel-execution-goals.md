# CYRENE Kernel Execution Goals

**Status:** active implementation and acceptance tracker. This document does
not redefine semantic v1; the authoritative v1 contract is
[kernel-semantic-contract-v1.md](../contracts/kernel-semantic-contract-v1.md).

The design intent is recorded in
[Kernel Design Goals](../architecture/kernel-design-goals.md). The deployment
shape and operator-facing baseline are in
[Kernel Runtime Baseline](kernel-runtime.md).

## Current baseline

The following work is already represented in the repository and is not a new
execution phase:

- Kernel daemon responsibilities have been split into a composition root and
  focused adapter, conversion, and RPC modules; do not reopen a structural
  split just because of the former monolithic document.
- The v1 semantic contract and its TCK live under `contracts/`; v1 uses the
  contract's bounded cursor-to-page event projection.
- The Linux-oriented runtime boundary is composed from the pure Safe Kernel,
  an external sandbox service, and external hardware adapters. Its operational
  assumptions are documented in `kernel-runtime.md`.

## P2 implementation and acceptance goals

### 1. Local privileged-IPC admission

Make the admission boundary executable, not only documented.

- Configure and enforce an explicit peer policy for Kernel-to-sandbox and
  Kernel-to-hardware-adapter UDS connections (dedicated account or group).
- Require peer-credential validation for every privileged request path.
- Demonstrate that an untrusted local account cannot issue a raw privileged
  UDS request, while the authorized Kernel identity can.
- Keep socket ownership, mode, service-account, and expected-peer settings
  coherent across systemd units and adapter configuration.

### 2. Linux process and resource isolation evidence

Validate the actual Linux environment, rather than inferring isolation from a
Windows build or from the presence of adapter code.

- Verify cgroup v2 hierarchy and the controllers required by the selected
  resource policy.
- Exercise create, attach, terminate, and cleanup paths, including
  `cgroup.kill` where supported, pidfd signaling, reaping, timeout, and
  out-of-memory outcomes.
- Prove device and GPU restrictions in the chosen isolation backend, including
  a denial case for an unleased or unauthorized device.
- Capture the kernel, cgroup, service-manager, and adapter versions with each
  acceptance run so results are reproducible.

### 3. Authority, lifecycle, and recovery behaviour

- Run concurrent lease, renewal, release, and stale-fence tests against real
  provider state; a stale authority must never mutate a newer lease.
- Validate worker start, stop, unexpected exit, restart, and reconciliation.
  Recovery must clean up or reconcile known ownership and must not adopt an
  unverified foreign process.
- Exercise provider inventory drift and confirm reconciliation yields an
  explainable result and appropriate semantic events.

### 4. Contract and integration conformance

- Run the semantic v1 TCK and the applicable Rust, gRPC, runtime, sandbox, and
  hardware-adapter tests for each change.
- Verify event cursor, ordering, retention-expiry, and slow-consumer behaviour
  according to the v1 bounded-page contract.
- Treat semantic-contract changes as versioned changes: update the contract,
  generated projections, and TCK together before integration.

## Evidence required to close a goal

A goal is complete only when its relevant automated tests pass and the target
Linux acceptance environment has produced retained evidence for the stated
security or isolation claim. Local compilation is useful feedback but is not
evidence of Linux cgroup, pidfd, BPF/device, GPU, or peer-credential behaviour.

Report each acceptance run with the commit, environment identity, commands or
test suite, expected result, actual result, and any unsupported platform
feature. Unsupported capabilities must be surfaced explicitly rather than
being represented as a successful no-op.

## Deferred semantic evolution

Namespace-scoped identity, snapshot-plus-stream events, server-streaming
delivery, new provider operations, C ABI evolution, and JVM/Kotlin projections
remain design candidates. They are not P2 acceptance criteria until a new or
revised semantic contract and its conformance cases are approved.
---

<!-- Chinese Translation / 中文翻译 -->

# CYRENE Kernel 执行目标

**状态：**当前实现与验收跟踪文件。本文不重新定义 semantic v1；规范 v1 契约见 [kernel-semantic-contract-v1.md](../contracts/kernel-semantic-contract-v1.md)。

设计意图记录在 [Kernel Design Goals](../architecture/kernel-design-goals.md)。部署形态与运维基线见 [Kernel Runtime Baseline](kernel-runtime.md)。

## 当前基线

以下工作已反映在仓库中，不是新的执行阶段：

- Kernel daemon 职责已拆分到 composition root、adapter、conversion 和 RPC 等专门 module；不能因为旧的单体文档就重新开启结构拆分。
- v1 semantic contract 和对应 TCK 位于 \`contracts/\` 下；v1 使用契约中有界的 cursor-to-page event projection。
- 面向 Linux 的 runtime 边界由纯安全 Kernel、外部 sandbox service 和外部 hardware adapter 组成。其运维假设见 \`kernel-runtime.md\`。

## P2 实现与验收目标

### 1. 本地特权 IPC 准入

让准入边界能够实际执行，而不只停留在文档。

- 为 Kernel-to-sandbox 和 Kernel-to-hardware-adapter UDS connection 配置并实施明确的 peer policy（专用账号或组）。
- 每条特权 request 路径都必须校验 peer credential。
- 证明不受信任的本地账号不能发送原始特权 UDS request，而授权的 Kernel identity 可以。
- 确保 systemd unit 与 adapter 配置中的 socket ownership、mode、service account 和 expected peer 设置保持一致。

### 2. Linux 进程与资源隔离证据

验证真实 Linux 环境，不能根据 Windows build 或 adapter 源码存在就推断已实现隔离。

- 校验 cgroup v2 层级结构和所选资源策略要求的 controller。
- 测试创建、附加、终止和清理路径，包括受支持时的 \`cgroup.kill\`、pidfd signaling、reap、timeout 和 OOM 结果。
- 证明选定 isolation backend 的 device/GPU 限制有效，包括拒绝未租用或未授权 device 的场景。
- 每次 acceptance run 都记录 kernel、cgroup、service manager 和 adapter 版本，确保结果可复现。

### 3. Authority、生命周期与恢复行为

- 使用真实 Provider 状态并发执行 Lease、续期、释放和过期 fence 测试；过期 authority 绝不能修改更新的 Lease。
- 验证 Worker 启动、停止、意外退出、重启和 reconciliation。恢复过程必须清理或协调已知 ownership，并且不得接管未经验证的外部进程。
- 测试 Provider inventory 漂移，并确认 reconciliation 产生可解释结果和正确的语义事件。

### 4. 契约与集成符合性

- 每次变更都运行 semantic v1 TCK，以及适用的 Rust、gRPC、runtime、sandbox 和 hardware-adapter 测试。
- 按 v1 bounded-page 契约验证 event cursor、顺序、保留期过期和慢 consumer 行为。
- 将 semantic contract 变更作为版本化变更处理：集成前同时更新契约、生成投影和 TCK。

## 关闭目标所需证据

只有相关自动化测试通过，并且目标 Linux acceptance 环境为所声明的安全或隔离保证生成并保留证据，目标才算完成。本地编译有助于快速反馈，但不能证明 Linux cgroup、pidfd、BPF/device、GPU 或 peer credential 行为。

每次 acceptance run 都要报告 commit、环境身份、命令或 test suite、预期结果、实际结果和任何不受支持的平台功能。不支持的能力必须明确报告，不能伪装成成功的 no-op。

## 延后演进的语义

Namespace-scoped identity、snapshot-plus-stream event、server-streaming delivery、新 Provider operation、C ABI evolution 和 JVM/Kotlin projection 仍是设计候选项。只有批准新的或修订后的 semantic contract 及其一致性用例后，才会成为 P2 验收标准。
