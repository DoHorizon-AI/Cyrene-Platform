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
