# cy-mtls-channel-client

`cy-mtls-channel-client` builds outbound HTTPS Tonic channels with a supplied
server CA, TLS server name, and client certificate/key. It requires HTTPS and
does not provide an insecure TLS mode.

The caller owns endpoint policy and identity material. This crate does not issue
or verify application identities, authorize users or devices, select Workspace
routes, speak the Relay protocol, or dispatch Workspace API requests. Those
responsibilities remain in `cy-workspace-fabric`.

`cy-workspace-fabric` uses this library after its descriptor checks and exact
Relay endpoint/SNI checks. It retains direct-versus-Relay selection, fallback
policy, credential loading, authorization, and connector request dispatch.

## 中文说明

`cy-mtls-channel-client` 使用调用方提供的 server CA、TLS server name 与
client certificate/key 构造出站 HTTPS Tonic channel。它强制使用 HTTPS，不提供
不安全 TLS 模式。

调用方负责 endpoint policy 与身份材料。本 crate 不签发或校验应用身份、不授权用户或
设备、不选择 Workspace route、不处理 Relay protocol，也不 dispatch Workspace API
request。这些职责仍属于 `cy-workspace-fabric`。

`cy-workspace-fabric` 在完成 descriptor 与 Relay endpoint/SNI 校验后使用该 library；
直连与 Relay 的选择、fallback policy、凭据加载、authorization 和 connector request
dispatch 仍由 Fabric 保留。
