# System Adapter 边界

英文 canonical source：[docs/architecture/system-adapter.md](../../architecture/system-adapter.md)。

`SystemAdapter` 是单个构建目标的主机事实与通用主机资源边界。它让 Kernel 消费
标准化 CPU、内存、NUMA、OS 能力、binding 和 health，而不把 Linux `/proc`/`sysfs`
解析或其他 OS API 放入 Kernel。

## 规范端口

端口定义在 `contracts/rust/cy-kernel-contract/src/adapter.rs`：

| 端口 | 职责 |
| --- | --- |
| `probe_inventory` | 返回带 generation 的 inventory snapshot 与 node capabilities |
| `adapter_id` | 返回稳定的本地 adapter identity |
| `probe_resources` | 返回本适配器拥有的标准化资源 |
| `create_binding` | 将资源投影为 Kernel 批准的 binding |
| `read_health` | 返回资源健康证据 |
| `system_id` | 标识目标系统实现 |

它不拥有 Lease、fence token、生命周期策略、产品状态或 GPU 厂商语义。

## 当前 Linux 实现

`adapters/hardware/linux_sys/src/probe.rs` 中的 `LinuxSystemProvider` 由
`cyrene-linux-sys-adapter` 进程提供服务，当前报告：

- `cpu-host`：CPU 容量、架构、型号、内核版本、NUMA 数量和 CPU capability；
- `ram-host`：总/可用内存、swap 事实、NUMA 数量和低内存 degraded 状态；
- inventory generation、节点能力、binding 和资源 health。

CPU/RAM binding 不包含设备节点并报告 `Soft` enforcement。NVIDIA discovery、
topology、设备节点和厂商 health 仍属于独立 NVIDIA Hardware Adapter。

## 传输与构建

Linux 实现通过有界帧格式的 `cyrene.hardware.v1` UDS 运行，Kernel 显式注册绝对
socket 路径与 adapter identity。公共端口与 OS 无关，但当前实现仅支持 Linux；未来
目标系统需要提供同一端口的独立实现，并在目标构建中选择，不能向 Kernel 添加 OS
专属方法。

systemd 参考实现：`infrastructure/systemd/cyrene-linux-sys-adapter.service`。
生产部署应在 Kernel 与 Adapter 两侧都显式配置 peer UID/GID；当前允许省略是部署
兼容性状态，不是语义端口缺失。

## 当前状态

| 范围 | 状态 |
| --- | --- |
| 公共 `SystemAdapter` 端口 | `COMPLETE` |
| Linux inventory/binding/health | `COMPLETE` |
| Kernel 显式 endpoint 集成 | `COMPLETE` |
| NVIDIA 厂商事实 | `NOT_APPLICABLE` |
| 非 Linux provider | `DEFERRED` |
| 真实特权/systemd 部署 | `DEFERRED` |
