# cy-workspace-fabric

`cy-workspace-fabric` provides the product-neutral Account/Directory boundary,
transport-neutral Workspace connection descriptor, stable Workspace API port,
an authenticated private mTLS direct endpoint, and the outbound mTLS
application relay used by the v1 reference vertical. Candidate selection tries
`LAN_DIRECT` before `RELAY` according to descriptor priority.

本 crate 提供产品无关的 Account/Directory 边界、transport-neutral Workspace
连接描述符、稳定 Workspace API port、私网 mTLS 直连端点与 v1 参考纵向使用的出站
mTLS 应用层 Relay。候选选择按描述符优先级先尝试 `LAN_DIRECT`，再选择 `RELAY`。

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
| `src/persistent_directory.rs` | Private, single-owner snapshot storage for memberships and descriptors. | 成员关系与描述符的私有单实例快照存储。 |
| `src/api.rs` | Stable frontend-to-Workspace API port and LOCAL adapter. | 稳定 frontend API port 与 LOCAL adapter。 |
| `src/direct.rs` | Workspace API endpoint with session and membership checks. | 校验会话与成员关系的 Workspace API 直连端点。 |
| `src/relay.rs` | Live application request routing without Workspace authority. | 不拥有 Workspace 权威的实时应用请求路由。 |
| `src/transport.rs` | Direct candidate selection and outbound mTLS Relay transport. | 直连候选选择与出站 mTLS Relay 传输。 |
| `src/bin/` | Real acceptance relay, connector, and reference frontend. | 真实验收 Relay、Connector 与参考 frontend。 |

`FileWorkspaceDirectory::open` needs a private directory on a persistent volume
with one active owner. `replace` publishes a complete membership and descriptor
revision atomically. It stores no session credentials and does not itself run a
Directory service.

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
| `src/persistent_directory.rs` | 成员关系与描述符的私有单实例快照存储；需挂载持久卷。 |
| `src/api.rs` | 稳定的 frontend-to-Workspace API port 与 LOCAL adapter。 |
| `src/direct.rs` | 校验会话与成员关系的 Workspace API 私网直连端点。 |
| `src/relay.rs` | 不拥有 Workspace authority 的实时应用请求路由。 |
| `src/transport.rs` | 直连候选选择、出站 mTLS Relay client 和 connector session。 |
| `src/bin/` | 真实 acceptance relay、connector 和参考 frontend。 |

`FileWorkspaceDirectory::open` 需要挂载在持久卷上的私有目录，同一时间仅允许一个实例持有。
`replace` 原子发布完整的成员关系和描述符版本。该存储不包含会话凭据，也不自行运行 Directory 服务。

运行：

```bash
cargo test --locked -p cy-workspace-fabric
cargo clippy --locked -p cy-workspace-fabric --all-targets -- -D warnings
```
