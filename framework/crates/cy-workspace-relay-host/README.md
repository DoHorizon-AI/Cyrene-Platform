# cy-workspace-relay-host

This independently versioned Cargo package owns the Workspace Relay process
entrypoint and container image. It consumes the shared `cy-workspace-fabric`
library; Directory, device authorization, identity, and readiness policy remain
owned by Platform.

Build the package or image independently from the Platform workspace root:

```bash
cargo build --locked --release -p cy-workspace-relay-host
docker build -f framework/crates/cy-workspace-relay-host/Dockerfile .
```

The existing Relay image workflow publishes an immutable commit-SHA image but
does not deploy it. Runtime configuration, health behavior, and the current
fail-closed authorization/readiness gates are documented in
[`RUNTIME.md`](RUNTIME.md).

本 Cargo package 独立拥有 Workspace Relay 进程入口与容器镜像，并消费共享的
`cy-workspace-fabric` library。Directory、设备授权、identity 与 readiness policy 仍由
Platform 拥有。

可在 Platform workspace 根目录独立构建 package 或镜像。现有 Relay image workflow 会发布
带 commit SHA 的不可变镜像，但不会部署。运行配置、探针行为以及当前 fail-closed
授权/readiness gate 见上方运行说明。
