# Relay Host source | Relay Host 源码

`main.rs` is the Relay process composition root. It validates startup inputs,
starts the Relay and health listeners, and preserves the Platform authorization
and fail-closed readiness decisions.

`main.rs` 是 Relay 进程组合入口，负责校验启动输入、启动 Relay 与 health listener，并保留
Platform 的授权与 fail-closed readiness 决策。
