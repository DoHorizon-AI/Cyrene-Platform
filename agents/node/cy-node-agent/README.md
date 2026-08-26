# CYRENE Node Agent

`cy-node-agent` is an external bridge, not part of the Rust Kernel. It keeps
one outbound mTLS `NodeControlService.Connect` stream to the Kotlin control
plane and forwards only typed `KernelCommand` oneof requests to the local
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
