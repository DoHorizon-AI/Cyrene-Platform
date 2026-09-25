# Platform Plugin Control Contract v1

Platform owns only the generic control-plane projection used to select and
supervise a Plugin. `Cyrene-Plugins-Official` owns `plugin.manifest.json` and all
capability payload contracts.

The Platform projection contains:

- Plugin identity and release version;
- opaque capability id and interface version;
- `INLINE`, `WORKER`, or `SERVICE` placement compatibility;
- optional immutable artifact reference;
- optional package launch command for a managed service;
- deterministic PluginSet resolution and lock evidence.

For package activation, the manifest's `runtime.launch` contains an executable
and arguments. `prepared-runtime` selects the executable produced by the
configured dependency preparer; any other value is a safe package-relative
path. Platform appends only `--capability`, `--interface-version`, and `--listen`
for the generic readiness handshake. The process publishes a bounded readiness
record containing the same capability/interface plus an opaque
`connection_ref`.

No capability method name, payload schema, Product state, language taxonomy,
provider type, or business policy is copied into the Platform model. A Product
uses the Plugin-owned SDK or wire contract to call `connection_ref` directly.
---

<!-- Chinese Translation / 中文翻译 -->

# Platform Plugin 控制契约 v1

Platform 只拥有用于选择并监管 Plugin 的通用控制平面投影。plugin.manifest.json 及所有 capability payload 契约由 Cyrene-Plugins-Official 拥有。

Platform 投影包含：

- Plugin 身份与 release 版本；
- 不透明 capability ID 与接口版本；
- INLINE、WORKER 或 SERVICE 部署位置兼容性；
- 可选的不可变 artifact 引用；
- 托管 service 可选的 package 启动命令；
- 确定性的 PluginSet 解析与锁定证据。

启用 package 时，manifest 中的 runtime.launch 包含可执行文件及参数。prepared-runtime 表示选择配置的依赖准备器生成的可执行文件；其他值都是相对于 package 的安全路径。Platform 只追加 --capability、--interface-version 和 --listen，用于通用 readiness handshake。进程会发布有界 readiness 记录，其中包含相同的 capability/interface 以及一个不透明 connection_ref。

Platform 模型不会复制 capability 方法名、payload schema、Product 状态、语言分类、provider 类型或业务策略。Product 使用 Plugin 所有的 SDK 或 wire 契约，直接调用 connection_ref。
