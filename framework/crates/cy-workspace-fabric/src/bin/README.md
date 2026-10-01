# cy-workspace-fabric binaries | cy-workspace-fabric 可执行程序

This directory contains only the fabric acceptance fixture and the restricted
Directory provisioning CLI. Production Relay, Connector, and Sidecar hosts are
separate Cargo packages under `framework/crates/`.

| File | Responsibility |
| --- | --- |
| `cy-workspace-fabric-fixture.rs` | Acceptance-only Relay and Connector fixture. |
| `cy-workspace-directory-admin.rs` | Restricted local PostgreSQL Directory provisioning CLI. |

Read the Relay package's [`RUNTIME.md`](../../../cy-workspace-relay-host/RUNTIME.md)
for Relay runtime settings. Connector package and identity requirements are in
[`WORKSPACE_CONNECTOR_HOST.md`](../../WORKSPACE_CONNECTOR_HOST.md).

本目录只包含 fabric acceptance fixture 与受限 Directory 配置 CLI。生产 Relay、Connector
和 Sidecar host 是位于 `framework/crates/` 下的独立 Cargo package。

| 文件 | 职责 |
| --- | --- |
| `cy-workspace-fabric-fixture.rs` | 仅用于验收的 Relay 与 Connector fixture。 |
| `cy-workspace-directory-admin.rs` | 受限的本地 PostgreSQL Directory 配置 CLI。 |

Relay 运行配置见 Relay package 的 [`RUNTIME.md`](../../../cy-workspace-relay-host/RUNTIME.md)。
Connector package 与 identity 要求见 [`WORKSPACE_CONNECTOR_HOST.md`](../../WORKSPACE_CONNECTOR_HOST.md)。
