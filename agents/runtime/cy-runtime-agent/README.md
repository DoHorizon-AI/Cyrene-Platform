# CYRENE Runtime Agent

`cy-runtime-agent` is the unprivileged `CONTAINER_AGENT` implementation of
[Distributed Execution Fabric v1](../../../docs/architecture/distributed-execution-fabric-v1.md).
It opens one outbound authenticated control stream, validates a preconfigured
workload assignment against the canonical Runtime generation and Lease/Fence,
stages verified Artifacts, and supervises one child process group.

`cy-runtime-agent` 是容器内无特权 `CONTAINER_AGENT`。它不需要 systemd、Docker
socket、宿主机 root 或入站端口，也不接受控制面传入的 shell/argv。workload 命令在
容器启动时固定：

```text
cy-runtime-agent run [agent options] -- <workload command>
```

The development enrollment token is single-use bootstrap material, not a
production identity solution. Production deployments must replace it through
the frozen enrollment provider seam. Until the first authenticated post-Welcome
frame arrives, retrying the same bootstrap identity replays the same pending
grant; it never consumes a second enrollment proof or creates a second token.
After Welcome, the Agent atomically persists only the grant-lifetime resume
token, namespaced by the exact Runtime generation and `NodeRef`. The state
directory is opened without following symlinks, forced to mode `0700`, and held
by a per-Runtime/Node process lock; credential state files are regular,
owner-only `0600` files replaced relative to the opened directory descriptor.
The enrollment proof is never persisted.

This supports reconnect and a fresh Agent invocation while the same
execution-control service retains the matching grant/token. It does not claim
control-service restart or workload recovery: session/grant state is currently
in memory, `CONTAINER_AGENT` advertises `RestartCapability::None`, and accepted
assignment and child-process state are not persisted.

Remote Artifact assignments must carry Artifact Plane-selected sources and a
short-lived, source/destination/part/expiry/byte-scoped transfer ticket. The
Agent verifies the ticket before opening the HTTPS Range source, then verifies
every part and the full Artifact digest before atomic publication. The
`--artifact-ticket-key` / `CYRENE_ARTIFACT_TICKET_KEY` option wires the symmetric
development/conformance verifier only; production must replace that authority
with its managed Artifact Plane verifier and must not distribute an issuer key
to the Agent.
---

<!-- Chinese Translation / 中文翻译 -->

# CYRENE Runtime Agent

`cy-runtime-agent` 是 [Distributed Execution Fabric v1](../../../docs/architecture/distributed-execution-fabric-v1.md) 中无特权的 `CONTAINER_AGENT` 实现。它打开一条出站认证控制流，根据规范 Runtime generation 和 Lease/Fence 校验预先配置的 workload assignment，暂存已验证 Artifact，并监管一个子进程组。

该 Agent 在容器中运行，不需要 systemd、Docker socket、宿主机 root 权限或入站端口，也不接受 control plane 传来的 shell/argv。workload 命令在容器启动时固定：

```text
cy-runtime-agent run [agent options] -- <workload command>
```

开发环境的 enrollment token 是一次性的 bootstrap 材料，不是生产身份方案。生产部署必须通过冻结的 enrollment provider seam 替换它。首个认证的 Welcome 后 frame 到达前，重试相同 bootstrap identity 会重放同一待处理 grant；不会消耗第二个 enrollment proof，也不会创建第二个 token。Welcome 后，Agent 仅原子持久化 grant 有效期内的 resume token，并使用精确 Runtime generation 和 `NodeRef` 命名空间隔离。状态目录打开时不会跟随符号链接，权限强制设为 `0700`，并由每个 Runtime/Node 的进程锁保护；credential state 文件必须是普通文件、仅 owner 可读写（`0600`），并相对于已打开的目录描述符替换。Enrollment proof 从不持久化。

当同一个 execution-control service 保留匹配的 grant/token 时，这支持断线重连和重新启动 Agent。它不代表 control service 重启或 workload 恢复可用：session/grant 状态当前保存在内存中，`CONTAINER_AGENT` 声明 `RestartCapability::None`，已接受的 assignment 和子进程状态也未持久化。

远程 Artifact assignment 必须带有由 Artifact Plane 选择的 source，以及短时有效、受 source/destination/part/expiry/byte 范围约束的 transfer ticket。Agent 在打开 HTTPS Range source 前先验证 ticket，然后在原子发布前校验每个 part 和完整 Artifact digest。选项 `--artifact-ticket-key` / `CYRENE_ARTIFACT_TICKET_KEY` 仅连接对称开发/一致性验证器；生产环境必须改用由其管理的 Artifact Plane verifier，且不得将 issuer key 分发给 Agent。
