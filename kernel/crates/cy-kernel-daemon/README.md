# cy-kernel-daemon Directory Guide | kernel/crates/cy-kernel-daemon 目录指南

## Purpose | 目录职责

This directory groups one boundary of the CYRENE Platform source, protocol, fixture, or test tree.
本目录承载 CYRENE Platform 源码、协议、fixture 或测试树中的一个边界。

## Worker execution ceilings / Worker 执行上限

Canonical `StartWorker` maps `Worker.limits` into the sandbox LaunchPlan before
spawning. Supported limits are `memory.bytes` in `byte` and `cpu.time` in
`millicore`, both positive. Unsupported names/units, zero and CPU overflow fail
with `WORKER_LIMIT_UNSUPPORTED`. Existing Lease ceilings are retained or tightened;
the Lease cpuset is preserved. These are execution ceilings, not a second resource
allocation authority. Hardware binding strength remains independent.

`StartWorker` 在启动前把上述通用执行上限传给 sandboxd。不会用默认 Lease 限额
覆盖 Worker 的要求，也不会放宽已有 Lease 上限。资源依然由 Lease 分配；CPU/RAM
cgroup 限制和 GPU 设备绑定强度分别报告。回归测试直接检查经过权威启动入口交给
sandbox 的 LaunchPlan，防止只校验字段却不执行约束。

## Contents | 内容

| Entry | Responsibility | 一句话职责 |
| --- | --- | --- |

## Suggested reading / execution order | 推荐阅读 / 执行顺序

Read this guide first, then the direct files above in dependency order, and finally the nested directory guides.
先读本指南，再按依赖顺序阅读上方直接文件，最后进入嵌套目录指南。

## Contents snapshot | 内容快照

| Entry | Responsibility | 一句话职责 |
| --- | --- | --- |
| `Cargo.toml` | Rust package manifest. | Rust 包清单。 |
| `src/` | Nested source or contract boundary; read its README next. | 嵌套源码或契约边界，下一步阅读其 README。 |
| `tests/` | Nested source or contract boundary; read its README next. | 嵌套源码或契约边界，下一步阅读其 README。 |

This snapshot is intentionally limited to direct entries; nested directories own their detailed guides.
本快照只列出直接内容；嵌套目录由各自 README 负责详细说明。

The semantic `StartWorker` boundary validates plugin environment keys without
pre-injecting device selection. Sandboxd injects Adapter-owned keys exactly once;
plugin attempts to override them still fail closed. The canonical launch test
exercises this same merge boundary together with the declared memory ceiling.

语义 Worker 启动先校验插件环境，设备选择变量由 sandboxd 在执行边界注入一次。
插件覆盖设备变量仍被拒绝；回归测试同时验证该边界及内存上限。

Fresh hardware samples refresh the semantic inventory expiry using a separate
publication generation, including when the observed facts are unchanged. Replayed
samples do not extend liveness; resource identity and allocation checks retain
their existing authority.

新的硬件采样通过独立发布代次刷新有效期;事实未变化也需要刷新,重复缓存采样不能
延长在线状态。资源身份与分配检查继续使用既有权威。
---

<!-- Chinese Translation / 中文翻译 -->

# cy-kernel-daemon Directory Guide | kernel/crates/cy-kernel-daemon 目录指南

## 目录职责

本目录承载 CYRENE Platform source、protocol、fixture 或 test tree 中的一个 boundary。

## Worker execution ceiling

规范 StartWorker 会在启动前将 Worker.limits 映射到 sandbox LaunchPlan。支持的 limit 为单位 byte 的 memory.bytes 和单位 millicore 的 cpu.time；两者都必须为正数。Unsupported name/unit、零值和 CPU overflow 均以 WORKER_LIMIT_UNSUPPORTED 拒绝。现有 Lease ceiling 会保留或进一步收紧；Lease cpuset 得以保留。这些是执行上限，不是第二套 resource allocation authority。Hardware binding 强度独立处理。

StartWorker 启动前将这些通用执行 ceiling 传给 sandboxd。不能用默认 Lease limit 覆盖 Worker 要求，也不能放宽现有 Lease 上限。Resource 仍由 Lease 分配；CPU/RAM cgroup limit 与 GPU device binding 强度分别报告。Regression test 会检查经权威启动入口传给 sandbox 的 LaunchPlan，避免只校验字段却没有实际执行约束。

## 内容

| 条目 | 职责 | 一句话职责 |
|---|---|---|

## 推荐阅读 / 执行顺序

先阅读本指南，再按依赖顺序查看上方直接文件，最后进入嵌套目录指南。

## 内容快照

| 条目 | 职责 | 一句话职责 |
|---|---|---|
| Cargo.toml | Rust package manifest。 | Rust 包清单。 |
| src/ | 嵌套源码或 contract boundary；下一步阅读其中的 README。 | 嵌套源码或契约边界，下一步阅读其 README。 |
| tests/ | 嵌套源码或 contract boundary；下一步阅读其中的 README。 | 嵌套源码或契约边界，下一步阅读其 README。 |

此快照有意只列直接条目；嵌套目录由各自 README 提供详细指南。

Semantic StartWorker boundary 会校验 plugin environment key，但不会预先注入 device selection。sandboxd 只注入一次由 Adapter 所有的 key；plugin 试图覆盖这些 key 时仍会 fail closed。规范 launch test 同时验证该 merge boundary 和声明的 memory ceiling。

新的 hardware sample 会使用独立 publication generation 刷新 semantic inventory expiry，即使观测事实没有变化也如此。Replay sample 不会延长 liveness；resource identity 与 allocation check 仍由原有 authority 负责。
