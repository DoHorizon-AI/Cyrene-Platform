# cy-package-runtime

`cy-package-runtime` is the node-local control-plane owner for verified package
installation, locked dependency preparation, binding activation, process
supervision, status, upgrade, rollback, and cleanup.

A package supplies a language-neutral launch command. A configured external
adapter prepares language dependencies through
`cyrene.package-dependency-preparer.v1`; Platform validates its bounded JSON
evidence without knowing Python, Java, .NET, or another toolchain. The
supervisor then executes a verified package-relative binary or the prepared
runtime executable, appends the generic readiness arguments, and returns an
opaque `connection_ref`. Language adapters and capability protocols stay in the
package repository.

The Product opens `connection_ref` using the Plugin-owned versioned client.
This crate has no invoke, stream, subscribe, method, request/response payload,
or domain-error API.
---

<!-- Chinese Translation / 中文翻译 -->

# cy-package-runtime

`cy-package-runtime` 是节点本地 control plane owner，负责已验证 package 的安装、锁定依赖准备、binding 激活、进程监管、状态、升级、回滚和清理。

Package 提供与语言无关的 launch command。配置好的外部 adapter 通过 `cyrene.package-dependency-preparer.v1` 准备语言依赖；Platform 校验其有界 JSON 证据，但不需要理解 Python、Java、.NET 或其他 toolchain。随后 supervisor 执行已验证的、相对于 package 的 binary 或准备好的 runtime executable，附加通用 readiness 参数，并返回不透明的 `connection_ref`。语言 adapter 和 capability 协议留在 package 所属仓库。

Product 使用 Plugin 所有的版本化 client 打开 `connection_ref`。此 crate 不提供 invoke、stream、subscribe、method、request/response payload 或 domain-error API。
