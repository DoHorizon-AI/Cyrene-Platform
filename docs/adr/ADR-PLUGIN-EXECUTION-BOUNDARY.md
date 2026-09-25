# ADR-PLUGIN-EXECUTION-BOUNDARY: Installable Plugin Isolation

- Status: Approved / Normative
- Date: 2026-08-10
- Updated: 2026-09-09

## Decision

Installable business Plugins run outside Platform and Kernel processes. The
Plugin package owns its language runtime adapter, direct data-plane protocol,
payload contracts, and entrypoint. Platform owns verified installation,
compatibility selection, process lifecycle, health, permissions, and an opaque
`connection_ref`.

A managed service package supplies a language-neutral launch command. Platform
passes only generic readiness arguments and never imports a Python module,
loads a JVM/.NET assembly, or dispatches a capability method. Product clients
call the Plugin endpoint directly.

## Required invariants

- launch uses a verified package-relative executable or prepared runtime and an
  argv array, never a shell command string;
- identity, interface version, package digest, permissions, and readiness match
  before the endpoint is published;
- crash, timeout, cancellation, and protocol corruption are observable failures
  and cannot crash Kernel;
- stdout readiness is bounded, logs use stderr, and `connection_ref` is opaque;
- capability request/response/stream bytes never enter Platform control APIs.
---

<!-- Chinese Translation / 中文翻译 -->

# ADR-PLUGIN-EXECUTION-BOUNDARY：可安装 Plugin 隔离

- 状态：Approved / Normative
- 日期：2026-08-10
- 更新：2026-09-09

## 决策

可安装的业务 Plugin 在 Platform 和 Kernel 进程之外运行。Plugin package 拥有其语言 runtime adapter、direct data-plane 协议、payload contract 和 entrypoint。Platform 负责已验证安装、兼容性选择、进程生命周期、health、权限以及不透明的 `connection_ref`。

受管 Service package 提供与语言无关的 launch command。Platform 只传递通用 readiness 参数，不会导入 Python module、加载 JVM/.NET assembly，也不会分发 capability method。Product client 直接调用 Plugin endpoint。

## 必须满足的不变量

- 启动必须使用已验证的、相对于 package 的 executable 或已准备 runtime，并传递 argv array；绝不使用 shell command string。
- 发布 endpoint 前，identity、interface version、package digest、permissions 和 readiness 必须匹配。
- crash、timeout、cancellation 和协议损坏都是可观测的失败，且不能使 Kernel 崩溃。
- stdout readiness 内容有大小限制，日志写入 stderr，`connection_ref` 必须是不透明值。
- capability request/response/stream 字节不得进入 Platform control API。
