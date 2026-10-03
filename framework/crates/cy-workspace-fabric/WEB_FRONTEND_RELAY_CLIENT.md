# Web Frontend Relay client

Status: legacy V1 transport reference for Fabric compatibility fixtures. The
Platform Relay host and its XFCC handoff path have been retired; this document
does not describe the current Plugins-owned Relay runtime or prove its
production integration.

`FrontendRelayClient` is an outbound caller for a Web BFF. It is created for one
`VerifiedWebPrincipal` and one trusted `WebRelaySessionCredentialIssuer`:

```rust,ignore
let mut relay = FrontendRelayClient::connect(config, &principal, &issuer).await?;
let descriptors = relay.discover_workspaces().await?;
let response = relay.execute(workspace_api_request).await?;
```

`FrontendWorkspaceTransport` is the async interface for BFF composition. Keep
each transport instance scoped to the principal used to create it. A Workspace
ID alone is not a session key and must not select a shared user transport.

## Trust and binding

- The BFF first verifies its inbound AAD access token and obtains the immutable
  `VerifiedWebPrincipal`. Only that object supplies the RelayHello user and
  organization fields.
- The trusted handoff issuer creates the short-lived opaque
  `session_credential`. The inbound AAD bearer is not copied into RelayHello and
  is not used as a TLS client identity.
- `FrontendRelayClientConfig` accepts an HTTPS origin, expected TLS server
  name, Relay CA, and BFF workload certificate/key. The TLS connection verifies
  Relay using both the configured CA and SNI. The client certificate identifies
  the BFF workload; it must not be a registered Workspace device certificate.
- RelayHello is built internally with role `Frontend`, the exact verified user
  and organization, an empty Workspace ID, and no device identity.
- Discovery runs inside that Relay session. The client rejects descriptors
  with the wrong organization, invalid version/shape, expired validity, no
  candidate for the exact configured Relay endpoint and server name, or a
  duplicate Workspace ID. Failed refreshes clear the descriptor cache.
- An API request can target only a descriptor discovered by the same client.
  Its Workspace ID and descriptor are rechecked before sending. The client uses
  a fresh random wire request ID and accepts only the matching response ID;
  after that check it restores the caller's request ID.
- `WorkspaceApiResponse` does not carry a Workspace ID. Therefore the client
  does not claim the response proves its own Workspace source. V1 relies on the
  authenticated Relay to bind the forwarded request to the requested Workspace
  route and to match the response through its pending-request table.

## Host wiring boundary

This client is a transport slice; it does not configure a BFF listener, inbound
AAD verification, Directory storage, signing-key delivery, Relay host verifier,
or production certificates. A host must provide those trusted dependencies and
compose the BFF request path with a transport created from the same verified
principal. The Relay host must use the matching
`WebRelaySessionVerifier`. A resolver that receives only a Workspace descriptor
cannot safely mint or choose a per-user session by Workspace ID alone; it needs
an already principal-scoped transport or a principal-aware composition seam.

The current Workspace API response schema has no Workspace echo. If a future
protocol revision adds one, validate it against the request and Directory
descriptor before changing the documented routing trust premise.

---

# Web Frontend Relay 客户端

状态：旧 V1 Fabric 兼容 fixture 的 transport 参考。Platform Relay host 与其 XFCC
handoff 路径已退役；本文不描述当前 Plugins 所有的 Relay runtime，也不证明其生产集成。

`FrontendRelayClient` 是 Web BFF 使用的出站 caller。每个客户端绑定一个
`VerifiedWebPrincipal` 和一个可信 `WebRelaySessionCredentialIssuer`：

```rust,ignore
let mut relay = FrontendRelayClient::connect(config, &principal, &issuer).await?;
let descriptors = relay.discover_workspaces().await?;
let response = relay.execute(workspace_api_request).await?;
```

`FrontendWorkspaceTransport` 是 BFF 装配使用的异步接口。每个 transport 实例必须只绑定创建它时使用的 principal。不能只用 Workspace ID 作为 session key，也不能跨用户共享 transport。

## 信任与绑定

- BFF 先验证传入的 AAD access token 并取得不可变的 `VerifiedWebPrincipal`。只有该对象能提供 RelayHello 的 user 和 organization 字段。
- 可信 handoff issuer 签发短时不透明 `session_credential`。传入的 AAD bearer 不会复制进 RelayHello，也不作为 TLS 客户端身份。
- `FrontendRelayClientConfig` 接受 HTTPS origin、期望的 TLS server name、Relay CA 和 BFF workload 证书/密钥。TLS 连接同时使用配置的 CA 和 SNI 验证 Relay。客户端证书标识 BFF workload，不得使用已注册的 Workspace device 证书。
- 客户端内部构造的 RelayHello 固定为 `Frontend`，包含精确的已验证 user 和 organization、空 Workspace ID，且不包含 device identity。
- Discovery 在该 Relay session 内执行。客户端拒绝 organization 不匹配、version/shape 无效、已过期、缺少精确配置 Relay endpoint 与 server name candidate 或 Workspace ID 重复的 descriptor。刷新失败会清空 descriptor cache。
- API request 只能发送到同一客户端刚刚发现的 descriptor。发送前会再次校验 request Workspace ID 和 descriptor。客户端为 wire request ID 生成随机新值，只接受匹配的 response ID；校验后再恢复调用方 request ID。
- `WorkspaceApiResponse` 不包含 Workspace ID。因此客户端不声称 response 自身证明 Workspace 来源。V1 依赖已认证 Relay 将转发 request 绑定到指定 Workspace route，并通过 pending-request table 匹配 response。

## Host 接线边界

该客户端只提供 transport slice；不配置 BFF listener、入站 AAD 验证、Directory storage、签名密钥传递、Relay host verifier 或 production 证书。Host 必须提供这些可信依赖，并把 BFF request path 与由同一 verified principal 创建的 transport 组合。Relay host 必须使用匹配的 `WebRelaySessionVerifier`。如果 resolver 只接收 Workspace descriptor，就不能只凭 Workspace ID 安全地签发或选择 per-user session；它需要已绑定 principal 的 transport，或接收 principal 的装配接口。

当前 Workspace API response schema 不回传 Workspace ID。未来协议若增加该字段，应先将它与 request 和 Directory descriptor 比较，再更新本文中的 route 信任前提。
