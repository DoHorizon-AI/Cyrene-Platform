# Sidecar source | Sidecar 源码

`main.rs` is the loopback-only gRPC composition root. It loads the external
credential bundle and local bearer token before opening the listener.

`main.rs` 是仅 loopback 的 gRPC 组合入口；在打开 listener 前加载外部 credential bundle 与本地 bearer token。
