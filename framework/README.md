# Framework

The framework is the language-friendly control and extension layer above the
kernel. It consumes the contracts in `../contracts` and asks the kernel to
enforce lifecycle and resource decisions.

The Rust crates in this directory provide the public platform API, not
first-party service implementations. New public APIs must be protocol-first so
the orchestration layer can evolve without changing plugins or the kernel.

Advanced services are discovered through manifests. The framework must never
hard-code one of the six CYRENE products.
---

<!-- Chinese Translation / 中文翻译 -->

# Framework

Framework 是位于 Kernel 之上的、便于各语言使用的控制与扩展层。它消费 `../contracts` 中的契约，并请求 Kernel 强制执行生命周期与资源决策。

本目录中的 Rust crate 提供公开 Platform API，不提供第一方 Service 实现。新的公开 API 必须以 protocol-first 方式设计，使 orchestration 层可演进，而无需修改 Plugins 或 Kernel。

高级 Service 通过 manifest 发现。Framework 绝不能硬编码 CYRENE 六个 Product 中的任何一个。
