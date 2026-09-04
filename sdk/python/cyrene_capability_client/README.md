# Cyrene Capability Client | Cyrene 能力执行客户端

This package is the Platform-owned Python projection of
`cyrene.capability.v1.CapabilityExecutionService`. Products use it to invoke a
configured capability binding without learning plugin entrypoints, worker
processes, or runtime generations.

本包是 Platform 对 `cyrene.capability.v1.CapabilityExecutionService` 的 Python
权威投影。Product 可通过稳定 binding 调用能力，但不会接触插件入口、worker
进程或 runtime generation。

The checked-in protobuf files are generated from the canonical CES,
model-provider, and training-backend contracts under `contracts/proto/`.
Regenerate or verify them with:

```bash
python scripts/generate_proto.py --write
python scripts/generate_proto.py --check
```

Use `model_provider_v1.py` and `training_engine_v1.py` for the stable
capability identifiers, canonical message types, and strict typed-payload
pack/unpack helpers. Application code must not hand-build protobuf wire bytes.

请使用 `model_provider_v1.py` 与 `training_engine_v1.py` 提供的稳定能力标识、
canonical 消息类型和严格 typed-payload 辅助函数；应用代码不得手工拼装
protobuf 线格式。

Unencrypted TCP is intentionally limited to loopback endpoints. Deployments
that cross a host boundary must inject an authenticated `grpc.Channel`.

未加密 TCP 仅允许 loopback。跨主机部署必须注入经过认证的 `grpc.Channel`。
