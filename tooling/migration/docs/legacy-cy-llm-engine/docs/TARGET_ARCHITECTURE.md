# CY-LLM 目标架构设计

> 本文描述从当前 Python Lite Gateway/Coordinator/Worker 架构，渐进迁移到面向远程
> GPU 的 Runtime Control Plane。产品行为以 `PRODUCT_REQUIREMENTS.md` 为准，实施顺序
> 以根目录 `REBUILD_PLAN.md` 为准。

## 1. 架构目标

目标系统需要同时满足：

- 本地或 WSL 图形控制端管理远程 SSH GPU 节点；
- SSH 与 UI 断开后任务继续；
- 自动探测硬件和系统能力；
- 生成可解释的一到三个训练/推理方案；
- 按需构建 Docker + uv Runtime；
- 模型、数据、checkpoint 与镜像分离；
- 训练具备 checkpoint、revision、canary 和 rollback；
- 训练产物可针对另一台硬件重新量化和部署；
- 推理部署提供 OpenAI-compatible Gateway；
- Community 单用户安装简单，Pro 可扩展到共享控制面、多租户与审计。

## 2. 当前架构与主要缺口

当前 Community 主路径为：

```text
HTTP Client
  -> Python FastAPI Gateway Lite
  -> Python Coordinator Lite
  -> Python cy_exec Worker
  -> vLLM/TRT/legacy training
```

该路径可以保留作为兼容执行链，但存在以下结构性缺口：

1. Gateway/Coordinator 主要做请求转发，缺少完整 Runtime 决策控制面。
2. Worker 同时承担分析、决策、执行和恢复，职责过重。
3. 没有本地控制端与远程节点代理的正式边界。
4. 训练任务生命周期依赖进程实现，缺少持久控制状态。
5. 依赖组合、镜像、模型和验证证据没有统一资源模型。
6. 训练监控与训练控制没有统一事件流。
7. 量化、制品和推理部署没有形成连续供应链。

仓库已经具备目标架构的部分基础：

- `schemas/manifests/`：Manifest JSON Schema；
- `crates/cy-manifest` 与 `python/cy_manifest`：跨语言确定性哈希；
- `crates/cy-node-agent`：Node Agent 边界；
- `crates/cy-control-plane`：控制面边界；
- `python/cy_exec`：现有执行层；
- `python/cy_gateway_lite`：过渡 Community Gateway；
- `crates/cy-proxy`：未来 Rust 数据面基础；
- `plugins/pro`：企业能力与扩展来源。

## 3. 目标系统上下文

```text
┌──────────────────────────────────────────────────────────────┐
│ 用户本地 / WSL                                               │
│                                                              │
│ Browser UI <-> Local Controller <-> Secret Store             │
│                         |                                    │
│                         | SSH / SSH tunnel                    │
└─────────────────────────|────────────────────────────────────┘
                          |
┌─────────────────────────|────────────────────────────────────┐
│ 远程 GPU 节点            v                                    │
│                                                              │
│ Node Agent                                                   │
│   ├── Hardware Probe                                         │
│   ├── Artifact Manager                                       │
│   ├── BuildKit Runtime Builder                               │
│   ├── Job Supervisor                                         │
│   ├── Metrics/Event Collector                                │
│   └── Local State Journal                                    │
│           |                     |                             │
│           v                     v                             │
│   Training Container     Inference Container                 │
│           |                     |                             │
│           v                     v                             │
│   Persistent Artifacts   OpenAI Gateway                      │
└──────────────────────────────────────────────────────────────┘
```

## 4. 逻辑组件

### 4.1 Local Controller

职责：

- 提供本地 HTTP API 和浏览器 UI；
- 管理多个远程 Target；
- 通过 SSH bootstrap、升级和连接 Agent；
- 执行模型分析、兼容规划和 Why Report；
- 保存项目、计划、任务索引和用户偏好；
- 聚合远程事件并展示控制面板；
- 调用本机系统凭据存储。

推荐形态：Community 为模块化单体，不要一开始拆成多套可独立部署的微服务。

### 4.2 Node Agent

职责：

- 以静态或自包含二进制运行在 GPU 节点；
- 产出 HardwareManifest；
- 管理持久目录、配额和磁盘预检；
- 下载、校验和缓存模型/数据；
- 调用 Docker/BuildKit 构建与运行；
- 作为训练和推理 Job Supervisor；
- 采集指标、日志和事件；
- 保存最小本地 journal，支持控制端离线后继续运行；
- 重连后报告真实状态，不依赖控制端猜测。

Agent 默认通过 UDS 或 loopback 服务，只允许 SSH tunnel 或显式配对连接。

### 4.3 Manifest Service

以 `schemas/manifests/` 为唯一契约源：

- HardwareManifest；
- ModelManifest；
- WorkloadRequest；
- RuntimeManifest；
- 后续增加 WhyReport、ValidationResult、TrainingRevision、
  CheckpointMetadata 和 ArtifactManifest。

所有不可变资源使用 canonical content hash。Secret 只能以引用存在，不能进入 hash
preimage。

### 4.4 Hardware Analyzer

Node Agent 内运行真实探测，控制面负责解释与归一化：

- OS、内核、glibc、CPU、RAM、磁盘；
- GPU、显存、compute capability、Driver；
- CUDA 能力、精度能力、拓扑；
- Docker、BuildKit、Container Toolkit；
- 持久目录和基础性能。

实现顺序：Linux/NVIDIA 实探测优先，离线硬件模板作为低证据等级补充。

### 4.5 Model Analyzer

放在 Python 生态层，复用 Hugging Face/Transformers 能力：

- 读取远程或本地模型元数据；
- 识别架构、权重、量化、remote code；
- 估算训练权重、梯度、optimizer、activation；
- 估算推理权重与 KV cache；
- 输出可行策略候选，而不直接启动模型。

### 4.6 Compatibility Resolver

输入：

```text
HardwareManifest + ModelManifest + WorkloadRequest + PluginCapabilities
```

输出：

```text
1..3 RuntimePlan + WhyReport + Confidence + EvidenceLevel
```

Resolver 使用三类事实：

1. 上游声明与版本规则；
2. 插件 capability；
3. 本项目实际构建、验证和 benchmark 结果。

不能预生成笛卡尔积。只保存稀疏规则、候选和真实验证结果。

### 4.7 Runtime Builder

运行在目标节点，负责：

- 选择固定 digest 的基础镜像；
- 从 RuntimeManifest 生成项目依赖输入；
- 使用 uv 生成/消费 lock；
- 生成 Dockerfile/BuildKit bake definition；
- 使用本地和远程 layer cache；
- 构建 smoke test stage 与最终 runtime stage；
- 生成 SBOM、provenance、image digest；
- 本地保留、推送 OCI Registry 或导出 OCI Archive。

模型和数据绝不作为默认镜像层。

### 4.8 Artifact Manager

管理：

- 模型 snapshot；
- 数据集；
- checkpoint；
- 合并模型；
- 量化产物；
- logs、SBOM、评测和构建报告。

建议远程布局：

```text
<persistent-root>/
  models/
  datasets/
  checkpoints/
  runs/
  artifacts/
  build-cache/
  agent-state/
```

Artifact 使用 digest 去重，并记录来源、revision、大小、完整性与引用计数。

### 4.9 Execution Adapter

`python/cy_exec` 从“决策 + 执行”重塑为薄执行层：

- 加载/卸载模型；
- 运行推理；
- 启动训练框架；
- 保存/恢复 checkpoint；
- 输出结构化指标和错误；
- 不自行决定 CUDA、框架版本或训练策略。

推理与训练后端通过进程外插件契约接入，保持 Python AI 生态兼容。

### 4.10 Training Controller

负责持久训练状态机和 checkpoint 驱动调整：

```text
CREATED -> PREPARING -> CALIBRATING -> READY -> RUNNING
        -> CHECKPOINTING -> ANALYZING -> REVISING
        -> CANARY -> RUNNING / ROLLING_BACK
        -> COMPLETED / FAILED / CANCELED
```

控制器不直接实现反向传播。它生成 immutable TrainingRevision，通过 Execution
Adapter 驱动 LLaMA-Factory，并在 checkpoint 边界做兼容性检查、恢复和回滚。

### 4.11 Metrics and Event Service

指标分为：

- 高频指标：显存、利用率、tokens/s、loss 等；
- 持久事件：checkpoint、revision、OOM、恢复、完成；
- 大型日志：对象存储或远程文件；
- 聚合摘要：控制面数据库。

TensorBoard 可以作为按需兼容 UI，不能承担任务控制和真实状态所有权。

### 4.12 Quantization Pipeline

量化是独立转换 Job：

```text
Source Artifact + Target Hardware + Quality Constraints
  -> Quantization Plan
  -> Calibration
  -> Convert
  -> Quality Gate
  -> Benchmark
  -> New ArtifactManifest
```

量化插件声明支持的模型、GPU、引擎和校准需求。失败不能覆盖源制品。

### 4.13 Inference Gateway

目标数据面由 `cy-proxy`/`cy-gateway` 演进：

- OpenAI-compatible API；
- SSE 真流式；
- health/readiness；
- API key/JWT 验证；
- rate limit；
- model alias 和路由；
- metrics、审计和错误映射。

Community 迁移期间继续保留 Python Gateway Lite。Rust Gateway 达到协议与行为对拍后
再成为默认入口。

## 5. 物理部署边界

### 5.1 Community 本地模式

```text
Browser
  -> Local Controller (embedded persistence)
  -> Local Node Agent
  -> Docker workloads
```

默认不应要求用户先安装 PostgreSQL。Community 可使用嵌入式持久层，数据库接口保持
可替换。

### 5.2 Community 远程模式

```text
Browser -> Local Controller -> SSH tunnel -> Remote Node Agent
                                      -> Docker workloads
                                      -> Persistent volume
```

Local Controller 离线期间，Agent journal 是节点任务状态的事实源。重连时进行状态
reconciliation。

### 5.3 Pro/团队模式

```text
Users -> Shared Control Plane -> PostgreSQL/Object Storage
                             -> Multiple Node Agents
                             -> Registry/Cloud integrations
```

增加 OIDC、tenant、quota、billing、audit、HA 和共享规则服务，但不改变 Agent 与
RuntimeManifest 契约。

## 6. 数据与存储

### 6.1 Community

- Local Controller：嵌入式数据库保存 target、project、plan、任务索引和偏好；
- Node Agent：本地 journal 保存任务状态、租约和事件游标；
- 持久磁盘：模型、数据、checkpoint、日志和制品；
- OCI Registry：可选。

### 6.2 Pro

- PostgreSQL：Manifest、规则、验证、任务、revision、审计；
- 对象存储：大日志、报告、checkpoint 元数据附件；
- OCI Registry：Runtime 镜像与 attestation；
- 可选时序存储：高频指标。

### 6.3 数据一致性

- Runtime、TrainingRevision、Checkpoint、Artifact 使用不可变 ID；
- 可变任务记录只指向不可变资源；
- Node Agent 使用 operation ID 保证命令幂等；
- 重连通过事件游标和资源状态 reconciliation；
- 删除采用引用检查和保留策略，不级联误删模型/checkpoint。

## 7. 通信协议

### 7.1 Controller 到 Agent

建议使用版本化 gRPC：

- Probe；
- Artifact transfer/control；
- Build lifecycle；
- Job lifecycle；
- Event stream；
- Log/metric query；
- Health/capabilities。

传输默认经过 SSH tunnel。大型模型不通过 gRPC 中转，由 Agent 从来源直接下载。

### 7.2 Agent 到工作负载

- 容器生命周期通过 Docker Engine/Containerd API；
- 控制与指标使用 UDS 或 loopback gRPC；
- 模型和数据通过受限 Volume；
- Secret 通过 runtime secret/mount 注入。

### 7.3 推理数据面

外部 HTTP/OpenAI API 与内部控制 RPC 分离。训练控制请求不得混入每 token 的推理
热路径。

## 8. 安全边界

信任域：

1. 用户本地设备；
2. SSH 远程节点；
3. 模型/数据来源；
4. Registry；
5. 运行中的容器；
6. 对外推理客户端。

核心要求：

- 首次连接固定 SSH host key；
- Agent 不默认开放公网端口；
- Secret 与 Manifest 分离；
- 基础镜像固定 digest；
- 模型固定 revision 并校验 hash；
- remote code 默认拒绝；
- Builder 生成 SBOM/provenance；
- 训练容器最小权限、非 root、限制 Volume 和网络；
- Gateway 公网暴露前强制认证和 TLS 检查；
- 所有自动调整和制品转换形成审计事件。

## 9. 插件边界

### 9.1 Python 后端插件

- 推理：vLLM、SGLang、TensorRT-LLM 等；
- 训练：LLaMA-Factory、TRL、DeepSpeed adapter；
- 量化：AWQ、GPTQ、FP8 等。

使用 manifest + entry point/显式发现。插件缺失依赖时报告 `unavailable`，不能使控制
面 import 崩溃。

### 9.2 Rust 网关插件

auth、rate limit、tenant、billing、audit 通过 Cargo feature 和 tower Layer 组合，
不在热路径动态加载不稳定 ABI。

### 9.3 规则插件

兼容规则、Why 模板和 benchmark profile 为数据驱动包。声明能力与真实验证结果必须
分开记录。

### 9.4 平台核心语言与插件装载模型

- **平台核心语言 = Rust**：`cy-control-plane` 作为插件宿主 + 编排 + 契约 + 状态机 +
  Controller↔Agent gRPC。
- **Worker / 执行层、model-analyzer、量化 = Python 插件（进程外）**；**数据面网关 = Rust**。

**插件契约定义在「协议 + 能力」层（语言中立），不在 ABI 层。** 存在两种装载方式：

| 装载方式 | 允许语言 | 机制 | 用途 |
| --- | --- | --- | --- |
| 进程内 / 一起编译 | **仅 Rust** | Cargo feature + workspace crate + tower Layer | 热路径、网关过滤器、性能敏感的核心扩展 |
| 进程外 | **任意语言**（Python / Java / Rust） | gRPC + `plugin.toml` 能力清单发现 | engines / training / analyzer / 量化 / 企业业务插件 |

### 9.5 语言与装载模型的诚实边界

1. **「一起编译」只适用于 Rust**；Java / Python 是运行时发现 + 进程外拉起，产物不是
   单一胖二进制。
2. **每-token 热路径只允许进程内 Rust**；粗粒度后端一律进程外。
3. **每种语言需要一个薄 plugin SDK 包住 gRPC 契约**；增量顺序为 Python 先、Rust 次之、
   JVM 最后。

### 9.6 Kotlin 网关到 Rust 网关

网关功能（auth / jwt / tenant / quota / billing / audit）重写为**进程内 Rust 插件**。
现有 Pro Kotlin 网关留在 `plugins/pro` 作为**对拍基准**，Rust 版达标后退役；企业若坚持
JVM，可作为**进程外 Pro 业务服务插件**（非热路径）继续存在。

### 9.7 Python 原型到 Rust 正式版

因为契约是**协议级**（而非 ABI 级），任一插件可以先用 Python 进程外做原型，再用 Rust
进程内做正式版，**核心与调用方零改动**。

## 10. 当前到目标的迁移映射

| 当前模块 | 目标处置 |
| --- | --- |
| `python/cy_gateway_lite` | 保留为 Community 兼容入口，后续与 Rust Gateway 对拍 |
| Python Coordinator Lite | 过渡期保留；任务控制迁入 Control Plane/Agent，推理转发后续并入 Gateway/Proxy |
| `python/cy_exec` | 拆出决策逻辑，保留薄 Execution Adapter |
| `worker/core/server.py` | 模型执行保留；资源决策上移 Model Analyzer/Training Controller |
| `vram_optimizer.py` | 纯估算逻辑上移 Model Analyzer，并保留真实测试 |
| `training_engine.py` | 框架执行保留；状态机、revision、rollback 上移 Training Controller |
| `crates/cy-manifest` | 继续作为确定性 Manifest 核心 |
| `crates/cy-node-agent` | 实现远程节点生命周期与探测 |
| `crates/cy-control-plane` | 实现本地/服务化控制 API 与编排 |
| `crates/cy-proxy` | 修复已知问题后演进为 Rust 数据面 |
| `plugins/pro` | 保持可选边界，不进入 Community 默认产物 |
| legacy Conda/requirements | Runtime Builder 稳定后逐步删除 |

## 11. 建议实施顺序

> **唯一权威执行序列见 [`REBUILD_PLAN.md`](../REBUILD_PLAN.md)**（里程碑 M0–M8 / Epic
> E0–E8 + Ticket + 验收门）。本节 A–H 是同一计划的**架构视图**，与 REBUILD_PLAN 的
> Epic 一一对应；若有分歧以 REBUILD_PLAN 为准，避免多份路线图漂移。

### A. 产品契约补全

- 在现有四类 Manifest 上增加 WhyReport、ValidationResult、TrainingRevision、
  CheckpointMetadata、ArtifactManifest；
- 定义 Target、Project、Plan、Build、Run、Deployment 的资源状态机；
- 固定 Controller-Agent gRPC 契约。

### B. 远程节点最小闭环

- 实现 `cy target add ssh://...`；
- Agent bootstrap 与升级；
- Linux/NVIDIA probe；
- 本地 journal；
- 断线重连和任务 reconciliation。

### C. Explainable Planner

- Model Analyzer；
- NVIDIA + LLaMA-Factory + vLLM 最小规则集；
- 一到三个方案与 Why Report；
- 静态估算与 evidence level。

### D. 远程 Runtime Builder

- 模型元数据先行与远程下载；
- Docker + uv 按需构建；
- 本地、Registry、OCI Archive 三种交付；
- smoke test、digest、SBOM。

### E. 持久训练任务

- LLaMA-Factory adapter；
- calibration；
- 训练状态、事件、监控；
- checkpoint 和通知；
- SSH/UI 断开后继续。

### F. Guarded Auto

- TrainingRevision；
- checkpoint 边界调整；
- canary/rollback；
- 白名单参数和质量保护。

### G. 制品、量化与部署

- Artifact lineage；
- 量化独立 Job；
- vLLM Serving Runtime；
- Gateway 与 OpenAI 调用信息；
- 推理 benchmark 和回滚。

### H. Rust 数据面与 Pro

- 修复 `cy-proxy` 已知问题；
- 与 Python Gateway 行为对拍；
- 再接 tenant、billing、audit 和共享 PostgreSQL 控制面。

## 12. 关键架构决策

1. **UI 采用浏览器优先**：远程 GPU 节点通常无桌面，原生桌面不是 MVP 必需。
2. **Community 使用本地控制端 + 远程 Agent**：不要求公共 SaaS 控制面。
3. **远程直接构建是默认路径**：Registry 是复用选项，不是训练前置条件。
4. **Agent 是任务事实源之一**：控制端离线时仍能恢复真实状态。
5. **模型不进入镜像**：避免镜像组合和存储爆炸。
6. **Python 保留 AI 执行生态**：Rust 不重写训练和推理框架。
7. **训练调整发生在 checkpoint 边界**：可审计、可恢复、可回滚。
8. **量化产出新制品**：不覆盖原模型，也不与普通部署开关混淆。
9. **Gateway 与控制面分离**：训练管理流量不进入推理热路径。
10. **先做模块化单体**：组件是代码边界，不等于第一版必须部署十个微服务。
11. **核心与数据面用 Rust，执行层用 Python**：平台核心（插件宿主 / 编排 / 契约 /
    状态机 / Controller↔Agent gRPC）与数据面网关用 Rust；Worker / 执行层 /
    model-analyzer / 量化保持 Python 进程外插件。Rust 改写只发生在核心、Node Agent
    与数据面，**不存在「把 Python 执行层重写成 Rust」的目标**（与 §12.6 一致，另见
    `REBUILD_PLAN.md` 的 Epic 分解）。
12. **插件契约在「协议 + 能力」层而非 ABI 层**：进程内一起编译仅限 Rust（Cargo
    feature + tower Layer）；跨语言插件（Python / Java / Rust）经进程外 gRPC +
    `plugin.toml` 发现（详见 §9.4–§9.7）。

## 13. 第一条验收纵向切片

目标是用最少组件证明新架构成立：

```text
本地浏览器
-> Local Controller
-> SSH安装Agent
-> probe远程NVIDIA机器
-> 选择Hugging Face模型
-> 输出3个训练方案与Why Report
-> 远程BuildKit构建Docker+uv镜像
-> 模型下载到持久磁盘
-> calibration
-> 启动LLaMA-Factory LoRA/QLoRA
-> 断开SSH
-> 重连并继续查看指标/checkpoint
```

只有这条纵向切片完成后，才进入复杂自动调参、多后端、多云和 Pro 控制面扩展。
