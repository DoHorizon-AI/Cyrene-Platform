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
| `src/auth.rs` | Replaceable Frontend user-session verifier. | 可替换的 Frontend 用户会话校验器。 |
| `src/device_auth.rs` | Tonic mTLS peer-certificate facts and registry-backed Workspace connector authorization. | Tonic mTLS 对端证书事实与注册表驱动的 Workspace Connector 授权。 |
| `src/directory.rs` | Membership-scoped Workspace discovery. | 基于成员关系的 Workspace 发现。 |
| `src/device_registry.rs` | Import and revocation port for externally approved device certificates. | 外部批准设备证书的导入与撤销接口。 |
| `src/persistent_directory.rs` | Private, single-owner snapshot storage for memberships, descriptors, and device records. | 成员关系、描述符与设备记录的私有单实例快照存储。 |
| `src/api.rs` | Stable frontend-to-Workspace API port and LOCAL adapter. | 稳定 frontend API port 与 LOCAL adapter。 |
| `src/direct.rs` | Workspace API endpoint with session and membership checks. | 校验会话与成员关系的 Workspace API 直连端点。 |
| `src/relay.rs` | Live application request routing without Workspace authority. | 不拥有 Workspace 权威的实时应用请求路由。 |
| `src/transport.rs` | Direct candidate selection and outbound mTLS Relay transport. | 直连候选选择与出站 mTLS Relay 传输。 |
| `src/bin/` | Real acceptance relay, connector, and reference frontend. | 真实验收 Relay、Connector 与参考 frontend。 |

`FileWorkspaceDirectory::open` needs a private directory on a persistent volume
with one active owner. `replace` publishes a complete membership and descriptor
revision atomically while preserving device records. The registry imports a
certificate record only after external approval and issuance have completed;
the imported authorization state is `Approved`. It stores a scoped identity
and a supplied SHA-256 certificate fingerprint, supports lookup by device key
or fingerprint, rejects duplicate identities and fingerprints, and allows
revocation. Revoked identities and fingerprints cannot be re-imported; a
replacement certificate needs a future explicit rotation operation. The
registry does not represent pending requests or perform approval decisions.

`RegistryWorkspaceDeviceVerifier` authorizes Relay `WorkspaceConnector`
participants from the leaf certificate returned by Tonic's TLS peer metadata.
Tonic's server TLS layer validates the peer chain; this adapter computes a
lowercase SHA-256 fingerprint over the leaf DER, reads its `notAfter` time, and
requires a matching `Approved` registry record that matches the RelayHello
organization, Workspace, and device IDs. RelayHello `user`,
`session_credential`, and `enrollment_state` do not establish connector
identity. `WorkspaceRelay::new` leaves connector authentication disabled;
callers must explicitly provide a device registry with
`with_workspace_device_registry`. Frontend Relay sessions and the direct
Workspace endpoint continue to use user-session authentication and membership
checks.

This certificate authenticates the Workspace connector to the Relay; it does
not authenticate the Relay service to the connector. A Workspace receiver may
trust forwarded `caller_roles` only on a Relay session whose server certificate
chains to its configured Relay CA and matches the configured server name. The
outbound transport verifies those server facts independently from the
connector's registered client certificate. The Direct endpoint remains a
Frontend user-session path and must not use a Workspace device certificate as
its user identity.

The registry store validates fingerprint syntax but does not parse
certificates, verify TLS peers, or issue credentials. No private keys,
certificate bodies, or session credentials are stored, and this crate does not
itself run a Directory service.

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
| `src/auth.rs` | 可替换的 Frontend 用户会话校验器。 |
| `src/device_auth.rs` | Tonic mTLS 对端证书事实与注册表驱动的 Workspace Connector 授权。 |
| `src/directory.rs` | 以成员关系为作用域的 Workspace 发现。 |
| `src/device_registry.rs` | 外部批准设备证书的导入与撤销接口。 |
| `src/persistent_directory.rs` | 成员关系、描述符与设备记录的私有单实例快照存储；需挂载持久卷。 |
| `src/api.rs` | 稳定的 frontend-to-Workspace API port 与 LOCAL adapter。 |
| `src/direct.rs` | 校验会话与成员关系的 Workspace API 私网直连端点。 |
| `src/relay.rs` | 不拥有 Workspace authority 的实时应用请求路由。 |
| `src/transport.rs` | 直连候选选择、出站 mTLS Relay client 和 connector session。 |
| `src/bin/` | 真实 acceptance relay、connector 和参考 frontend。 |

`FileWorkspaceDirectory::open` 需要挂载在持久卷上的私有目录，同一时间仅允许一个实例持有。`replace` 原子发布成员关系和描述符版本，并保留设备记录。注册表只导入已经外部批准并签发的证书记录，导入时状态为 `Approved`；记录作用域身份和调用方提供的 SHA-256 证书指纹，支持按设备键或指纹查找，拒绝重复身份/指纹，并允许撤销。撤销后的身份和指纹不能重新导入；证书轮换需由未来的显式操作处理。注册表不表示待审批请求，也不执行审批决策。

`RegistryWorkspaceDeviceVerifier` 只从 Tonic TLS 对端元数据取得叶子证书，由 Tonic 服务端 TLS 层先验证证书链；适配器再计算 DER 的小写 SHA-256 指纹、读取 `notAfter`，并要求指纹匹配已批准的注册表记录，同时校验 RelayHello 中的组织、Workspace 和设备 ID。RelayHello 的 `user`、`session_credential` 和 `enrollment_state` 不构成 Connector 身份。`WorkspaceRelay::new` 默认关闭 Connector 认证；调用方必须显式通过 `with_workspace_device_registry` 提供设备注册表。Frontend Relay 会话和 Workspace 直连端点仍使用用户会话认证与成员关系校验。

此证书只认证 Workspace Connector 到 Relay 的客户端身份，不认证 Relay 服务到 Connector 的服务端身份。Workspace 接收端只能在 Relay 会话的服务端证书由配置的 Relay CA 验证且匹配配置的 server name 时信任转发的 `caller_roles`。出站 transport 会独立校验这些服务端事实与 Connector 已注册客户端证书。Direct 端点仍是 Frontend 用户会话路径，不得把 Workspace 设备证书用作用户身份。

存储层只校验指纹格式，不解析证书、不验证 TLS 对端、不签发凭据。这里不保存私钥、证书正文或会话凭据，crate 也不自行运行 Directory 服务。

运行：

```bash
cargo test --locked -p cy-workspace-fabric
cargo clippy --locked -p cy-workspace-fabric --all-targets -- -D warnings
```
