# nvidia Directory Guide | adapters/hardware/nvidia 目录指南

## Purpose | 目录职责

This directory groups one boundary of the CYRENE Platform source, protocol, fixture, or test tree.
本目录承载 CYRENE Platform 源码、协议、fixture 或测试树中的一个边界。

## CUDA execution scope / CUDA 执行范围

The CLI runs `nvidia-smi` by default; `--nvidia-smi PATH` selects an installed
executable. Provider identity remains `nvidia-smi`. Native Linux requires the
selected `/dev/nvidia*` devices and HARD device-BPF enforcement.

WSL requires explicit `--wsl-shared-device` admission. It accepts exactly one
observed NVIDIA GPU and a real `/dev/dxg` character device. The binding reports
SOFT enforcement and the selected CUDA UUID; it does not promise per-GPU or
multi-tenant isolation. Kernel Lease/Fence and normal process supervision still
apply. Multiple GPUs and missing dxg fail closed. Cgroup enforcement is an
independent sandbox capability and is not disabled by this option.

默认执行 `nvidia-smi`，可用 `--nvidia-smi PATH` 指定已安装程序，provider identity
保持 `nvidia-smi`。原生 Linux 仍要求 `/dev/nvidia*` 与 HARD device-BPF。
WSL 必须显式启用 `--wsl-shared-device`，只支持一张观测到的 NVIDIA GPU 和真实
`/dev/dxg` 字符设备；声明 SOFT 与选定 UUID，不宣称逐 GPU 或多租户硬隔离。
Kernel Lease/Fence、正常进程监管和独立的 cgroup 执行要求仍然适用。

The legacy `KernelCapabilities.enforcement` projection must preserve the scope
of each report: process-tree enforcement is `UNSPECIFIED` in its coarse resource
enum, never `ACCELERATOR`. Actual device enforcement comes from the Lease binding.
旧版能力投影的粗粒度枚举没有 process-tree，应标为 `UNSPECIFIED`，不得把
cgroup 进程监管映射为 GPU 硬隔离。设备执行强度以 Lease 实际绑定为准。

Validation on 2026-09-06: `cargo test --locked -p cy-kernel-daemon -p
cyrene-nvidia-adapter` produced 107 passes, zero failures and two pre-existing
environment-dependent ignores (external .NET hosting and cross-account UDS).
Matching `cargo clippy --all-targets -- -D warnings` and scoped formatting passed.
The real WSL NVIDIA adapter, sandboxd and Kernel communicated over UDS under a
user-systemd delegated cgroup, without dev mode. Kernel observed one RTX 5070,
its CUDA capability and `wsl-shared-soft`. This is hardware/control evidence;
it is not evidence of a Reactor model Deployment or a second physical GPU.

2026-09-06：上述测试 107 通过、0 失败、2 项已有环境依赖忽略，lint/格式检查通过。
真实 WSL 硬件适配器、sandboxd、Kernel 已在用户 systemd 委派 cgroup 下连通，
未使用 dev mode，读到 RTX 5070、CUDA capability 和 `wsl-shared-soft`。
这仅证明硬件和控制前置，不代表 Reactor 模型部署或第二张物理 GPU 已验收。

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

This snapshot is intentionally limited to direct entries; nested directories own their detailed guides.
本快照只列出直接内容；嵌套目录由各自 README 负责详细说明。

Available-memory changes advance the inventory generation while preserving the
UUID-addressed GPU resource identity. Loading a model must not make its leased
device appear replaced.

显存变化推进清单代次,但保留 GPU 的 UUID 资源身份,避免将模型加载误判为设备更换。
