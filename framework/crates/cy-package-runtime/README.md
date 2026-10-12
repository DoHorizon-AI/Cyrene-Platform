# cy-package-runtime

`cy-package-runtime` is the node-local control-plane owner for verified package
installation, locked dependency preparation, binding activation, process
supervision, status, upgrade, rollback, and cleanup. Production offline
installation and uninstall use the root-only `PACKAGE_ONLY` maintenance path;
Product mutations use the authenticated Unix-domain socket.

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

On activation, the runtime resolves durable Plugin storage beneath
`${runtime-root}/binding-private/<binding-id>/<package-id>` and injects that
absolute path as `CYRENE_PLUGIN_STORAGE_PATH`. The binding and package identities come
from the activation request and verified installation record; callers cannot
select the filesystem path, and any caller-supplied value for the reserved
environment key is overwritten. The directory is owned by the runtime account
with mode `0700`. Its location is stable across upgrades of the same package and
survives deactivation, binding-reference removal, uninstall, and routine runtime
cleanup. Plugin-specific configuration such as Account Center `storage_path`
may be omitted and derived from this environment value.

`binding_activation_fact(binding_id)` exposes the resolved binding, installation,
package, and private data path to trusted Platform code. It takes no caller path
or package selector.
---

<!-- Chinese Translation / 中文翻译 -->

# cy-package-runtime

`cy-package-runtime` 是节点本地 control plane owner，负责已验证 package 的安装、锁定依赖准备、binding 激活、进程监管、状态、升级、回滚和清理。

Package 提供与语言无关的 launch command。配置好的外部 adapter 通过 `cyrene.package-dependency-preparer.v1` 准备语言依赖；Platform 校验其有界 JSON 证据，但不需要理解 Python、Java、.NET 或其他 toolchain。随后 supervisor 执行已验证的、相对于 package 的 binary 或准备好的 runtime executable，附加通用 readiness 参数，并返回不透明的 `connection_ref`。语言 adapter 和 capability 协议留在 package 所属仓库。

Product 使用 Plugin 所有的版本化 client 打开 `connection_ref`。此 crate 不提供 invoke、stream、subscribe、method、request/response payload 或 domain-error API。

激活时 runtime 会解析 `${runtime-root}/binding-private/<binding-id>/<package-id>` 绝对路径，并仅在启动 Plugin 时通过 `CYRENE_PLUGIN_STORAGE_PATH` 注入。binding 身份和 package 身份分别来自激活与已验证 installation；调用方无法选择文件系统路径，调用方传入的同名环境变量会被覆盖。目录由 runtime 账户拥有且权限为 `0700`。同一 package 升级后路径保持稳定，停用、移除 binding 引用、卸载及常规 runtime cleanup 均会保留该目录。Account Center 等 Plugin 可以省略专属配置中的 `storage_path`，并从该环境变量派生。

可信 Platform 代码可调用 `binding_activation_fact(binding_id)` 查询 runtime 解析出的 binding、installation、package 和私有数据路径。该方法不接受调用方提供的路径或 package 选择器。

生产离线安装和卸载走 root-only `PACKAGE_ONLY` maintenance 流程；Product mutation 通过已认证 Unix-domain socket 执行。
