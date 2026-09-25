# Platform Checkout Boundary

Cyrene-Platform is an independently buildable repository. Its build, tests, and
release tooling discover only files in this checkout and do not clone, inspect,
or mutate sibling repositories.

## Platform checkout

```text
Cyrene-Platform/
├── adapters/        # Out-of-process hardware and sandbox adapters
├── agents/          # Platform node agents
├── contracts/       # Versioned wire and schema contracts
├── framework/       # Generic runtime and language boundaries
├── infrastructure/  # Units for Platform-owned daemons only
├── kernel/          # Rust Kernel implementation
├── sdk/             # Platform client SDKs
└── tooling/         # Repository-local CI, codegen, release, and runtime tools
```

Run the local verification entry point from this repository:

```bash
python tooling/ci/verify.py --scope all-light
cargo check --locked --workspace --all-targets
```

Multi-repository layout, checkout profiles, dependency pins, and status tooling
are owned by [Cyrene-Workspace](https://github.com/DoHorizon-AI/Cyrene-Workspace).
Changing that topology does not require a Platform change.
---

<!-- Chinese Translation / 中文翻译 -->

# Platform 检出边界

Cyrene-Platform 是可独立构建的仓库。其构建、测试和发布工具只会发现当前检出中的文件，不会克隆、检查或修改同级仓库。

## Platform 检出目录

```text
Cyrene-Platform/
├── adapters/        # 进程外硬件与沙箱适配器
├── agents/          # Platform 节点代理
├── contracts/       # 版本化线协议与 schema 契约
├── framework/       # 通用运行时与语言边界
├── infrastructure/  # 仅包含 Platform 所有 daemon 的 unit
├── kernel/          # Rust Kernel 实现
├── sdk/             # Platform 客户端 SDK
└── tooling/         # 仓库本地 CI、代码生成、发布与运行时工具
```

从本仓库运行本地验证入口：

```bash
python tooling/ci/verify.py --scope all-light
cargo check --locked --workspace --all-targets
```

多仓布局、检出 profile、依赖 pin 和状态工具由 [Cyrene-Workspace](https://github.com/DoHorizon-AI/Cyrene-Workspace) 所有。修改该拓扑不要求 Platform 随之变更。
