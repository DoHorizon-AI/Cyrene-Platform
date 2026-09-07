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

`CapabilityExecutionClient.invoke_stream` consumes the additive CES typed
stream. It requires the worker's negotiated
`cyrene.worker.typed-invocation-stream.v1` feature and fails closed when an old
worker cannot provide it; it never falls back to replaying a unary response as
fake streaming output. Response and terminal sequences start at one; a
pre-execution error/end marker may use sequence zero. EOF without a terminal
marker is a protocol failure.

`CapabilityExecutionClient.invoke_stream` 使用 CES 新增的类型化流。它要求 worker
在握手中协商 `cyrene.worker.typed-invocation-stream.v1`；旧 worker 不支持时会直接
fail closed，不会把 unary 响应重新拆成伪流。响应和终止序号从 1 开始；执行前的
错误/终止标记可以使用序号 0；没有终止标记就结束的 EOF 会被视为协议错误。
