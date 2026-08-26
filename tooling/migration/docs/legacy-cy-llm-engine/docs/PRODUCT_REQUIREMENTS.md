# CY-LLM 产品需求与交互流程

> 状态：目标产品需求基线  
> 适用范围：Community 第一版与后续 Pro 扩展  
> 目标用户：通过 SSH 或 Cloud Shell 使用云端 GPU 训练、微调和部署大模型的个人开发者与小型团队

## 1. 产品定位

CY-LLM 是面向远程 GPU 工作负载的 Runtime Control Plane。它不重新实现
PyTorch、LLaMA-Factory 或 vLLM，而是把机器探测、模型分析、依赖规划、
Docker + uv 构建、训练控制、制品转换和推理部署串成一条可解释、可恢复的流程。

系统必须帮助用户回答：

1. 这台机器能否训练或部署目标模型？
2. 应选择哪组 CUDA、Python、PyTorch、框架与精度？
3. 为什么推荐这组方案，为什么拒绝其他方案？
4. 预计显存、吞吐、耗时和风险是多少？
5. 环境是否只是解析成功，还是已经真实构建、加载和验证？
6. SSH 断开后训练是否继续，失败后能否恢复？
7. 训练产物如何针对另一台机器重新量化和部署？

## 2. 典型用户现状

当前常见流程是：

1. 在 Hugging Face、ModelScope 或其他站点找到模型。
2. 手工把权重下载到云机器的持久磁盘。
3. 在 GPU 机器上反复试装 CUDA、Torch、Transformers、vLLM、
   LLaMA-Factory、FlashAttention 等依赖。
4. 找到能运行的组合后手写 Dockerfile 和依赖文件。
5. 可选地把镜像推送到 Docker Hub、ACR 或其他 Registry。
6. 在目标机器拉取镜像、挂载模型和数据集。
7. 编写训练脚本并试调训练参数。
8. 启动训练，并通过日志或 TensorBoard 间歇查看状态。
9. 训练完成后再手工合并、量化、构建推理环境和开放 API。

该流程的问题不是单一安装命令复杂，而是缺少可复用的决策、验证证据、
任务生命周期和制品链路。

## 3. 产品原则

### 3.1 连接优先，不要求用户手填机器配置

用户通常只提供 SSH 连接信息。系统安装或连接远程 Node Agent，自动产出
HardwareManifest。手工机器配置只用于尚未租用机器时的离线规划，并必须标记为
`declared`，不能冒充实机探测结果。

### 3.2 元数据先行，权重后下载

选择模型后先获取 `config.json`、tokenizer、文件列表、revision、license 和
权重格式等小型元数据。确认硬件可行并选定方案后，才把完整模型直接下载到目标
机器的持久磁盘。

### 3.3 模型与 Runtime 镜像分离

模型、数据集、checkpoint 和训练产物属于持久制品；CUDA、Python、Torch 和框架
属于 Runtime 镜像。模型默认通过只读 Volume 挂载，不打入镜像。

### 3.4 解释先于自动化

任何推荐、拒绝、参数调整、量化或回滚都必须提供结构化原因、输入证据、置信度和
验证等级。

### 3.5 正常路径零干预，异常路径可恢复

鉴权、license 接受、公开网络、预算超限、数据歧义、质量风险和破坏实验语义的
参数变更必须由用户确认。系统不能以“全自动”为由绕过安全与质量门。

### 3.6 UI 可以关闭，任务必须继续

训练和推理任务由远程 Node Agent 与容器运行时托管，不依赖 SSH session、Cloud
Shell、浏览器、`screen` 或 `tmux` 存活。

## 4. 支持的部署形态

### 4.1 本地控制、远程执行（默认）

- 浏览器 UI 和 Local Controller 运行在用户本地或 WSL。
- Node Agent 运行在远程 Linux GPU 机器。
- 控制连接默认通过 SSH 和 SSH tunnel，不要求暴露远程控制端口。

### 4.2 单机模式

Local Controller、Node Agent 和工作负载运行在同一台 Linux/WSL 机器，适合本地
GPU 和开发验证。

### 4.3 Cloud Shell 模式

控制服务可以在 Cloud Shell 启动并输出访问地址或 tunnel 指令；真正的 Node
Agent 仍安装在 GPU 节点，而不是把 Cloud Shell 当作训练节点。

### 4.4 服务化控制面（后续）

为团队提供长期运行的共享控制面、PostgreSQL、多用户、OIDC、配额和审计。该模式
不属于 Community MVP 的强制依赖。

## 5. 首次连接流程

### 5.1 添加执行目标

入口选项：

- 本地 WSL/Linux；
- 远程 SSH；
- 已安装 Node Agent；
- 离线硬件配置。

远程 SSH 输入只要求：

- Host 与 Port；
- Username；
- SSH Key 或受支持的认证方式；
- 可选工作根目录。

系统必须：

1. 展示并确认首次 SSH host key。
2. 检查目标 OS 与架构。
3. 安装或升级 Node Agent。
4. 将 Agent 注册为受控后台服务。
5. 建立安全通道并执行探测。
6. 返回 HardwareManifest 与探测证据。

### 5.2 硬件与系统页面

至少展示：

- OS、内核、glibc、CPU、内存、Swap；
- GPU 型号、数量、显存、compute capability；
- Driver 与可支持的 CUDA 上限；
- BF16、FP16、FP8 等能力；
- NVLink/PCIe 拓扑；
- Docker、Container Toolkit、BuildKit；
- 持久磁盘路径、剩余空间与基本 I/O 信息；
- 当前告警、缺失组件和可修复建议；
- `declared`、`probed`、`verified` 等证据状态。

## 6. 训练交互流程

### 6.1 创建训练项目

用户选择“训练/微调”，然后指定：

- 基础模型来源；
- 模型 revision 或 commit；
- 数据集来源；
- 训练任务类型；
- 输出与 checkpoint 的持久目录；
- 质量、速度、成本和资源约束。

模型来源至少支持：

- Hugging Face model ID；
- ModelScope；
- 本地或远程目录；
- S3/兼容对象存储；
- 自定义 URL；
- 已缓存模型。

### 6.2 模型与数据预检

模型预检包括：

- 架构、参数规模、上下文长度；
- 权重精度与格式；
- tokenizer/template；
- 量化格式；
- remote code；
- license 与鉴权要求；
- 文件大小和预计磁盘占用；
- 固定 revision 与文件 hash。

数据预检包括：

- UTF-8、JSON/JSONL/Parquet 等格式；
- schema 与对话模板映射；
- 空样本、重复样本、截断风险；
- token 数估算；
- train/eval 划分；
- license、隐私和敏感数据提示。

### 6.3 选择优化目标

用户选择一个主要目标：

- 均衡；
- 最高吞吐；
- 最低显存；
- 最低预计成本；
- 质量优先；
- 最快完成时间。

可选约束包括：

- 最大 GPU 数；
- 最大费用或训练时长；
- 最低上下文长度；
- 是否允许量化、CPU/NVMe offload；
- 是否允许受控自动调整。

### 6.4 推荐方案

系统输出一到三个目的明确的方案，默认使用：

1. 均衡方案；
2. 性能方案；
3. 低资源方案。

每张方案卡必须展示：

- 训练后端和训练策略；
- Python、CUDA、Torch、Transformers、LLaMA-Factory 等精确版本；
- FP16、BF16、FP8、LoRA 或 QLoRA；
- micro batch、gradient accumulation、effective batch；
- sequence length、gradient checkpointing、FlashAttention；
- 预计显存、磁盘和构建时间；
- 预计 tokens/s、总耗时和费用范围；
- 估算置信度；
- 当前验证等级；
- 推荐原因、风险与被拒绝方案。

静态性能预测必须显示范围与置信度，不能伪装成精确实测值。

### 6.5 Calibration

在正式训练前执行短时校准：

- 验证模型加载；
- 运行少量 batch；
- 测量显存峰值、tokens/s、数据等待和通信开销；
- 检查 OOM、NaN、gradient overflow；
- 更新耗时和费用预测；
- 必要时生成修订后的推荐。

### 6.6 Runtime 构建与分发

提供三种模式：

1. **目标机器直接构建并运行**：默认方式，不经过 Registry。
2. **推送 OCI Registry**：适合复用，支持 Docker Hub、ACR、GHCR、Harbor、
   ECR 等通用 OCI Registry。
3. **导出 OCI Archive**：适合离线和私有环境。

构建结果必须包含：

- RuntimeManifest 与 runtime ID；
- Docker/OCI image digest；
- `uv.lock` digest；
- 构建日志；
- SBOM 与 provenance；
- smoke test 和安全扫描状态。

### 6.7 最终确认与启动

启动前展示：

- 目标机器、模型、数据和输出目录；
- 预计费用、显存、磁盘和时间；
- 将要使用的 secret 引用；
- 网络和安全策略；
- 自动调整级别；
- checkpoint 与保留策略。

确认后由 Node Agent 启动训练容器。SSH 和 UI 断开不影响任务。

### 6.8 训练监控

运行期间至少展示：

- step、epoch、已处理 token；
- train/eval loss、learning rate、grad norm；
- tokens/s、step time；
- GPU utilization、显存、温度与功耗（可用时）；
- dataloader wait 和多卡通信；
- 预计剩余时间；
- checkpoint 列表；
- Training Revision 与自动调整记录；
- warning、error 和恢复状态。

TensorBoard 是兼容视图，不是唯一监控控制器。计划、构建和校准进程完成后应退出，
训练阶段只保留 Node Agent、训练容器和必要的轻量指标采集。

### 6.9 Checkpoint 与受控自动调整

支持三种模式：

- `observe`：只给建议；
- `guarded-auto`：只修改白名单参数，默认模式；
- `experimental-auto`：允许更广的探索和训练分支。

默认可自动调整：

- micro batch；
- gradient accumulation；
- dataloader worker/prefetch；
- checkpoint/eval 间隔；
- OOM 恢复；
- 安全的 precision fallback。

模型结构、tokenizer、LoRA rank、adapter target、数据语义和量化布局不得静默修改。

每次调整必须形成不可覆盖的 TrainingRevision，记录原因、证据、checkpoint、状态保留
策略、canary 结果与回滚结果。

### 6.10 通知

Checkpoint、异常、恢复和训练完成产生持久事件。UI 重连后可以补拉。后续可通过
浏览器通知、webhook、邮件或消息插件投递。

## 7. 训练完成与制品管理

训练完成后进入制品确认页：

1. 选择最佳 checkpoint；
2. 验证完整性；
3. 运行基础评测；
4. 可选合并 LoRA；
5. 计算模型 hash；
6. 绑定基础模型、数据集、Runtime 和 Revision 历史；
7. 选择导出格式；
8. 进入部署规划。

任何最终模型制品都必须可追溯到：

- 基础模型 revision；
- 数据集 digest；
- RuntimeManifest；
- 训练参数修订链；
- checkpoint；
- 评测与完整性结果。

## 8. 量化流程

量化是独立制品转换任务，不是部署页面中的无条件开关。

流程为：

1. 选择源模型制品；
2. 指定或探测目标硬件；
3. 生成量化候选；
4. 准备 calibration 数据；
5. 执行转换；
6. 做质量与性能评测；
7. 生成新的 ModelManifest 和制品 digest。

方案必须说明显存、吞吐、引擎支持、转换耗时、校准要求和质量变化。无明显收益时应
允许推荐“不量化”。

## 9. 推理部署流程

部署向导：

1. 选择训练产物或现有模型；
2. 选择并探测目标机器；
3. 推荐一到三个推理方案；
4. 可选进入量化流程；
5. 生成 Serving Runtime；
6. 构建、推送或拉取镜像；
7. 启动推理引擎和 Gateway；
8. 执行健康检查与性能 smoke test；
9. 输出调用方式。

系统自动生成：

- Base URL；
- API Key；
- Model Alias；
- OpenAI-compatible endpoints；
- curl、Python SDK 和环境变量示例。

默认仅监听 loopback 或私有网络。公网暴露必须经过 TLS、认证、限流、防火墙和日志
脱敏确认。

## 10. Secret 与安全要求

- SSH key、Hub token、Registry token 和对象存储凭据不写入 Manifest、Dockerfile、
  镜像层或普通日志。
- Local Controller 优先使用系统凭据存储；远程任务只接收短期 secret 引用或受控
  注入。
- 默认固定模型 revision；浮动分支必须显式提示。
- `trust_remote_code` 默认关闭，需要确认并产生审计事件。
- 下载支持断点续传、hash 校验与来源记录。
- 远程 Agent 默认只监听 loopback/UDS，并通过 SSH tunnel 管理。
- 公网控制面不属于 Community 默认安装方式。

## 11. UI 信息架构

第一版页面：

1. 首页/任务概览；
2. 执行目标；
3. 新建训练或部署项目；
4. 模型与数据；
5. 目标与约束；
6. 推荐方案与 Why Report；
7. Calibration；
8. Runtime 构建与分发；
9. 最终确认；
10. 训练监控或推理服务；
11. Checkpoint 与制品；
12. 量化与部署。

任务状态至少包括：

```text
draft -> probing -> analyzing -> planned -> calibrating
      -> building -> validating -> ready -> running
      -> checkpointing/adapting -> completed/failed/canceled
```

## 12. Community MVP

第一版范围：

- 本地 WSL/Linux 浏览器控制界面；
- SSH 远程 Linux GPU；
- 自动安装和重连 Node Agent；
- NVIDIA GPU；
- Hugging Face 模型；
- 本地、远程或对象存储数据集；
- LLaMA-Factory LoRA/QLoRA；
- vLLM 推理；
- 目标机器直接构建；
- 可选通用 OCI Registry；
- 基础训练监控和 checkpoint 通知；
- OpenAI-compatible Gateway。

首版暂缓：

- 自动购买或开关云主机；
- 多节点训练；
- AMD、Ascend；
- 任意层级混合精度；
- 完全自主超参数搜索；
- 多租户、计费；
- 公网共享控制面；
- 原生桌面客户端；
- 所有量化格式。

## 13. MVP 成功标准

> **唯一权威执行序列见 [`../REBUILD_PLAN.md`](../REBUILD_PLAN.md)**（里程碑 M0–M8 /
> Epic E0–E8 + Ticket + 验收门）。本节是同一计划的**产品视图**（验收纵向切片），与
> REBUILD_PLAN 的 Epic 对应；若有分歧以 REBUILD_PLAN 为准，避免多份路线图漂移。

在一台本地 WSL 控制端和一台远程 NVIDIA Linux GPU 机器之间完成：

```text
SSH连接
-> 自动安装Agent
-> 硬件探测
-> 选择模型和数据
-> 生成并解释训练方案
-> 远程构建Docker+uv Runtime
-> 下载模型到持久磁盘
-> calibration
-> 启动训练
-> SSH断开后继续运行
-> 查看指标和checkpoint
-> 选择产物
-> 生成vLLM部署
-> 通过OpenAI-compatible API调用
```

每个关键结果都必须带 Manifest、digest、验证等级和 Why Report，不能只有日志文本。
