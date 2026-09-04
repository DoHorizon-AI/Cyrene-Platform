# Cyrene Capability Client | Cyrene 能力执行客户端

This package is the Platform-owned Python projection of
`cyrene.capability.v1.CapabilityExecutionService`. Products use it to invoke a
configured capability binding without learning plugin entrypoints, worker
processes, or runtime generations.

本包是 Platform 对 `cyrene.capability.v1.CapabilityExecutionService` 的 Python
权威投影。Product 可通过稳定 binding 调用能力，但不会接触插件入口、worker
进程或 runtime generation。

The checked-in protobuf files are generated from
`contracts/proto/cyrene/capability/v1/capability_execution.proto`. Regenerate or
verify them with:

```bash
python scripts/generate_proto.py --write
python scripts/generate_proto.py --check
```

Unencrypted TCP is intentionally limited to loopback endpoints. Deployments
that cross a host boundary must inject an authenticated `grpc.Channel`.

未加密 TCP 仅允许 loopback。跨主机部署必须注入经过认证的 `grpc.Channel`。
