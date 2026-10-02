# cy-workspace-fabric source map | 源码导航

| File | Responsibility | 职责 |
| --- | --- | --- |
| `lib.rs` | Public product-neutral surface. | 产品无关公共接口。 |
| `auth.rs` | User/device session separation and verifier port. | User/device 会话分离与 verifier port。 |
| `caller.rs` | Verified caller principal, Workspace scope, and Directory-derived roles. | 已验证 caller principal、Workspace scope 与 Directory 派生角色。 |
| `aca_forwarded_certificate.rs` | Explicit ACA XFCC parsing and Rustls-webpki client-certificate validation. | 显式 ACA XFCC 解析与 Rustls-webpki 客户端证书校验。 |
| `device_auth.rs` | Tonic TLS peer-certificate extraction and registry-backed connector verification. | Tonic TLS 对端证书提取与注册表驱动的 Connector 校验。 |
| `device_registry.rs` | Import and revocation port for approved device certificates. | 已批准设备证书的导入与撤销接口。 |
| `directory.rs` | Membership port; descriptor validation is shared with the client SDK. | 成员关系 port；descriptor 校验由 client SDK 共享。 |
| `persistent_directory.rs` | Private, single-owner snapshot storage for Directory and device records. | Directory 与设备记录的私有单实例快照存储。 |
| `durable_directory.rs` | Async PostgreSQL Directory and stable device-registration authority. | 异步 PostgreSQL Directory 与稳定设备 registration authority。 |
| `api.rs` | Workspace-owned API port, LOCAL adapter, and bounded gRPC server builders. | Workspace 权威 API port、LOCAL adapter 与有界 gRPC server 构造器。 |
| `control_plane.rs` | Workspace authority-pinned API handler and Product dispatch port. | 固定 Workspace 权威的 API handler 与 Product 分发 port。 |
| `product_authorization.rs` | Versioned fail-closed Product read and command role matrix. | 版本化且默认拒绝的 Product 读取与命令角色矩阵。 |
| `product_projection.rs` | Closed Product API operation map, bounded JSON projection, and invocation port. | 封闭 Product API 操作映射、有界 JSON 投影及 invocation port。 |
| `direct.rs` | Session- and membership-checked private API endpoint. | 校验会话与成员关系的私网 API 端点。 |
| `frontend_relay_client.rs` | Verified-principal Web Frontend client for the outbound mTLS Relay session. | 基于已验证主体的 Web Frontend 出站 mTLS Relay 客户端。 |
| `relay.rs` | Ephemeral authenticated routing. | 临时认证路由。 |
| `transport.rs` | Connector-facing Relay session service path; outbound client transport is in the SDK. | Connector 侧 Relay session service path；出站 client transport 位于 SDK。 |
| `../../cy-workspace-client-sdk/` | Lightweight outbound Workspace discovery and Relay client. | 轻量级 Workspace 发现与 Relay 出站 client。 |
| `../../cy-workspace-sidecar/src/sidecar.rs` | Local gRPC bridge implementation with no Fabric dependency. | 不依赖 Fabric 的本地 gRPC bridge 实现。 |
| `user_code_secret.rs` | Versioned HMAC key ring for user-code storage and verification. | user code 存储与校验使用的带版本 HMAC key ring。 |
| `bin/` | Acceptance fixtures and restricted Directory provisioning CLI. | Acceptance fixture 与受限 Directory 配置 CLI。 |

`cy-workspace-directory-admin` uses separate operator and migration database URLs. Membership/role/descriptor writes and audit records commit atomically; the runtime reader role is SELECT-only. Device registration uses a separate registrar role and stores only a domain-separated digest of its 256-bit recovery credential. The binding adapter alone does not enable enrollment start or approval; production must compose binding and authorization-state CAS under one PostgreSQL transaction and device-row lock. OIDC verification, database users/secrets/deployment, and production host configuration remain external work.

The Web Frontend Relay client accepts only a `VerifiedWebPrincipal` and a trusted `WebRelaySessionCredentialIssuer`; it constructs a user-scoped Frontend `RelayHello` internally. Its mTLS certificate is the BFF workload identity and must remain separate from the registered Workspace device identity. It checks Directory descriptor organization, expiry, and exact configured Relay endpoint/SNI before forwarding requests. The response contract has no Workspace ID field, so response Workspace attribution relies on the authenticated Relay's Workspace route binding and pending-request correlation. See [`WEB_FRONTEND_RELAY_CLIENT.md`](../WEB_FRONTEND_RELAY_CLIENT.md) for the host wiring boundary.

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
| `directory.rs` | 成员关系 port；descriptor 校验与 client SDK 共享。 |
| `persistent_directory.rs` | Directory 与设备记录的私有单实例持久化快照存储。 |
| `durable_directory.rs` | 异步 PostgreSQL Directory 与稳定设备 registration authority。 |
| `api.rs` | Workspace 所有的 API port、LOCAL adapter 与有界 gRPC server 构造器。 |
| `control_plane.rs` | 固定 Workspace 权威的 API handler 与 Product 分发 port。 |
| `product_authorization.rs` | 版本化且默认拒绝的 Product 读取与命令角色矩阵。 |
| `product_projection.rs` | 封闭 Product API 操作映射、有界 JSON 投影及 invocation port。 |
| `direct.rs` | 校验会话与成员关系的私网 API 端点。 |
| `frontend_relay_client.rs` | 基于已验证主体的 Web Frontend 出站 mTLS Relay 客户端。 |
| `relay.rs` | 临时认证路由。 |
| `transport.rs` | Connector 侧 Relay session service path；出站 client transport 位于 SDK。 |
| `../../cy-workspace-client-sdk/` | 轻量级 Workspace 发现与 Relay 出站 client。 |
| `../../cy-workspace-sidecar/src/sidecar.rs` | 不依赖 Fabric 的本地 gRPC bridge 实现。 |
| `user_code_secret.rs` | user code 存储与校验使用的带版本 HMAC key ring。 |
| `bin/` | Acceptance fixture 与受限 Directory 配置 CLI。 |

Web Frontend Relay 客户端只接受 `VerifiedWebPrincipal` 和可信的 `WebRelaySessionCredentialIssuer`，并在内部构造用户作用域的 Frontend `RelayHello`。其 mTLS 证书标识 BFF workload，必须与已注册的 Workspace device 身份分离。转发前会校验 Directory descriptor 的组织、有效期以及精确配置的 Relay endpoint/SNI。response contract 不含 Workspace ID，因此 response 的 Workspace 归属依赖已认证 Relay 对 Workspace route 的绑定和 pending-request correlation。host 接线边界见 [`WEB_FRONTEND_RELAY_CLIENT.md`](../WEB_FRONTEND_RELAY_CLIENT.md)。

`cy-workspace-directory-admin` 使用分离的 operator 和 migration 数据库 URL。成员/角色/descriptor 变更与审计记录原子提交；服务 reader 角色仅能 SELECT。设备 registration 使用单独 registrar 角色，数据库只保存 256-bit recovery credential 的 domain-separated digest。仅有 binding adapter 不会启用 enrollment start 或 approval；生产使用必须在同一 PostgreSQL transaction 和 device 行锁下组合 binding 与授权状态 CAS。OIDC 验证、数据库用户/secret/部署，以及 production host 配置仍属于外部工作。

Relay 和 Directory 代码绝不能成为 Product、execution、Lease/Event 或 Artifact identity authority。Workspace Connector 仅在 Tonic TLS 对端证书或显式配置的 ACA XFCC 证书匹配已批准且未撤销的设备记录时接受；frontend 直连仍校验用户会话和成员关系。

已注册设备证书只认证 Connector 到 Relay 的客户端身份，不认证 Relay 服务到 Workspace 接收端的服务端身份。信任转发 `caller_roles` 的接收端必须使用服务端证书由配置的 Relay CA 验证且匹配配置的 server name 的 Relay 会话。Direct 请求仍使用 Frontend 用户会话。

ACA 转发证书适配器需显式启用并配置私有客户端 CA 信任 bundle。它只可用于 ACA HTTP/2 ingress 后方，因为 Envoy 会覆盖客户端提交的 XFCC metadata。适配器先校验证书链和 client-auth EKU，再复用设备注册表批准、撤销与 Workspace scope 校验。普通 ingress 绝不把 XFCC 当作身份来源。
