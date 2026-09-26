# Platform package runtime

`cy-package-runtime` is the Platform/Control Plane production seam for generic
capability package lifecycle. It consumes the existing Workspace Package Spec
descriptor and a repository-owned `plugin.manifest.json`; it does
not define another manifest, catalog, registry, or Product policy model.

The runtime supports inspection, verification, installation, durable get/list,
external dependency preparation, binding activation/deactivation, process-backed
runtime status, upgrade, rollback, uninstall protection, offline reinstall,
and cache/staging reclamation. Service activation starts the package's
Plugins-owned direct runtime and returns its opaque `connection_ref`. Products
open that endpoint with the Plugin-owned protocol; Platform never invokes a
business method or carries its payload.

## Identity and state ownership

- `PackageId`, `PackageVersion`, and `ArtifactDigest` identify immutable package
  input.
- `InstallationId` identifies one digest-bound local publication and is never a
  package ID.
- `CapabilityId` is the callable contract identity.
- `BindingId` remains stable Product configuration identity.
- `RuntimeGeneration` changes on every activation, recovery, upgrade, or
  rollback and is never a binding or installation ID.
- `AVAILABLE` comes from the catalog, `VERIFIED` from verification evidence,
  `INSTALLED` from the atomic installation record, `CONFIGURED` from Product
  binding state, `ENABLED` from Product policy, and `RUNNING` only from the
  worker supervisor's child-process poll.

## Install transaction

Installation validates the published descriptor, archive digest, member paths,
symlink policy, extracted artifact digest, repository manifest identity, and
dependency lock digest. It then prepares dependencies from the exact lock,
writes verification and preparation evidence inside an invisible staging
directory, fsyncs it, and atomically renames it beneath `installations/`.

Failed preparation never creates a final installation directory or an
`INSTALLED` record. Archive and dependency bytes are content-addressed for
repeat and offline installation. A deterministic, digest-derived
`InstallationId` makes repeat and concurrent installation idempotent.

Archive members containing traversal, absolute/platform-specific paths,
duplicates, or symbolic links are rejected before extraction. Activation
revalidates the installed payload and dependency evidence before worker spawn.

## Dependency adapter protocol

The runtime is started with `--dependency-preparer <path>` and optional repeated
`--dependency-preparer-arg <value>` arguments. For each immutable dependency
lock, Platform appends `--package-root`, `--runtime-root`, and `--lock-digest`.
The adapter writes beneath `runtime-root` and emits one bounded JSON document:

```json
{
  "protocol": "cyrene.package-dependency-preparer.v1",
  "preparer": "plugin-owned-adapter-v1",
  "runtime_digest": "sha256:<64 lowercase hex characters>",
  "runtime_executable": "relative/path/to/runtime"
}
```

`runtime_executable` may be omitted when the package launches its own verified
binary. Python virtual environments, Java distributions, .NET hosts, and other
language details are implemented outside Platform.

运行时通过 `--dependency-preparer <path>` 和可重复的
`--dependency-preparer-arg <value>` 启动外部准备器。Platform 仅追加包目录、输出目录和
锁摘要，校验统一 JSON 证据；Python 虚拟环境、Java 发行版、.NET Host 等实现均留在
Platform 之外。

## Recovery and cleanup

Startup removes incomplete staging transactions but never synthesizes an
installed record. Durable activation records contain package-runtime identity
only; environment and secret values are not persisted. Product supplies those
values again to `recover_binding` after restart.

Uninstall rejects current and rollback references. Cache bytes remain available
for offline reinstall until explicit cleanup. Cleanup terminates any supervised
worker not backed by an active runtime record and reports the final orphan
count.

## Node-local control process

`cy-package-runtime --root <state-root> --dependency-preparer <path>` owns the lifecycle and supervised
workers for one node. Adapters exchange one JSON request and response per line
using `cy-package-runtime.control.v1`. The channel exposes structured error
codes and remediation, and supports the same inspect, verify, install,
activation, recovery, upgrade, rollback, uninstall, and cleanup operations as
the Rust API.

Descriptor/archive paths, dependency paths, worker environment values, and the
worker protocol are internal to this node-local seam. A Product adapter must
project only package identity/version, installation identity, verification,
binding policy, runtime status, and structured failure/remediation. It must not
project cache, staging or prepared-runtime paths, ZIP internals, executables, stdio,
PIDs, or runtime implementation details.
---

<!-- Chinese Translation / 中文翻译 -->

# Platform 包运行时

`cy-package-runtime` 是 Platform/Control Plane 中面向生产环境的通用能力包生命周期边界。它使用现有的 Workspace Package Spec 描述符和由仓库负责维护的 `plugin.manifest.json`；它不会再定义另一套清单、目录、注册表或 Product 策略模型。

运行时支持检查、验证、安装、持久化 get/list、外部依赖准备、绑定激活/停用、基于进程的运行时状态、升级、回滚、卸载保护、离线重装以及缓存/暂存回收。Service 激活会启动由 Plugins 所有的 direct runtime，并返回不透明的 `connection_ref`。Products 通过 Plugin 所有的协议连接该端点；Platform 不调用业务方法，也不承载其负载。

## 身份与状态归属

- `PackageId`、`PackageVersion` 和 `ArtifactDigest` 用于标识不可变的包输入。
- `InstallationId` 标识一次与摘要绑定的本地发布，不得用作包 ID。
- `CapabilityId` 是可调用契约的身份。
- `BindingId` 是稳定的 Product 配置身份。
- `RuntimeGeneration` 在每次激活、恢复、升级或回滚时改变，不能充当绑定 ID 或安装 ID。
- `AVAILABLE` 来自目录；`VERIFIED` 来自验证证据；`INSTALLED` 来自原子安装记录；`CONFIGURED` 来自 Product 绑定状态；`ENABLED` 来自 Product 策略；只有 worker supervisor 对子进程的轮询结果才能表明 `RUNNING`。

## 安装事务

安装时会校验已发布的描述符、压缩包摘要、成员路径、符号链接策略、解压后制品摘要、仓库清单身份和依赖锁摘要。之后，运行时根据精确锁文件准备依赖，在不可见的暂存目录中写入验证与准备证据、执行 fsync，再将目录原子重命名到 `installations/` 下。

准备失败时不会创建最终安装目录，也不会产生 `INSTALLED` 记录。压缩包和依赖字节按内容寻址，以支持重复安装和离线安装。确定性的摘要派生 `InstallationId` 使重复安装和并发安装具备幂等性。

在解压前会拒绝包含路径穿越、绝对路径或平台专属路径、重复成员或符号链接的压缩包成员。启动 worker 前会重新验证已安装负载和依赖证据。

## 依赖适配器协议

运行时使用 `--dependency-preparer <path>` 和可选的、可重复的 `--dependency-preparer-arg <value>` 参数启动。对于每个不可变依赖锁，Platform 会追加 `--package-root`、`--runtime-root` 和 `--lock-digest`。适配器在 `runtime-root` 下写入数据，并输出一个有大小上限的 JSON 文档：

```json
{
  "protocol": "cyrene.package-dependency-preparer.v1",
  "preparer": "plugin-owned-adapter-v1",
  "runtime_digest": "sha256:<64 lowercase hex characters>",
  "runtime_executable": "relative/path/to/runtime"
}
```

如果包启动自己的、已验证二进制文件，可以省略 `runtime_executable`。Python virtualenv、Java 发行版、.NET host 及其他语言相关细节均在 Platform 之外实现。

## 恢复与清理

启动时会删除未完成的暂存事务，但不会伪造已安装记录。持久激活记录只包含 package-runtime 身份；不会持久化环境变量或密钥值。重启后，Product 会在调用 `recover_binding` 时再次提供这些值。

卸载时会拒绝删除仍被当前运行时或回滚引用的安装。缓存字节在显式清理前会保留，以支持离线重装。清理会终止任何没有活动运行时记录支撑的受监管 worker，并报告最终孤儿数量。

## 节点本地控制进程

`cy-package-runtime --root <state-root> --dependency-preparer <path>` 负责一个节点上的生命周期和受监管 worker。适配器通过 `cy-package-runtime.control.v1` 协议逐行交换一个 JSON 请求和响应。该通道提供结构化错误码及修复建议，并支持与 Rust API 相同的检查、验证、安装、激活、恢复、升级、回滚、卸载和清理操作。

描述符/压缩包路径、依赖路径、worker 环境值和 worker 协议都是此节点本地边界的内部内容。Product 适配器只能投影包身份/版本、安装身份、验证结果、绑定策略、运行时状态以及结构化失败/修复建议。不得投影缓存、暂存或准备后运行时路径、ZIP 内部信息、可执行文件、stdio、PID 或运行时实现细节。
