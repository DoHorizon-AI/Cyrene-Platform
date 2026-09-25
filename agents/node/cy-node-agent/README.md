# CYRENE Node Agent

`cy-node-agent` is an external bridge, not part of the Rust Kernel. It keeps
one outbound mTLS `NodeControlService.Connect` stream to the Rust
`cy-execution-control` control plane and forwards only typed `KernelCommand`
oneof requests to the local
`KernelService` UDS endpoint.

It never creates cgroups, scans hardware, loads drivers, spawns Workers, or
accepts a shell command, argv, environment, image, or arbitrary payload.

## Control-session invariants

- The first outbound frame is `NodeHello`; the first inbound frame must be a
  `NodeWelcome` with the same non-empty `session_id` as the envelope.
- Every later command/result/heartbeat carries that session id, and inbound
  control-plane sequence numbers must strictly increase. A prior reconnect or
  replay is fenced before it reaches the local Kernel.
- Before every remote reconnect the Agent queries local `KernelService` for the
  current `NodeRef`. If its epoch changed, the former resume cursor is dropped
  and the control plane receives a new, fenced node epoch.
- Remote loss uses bounded exponential reconnect. Only mTLS is accepted;
  plaintext `http://` control-plane endpoints are rejected.

## Linux service setup

Install `infrastructure/systemd/cy-node-agent.service`, then create a root-owned
`/etc/cyrene/node-agent.env` (mode `0640`) containing file paths, not PEM
contents:

```ini
CYRENE_CONTROL_PLANE_ENDPOINT=https://control.example:7443
CYRENE_CONTROL_PLANE_SERVER_NAME=control.example
CYRENE_CONTROL_PLANE_CA=/etc/cyrene/agent/control-ca.pem
CYRENE_AGENT_CLIENT_CERT=/etc/cyrene/agent/node.pem
CYRENE_AGENT_CLIENT_KEY=/etc/cyrene/agent/node.key
CYRENE_NODE_ID=node-01
```

The private key must be owned by `cyrene-agent` and mode `0600`; the Agent
rejects keys readable by group or other users. Package provisioning must create
the `cyrene` group, the `cyrene-kernel` and `cyrene-agent` users, and assign
only trusted Kernel/Adapter/Agent services to that group. Workers must never be
members: access to `/run/cyrene/kernel.sock` is the local command authority.
---

<!-- Chinese Translation / 中文翻译 -->

# CYRENE Node Agent

`cy-node-agent` 是一个外部桥接层，不属于 Rust Kernel。它通过一条出站 mTLS `NodeControlService.Connect` stream 连接 Rust `cy-execution-control` control plane，并只将类型化的 `KernelCommand` oneof 请求转发到本地 `KernelService` UDS endpoint。

它不会创建 cgroup、扫描硬件、加载 driver、启动 Worker，也不接受 shell command、argv、环境变量、镜像或任意负载。

## Control Session 不变量

- 第一个出站 frame 是 `NodeHello`；第一个入站 frame 必须是 `NodeWelcome`，且 envelope 与 welcome 中的非空 `session_id` 相同。
- 后续每条 command/result/heartbeat 都携带该 session ID；入站 control-plane 序号必须严格递增。过期连接或重放会在到达本地 Kernel 前被 fencing。
- 每次远端重连前，Agent 都会向本地 `KernelService` 查询当前 `NodeRef`。若 epoch 已改变，就丢弃旧 resume cursor，并向 control plane 提供新的 fenced node epoch。
- 远端连接丢失时使用有界指数重连。只接受 mTLS；拒绝明文 `http://` control-plane endpoint。

## Linux Service 配置

安装 `infrastructure/systemd/cy-node-agent.service`，然后创建 root 所有、权限为 `0640` 的 `/etc/cyrene/node-agent.env`。文件内容是路径，而非 PEM 文本：

```ini
CYRENE_CONTROL_PLANE_ENDPOINT=https://control.example:7443
CYRENE_CONTROL_PLANE_SERVER_NAME=control.example
CYRENE_CONTROL_PLANE_CA=/etc/cyrene/agent/control-ca.pem
CYRENE_AGENT_CLIENT_CERT=/etc/cyrene/agent/node.pem
CYRENE_AGENT_CLIENT_KEY=/etc/cyrene/agent/node.key
CYRENE_NODE_ID=node-01
```

私钥必须归 `cyrene-agent` 所有，权限为 `0600`；Agent 会拒绝组或其他用户可读的密钥。Package 配置必须创建 `cyrene` 组、`cyrene-kernel` 和 `cyrene-agent` 用户，并只将受信任的 Kernel/Adapter/Agent 服务加入该组。Worker 绝不能加入此组：访问 `/run/cyrene/kernel.sock` 就是本地命令权威。
