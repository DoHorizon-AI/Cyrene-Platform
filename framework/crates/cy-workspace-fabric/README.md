# cy-workspace-fabric

`cy-workspace-fabric` provides the product-neutral Account/Directory boundary,
transport-neutral Workspace connection descriptor, stable Workspace API port,
and outbound mTLS application relay used by the v1 reference vertical.

本 crate 提供产品无关的 Account/Directory 边界、transport-neutral Workspace
连接描述符、稳定 Workspace API port 与 v1 参考纵向使用的出站 mTLS 应用层 Relay。

It consumes the existing Execution Fabric connectivity provider and semantic
Operation identity. It does not own Product state, Lease/Fence, Runtime state,
Artifact identity/content, or user credential issuance.

它消费既有 Execution Fabric connectivity provider 与 semantic Operation
identity；不拥有 Product 状态、Lease/Fence、Runtime 状态、Artifact identity/content
或用户凭据签发。

## Layout / 目录

| Entry | Responsibility | 职责 |
| --- | --- | --- |
| `src/auth.rs` | Replaceable short-lived relay authentication seam. | 可替换的短期 Relay 认证 seam。 |
| `src/directory.rs` | Membership-scoped Workspace discovery. | 基于成员关系的 Workspace 发现。 |
| `src/api.rs` | Stable frontend-to-Workspace API port and LOCAL adapter. | 稳定 frontend API port 与 LOCAL adapter。 |
| `src/relay.rs` | Live application request routing without Workspace authority. | 不拥有 Workspace 权威的实时应用请求路由。 |
| `src/transport.rs` | Outbound mTLS Relay client and connector session. | 出站 mTLS Relay client 与 connector session。 |
| `src/bin/` | Real acceptance relay, connector, and reference frontend. | 真实验收 Relay、Connector 与参考 frontend。 |

Run:

```bash
cargo test --locked -p cy-workspace-fabric
cargo clippy --locked -p cy-workspace-fabric --all-targets -- -D warnings
```
---

<!-- Chinese Translation / 中文翻译 -->

# cy-workspace-fabric

`cy-workspace-fabric` 提供与 Product 无关的 Account/Directory 边界、transport-neutral Workspace connection descriptor、稳定 Workspace API port，以及 v1 参考纵向使用的出站 mTLS application relay。

它使用既有 Execution Fabric connectivity provider 和 semantic Operation identity。不拥有 Product 状态、Lease/Fence、Runtime 状态、Artifact identity/content 或用户凭据签发。

## 目录结构

| 条目 | 职责 |
|---|---|
| `src/auth.rs` | 可替换的短时 Relay 认证边界。 |
| `src/directory.rs` | 以成员关系为作用域的 Workspace 发现。 |
| `src/api.rs` | 稳定的 frontend-to-Workspace API port 与 LOCAL adapter。 |
| `src/relay.rs` | 不拥有 Workspace authority 的实时应用请求路由。 |
| `src/transport.rs` | 出站 mTLS Relay client 和 connector session。 |
| `src/bin/` | 真实 acceptance relay、connector 和参考 frontend。 |

运行：

```bash
cargo test --locked -p cy-workspace-fabric
cargo clippy --locked -p cy-workspace-fabric --all-targets -- -D warnings
```
