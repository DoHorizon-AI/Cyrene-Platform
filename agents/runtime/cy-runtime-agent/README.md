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
the frozen enrollment provider seam.
