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

The Agent accepts only `KernelCommand`'s six typed requests: capability query,
resource reserve/release, launch/terminate, and operation cancel. It maps each
to the matching Kernel RPC and returns the typed result or a gRPC status. It
does not turn any remote string into a process command.

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
