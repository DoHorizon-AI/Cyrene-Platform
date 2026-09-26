# cy-workspace-fabric source map | 源码导航

| File | Responsibility | 职责 |
| --- | --- | --- |
| `lib.rs` | Public product-neutral surface. | 产品无关公共接口。 |
| `auth.rs` | User/device session separation and verifier port. | User/device 会话分离与 verifier port。 |
| `caller.rs` | Verified caller principal, Workspace scope, and Directory-derived roles. | 已验证 caller principal、Workspace scope 与 Directory 派生角色。 |
| `aca_forwarded_certificate.rs` | Explicit ACA XFCC parsing and Rustls-webpki client-certificate validation. | 显式 ACA XFCC 解析与 Rustls-webpki 客户端证书校验。 |
| `device_auth.rs` | Tonic TLS peer-certificate extraction and registry-backed connector verification. | Tonic TLS 对端证书提取与注册表驱动的 Connector 校验。 |
| `device_registry.rs` | Import and revocation port for approved device certificates. | 已批准设备证书的导入与撤销接口。 |
| `directory.rs` | Membership and descriptor validation. | 成员关系与 descriptor 校验。 |
| `persistent_directory.rs` | Private, single-owner snapshot storage for Directory and device records. | Directory 与设备记录的私有单实例快照存储。 |
| `durable_directory.rs` | Async PostgreSQL Directory and stable device-registration authority. | 异步 PostgreSQL Directory 与稳定设备 registration authority。 |
| `api.rs` | Workspace-owned API port, LOCAL adapter, and bounded gRPC server builders. | Workspace 权威 API port、LOCAL adapter 与有界 gRPC server 构造器。 |
| `control_plane.rs` | Workspace authority-pinned API handler and Product dispatch port. | 固定 Workspace 权威的 API handler 与 Product 分发 port。 |
| `product_authorization.rs` | Versioned fail-closed Product read and command role matrix. | 版本化且默认拒绝的 Product 读取与命令角色矩阵。 |
| `product_projection.rs` | Closed Product API operation map, bounded JSON projection, and invocation port. | 封闭 Product API 操作映射、有界 JSON 投影及 invocation port。 |
| `direct.rs` | Session- and membership-checked private API endpoint. | 校验会话与成员关系的私网 API 端点。 |
| `relay.rs` | Ephemeral authenticated routing. | 临时认证路由。 |
| `transport.rs` | Direct candidate selection and outbound mTLS transport. | 直连候选选择与出站 mTLS 传输。 |
| `sidecar.rs` | External-credential consumer and authenticated local gRPC bridge. | 外部凭据 consumer 与本地认证 gRPC 代理。 |
| `user_code_secret.rs` | Versioned HMAC key ring for user-code storage and verification. | user code 存储与校验使用的带版本 HMAC key ring。 |
| `bin/` | Acceptance fixtures, Workspace sidecar, and restricted Directory provisioning CLI. | Acceptance fixture、Workspace sidecar 与受限 Directory 配置 CLI。 |

`cy-workspace-directory-admin` uses separate operator and migration database URLs. Membership/role/descriptor writes and audit records commit atomically; the runtime reader role is SELECT-only. Device registration uses a separate registrar role and stores only a domain-separated digest of its 256-bit recovery credential. The binding adapter alone does not enable enrollment start or approval; production must compose binding and authorization-state CAS under one PostgreSQL transaction and device-row lock. OIDC verification, database users/secrets/deployment, and production host configuration remain external work.

Relay and Directory code must never become Product, execution, Lease/Event,
or Artifact identity authorities. A Workspace connector is accepted only when
either its Tonic TLS peer certificate or the explicitly configured ACA XFCC
certificate matches an approved, non-revoked device record; frontend direct
requests remain user-session and membership checked.

The ACA forwarded-certificate adapter is opt-in and requires a configured
private client-CA trust bundle. It must only be selected behind ACA HTTP/2
ingress, where Envoy overwrites client-supplied XFCC metadata. It validates the
forwarded chain and client-auth EKU before using the same device registry
approval, revocation, and Workspace scope checks. Ordinary ingress never uses
XFCC as an identity source.

The registered device certificate authenticates the connector to the Relay; it
does not authenticate the Relay service to a Workspace receiver. A receiver
that trusts forwarded `caller_roles` must use a Relay session whose server
certificate chains to its configured Relay CA and matches the configured
server name. Direct requests remain Frontend user-session requests.

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
| `aca_forwarded_certificate.rs` | 仅在显式 ACA 模式中解析 XFCC 并校验客户端证书链。 |
| `device_auth.rs` | Tonic TLS 对端证书提取与注册表驱动的 Connector 校验。 |
| `device_registry.rs` | 已批准设备证书的导入与撤销接口。 |
| `directory.rs` | 成员关系和 descriptor 校验。 |
| `persistent_directory.rs` | Directory 与设备记录的私有单实例持久化快照存储。 |
| `durable_directory.rs` | 异步 PostgreSQL Directory 与稳定设备 registration authority。 |
| `api.rs` | Workspace 所有的 API port、LOCAL adapter 与有界 gRPC server 构造器。 |
| `control_plane.rs` | 固定 Workspace 权威的 API handler 与 Product 分发 port。 |
| `product_authorization.rs` | 版本化且默认拒绝的 Product 读取与命令角色矩阵。 |
| `product_projection.rs` | 封闭 Product API 操作映射、有界 JSON 投影及 invocation port。 |
| `direct.rs` | 校验会话与成员关系的私网 API 端点。 |
| `relay.rs` | 临时认证路由。 |
| `transport.rs` | 直连候选选择与出站 mTLS 传输。 |
| `user_code_secret.rs` | user code 存储与校验使用的带版本 HMAC key ring。 |
| `bin/` | Acceptance fixture、Workspace sidecar 与受限 Directory 配置 CLI。 |

`cy-workspace-directory-admin` 使用分离的 operator 和 migration 数据库 URL。成员/角色/descriptor 变更与审计记录原子提交；服务 reader 角色仅能 SELECT。设备 registration 使用单独 registrar 角色，数据库只保存 256-bit recovery credential 的 domain-separated digest。仅有 binding adapter 不会启用 enrollment start 或 approval；生产使用必须在同一 PostgreSQL transaction 和 device 行锁下组合 binding 与授权状态 CAS。OIDC 验证、数据库用户/secret/部署，以及 production host 配置仍属于外部工作。

Relay 和 Directory 代码绝不能成为 Product、execution、Lease/Event 或 Artifact identity authority。Workspace Connector 仅在 Tonic TLS 对端证书或显式配置的 ACA XFCC 证书匹配已批准且未撤销的设备记录时接受；frontend 直连仍校验用户会话和成员关系。

已注册设备证书只认证 Connector 到 Relay 的客户端身份，不认证 Relay 服务到 Workspace 接收端的服务端身份。信任转发 `caller_roles` 的接收端必须使用服务端证书由配置的 Relay CA 验证且匹配配置的 server name 的 Relay 会话。Direct 请求仍使用 Frontend 用户会话。

ACA 转发证书适配器需显式启用并配置私有客户端 CA 信任 bundle。它只可用于 ACA HTTP/2 ingress 后方，因为 Envoy 会覆盖客户端提交的 XFCC metadata。适配器先校验证书链和 client-auth EKU，再复用设备注册表批准、撤销与 Workspace scope 校验。普通 ingress 绝不把 XFCC 当作身份来源。
