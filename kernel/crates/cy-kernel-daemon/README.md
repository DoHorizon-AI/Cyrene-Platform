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
