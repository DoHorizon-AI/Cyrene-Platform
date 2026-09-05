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
