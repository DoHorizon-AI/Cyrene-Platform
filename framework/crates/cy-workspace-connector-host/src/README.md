# Connector Host source | Connector Host 源码

`main.rs` owns the outbound process lifecycle, private-file validation, TLS
material checks, and bounded reconnect loop. `run_host()` loads the release
lock pins, verifies the mounted owner bundle and Platform policy, loads exact
owner/organization/Workspace HTTPS endpoints, then composes the generic HTTP
adapter with the control plane. Product routing comes from pinned OpenAPI
catalogs; current caller membership and Platform policy remain control-plane
checks.

`main.rs` 管理出站进程生命周期、私有文件校验、TLS material 检查和有界重连。`run_host()`
加载 release lock pins，校验挂载的 owner bundle 与 Platform policy，读取精确绑定 owner、组织和
Workspace 的 HTTPS endpoint，再将通用 HTTP adapter 与 control plane 组合。Product 路由来自固定
OpenAPI catalog；当前调用者 membership 和 Platform policy 仍由 control plane 检查。
