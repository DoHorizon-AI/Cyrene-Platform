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
count. The installation directory is atomically renamed out of the installed
set before payload removal. A failed rename leaves the installed identity
unchanged for retry; a post-commit cleanup failure is reclaimed from private
staging during runtime startup.

## Node-local control process

`cy-package-runtime --root <state-root> --dependency-preparer <path>` owns the
lifecycle and supervised workers for one node. Production mode serves one
authenticated JSON request and response on each connection at
`/run/cyrene-package-runtime/control.sock`; `--socket` may select another
absolute path. The source policy defaults to
`/etc/cyrene/runtime-package-sources.json` and can be overridden with
`--source-policy`. The daemon shares one runtime and supervisor across all
connections. Its compatibility `--stdio` entrypoint keeps the original
line-oriented control wire for read-only and deterministic local diagnostics;
it rejects all package and binding mutations with
`MAINTENANCE_ADMISSION_REQUIRED`. Production binding mutations use only the
authenticated UDS and broker admission path. The root-only bootstrap commands
also require the exact live `PACKAGE_ONLY` hold and never use the compatibility
control wire.

UDS requests retain the flattened `operation` control shape and add an
authentication envelope and generation:

```json
{
  "request_id": "product-generated-request-id",
  "operation": "runtime_status",
  "binding_id": "yield.llama-factory.primary",
  "auth": {"source_id": "cyrene-yield", "source_token": "<source secret>"},
  "catalog_generation": 12
}
```

The first `authority` request may omit `catalog_generation`; its result returns
`authority: "platform_package_runtime"`,
`protocol_version: "cy-package-runtime.control.v1"`, the current generation,
and `capabilities`. The daemon advertises
`cy-package-runtime.binding-operation-admission.v1` only when the local
maintenance broker reports protocol `cyrene.runtime-maintenance.broker.v1`,
both required capabilities `cyrene.runtime-maintenance.state.v2` and
`cyrene.runtime-maintenance.binding-operations.v1`, and the same catalog
generation. The Broker capability list must contain unique, non-empty strings;
additional valid capabilities are allowed but are never projected by this
daemon. Otherwise `capabilities` is empty and mutation requests fail closed.

`auth: {}` is accepted only from a root Unix peer. Product callers must provide
both source fields and the exact generation. The daemon checks `SO_PEERCRED`
UID/GID against a single source-policy row and hashes the supplied token against
the row's SHA-256 digest with a constant-time comparison. It never reads the
root-only activity catalog or stores/logs the source token. The policy file and
all parent directories must be root-owned real paths without group/world write
permission; the policy file must be a single-link regular file opened without
following symlinks. Its top-level shape is:

```json
{
  "schema_version": 1,
  "generation": 12,
  "sources": [{
    "source_id": "cyrene-yield",
    "uid": 1001,
    "gid": 1001,
    "source_token_sha256": "<64 lowercase hex characters>",
    "bindings": [{
      "binding_id": "yield.llama-factory.primary",
      "package_id": "org.example.llama-factory",
      "installation_ids": ["install-<exact-id>"],
      "operations": ["activate", "recover_binding", "deactivate", "runtime_status", "get_installation"]
    }]
  }]
}
```

Product policy is exact per source, binding, package, and installation. Missing
rows, empty scopes, a generation mismatch, or an unrecognized operation deny
the request. Product operations cannot install packages or select paths. Input
frames are limited to 1 MiB, the environment map to 256 entries and 256 KiB,
each connection has a five-second read/write timeout, and at most 32 connections
are active at once.

The schema accepts multiple Product sources and also `"sources": []`. A package
may therefore be installed under a `PACKAGE_ONLY` hold before any Product source
or binding exists. With an empty source list, only root-peer authority remains;
Workspace adds the actual Product UID/GID, token digest, binding and exact
installation IDs when that Product is selected. A source with `bindings: []`
is also a valid disabled scope; each binding ID must have exactly one source
owner. A cataloged standalone package owner can use this same schema with its
own explicit source ID and binding ID, the runtime service UID/GID, the
SHA-256 digest of a protected source token, exact package/installation IDs,
and only the required operations. The Broker must recognize that same source
and binding owner for `AdmitBindingOperation`; a source-policy row alone does
not grant maintenance admission. Source-policy files contain only the token
digest, never a token-file path or plaintext token. The installer/service
chooses a protected credential path and loads it with
`PackageRuntimeClient.from_source_secret(source_id, token_path, ...)`; callers
must run with the row's UID/GID. Binding IDs are catalog inputs, not derived
from package IDs.

For example, the source row for a standalone plugin owner has the same closed
shape as a Product source. `uid` and `gid` below are illustrative; use the
installed `cyrene` account IDs and the exact binding ID selected by the
Workspace catalog:

```json
{
  "schema_version": 1,
  "generation": 12,
  "sources": [{
    "source_id": "cyrene-plugin-standalone-operator",
    "uid": 1001,
    "gid": 1001,
    "source_token_sha256": "0000000000000000000000000000000000000000000000000000000000000000",
    "bindings": [{
      "binding_id": "plugin.catalog.explicit-binding-1",
      "package_id": "org.example.plugin",
      "installation_ids": [
        "installation-1234567890abcdef1234567890abcdef",
        "installation-abcdef1234567890abcdef1234567890"
      ],
      "operations": [
        "activate",
        "recover_binding",
        "deactivate",
        "runtime_status",
        "get_installation"
      ]
    }]
  }]
}
```

The daemon reserves `activate`, `recover_binding` (broker operation `recover`),
and `deactivate` through the maintenance broker before changing runtime state.
The only allowed binding `operations` strings are exactly `activate`,
`recover_binding`, `deactivate`, `runtime_status`, and `get_installation`;
`authority` is source-authenticated but is not a binding-scoped operation.
`runtime_status` and `get_installation` are scoped reads and do not reserve a
Broker admission. Use `activate` with the new or old exact installation ID on
the same binding to upgrade or roll back; the policy must include each
authorized installation ID.
For activation it derives package identity from the exact installed record; for
recovery/deactivation it derives package and installation identity from the
durable binding record. A successful result retains the real `RuntimeStatus`
fields and adds `result.binding_operation`, which carries the request/source,
exact scope, both catalog and gate generations, the broker protocol version,
operation token, and replay flags. The Product owner must persist the returned
status and `connection_ref` with its own binding record, then call the broker's
`CompleteBindingOperation` directly using that same source credential, scope,
request ID, and token. The daemon never completes the owner lease itself.

An `already_in_flight` replay returns `BINDING_OPERATION_PENDING` with the
receipt under `error.binding_operation`; it never dispatches the mutation a
second time. An `already_completed` replay returns only a fresh read-only
runtime status plus its receipt. Mutation errors after reservation also carry
the receipt so the owner can reconcile durable state explicitly. If first
activation fails before a binding record exists, the SDK's
`commit_binding_operation_outcome(receipt, persist_outcome=...)` lets the owner
durably record that explicit failure before completing the lease; ordinary
runtime readback reconciliation is not possible without a binding record. If
the mutation reply was lost and a replay returns an in-flight receipt,
`reconcile_binding_operation_and_persist` waits for lifecycle-serialized status
readback; it accepts no binding record only for a failed first `activate`, and
the owner callback must durably record that outcome before completion. For an
existing binding, selecting a different installation through `activate` uses
the runtime's transactional switch path: if replacement startup fails, the
old installation identity remains recorded and its service is restarted when
it was active before the switch. A process crash leaves the durable admission
pending and blocks updates until the owner reconciles and completes it. Root
operator mutations without a Product-scoped admission are denied.
Idle bindings are not reported as active tasks; update readiness remains based
on task admissions and the actual supervisor/process state.

Descriptor/archive paths, dependency paths, worker environment values, and the
worker protocol are internal to this node-local seam. A Product adapter must
project only package identity/version, installation identity, verification,
binding policy, runtime status, and structured failure/remediation. It must not
project cache, staging or prepared-runtime paths, ZIP internals, executables, stdio,
PIDs, or runtime implementation details.

## Root-only offline first install

The first package install runs while Workspace holds a root-validated
`PACKAGE_ONLY` maintenance transaction. Workspace remains responsible for
verifying official release attestations. It writes a private request and
candidate files under `/var/lib/cyrene-updates/plugin-package-bootstrap/<request_id>/`:
directories are root-owned mode `0700`, and `request.json`, `descriptor.json`,
and `archive.zip` are root-owned single-link regular files mode `0600`. The
request is limited to 64 KiB and rejects unknown fields. The Catalog
`componentId` and descriptor `package.id` are separate identities; the hold
map is keyed by `componentId` and its value must equal the archive artifact
digest. The
package artifact digest is distinct from the `cy-package-runtime` host binary
digest.

The input object contains `schema_version: 1`, a safe `request_id`, a
`maintenance` object with `transaction_id`, `maintenance_token`,
`target_kind: "PACKAGE_ONLY"`, `plan_id`, `plan_digest`, the complete
`component_artifact_digests` map, `expected_gate_generation`, and the current
`expected_catalog_generation`. Its `candidate` object contains the exact
descriptor/archive paths in that request directory, Catalog `component_id`,
descriptor `package_id`, `package_version`, and the artifact, archive, descriptor,
manifest, and dependency-lock SHA-256 digests. Unknown fields such as a caller
assertion that proof passed are rejected.

```json
{
  "schema_version": 1,
  "request_id": "bootstrap-request-1",
  "maintenance": {
    "transaction_id": "active-package-hold-request-id",
    "maintenance_token": "<private hold token>",
    "target_kind": "PACKAGE_ONLY",
    "plan_id": "workspace-plan-id",
    "plan_digest": "sha256:<64 lowercase hex characters>",
    "component_artifact_digests": {
      "cyrene-tools-example-plugin": "sha256:<plugin package digest>"
    },
    "expected_gate_generation": 5,
    "expected_catalog_generation": 9
  },
  "candidate": {
    "descriptor_path": "/var/lib/cyrene-updates/plugin-package-bootstrap/bootstrap-request-1/descriptor.json",
    "archive_path": "/var/lib/cyrene-updates/plugin-package-bootstrap/bootstrap-request-1/archive.zip",
    "component_id": "cyrene-tools-example-plugin",
    "package_id": "org.example.plugin",
    "package_version": "1.2.3",
    "artifact_digest": "sha256:<plugin package digest>",
    "archive_digest": "sha256:<64 lowercase hex characters>",
    "descriptor_digest": "sha256:<64 lowercase hex characters>",
    "manifest_digest": "sha256:<64 lowercase hex characters>",
    "dependency_lock_digest": "sha256:<64 lowercase hex characters>"
  }
}
```

One `PACKAGE_ONLY` plan may install multiple packages: keep its complete
component-to-digest map and hold identity unchanged, invoke this command
sequentially with a fresh `request_id` and private request directory for each
candidate, and include each candidate's Catalog `componentId` in the map. A
repeat install of the same package, version, and artifact returns the existing
InstallationId. The Workspace owner persists the exact plan outcome before
closing the hold.

The root coordinator invokes the installed `cy-package-runtime` native binary
with `--root /var/lib/cyrene/package-runtime`, `--dependency-preparer
/usr/libexec/cyrene-plugin-python-preparer`, the pinned `--uv
/opt/cyrene/uv/0.12.21/uv` and `--python
/opt/cyrene/python/3.12.14/bin/python3.12` arguments passed as repeated
`--dependency-preparer-arg` pairs, `--bootstrap-install-offline`, and
`--bootstrap-input-file
/var/lib/cyrene-updates/plugin-package-bootstrap/<request_id>/request.json`.
The command requires effective UID 0, reads the Broker operator credential
from its fixed root-only file, confirms the daemon is stopped, acquires the
same process lock as the daemon, and calls `ValidateMaintenanceHold` before and
after installation. It copies only the public descriptor and archive into a
root-owned, cyrene-readable handoff under
`/run/cyrene-package-runtime-bootstrap/<request_id>/`, then runs the normal
Platform verification and installation code as `cyrene`. External attestation
verification remains Workspace-owned.

The one-shot command emits one JSON line. Success is
`{request_id, ok:true, result:{..., installation:<actual InstallationRecord>}}`;
failure is `{request_id, ok:false, error:{code,message,remediation}}`. The
receipt includes the exact hold identity and generations plus the actual
installation ID, version, and digests. It contains no hold token, operator
credential, operation token, or `connection_ref`. Success and failure both
leave the maintenance hold active. Workspace updates the exact installation
scope and derived source policy, starts the daemon, verifies its
source-authenticated `authority` response at the current generation, and only
then closes the hold.

## Root-only offline uninstall

Workspace may remove one exact package installation while holding a current
`PACKAGE_ONLY` maintenance transaction. First read the complete installation
record through the authenticated Package Runtime UDS `get_installation` call.
Copy its installation ID, package/version identity, and artifact, archive,
descriptor, manifest, and dependency-lock digests into the request below. The
`component_artifact_digests` entry for `component_id` must equal the installed
artifact digest. The operation rejects current or rollback references, retains
the content-addressed cache, and never removes Product user data stored outside
the Package Runtime installation tree.

Write this 64 KiB-bounded, unknown-field-rejecting request as a root-owned
single-link regular file with mode `0600` under the same private request
directory described above:

```json
{
  "schema_version": 1,
  "request_id": "uninstall-request-1",
  "maintenance": {
    "transaction_id": "active-package-hold-request-id",
    "maintenance_token": "<private hold token>",
    "target_kind": "PACKAGE_ONLY",
    "plan_id": "workspace-plan-id",
    "plan_digest": "sha256:<64 lowercase hex characters>",
    "component_artifact_digests": {
      "cyrene-tools-example-plugin": "sha256:<installed package digest>"
    },
    "expected_gate_generation": 5,
    "expected_catalog_generation": 9
  },
  "installation": {
    "component_id": "cyrene-tools-example-plugin",
    "installation_id": "installation-1234567890abcdef1234567890abcdef",
    "package_id": "org.example.plugin",
    "package_version": "1.2.3",
    "artifact_digest": "sha256:<installed package digest>",
    "archive_digest": "sha256:<64 lowercase hex characters>",
    "descriptor_digest": "sha256:<64 lowercase hex characters>",
    "manifest_digest": "sha256:<64 lowercase hex characters>",
    "dependency_lock_digest": "sha256:<64 lowercase hex characters>"
  }
}
```

Run the installed native command as root:

```sh
cy-package-runtime \
  --root /var/lib/cyrene/package-runtime \
  --dependency-preparer /usr/libexec/cyrene-plugin-python-preparer \
  --dependency-preparer-arg --uv \
  --dependency-preparer-arg /opt/cyrene/uv/0.12.21/uv \
  --dependency-preparer-arg --python \
  --dependency-preparer-arg /opt/cyrene/python/3.12.14/bin/python3.12 \
  --bootstrap-uninstall-offline \
  --bootstrap-input-file \
  /var/lib/cyrene-updates/plugin-package-bootstrap/<request_id>/request.json
```

The command requires effective UID 0, validates the current hold before and
after mutation, confirms the daemon is stopped, and takes the daemon's same
process lock. It runs filesystem removal as `cyrene`; no control-socket or
stdio mutation path is added. Success emits one secret-free JSON receipt with
the exact hold/generations and installed identity plus `already_absent`; this
flag is true only for a safe retry after the same installation was already
removed. Failure emits a structured error and leaves the hold active. Workspace
keeps the hold until package state and the source policy have been reconciled.
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

安装目录会先原子重命名移出已安装集合，然后才删除 payload。重命名失败时原安装身份保持不变，可安全重试；提交后清理失败的私有暂存内容会在 runtime 启动时回收。

## 节点本地控制进程

`cy-package-runtime --root <state-root> --dependency-preparer <path>` 负责一个节点上的生命周期和受监管 worker。生产模式在 `/run/cyrene-package-runtime/control.sock` 上提供经认证的 UDS 控制；每个连接只处理一个请求和响应，`--socket` 可指定其他绝对路径。源策略默认为 `/etc/cyrene/runtime-package-sources.json`，可通过 `--source-policy` 覆盖。所有连接共享唯一 runtime 和 supervisor。兼容性 `--stdio` 入口保留逐行只读/诊断 control wire；所有 package 或 binding mutation 都以 `MAINTENANCE_ADMISSION_REQUIRED` 拒绝。生产 binding mutation 只能使用经认证 UDS 和 broker admission；root-only bootstrap 命令也必须验证精确有效的 `PACKAGE_ONLY` hold，不会通过兼容 control wire 绕过。

UDS 请求沿用扁平 `operation` 结构，并增加认证信封和目录代次。首次 `authority` 请求可以省略 `catalog_generation`；结果返回 `authority: "platform_package_runtime"`、`protocol_version: "cy-package-runtime.control.v1"`、当前 generation 和 `capabilities`。只有本机 maintenance broker 使用 `cyrene.runtime-maintenance.broker.v1`、包含 `cyrene.runtime-maintenance.state.v2` 与 `cyrene.runtime-maintenance.binding-operations.v1` 两项必需 capability，并返回相同目录代次时，daemon 才发布自己的 `cy-package-runtime.binding-operation-admission.v1`。Broker capability 列表必须是有效且无重复的非空字符串；额外的有效 capability 允许存在，但不会由 Package Runtime 转发。缺少必需项或协议/代次不符时 capabilities 为空，所有 mutation fail closed。

`auth: {}` 仅允许 UID 为 root 的 Unix peer。Product 必须同时提供 `source_id`、`source_token` 和精确代次。Daemon 通过 `SO_PEERCRED` 将 UID/GID 与唯一源策略行比较，并对 token 执行常量时间 SHA-256 摘要比较。它不读取 root-only activity catalog，也不存储或记录 token。策略文件和所有父目录必须是 root 所有的真实路径且不可被 group/world 写入；策略文件必须是单链接普通文件，并且不能跟随符号链接打开。策略结构与上方英文示例相同：每个 source 精确绑定 UID/GID、token 摘要、generation，以及每个 binding 的 package、installation IDs 和操作列表。未匹配行、空 scope、代次差异或未知操作都会拒绝请求。

除 Product 外，cataloged standalone package owner 也能使用此 schema：使用自己的显式 source ID 与 binding ID、运行服务 UID/GID、受保护 token 的 SHA-256 摘要、精确 package/installation IDs 和必要操作。Broker 也必须将同一 source/binding owner 识别为 `AdmitBindingOperation` 的授权 owner；source policy 单独并不能授予 maintenance admission。策略文件只存 token 摘要，不存 token 明文或 token 文件路径。Installer/service 选择受保护 credential 路径，并通过 `PackageRuntimeClient.from_source_secret(source_id, token_path, ...)` 加载；调用进程必须匹配策略行中的 UID/GID。Binding ID 由 catalog 显式提供，不从 package ID 派生。

Product 操作不能安装包或选择路径。输入帧上限为 1 MiB，环境映射最多 256 项且合计不超过 256 KiB；每个连接读写超时为 5 秒，同时最多接受 32 个连接。

每个 binding 的 `operations` 只允许五个精确字符串：`activate`、`recover_binding`、`deactivate`、`runtime_status`、`get_installation`。`authority` 通过 source auth，但不是 binding scope operation。`runtime_status` 和 `get_installation` 是受 scope 约束的读取，不申请 Broker admission。升级或回滚时，对同一个 binding 分别用精确的新旧 installation ID 调用 `activate`；策略中的 installation ID 集合必须包含每个获准 ID。

Owner SDK 是 Python distribution `cyrene-runtime-maintenance==0.1.0`
（`requires-python >=3.11`，支持 Python 3.12.14），公开导入路径为
`from cyrene_runtime_maintenance import PackageRuntimeClient,
PackageRuntimeError`。Platform 不会把该 SDK 装入插件依赖解释器
`/opt/cyrene/python`；Workspace 应将同版本 wheel 随 updater/helper 环境
发布，并由 Workspace 选择该环境的 site-packages。不要把 SDK 源文件复制到
helper。

Owner 可直接把现有 transaction journal 回调接到以下 API。第一个回调在
UDS mutation 前持久化 request ID 和精确 scope；第二个回调先持久化真实状态、
`connection_ref` 和回执，然后 SDK 才完成 Broker admission：

```python
from cyrene_runtime_maintenance import PackageRuntimeClient, PackageRuntimeError

client = PackageRuntimeClient.from_source_secret(
    source_id,
    source_token_path,
    socket_path="/run/cyrene-package-runtime/control.sock",
    catalog_generation=catalog_generation,
    maintenance_socket_path="/run/cyrene/runtime-maintenance.sock",
)
client.authority()  # 代次不符时在事务开始前失败。

try:
    status = client.activate_and_persist(
        binding_id,
        installation_id,
        package_id=package_id,
        request_id=request_id,
        persist_intent=journal.persist_binding_intent,
        persist=journal.persist_binding_runtime_state,
    )
except PackageRuntimeError as error:
    receipt = error.pending_binding_operation
    if receipt is None:
        raise  # 没有已 admission 的 mutation receipt。
    if receipt.already_in_flight or journal.has_durable_binding(binding_id):
        client.reconcile_binding_operation_and_persist(
            receipt,
            reconcile=lambda status, installation, pending: journal.reconcile_binding_readback(
                status, installation, pending, failure_code=error.code
            ),
        )
    else:
        client.commit_binding_operation_outcome(
            receipt,
            persist_outcome=lambda pending: journal.persist_binding_failure(error.code, pending),
        )
    raise  # journal 与 Broker 已收敛；保留原操作失败。
```

`persist_binding_intent(request_id, scope)` 必须同步持久化两项参数、拒绝
scope 不同的 request ID 重放，并返回 `None`。`persist_binding_runtime_state(status,
receipt)` 必须同步持久化精确状态、connection reference 和回执并返回 `None`。
`reconcile_binding_readback(status_or_none, installation, receipt, failure_code=...)`
校验 journal intent 和安装身份后持久化观察结果并返回 `None`；已有 binding（例如
升级失败）必须走此读回路径以保留旧安装身份。仅首次 activate 在创建 binding
record 前失败时 `status_or_none` 才会是 `None`。`persist_binding_failure(failure_code,
receipt)` 必须同步持久化首次激活失败结果和回执并返回 `None`。这些回调必须是同步
函数；抛出异常或返回非 `None` 会让 Broker lease 保持 pending，供后续重试/对账。

Daemon 在改变运行时状态之前，先向 maintenance broker 原子申请 `activate`、`recover_binding`（broker operation 为 `recover`）或 `deactivate`。激活时从精确安装记录取得 package identity；恢复和停用时从持久 binding 记录取得 package 与 installation identity。成功响应保留真实 `RuntimeStatus` 字段，并增加 `result.binding_operation`，其中包含 request/source、精确 scope、catalog 与 gate generation、broker 协议版本、operation token 和重放标志。Product/standalone owner 必须先将状态与 `connection_ref` 和自己的 binding 记录一同持久化，再使用同一 source credential、scope、request ID 和 token 直接调用 broker 的 `CompleteBindingOperation`。Daemon 不会替 owner 完成 lease。

`already_in_flight` 重放返回 `BINDING_OPERATION_PENDING`，并把 receipt 放在 `error.binding_operation` 中；daemon 不会再次执行 mutation。`already_completed` 重放只读取新的真实运行时状态并返回 receipt。reservation 后发生的 mutation 错误也附带 receipt，供 owner 显式对账。如果首次 activate 在创建 binding record 前失败，SDK 的 `commit_binding_operation_outcome(receipt, persist_outcome=...)` 会先要求 owner 持久化明确失败结果，再完成 lease。若原mutation回执丢失，in-flight receipt可调用 `reconcile_binding_operation_and_persist`；其runtime status读回会与生命周期mutation串行，首次activate失败且没有binding record时传入 `None`，owner callback必须确认并持久化此结果。对已有 binding 选择不同 installation ID 时，`activate` 使用 runtime 的事务式切换；新版本启动失败会保留旧 installation identity，并在切换前处于活动状态时尝试恢复旧服务。进程崩溃会留下持久 pending admission，在 owner 对账并完成前阻止更新。没有 Product scope 的 root operator mutation 会被拒绝。空闲 binding 不会伪装成活动任务；更新 readiness 仍依据任务 admission 和真实 supervisor/进程状态。

描述符/压缩包路径、依赖路径、worker 环境值和 worker 协议都是此节点本地边界的内部内容。Product 适配器只能投影包身份/版本、安装身份、验证结果、绑定策略、运行时状态以及结构化失败/修复建议。不得投影缓存、暂存或准备后运行时路径、ZIP 内部信息、可执行文件、stdio、PID 或运行时实现细节。

## Root-only 离线首次安装

首次安装必须位于 Workspace 持有并经 root 验证的 `PACKAGE_ONLY` maintenance transaction 中。Workspace 仍负责官方 release attestation 验证。它会在 `/var/lib/cyrene-updates/plugin-package-bootstrap/<request_id>/` 下写入私有请求和候选文件：目录为 root 所有、权限 `0700`；`request.json`、`descriptor.json` 和 `archive.zip` 是 root 所有的单链接普通文件、权限 `0600`。请求上限为 64 KiB，未知字段会被拒绝；Catalog `componentId` 和 descriptor `package.id` 是不同身份，hold map 以 component ID 为键、artifact digest 为值。插件包的 artifact digest 与 `cy-package-runtime` 宿主二进制 digest 是两个不同值。

请求包含 `schema_version: 1`、安全的 `request_id`，以及 `maintenance` 对象中的 `transaction_id`、`maintenance_token`、`target_kind: "PACKAGE_ONLY"`、`plan_id`、`plan_digest`、完整 `component_artifact_digests` 映射、`expected_gate_generation` 和当前 `expected_catalog_generation`。`candidate` 对象包含该请求目录内的精确 descriptor/archive 路径、Catalog `component_id`、descriptor `package_id`、`package_version`，以及 artifact、archive、descriptor、manifest 和 dependency-lock 的 SHA-256 digest。包括“调用方证明已通过验签”在内的未知字段都会被拒绝。

英文部分的 JSON 示例展示完整字段结构；其中 `component_artifact_digests[component_id]` 必须等于 `candidate.artifact_digest`。一个完整 `PACKAGE_ONLY` hold 可覆盖多个 package candidate；Workspace 复用同一 transaction、token、plan、完整 digest map 和代次，逐个顺序调用 installer，每个候选使用新的 request ID 和私有请求目录。如果同一 plan 也更新宿主二进制，`cy-package-runtime` 使用自己的独立 component ID 和 digest 项。

Root coordinator 调用已安装的 `cy-package-runtime` native binary，传入 `--root /var/lib/cyrene/package-runtime`、`--dependency-preparer /usr/libexec/cyrene-plugin-python-preparer`、固定 `--uv /opt/cyrene/uv/0.12.21/uv` 和 `--python /opt/cyrene/python/3.12.14/bin/python3.12` 参数（每项都以重复的 `--dependency-preparer-arg` 和独立 argv 传入）、`--bootstrap-install-offline` 以及 `--bootstrap-input-file /var/lib/cyrene-updates/plugin-package-bootstrap/<request_id>/request.json`。命令要求 effective UID 0，从固定 root-only 文件读取 Broker operator credential，确认 daemon 已停止，取得与 daemon 相同的 process lock，并在安装前后调用 `ValidateMaintenanceHold`。它只把公开 descriptor/archive 复制到 `/run/cyrene-package-runtime-bootstrap/<request_id>/` 下 root 所有且 cyrene 可读的 handoff，再以 `cyrene` 身份运行 Platform 原有校验和安装逻辑。官方 attestation 验签仍由 Workspace 负责。

一次性命令输出一行 JSON。成功格式为 `{request_id, ok:true, result:{..., installation:<实际 InstallationRecord>}}`；失败格式为 `{request_id, ok:false, error:{code,message,remediation}}`。回执包含精确 hold 身份、代次和真实安装 ID、版本及摘要，不包含 hold token、operator credential、operation token 或 `connection_ref`。成功或失败都会保留 maintenance hold。Workspace 更新精确 installation scope 和派生 source policy、启动 daemon、以 source auth 在当前代次验证 `authority` 响应后，才关闭 hold。

## Root-only 离线卸载

Workspace 可以在有效的 `PACKAGE_ONLY` maintenance transaction 中删除一个精确 package installation。先通过已认证的 Package Runtime UDS `get_installation` 读取完整安装记录，再将 installation ID、package/version 身份、artifact、archive、descriptor、manifest 和 dependency-lock 摘要写入请求。`component_artifact_digests[component_id]` 必须与已安装 artifact 摘要相同。仍被当前或回滚 binding 引用的安装会被拒绝；缓存会保留；Package Runtime 安装目录之外的 Product 用户数据不会被删除。

写入 64 KiB 上限、拒绝未知字段的请求文件；文件应位于前文相同的私有 request 目录中，并由 root 拥有、为单链接普通文件、权限 `0600`。请求 schema 与英文 JSON 示例相同，根对象含 `schema_version: 1`、`request_id`、`maintenance` 和 `installation`。`maintenance` 含当前 transaction/token、`PACKAGE_ONLY`、plan 身份、完整 artifact map 及 gate/catalog 代次；`installation` 含 component/package/version、精确 installation ID 和五项完整摘要字段。

Root coordinator 运行已安装 native command：

```sh
cy-package-runtime \
  --root /var/lib/cyrene/package-runtime \
  --dependency-preparer /usr/libexec/cyrene-plugin-python-preparer \
  --dependency-preparer-arg --uv \
  --dependency-preparer-arg /opt/cyrene/uv/0.12.21/uv \
  --dependency-preparer-arg --python \
  --dependency-preparer-arg /opt/cyrene/python/3.12.14/bin/python3.12 \
  --bootstrap-uninstall-offline \
  --bootstrap-input-file \
  /var/lib/cyrene-updates/plugin-package-bootstrap/<request_id>/request.json
```

命令要求 effective UID 0，在 mutation 前后验证当前 hold，确认 daemon 已停止，并取得与 daemon 相同的 process lock；文件系统卸载逻辑以 `cyrene` 身份运行，不增加 UDS/stdin mutation 入口。成功时输出不含秘密的 JSON 回执，其中含精确 hold/代次、安装身份和 `already_absent`；仅当同一安装已被删除且本次是安全重试时，该值为 true。失败返回结构化错误并保留 hold。Workspace 完成 package state 和 source policy 对账后再关闭 hold。
