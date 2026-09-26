# First Platform Checkout

Clone Cyrene-Platform when working on generic contracts, Kernel mechanisms,
adapters, or SDK primitives:

```bash
git clone https://github.com/DoHorizon-AI/Cyrene-Platform.git
cd Cyrene-Platform
python tooling/ci/verify.py --scope all-light
cargo check --locked --workspace --all-targets
```

This repository builds without sibling Products or plugin repositories.
For a multi-repository development checkout, follow the bootstrap instructions
owned by [Cyrene-Workspace](https://github.com/DoHorizon-AI/Cyrene-Workspace).
---

<!-- Chinese Translation / 中文翻译 -->

# 首次检出 Platform

处理通用契约、Kernel 机制、适配器或 SDK 基元时，请克隆 Cyrene-Platform：

```bash
git clone https://github.com/DoHorizon-AI/Cyrene-Platform.git
cd Cyrene-Platform
python tooling/ci/verify.py --scope all-light
cargo check --locked --workspace --all-targets
```

本仓库无需同级 Products 或插件仓库即可构建。若要进行多仓库开发检出，请遵循 [Cyrene-Workspace](https://github.com/DoHorizon-AI/Cyrene-Workspace) 维护的引导说明。
