# cy-workspace-fabric source map | 源码导航

| File | Responsibility | 职责 |
| --- | --- | --- |
| `lib.rs` | Public product-neutral surface. | 产品无关公共接口。 |
| `auth.rs` | User/device session separation and verifier port. | User/device 会话分离与 verifier port。 |
| `directory.rs` | Membership and descriptor validation. | 成员关系与 descriptor 校验。 |
| `api.rs` | Workspace-owned API port and LOCAL adapter. | Workspace 权威 API port 与 LOCAL adapter。 |
| `relay.rs` | Ephemeral authenticated routing. | 临时认证路由。 |
| `transport.rs` | Outbound mTLS client transport. | 出站 mTLS client transport。 |
| `bin/` | Acceptance-only executable fixture. | 仅用于 acceptance 的可执行 fixture。 |

Relay and Directory code must never become Product, execution, Lease/Event,
or Artifact identity authorities.

Relay 与 Directory 代码不得成为 Product、Execution、Lease/Event 或 Artifact
identity 权威。
