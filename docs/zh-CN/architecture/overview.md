# CYRENE Platform 架构总览

英文 canonical source：[docs/architecture/overview.md](../../architecture/overview.md)。

```text
Product
  ↓
SDK / Worker
  ↓
Framework / Resolver
  ↓
Contracts: Protobuf + Schema + Semantic TCK
  ↓
Kernel Daemon
  ├─ Node Agent
  ├─ Linux System Adapter
  ├─ NVIDIA / other Hardware Adapter
  ├─ Sandbox Adapter Host
  └─ Local Journal / Resource Ledger
```

## 请求与证据流

1. Product 表达与产品无关的 capability 或 workload intent。
2. Framework 解析兼容扩展，并依据版本化 contract 生成执行请求。
3. Kernel 校验 authority、revision、lease 和 resource facts；持久分配账本与新鲜
   观测分离。
4. 硬件和沙箱操作跨越受认证的本地 IPC；厂商库、cgroup、设备和进程原语不进入
   Kernel 地址空间。
5. 生命周期事件与退出证据沿 contract projection 返回，Product 只做 reconcile，
   不拥有 Kernel 内部状态。

## 关键规则

- Semantic contract 是名词、状态转换和拒绝条件的权威来源。
- Protobuf 和 SDK 是投影，不得引入隐藏语义。
- Framework 负责 discovery、policy routing 和 plugin composition。
- Kernel 负责本地准入、租约、fencing、生命周期决策和通用传输。
- Adapter 负责独立的主机事实或强制执行；Linux 构建必须通过 Linux System Adapter
  获取主机事实。
