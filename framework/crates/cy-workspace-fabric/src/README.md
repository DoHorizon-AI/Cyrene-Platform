# cy-workspace-fabric source map | 源码导航

| File | Responsibility | 职责 |
| --- | --- | --- |
| `lib.rs` | Public product-neutral surface. | 产品无关公共接口。 |
| `auth.rs` | User/device session separation and verifier port. | User/device 会话分离与 verifier port。 |
| `caller.rs` | Verified caller principal, Workspace scope, and Directory-derived roles. | 已验证 caller principal、Workspace scope 与 Directory 派生角色。 |
| `device_registry.rs` | Import and revocation port for approved device certificates. | 已批准设备证书的导入与撤销接口。 |
| `directory.rs` | Membership and descriptor validation. | 成员关系与 descriptor 校验。 |
| `persistent_directory.rs` | Private, single-owner snapshot storage for Directory and device records. | Directory 与设备记录的私有单实例快照存储。 |
| `api.rs` | Workspace-owned API port, LOCAL adapter, and bounded gRPC server builders. | Workspace 权威 API port、LOCAL adapter 与有界 gRPC server 构造器。 |
| `control_plane.rs` | Workspace authority-pinned API handler and Product dispatch port. | 固定 Workspace 权威的 API handler 与 Product 分发 port。 |
| `product_projection.rs` | Closed Product API operation map, bounded JSON projection, and invocation port. | 封闭 Product API 操作映射、有界 JSON 投影及 invocation port。 |
| `direct.rs` | Session- and membership-checked private API endpoint. | 校验会话与成员关系的私网 API 端点。 |
| `relay.rs` | Ephemeral authenticated routing. | 临时认证路由。 |
| `transport.rs` | Direct candidate selection and outbound mTLS transport. | 直连候选选择与出站 mTLS 传输。 |
| `bin/` | Acceptance-only executable fixture. | 仅用于 acceptance 的可执行 fixture。 |

Relay and Directory code must never become Product, execution, Lease/Event,
or Artifact identity authorities.

Relay 与 Directory 代码不得成为 Product、Execution、Lease/Event 或 Artifact
identity 权威。
---

<!-- Chinese Translation / 中文翻译 -->

# cy-workspace-fabric 源码导航

| 文件 | 职责 |
|---|---|
| `lib.rs` | 与 Product 无关的公共接口。 |
| `auth.rs` | 用户/设备会话分离与 verifier port。 |
| `caller.rs` | 已验证调用者身份、Workspace scope 与 Directory 派生角色。 |
| `device_registry.rs` | 已批准设备证书的导入与撤销接口。 |
| `directory.rs` | 成员关系和 descriptor 校验。 |
| `persistent_directory.rs` | Directory 与设备记录的私有单实例持久化快照存储。 |
| `api.rs` | Workspace 所有的 API port、LOCAL adapter 与有界 gRPC server 构造器。 |
| `control_plane.rs` | 固定 Workspace 权威的 API handler 与 Product 分发 port。 |
| `product_projection.rs` | 封闭 Product API 操作映射、有界 JSON 投影及 invocation port。 |
| `direct.rs` | 校验会话与成员关系的私网 API 端点。 |
| `relay.rs` | 临时认证路由。 |
| `transport.rs` | 直连候选选择与出站 mTLS 传输。 |
| `bin/` | 仅用于 acceptance 的可执行 fixture。 |

Relay 和 Directory 代码绝不能成为 Product、execution、Lease/Event 或 Artifact identity authority。
