# cy-workspace-sidecar

This independently versioned Cargo package owns the loopback Workspace bridge
process for non-Rust clients. It consumes the lightweight
`cy-workspace-client-sdk` and the versioned `cyrene.workspace.local.v1` gRPC
contract. The listener is fixed to IPv4 loopback; the Sidecar consumes
externally issued credentials and never issues identity.

The Sidecar validates credential-file shape, enforces its private Workspace and
operation allowlists, and uses the client SDK for discovery and requests. Its
normal dependency graph contains no Workspace server, SQL, or WebAuthn
implementation. The SDK does not create trusted identity or authorization
objects; Relay remains authoritative for those decisions.

Build or run the binary independently from the Platform workspace root:

```bash
cargo build --locked --release -p cy-workspace-sidecar
cargo run --locked -p cy-workspace-sidecar
```

The Sidecar has a build-only GitHub Actions workflow that uploads a
commit-SHA-named binary artifact with a version and SHA-256 manifest. Artifacts
expire after 30 days; no system package, installer, or updater consumes them
yet. Credential file policy, local bearer requirements, and the shared
network-namespace requirement are documented in
[`../cy-workspace-fabric/README.md`](../cy-workspace-fabric/README.md#local-python-bridge).

本 Cargo package 独立拥有供非 Rust client 使用的 loopback Workspace bridge 进程，并消费轻量
`cy-workspace-client-sdk` 与版本化的 `cyrene.workspace.local.v1` gRPC 合同。listener 固定为
IPv4 loopback；Sidecar 只消费外部签发的凭据，不签发 identity。

Sidecar 校验 credential 文件结构，并执行私有 Workspace 与 operation allowlist；discovery 和
request 通过 client SDK 完成。普通依赖图不含 Workspace server、SQL 或 WebAuthn 实现。SDK
不会创建可信 identity 或 authorization object；Relay 仍是这些决策的权威方。

Sidecar 有 build-only GitHub Actions workflow，会生成带 commit SHA 名称、版本和 SHA-256
manifest 的二进制 artifact，保留 30 天；目前没有 system package、安装器或 updater 消费该
artifact。凭据文件策略、本地 bearer 要求和共享 network namespace 约束见上方文档。
