# Node Agent runtime

The Host Node Agent dials the Rust execution-control plane on the protocol-v1
Host role; protocol-v2 Runtime Agents establish separate Runtime-scoped
sessions. The Host Agent is deliberately outside the Kernel process, so mTLS
parsing, certificate rotation, network loss, backoff, and session state cannot
crash or block the Kernel.

The production path is:

```text
Rust `cy-execution-control::ExecutionControlService` -- mTLS bidi gRPC --> cy-node-agent -- UDS --> KernelService
```

The Agent accepts typed `KernelCommand` requests for capability query,
process launch/termination, operation cancellation, and the canonical
authority branch. The authority branch exposes finite `ReadEvents` as an
`EventPage`; continuous `WatchEvents` remains on the Kernel authority gRPC
stream because this one-command/one-result envelope is not a durable stream
subscription. It maps each supported request to the matching Kernel RPC and
returns the typed result or a gRPC status. It does not turn any remote string
into a process command.

`NodeWelcome`, `NodeToControlPlane`, and `ControlPlaneToNode` carry a session
id. The Agent accepts the initial welcome only when its envelope session id
equals the welcome session id. Each later inbound frame must retain that id and
have a strictly increasing sequence number. A stale session or replay is
rejected locally before a UDS request occurs.

On every reconnect, the Agent reads `KernelCapabilities.node` from the local
UDS before opening mTLS. If the Kernel has restarted and moved node epoch, the
Agent drops its former resume token; systemd additionally makes the Agent
`PartOf=cyrene-kernel.service` so a planned Kernel restart creates this clean
new bridge session immediately.

The Agent's control-plane endpoint must be `https://`, trust a configured CA,
present a client certificate and private key, and use an explicit server name.
Private keys must be mode `0600`. The external Agent user is in the trusted
`cyrene` group solely to connect to the Kernel socket; Worker accounts must not
join that group. This local UDS permission is separate from the remote mTLS
identity, and both are required for a command to reach the Kernel.
---

<!-- Chinese Translation / 中文翻译 -->

# Node Agent 运行时

Host Node Agent 使用 protocol-v1 Host 角色拨入 Rust execution-control plane；protocol-v2 Runtime Agent 则建立独立的、作用域为 Runtime 的会话。Host Agent 刻意放在 Kernel 进程之外，因此 mTLS 解析、证书轮换、网络中断、退避和会话状态不会导致 Kernel 崩溃或阻塞。

生产调用路径为：

```text
Rust cy-execution-control::ExecutionControlService -- mTLS bidi gRPC --> cy-node-agent -- UDS --> KernelService
```

Agent 接收类型化 `KernelCommand` 请求，涵盖能力查询、进程启动/终止、operation 取消和规范 authority 分支。authority 分支以 `EventPage` 返回有限的 `ReadEvents`；持续的 `WatchEvents` 仍位于 Kernel authority gRPC stream，因为这种“一条命令对应一个结果”的 envelope 并非持久流订阅。Agent 将每种支持的请求映射到对应 Kernel RPC，并返回类型化结果或 gRPC status。它不会把任何远端字符串转换为进程命令。

`NodeWelcome`、`NodeToControlPlane` 和 `ControlPlaneToNode` 都携带 session ID。只有当 envelope 的 session ID 与 welcome 的 session ID 相等时，Agent 才接受初始 welcome。之后的每个入站 frame 都必须保留该 ID，并具有严格递增的 sequence number。在发出 UDS 请求前，Agent 会在本地拒绝过期 session 或重放消息。

每次重新连接时，Agent 都会在开启 mTLS 前从本地 UDS 读取 `KernelCapabilities.node`。如果 Kernel 已重启并更换 node epoch，Agent 会丢弃旧的 resume token；此外，systemd 会将 Agent 设置为 `PartOf=cyrene-kernel.service`，因此计划内 Kernel 重启会立即建立这个全新的桥接会话。

Agent 的 control-plane endpoint 必须使用 `https://`、信任已配置的 CA、提供客户端证书和私钥，并指定明确的服务器名称。私钥权限必须为 `0600`。外部 Agent 用户加入受信任的 `cyrene` 组仅为连接 Kernel socket；Worker 账号不得加入该组。本地 UDS 权限与远端 mTLS 身份相互独立，两者都满足后命令才能到达 Kernel。
