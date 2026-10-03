# cy-workspace-fabric binaries | cy-workspace-fabric 可执行程序

This directory contains only the legacy V1 fabric acceptance fixture and the
restricted Directory provisioning CLI. The fixture is not a production
connection entrypoint. Current Connector and Relay runtime packages are owned
by `Cyrene-Plugins-Official` under `runtime/rust/`; the Platform Authority host
remains Platform-owned.

| File | Responsibility |
| --- | --- |
| `cy-workspace-fabric-fixture.rs` | Acceptance-only Relay and Connector fixture. |
| `cy-workspace-directory-admin.rs` | Restricted local PostgreSQL Directory provisioning CLI. |

The fixture preserves old Fabric protocol behavior for scoped compatibility
checks only. It does not validate the Plugins-owned Connector or Relay runtime,
Platform Authority deployment, device approval, or production connectivity.

本目录只包含旧 V1 fabric 验收 fixture 与受限 Directory 配置 CLI。fixture 不是生产连接入口。
当前 Connector 与 Relay runtime package 由 `Cyrene-Plugins-Official` 的 `runtime/rust/` 持有；
Platform Authority host 仍由 Platform 持有。

| 文件 | 职责 |
| --- | --- |
| `cy-workspace-fabric-fixture.rs` | 仅用于验收的 Relay 与 Connector fixture。 |
| `cy-workspace-directory-admin.rs` | 受限的本地 PostgreSQL Directory 配置 CLI。 |

fixture 仅保留旧 Fabric 协议行为以供范围明确的兼容检查；它不验证 Plugins 所有的 Connector
或 Relay runtime、Platform Authority 部署、设备审批或生产连接。
