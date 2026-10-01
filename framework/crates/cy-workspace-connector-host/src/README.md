# Connector Host source | Connector Host 源码

`main.rs` owns the outbound process lifecycle, private-file validation, TLS
material checks, and bounded reconnect loop. Product mapping is composed via
the shared Platform library and remains subject to its authorization gates.

`main.rs` 管理出站进程生命周期、私有文件校验、TLS material 检查和有界重连；Product
映射由共享 Platform library 组合，并继续受其授权 gate 约束。
