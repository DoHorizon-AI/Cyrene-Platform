# Canonical local GPU runtime / 本地 GPU 标准运行时

`cyrene-runtime` builds and starts the NVIDIA adapter, `sandboxd`, and Kernel in
that order. It derives the UDS peer identity from the service account, installs
binaries under one explicit runtime home, checks component startup, and writes a
private machine-readable `runtime.json`. Standard output contains only sanitized
evidence fields.

`cyrene-runtime` 按顺序构建并启动 NVIDIA adapter、`sandboxd` 与 Kernel。脚本从服务
账号解析 UDS 身份，把二进制安装到显式 runtime home，检查启动状态，并写入私有的
`runtime.json`。标准输出只包含可公开的白名单证据字段。

```bash
export CYRENE_RUNTIME_HOME=/var/lib/cyrene/reference-runtime
tooling/runtime/cyrene-runtime up
tooling/runtime/cyrene-runtime status
tooling/runtime/cyrene-runtime down
```

Native Linux is the canonical mode and keeps cgroup/device isolation enabled.
An Azure agent must have a delegated cgroup v2 subtree; use
`--sandbox-cgroup-root` when the service account's subtree is not named
`cyrene`. WSL2 requires explicit `--profile WSL_DEV_PROFILE`; that mode enables
the existing soft shared-device development path and never claims native Linux
hard isolation.

原生 Linux 是标准模式，保留 cgroup 与设备隔离。Azure Agent 服务账号需要被委派
cgroup v2 子树；如果子树不使用 `cyrene` 名称，通过 `--sandbox-cgroup-root` 指定。
WSL2 必须显式选择 `--profile WSL_DEV_PROFILE`；该模式仅用于共享设备开发，不作为
原生 Linux 硬隔离验收。

| File | Responsibility |
| --- | --- |
| `cyrene-runtime` | Stable operator entrypoint / 固定运维入口 |
| `cyrene_runtime.py` | Build, process lifecycle, health and manifest logic / 构建、进程、健康与清单逻辑 |
| `tests/test_cyrene_runtime.py` | Fail-closed state and evidence regressions / 失败关闭与证据回归 |
