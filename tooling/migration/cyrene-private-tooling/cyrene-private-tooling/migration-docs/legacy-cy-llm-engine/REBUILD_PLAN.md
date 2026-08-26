# CY-LLM 重构执行方案（交给 opencode 逐条执行）

> 本文件是**权威执行说明**。opencode 请以本文件为准，不要相信仓库里其它历史文档/README 的架构描述（它们描述的是已废弃的旧设计）。所有改动都在分支上进行，可回退（基线 tag：`pre-cleanup-baseline`）。

## 文档权威层级

- 产品场景、用户需求和交互流程以 [`docs/PRODUCT_REQUIREMENTS.md`](docs/PRODUCT_REQUIREMENTS.md) 为准。
- 组件边界、部署拓扑、状态所有权和迁移映射以 [`docs/TARGET_ARCHITECTURE.md`](docs/TARGET_ARCHITECTURE.md) 为准。
- 本文件负责里程碑、Ticket、实施顺序和验收门。
- `docs/ARCHITECTURE.md` 是已过时的旧设计文档（当前实现的过渡架构以目标架构文档与实际代码为准）。

> **本文件是唯一权威执行序列（Single Source of Truth for execution）。** 里程碑
> M0–M8（§7）、Epic E0–E8（§7.1）、Ticket、实施顺序与验收门只在本文件定义。
> `docs/PRODUCT_REQUIREMENTS.md §13` 与 `docs/TARGET_ARCHITECTURE.md §11` 分别是同一
> 计划的**产品视图**与**架构视图**，若与本文件冲突以本文件为准。

---

## 0. opencode 使用说明

- **自包含**：执行者没有此前对话上下文，只依赖本文件 + 实际代码。
- **逐条执行**：按 §7 的阶段顺序，每个 Ticket 独立成一次（或多次）提交，提交后跑该 Ticket 的“验收命令”。
- **只信代码**：判断某功能是否“已实现”以实际代码为准，不以注释/文档为准。
- **不碰护栏**：§3 标 `KEEP` 的资产是真实可用的执行层逻辑，只允许“搬运/包装/改接口”，不允许推倒重写其内部算法。
- **两个最高优先级**（用户明确关心）：
  1. **工作块 A（§5）**：重做“核心组件提供的功能”——因为最初的功能定位与现在需要的有较大出入。
  2. **工作块 B（§6）**：做完整的**插件系统**，并以插件区分**专业版 / 普通版（open-core）**。

---

## 1. 项目定位与技术栈

**定位**：CY-LLM 从“大一统训练/推理框架”重定位为
> **Runtime Control Plane**：自动识别目标硬件与模型需求 → 解析并构建 Docker+uv 运行环境 → 给出可解释的兼容性/安全/性能证据 → 通过 checkpoint 驱动的闭环控制优化训练 → 产出经过验证的推理服务。

**技术栈（分层，语言按层选择，不要全语言统一）**：

| 层 | 语言 | 说明 |
|---|---|---|
| 控制面 API（分析/解析/构建/验证/训练控制） | **Rust**（axum + sqlx）；也可 Python，二选一见 §10 | I/O-bound 业务逻辑 |
| 数据面网关（推理热路径） | **Rust**（axum + tonic + tower + rustls），由 `cy-proxy` 长出 | 唯一吃语言性能的地方 |
| 简版网关（个人开发者） | **Python**（FastAPI），保留 | 面向个人/普通版，简单够用 |
| 执行层（真正调用 vLLM/TRT/transformers/LoRA） | **Python**，保留并重塑为控制面驱动的 adapter | 框架本身就是 Python，不可 Rust 化 |
| 事实库 | **PostgreSQL** | manifests/规则/验证/benchmark/训练修订/checkpoint/安全发现 |
| Node Agent（硬件探测） | **Rust** 静态二进制（或 Python） | 目标机零运行时依赖 |

**边界铁律**：数据面/网关/控制面可用 Rust；**执行层保持 Python**；**不做 OAuth2 授权服务器（IdP）**，身份签发交给外部 IdP（Keycloak/Auth0/Ory Hydra），网关只做资源服务器校验。

---

## 2. 目标 monorepo 结构（单仓 + open-core）

现有 `CY-LLM-Engine` 仓库**就地演进**为唯一主仓库。`CY-LLM-Engine-Pro` 已经全量迁入本仓库的 `plugins/pro` 目录（详见 [plugins/pro/MIGRATION_INVENTORY.md](plugins/pro/MIGRATION_INVENTORY.md)），原 `CY-LLM-Engine-Pro` 仓库即刻冻结为历史只读备份，不再继续双线开发。

```
cy-llm/                          # = 现 CY-LLM-Engine 仓库
├── Cargo.toml                   # Rust workspace 根
├── crates/                      # ── Rust ──
│   ├── cy-proto/                # 由 proto/ai_service.proto 生成（唯一契约源）
│   ├── cy-manifest/             # Manifest 类型 + canonical hash（共享）
│   ├── cy-proxy/                # 数据面代理
│   ├── cy-gateway/              # 数据面网关二进制（axum + tonic）
│   ├── cy-gateway-plugins/      # 网关插件：auth / ratelimit / tenant / billing / audit
│   │   └── (community 默认 feature；pro 用 --features pro)
│   ├── cy-control-plane/        # 控制面 API：分析/解析/构建/验证/训练控制
│   ├── cy-node-agent/           # 硬件探测静态二进制
│   └── cy-*-pro/                # 【仅专业版】闭源 crate，community 构建不编译
├── python/                      # ── Python ──
│   ├── cy_exec/                 # 执行层（原 CY_LLM_Backend/worker 重塑而来）
│   │   ├── engines/             # 推理引擎插件（vllm/trt/nvidia/...）
│   │   ├── training/            # 训练后端插件（lora/qlora/llama-factory）
│   │   ├── vram/                # ← vram_optimizer.py
│   │   ├── probe/               # ← diagnose_env.py / diagnostic.py
│   │   └── adapter/             # gRPC servicer，实现 ai_service.proto（控制面驱动）
│   ├── cy_gateway_lite/         # ← CY_LLM_Backend/gateway_lite（保留的简版网关）
│   └── plugins/                 # 【第三方/专业版】Python 后端插件挂载点
├── proto/ai_service.proto       # 唯一契约源
├── migrations/                  # PostgreSQL (sqlx migrate)
├── deploy/{docker,compose}/     # 按 runtime 的 uv Dockerfile
├── docs/
└── REBUILD_PLAN.md              # 本文件
```

---

## 3. 模块处置矩阵（以实际代码为准）

| 现有模块 | 现状 | 处置 | 目标位置 / 说明 |
|---|---|---|---|
| `CY_LLM_Backend/worker/engines/{vllm_cuda,vllm_async,trt,nvidia}_engine.py` | 真实可用 | **KEEP → 重塑为插件** | `python/cy_exec/engines/`，按 §6 插件契约改造 |
| `CY_LLM_Backend/worker/engines/engine_factory.py` | 懒加载注册表（半插件） | **REBUILD** | 升级为 §6 的后端插件注册/发现系统 |
| `CY_LLM_Backend/worker/utils/vram_optimizer.py` | 真实 555 行 | **KEEP → 上移** | 显存预估/OOM 规划移入控制面 ModelAnalyzer/TrainingController |
| `CY_LLM_Backend/worker/core/server.py` | 真实（模型生命周期+VRAM+OOM+流式编排） | **SPLIT** | 决策部分上移控制面；执行部分留 `cy_exec/adapter` |
| `CY_LLM_Backend/worker/grpc_servicer.py` | 真实；含 `Linux-only` 硬断言 | **KEEP → 改接口** | 移入 adapter；去掉硬平台断言改为能力探测 |
| `CY_LLM_Backend/worker/training/*`, `training_engine.py` | 真实 LoRA/QLoRA/SFT | **KEEP → 插件化** | `python/cy_exec/training/`，成为训练后端插件 |
| `CY_LLM_Backend/coordinator_lite/` | 真实轮询 gRPC 代理 | **KEEP or FOLD** | 简单场景保留；能力可并入 cy-proxy |
| `CY_LLM_Backend/gateway_lite/` | 真实 FastAPI（非流式） | **KEEP** | `python/cy_gateway_lite/`，作普通版简版网关；补 SSE 流式 |
| `crates/cy-proxy/` | 真实 ~1.3k 行 tonic/tokio 代理 | **KEEP → 长成 cy-proxy/cy-gateway** | 加 axum HTTP+SSE 前端 |
| `proto/ai_service.proto` | 契约 | **KEEP** | 唯一契约源，生成 Rust+Python |
| `scripts/diagnose_env.py`, `worker/utils/diagnostic.py` | 真实硬件/依赖探测 | **KEEP → 搬入 Node Agent** | `python/cy_exec/probe/` 或 `crates/cy-node-agent` |
| `CY_LLM_Training/src/{train_lora,inference,dataset_converter}.py` | 真实 | **KEEP** | 并入训练后端插件 |
| `engines/mindie_engine.py`（华为 SDK 猜测桩）、`hybrid_engine.py`（experimental） | 桩/实验 | **DEFER** | 推迟到后端插件生态阶段 |
| `src/cy_llm/` | 陈旧重复副本 | **DELETE（需先解耦）** | 见 Ticket A0-3：先统一导入根再删 |
| `cy-llm` 脚本里 `cy start` / gradle / `java -jar` 路径 | 死代码 | **DELETE** | 见 Ticket A0-4 |
| `requirements-{base,nvidia,trt,vllm}.txt`（CUDA/torch 互斥） | 碎 | **REPLACE** | 迁 Docker+uv per-runtime 后删 |
| Pro 仓 Kotlin gateway（auth/jwt/tenant/quota/billing/audit/ratelimit + R2DBC Postgres） | 真实企业能力 | **REFERENCE** | 作 Rust 网关插件的“验收基准”，达到对标前不删 |

已删除（本轮清理，可从 tag 恢复）：7 份陈旧生成文档、`test_refactor.py`、`benchmark_throughput.py`、孤立的 `gateway/gateway_lite` 旧分叉、`refactor/`、`.kiro/`。

---

## 4. 核心契约（先定义，后一切）

所有 Manifest 为**确定性可 hash** 的结构体（同输入→同 hash）。定义在 `crates/cy-manifest/`（Rust）并镜像为 Python pydantic 模型（由同一 JSON Schema 生成）。

- **HardwareManifest**：OS/内核/glibc、CPU、内存、磁盘、GPU 型号/数量/显存/compute capability、驱动、驱动支持的 CUDA 上限、NVLink/PCIe 拓扑、容器运行时、BF16/FP16/FP8 能力。
- **ModelManifest**：架构、参数量、权重精度、上下文长度、格式(safetensors/…)、是否 remote code、tokenizer/template、量化格式、训练/推理显存估算。
- **WorkloadRequest**：model、workload(finetune|serve)、dataset、objective(priority)、constraints(max_gpu/max_cost)。
- **RuntimeManifest**：`runtime_id(sha256)`、workload、hardware_profile、python、cuda_runtime、torch、framework 版本、precision、training_strategy、base_image_digest、uv_lock_digest、validation_level。
- **WhyReport**：每个被选/被拒选项的理由（结构化）。
- **ValidationResult**：等级 `declared|resolved|built|smoke-tested|model-loaded|validated|benchmarked|certified`。
- **TrainingRevision** / **CheckpointMetadata**：见工作块 A 训练控制。

**验收**：`cy manifest hash <input.yaml>` 对同一输入两次输出相同 `runtime_id`。

---

## 5. 工作块 A —— 核心组件功能重做（优先级 1）

**为什么要重做**：旧设计里 `worker` 承担了“决策+执行”一切，`gateway/coordinator` 只是入口。新设计**反转**：控制面是“分析→决策→构建→验证”的大脑，执行层是被驱动的“薄肌肉”。因此需要把散落在 `worker/core/server.py`、`vram_optimizer.py` 里的**决策逻辑上移**到控制面，执行逻辑下沉为 adapter。核心组件因此需要重新定义边界与契约（不是丢弃已有算法）。

### 新核心组件（控制面）与其职责

| 组件 | 职责 | 复用来源 |
|---|---|---|
| `HardwareAnalyzer` | 产出 HardwareManifest | `diagnose_env.py`/`diagnostic.py` |
| `ModelAnalyzer` | 模型分析 + 训练/推理显存估算 | `vram_optimizer.py` |
| `CompatibilityResolver` | (硬件×驱动×CUDA×py×torch×框架×量化)→候选集 + 拒绝原因 | 新建，规则存 Postgres |
| `ManifestService` | Manifest 类型 + canonical hash | 新建 `cy-manifest` |
| `WhyReporter` | 解释为什么选/拒 | 新建 |
| `RuntimeBuilder` | RuntimeManifest→pyproject+uv.lock+Dockerfile→BuildKit 构建→smoke test→SBOM→签名→push→digest | 新建，包 Docker+uv |
| `Validator` | 分级验证状态机 | 新建 |
| `TrainingController` | 训练任务生命周期/指标/checkpoint 验证/Revision/canary/rollback | `training_engine.py` 重塑 |
| `ExecutionAdapter`（Python） | 实现 `ai_service.proto`，被控制面驱动，只做加载/生成/训练执行 | `worker/core/server.py` 执行部分 + engines |

### Tickets

**A0（前置：结构与解耦）**
- **A0-1 建 monorepo 骨架**：建 `Cargo.toml` workspace、`crates/`、`python/`、`migrations/`、`deploy/`；空 crate 能 `cargo build`。
- **A0-2 proto 归一**：`proto/ai_service.proto` 为唯一源，配 `cy-proto`(tonic-build) 与 Python 生成脚本；两侧生成物一致。
- **A0-3 统一导入根并删除 `src/cy_llm/`**：把执行层迁到 `python/cy_exec/`，统一包名为 `cy_exec`；修 `tests/unit/*`（原 `from cy_llm.*`）与 `pytest.ini`（`testpaths`/`--cov`）指向新根；确认 `pytest` 收集到测试后 `git rm -r src`。
  - 验收：`cd python && pytest -q`（无 GPU 的用例）能收集并跑通导入类测试；仓库无 `src/cy_llm`。
- **A0-4 删死代码**：从 `cy-llm` 脚本移除 `--skip-gradle`、`gradlew build`、`java -jar`、`pkill/pgrep ...SNAPSHOT.jar`、`cy start` 相关分支及帮助项。
  - 验收：`bash -n cy-llm` 语法通过；`grep -nE 'gradle|\.jar|java -jar' cy-llm` 无结果。

**A1 契约**：实现 §4 所有 Manifest 类型 + canonical hash（Rust `cy-manifest` + Python 镜像）。验收见 §4。

**A2 HardwareAnalyzer / Node Agent**：把探测逻辑封装为产出 HardwareManifest 的组件（`cy-node-agent` 或 `cy_exec/probe`）。
- 验收：目标机运行 `cy-node-agent probe` 输出合法 HardwareManifest JSON（schema 校验通过）。

**A3 ModelAnalyzer**：吸收 `vram_optimizer.py`，输入 ModelManifest+HardwareManifest+WorkloadRequest，输出训练/推理显存估算与可行精度/策略候选。
- 验收：给定 Qwen 7B + 单卡 24G，输出含 `qlora/bf16` 可行、`full/bf16` 因显存不足被标注。

**A4 ExecutionAdapter 重塑（关键）**：把 `worker/core/server.py` 拆分——决策（VRAM 预检/OOM 重试规划/配置推荐）上移至 A3/RuntimeBuilder/TrainingController；执行（load/generate/train）留在 `cy_exec/adapter`，通过 `ai_service.proto` 被控制面驱动；去掉 `grpc_servicer.py` 的 `platform.system()!="Linux"` 硬断言，改为能力探测降级。
- 验收：`cy_exec` 能在无 GPU 机器 import 且 servicer 可启动（引擎缺失时优雅降级，不崩）；Linux+GPU 上一次非流式推理 e2e 通过。

**A5 TrainingController**：训练任务生命周期状态机 + 指标采集 + checkpoint 验证 + TrainingRevision（不可覆盖、全量审计）+ canary + rollback；训练执行仍走 Python 训练后端。
- 验收：能从 checkpoint 用“修改后的安全参数”恢复并在失败时自动 rollback；每次调整留有 revision+理由+canary 结果。

> RuntimeBuilder / Validator / BenchmarkService 属工作块 A 的后续阶段（§7 M4+），契约在 A1 先定好。

---

## 6. 工作块 B —— 插件系统 + open-core（优先级 2）

**现状**：现有“插件”只有 `engine_factory.py` 的懒加载注册（不完整）、`cy-proxy` 的 Cargo feature、以及 Pro 仓的 Kotlin 企业版——三者不成体系。目标：建**统一插件系统**，并作为**专业版/普通版**的边界。

### 插件分类（按“插件缝粒度”放置，避免性能损失）

| 插件类型 | 机制 | 每次调用成本 | 用途 |
|---|---|---|---|
| **后端插件**（推理引擎、训练后端） | Python，**进程外**，走 `ai_service.proto` | 一次 IPC（µs–ms，相对 GPU 可忽略） | vllm/trt/nvidia/sglang/mindie；lora/llama-factory/deepspeed |
| **网关功能插件**（auth/tenant/ratelimit/billing/audit） | Rust crate + **Cargo feature** + tower Layer（进程内、静态/trait 分发） | 0～1 次 vtable | 数据面能力，open-core 主战场 |
| **控制面扩展插件**（兼容规则包、why-report 模板、validator、benchmark profile） | 数据/配置驱动（存 Postgres/config）；需运行时逻辑再用 WASM | 配置期为 0；WASM 有开销但不在热路径 | 规则/模板扩展 |

**铁律**：热路径（每 token/请求）**不做动态 `.so` 加载**（Rust 无稳定 ABI）；细粒度用编译期/静态分发，粗粒度后端用进程外 adapter。

### 插件契约（每类都要有）

- **插件清单 `plugin.toml`/manifest**：`id`、`kind(engine|training|gateway|rule)`、`version`、`capabilities`（硬件/精度/量化/流式 支持声明，供 CompatibilityResolver 使用）、`edition(community|pro)`、`license_gate`。
- **稳定接口**：Python 后端插件 = ABC（`load/generate/stream/unload` 等）；Rust 网关插件 = trait + tower Layer。
- **发现/注册**：Python 用 entry_points 或 `python/plugins/` 目录扫描 + 清单校验；Rust 用 Cargo feature 组合。

### Tickets

- **B1 后端插件契约**：定义 `cy_exec.engines.BaseEnginePlugin`（ABC）+ `plugin.toml` schema；把 `engine_factory.py` 升级为“清单驱动 + 能力声明”的注册/发现系统。
  - 验收：`cy plugins list` 列出已装引擎及其 capabilities；未装依赖的引擎显示为 `unavailable` 而非报错。
- **B2 迁移现有引擎为插件**：`vllm_cuda/vllm_async/trt/nvidia` 各自实现契约 + 清单；capabilities 真实反映硬件/精度支持。
  - 验收：CompatibilityResolver 能仅凭插件 capabilities 排除不可行组合（如某引擎不支持某量化格式）。
- **B3 训练后端插件**：`lora/qlora`（现有）+ `llama-factory` adapter 实现训练插件契约。
  - 验收：同一 WorkloadRequest 可切换 `trl-sft` 与 `llama-factory` 两个后端且都能起最小训练。
- **B4 网关功能插件 + Cargo features**：`cy-gateway-plugins` 把 auth(apikey/jwt-jwks)、ratelimit(tower-governor)、tenant/quota(sqlx→Postgres)、billing、audit 做成可编译开关的 tower Layer。
  - 验收：`cargo build -p cy-gateway`（community 默认）不含 tenant/billing；`cargo build -p cy-gateway --features pro` 含全部。
- **B5 open-core 边界与授权门**：专业版代码放独立 crate/目录（`crates/*-pro/`、`python/plugins/**/pro`），community 构建**不编译**；pro 能力在加载时做 license 校验。以 Pro 仓 Kotlin gateway 的 auth/tenant/quota/billing/audit 为对标基准逐项验收。
  - 验收：community 产物中静态不含 pro 符号；pro 产物无有效 license 时 pro 插件拒绝加载并给出明确错误。
- **B6（可选）控制面规则插件**：兼容规则包/why-report 模板做成可加载数据；普通版基础规则包，专业版扩展规则包。

---

## 7. 执行阶段顺序（opencode 逐条推进）

```
M0  结构与清理     A0-1 A0-2 A0-3 A0-4                （前置）
M1  核心契约       A1（Manifests + hash）
M2  核心组件重做   A2 A3 A4 A5                        ← 优先级 1
M3  插件系统       B1 B2 B3 B4 B5 (B6)                ← 优先级 2
M4  Runtime Builder（Docker+uv 按需构建 + Why Report）
M5  分级验证 + 安全（model-load/smoke/SBOM/签名/provenance/证书）
M6  Rust 数据面网关（cy-proxy→cy-gateway，补 SSE 真流式；Python 简版网关并存）
M7  Benchmark + 性能回归检测；训练 Guarded Auto（白名单参数自动调优）
M8  扩展后端（SGLang/TensorRT-LLM/Ray/ROCm/Ascend/K8s，按需）
```

> M2、M3 是用户最关心的两块，优先做；M0/M1 是其前置，必须先完成。M6 之前普通版继续用 Python 简版网关。

### 7.1 平台 + 扩展点分层 Epic 分解（E0–E8，权威）

把 §7 的里程碑按「平台核心 + 扩展点」重新组织为 Epic（借鉴 IntelliJ Platform 的
「插件宿主 + 扩展点」模型：核心提供扩展点，一切能力——包括普通版自带能力——都作为插件挂在扩展点上）。**本 Epic 序列与 §7 里程碑是同一计划的两种视图，均以本文件为权威。**

| Epic | 内容 | 层 | 语言 |
|---|---|---|---|
| **E0** | 平台核心 & 插件 SDK：契约 / 状态机 / 插件宿主 / 扩展点注册 / capability / Controller↔Agent gRPC | 核心 | Rust |
| **E1** | 扩展点定义：probe / model-analyzer / compat-rule / runtime-builder / execution-engine / training-backend / quantization / gateway-filter / notification / storage；每个给 trait/ABC + `plugin.toml` schema + capability + 参考插件；缺依赖报 `unavailable` 不崩核心 | 核心 | Rust + Python |
| **E2** | 普通版捆绑插件（每个都当插件做）：NVIDIA probe、HF model-analyzer[Python，含 vram_optimizer / diagnostic]、NVIDIA+LLaMA-Factory+vLLM 规则包、Docker+uv builder、LLaMA-Factory 训练后端、vLLM 引擎、Python Gateway Lite | 插件 | Python 为主 |
| **E3** | 远程节点闭环：`cy target add ssh`、Agent bootstrap / probe、journal、断线重连 reconcile | Agent | Rust |
| **E4** | 可解释规划器：Compatibility Resolver + Model Analyzer + Why Report + 1–3 方案 + evidence level | 核心 + 插件 | Rust + Python |
| **E5** | Runtime Builder：元数据先行→Docker+uv→本地 / Registry / OCI Archive→smoke test / digest / SBOM | Agent + 插件 | Python / Rust |
| **E6** | 持久训练 + Guarded Auto：LLaMA-Factory adapter、calibration、状态机、监控、checkpoint、TrainingRevision、canary / rollback | 核心 + 插件 | Rust + Python |
| **E7** | 制品 / 量化 / 部署：Artifact lineage、量化独立 Job、vLLM Serving、OpenAI 调用信息、benchmark | 插件 | Python |
| **E8** | Rust 数据面 + Pro：`cy-proxy`→`cy-gateway` 与 Python Gateway 对拍；再叠 tenant / billing / audit + 共享 PostgreSQL | 数据面 | Rust |

#### Epic ↔ 里程碑 ↔ 架构 ↔ 产品 映射

| Epic | §7 里程碑 (M) | `TARGET_ARCHITECTURE.md §11` (A–H) | `PRODUCT_REQUIREMENTS.md §13` 纵向切片 |
|---|---|---|---|
| E0 平台核心 & 插件 SDK | M0, M1 | A 产品契约补全 | 契约层（贯穿全流程） |
| E1 扩展点定义 | M3（契约先行） | A + §9 各插件边界 | 使各步骤可插拔 |
| E2 普通版捆绑插件 | M2, M3 | C / D / E / G 的默认实现 | 选择模型和数据 / 启动训练 / vLLM 部署 |
| E3 远程节点闭环 | M2 | B 远程节点最小闭环 | SSH连接 → 自动安装Agent → 硬件探测 / 断开后继续 |
| E4 可解释规划器 | M2 | C Explainable Planner | 生成并解释训练方案 |
| E5 Runtime Builder | M4 | D 远程 Runtime Builder | 远程构建Docker+uv Runtime / 下载模型 |
| E6 持久训练 + Guarded Auto | M2, M7 | E 持久训练 + F Guarded Auto | calibration / 启动训练 / 查看指标和checkpoint |
| E7 制品 / 量化 / 部署 | M5, M7 | G 制品、量化与部署 | 选择产物 / 生成vLLM部署 / OpenAI-compatible 调用 |
| E8 Rust 数据面 + Pro | M6, M8 | H Rust 数据面与 Pro | 通过 OpenAI-compatible API 调用（Rust 数据面） |

#### 关于 Rust 改写的范围（重要）

Rust 改写**不是独立赛道**：它只存在于 **E0（平台核心）、E3（Node Agent）、E8（数据面网关）**。
**不存在「把 Python 执行层重写成 Rust」的目标**——执行层长期保持 Python 进程外插件
（参见 [`docs/TARGET_ARCHITECTURE.md`](docs/TARGET_ARCHITECTURE.md) §12.6）。

#### 进度标注（截至最近核对）

- **M1（Manifests + 跨语言 canonical hash）：✅ DONE** —— `crates/cy-manifest` 与 Python 镜像 JCS 对拍通行。
- **E0–E8 骨架：⚠️ 已实现并接真外部工具，但仅 `built` 级、未经真机验证。** 控制面/网关/agent 编译通过、Rust 测试 94 passed；已接 `ssh`/`docker buildx`/`uv`/`nvidia-smi`/`syft`/`llamafactory-cli`/gRPC（驱动 `cy_exec`），假 fallback 与假 digest 已清除。但这些 `Command::new` 真路径在无 GPU/docker/远程环境**跑不了、未被验证**，仓库无任何真机验证痕迹。距 `validated` 差「真机跑一遍 §13 纵切」。
- **插件系统：🟡 架子在、未通电。** 10 个扩展点 trait + `plugin_host`/`plugin.toml` 解析已定义，但**真后端未 `impl` 扩展点 trait（仍是 `Mock*`）、无磁盘发现、community/pro 门控几乎为零、控制面无持久化** → 由下方 §7.2 F 阶段补全。
- **插件规范化（S1/S2）：✅ 已完成。** `docs/PLUGIN_SPEC.md` + `schemas/plugin.schema.json` + Rust `PluginKind/Edition/Runtime/PluginError/Plugin` 均已定义并对齐；7 个 community `plugin.toml` 和 1 个 pro bundle `plugin.toml` 全部按新规范写好；Rust 侧 schema 校验已接入（解析失败 → `Unavailable`，去掉了旧双重解析）。
- **Python 扩展点基类（S3 Python 侧）：✅ DONE。** `cy_exec.extension_points` 已创建完成（包含全部 10 个抽象基类如 `ProbeABC`, `ExecutionEngineABC`），7 个社区插件均已集成对接。
- **插件 SDK / 脚手架 / conformance（S4/S5）：✅ 基础已完成。** 新增 conformance 测试套件 `tests/test_plugin_conformance.py`（31 passed）；`cy-llm` CLI 已支持 `plugins list/validate/info`。

### 7.2 F 阶段：无 GPU 框架补全（当前重点）

**目标**：把「接了线但没通电」的骨架推进到 **插件架构真正立住 + 控制面可持久化可验证**，全部**本机可写、可真测**（判据为单测/集成测真跑过，不是"能编译"）。需要真 GPU/docker/远程机的验证（§13 纵切）**不在本阶段**。对应「补实 E1 + E0/E2/E4 的无硬件部分」，不新开路线图。

| Ticket | 内容 | 无 GPU 验收 |
|---|---|---|
| **F1 插件架构通电**（头号） | ①真后端（探测 / runtime_builder / 训练 / HF 分析）改为 `impl` 对应扩展点 trait，`Mock*` 仅留测试；②磁盘发现 `plugins/*/plugin.toml` → 注册 → capability 喂给 Resolver，加 `cy plugins list`；③控制面只经 trait 调用，去掉硬编码后端引用 | `cy plugins list` 列出插件 + capability；缺依赖插件报 `unavailable` 不崩核心；planner 用发现到的 capability；fake 插件单测通过 |
| **F2 Community/Pro 门控** | `plugin.toml` 的 `edition`/`license_gate` 生效；Cargo `feature="pro"`；pro 插件走门控路径 | `cargo build`（community）**不含 pro 符号**（CI 检查）；`--features pro` 才含；无 license 加载 pro → 明确拒绝、不崩 |
| **F3 可测性收口** | 抽 `CommandRunner` trait（真实现 + fake 实现）包住所有外部子进程（ssh/scp/docker/uv/nvidia-smi/syft/llamafactory-cli）；builder/probe/bootstrap/训练改为经它调用 | 用 `FakeCommandRunner` 单测覆盖 builder/probe/bootstrap 的**编排逻辑**（`Mock*` 从"返回写死值"升级为"对着 fake 被测"） |
| **F4 控制面持久化 + 资源状态机** | 嵌入式存储（sqlite/sled）+ Target/Project/Plan/Build/Run/Deployment 仓储层与状态机（TARGET §11.A、§6.1） | 建项目 → 进程重启 → 状态仍在；非法状态转移被拒；DB 集成测试通过 |
| **F5 离线规划链路** | 手填硬件配置 → 标 `declared` 的 HardwareManifest → Model Analyzer（真 HF 元数据）→ Compatibility Resolver → 1–3 方案 + Why Report（PRD §3.1/§5.1） | 给定离线硬件模板 + 真模型 id，端到端跑出带理由的方案卡，证据等级诚实标 `declared`；整链 fixture 测试 |
| **F6 Local Controller HTTP API** | 控制面操作暴露成 axum REST（未来 UI/CLI 入口，TARGET §4.1） | HTTP 集成测试覆盖 target/project/plan 的增查与状态流转 |

**依赖顺序**

```
F3(可测性) ─┐
            ├─► F1(插件通电) ─► F2(open-core 门控)
F4(持久化) ─┘
F1 + F5(离线规划) ─► 无 GPU 下端到端演示「分析→方案→Why Report」
F6(API) 收口对外
```

**F 阶段完成 = 插件架构真正立住（真后端作为插件被发现/调用）+ 普通/专业版可门控 + 控制面可持久化 + 可解释规划链在无 GPU 下端到端真跑通，且都有测试覆盖。** 至此框架「通电」；`validated`/`benchmarked` 仍需真 GPU 机（§13）。

---

### 7.3 插件规范化专项 Ticket（S3–S5 补全，与 F 阶段并行）

> 以下 Ticket 从 S1–S5 规范化审计中识别。S1/S2 已完成，S3/S4/S5 未做部分补充在此。**NS3-P0 是 F1 前置，必须先完成。**

#### 依赖顺序

```
NS3-P0（Python ABCs）─► NS3-P1（真实 impl）─► NS4-P2（conformance 测试）
                                               ─► NS5-P3（cy plugins list CLI）
NS4-P4（脚手架）独立，F 阶段后做
```

#### Ticket 列表

| Ticket | 内容 | 无 GPU 验收 |
|---|---|---|
| **NS3-P0 Python 扩展点基类**（F1 前置阻塞） | 在 `python/cy_exec/src/cy_exec/extension_points.py` 创建全部 10 个 ABC：`PluginBase`（`plugin_id/kind/api_version/capabilities`）+ `ProbeABC/ModelAnalyzerABC/CompatRuleABC/RuntimeBuilderABC/ExecutionEngineABC/TrainingBackendABC/QuantizationABC/GatewayFilterABC/NotificationABC/StorageABC`；在 `cy_exec/__init__.py` 导出；确认 `cy_manifest.models` Python 镜像可 import | `python -c "from cy_exec.extension_points import ProbeABC, ExecutionEngineABC"` 无报错；7 个 community 插件 `import` 无 ImportError |
| **NS3-P1 Python 插件真实 impl** | 7 个 community 插件 `.py` 从存根升级为真正实现 ABC 抽象方法（非空返回）；`vllm_engine.py` 包装 `cy_exec.engines.vllm_async_engine`；`nvidia_probe.py` 调用真实探测逻辑；`hf_model_analyzer.py` 调用 `vram_optimizer` | 各插件单测覆盖核心路径（无 GPU 时优雅返回 unavailable/fallback）；`pytest python/plugins/` 收集通过 |
| **NS4-P2 conformance 测试套件** | `tests/test_plugin_conformance.py`：对所有被发现的 `plugin.toml`①过 `jsonschema` 校验；②`kind` 属于 10 个扩展点或合法 bundle 种类；③`capabilities` 值在词表内；④`entrypoint` 模块可 import（缺依赖时标 `unavailable` 而非 error） | `pytest tests/test_plugin_conformance.py -q` 全部通过（无 GPU 环境）；非词表 capability 值在 CI 中被捕获 |
| **NS5-P3 `cy plugins` CLI** | 在 `cy-llm` 脚本中增加 `plugins` 子命令：`cy plugins list`（扫 `python/plugins/community/` + `plugins/pro/`，表格展示 id/kind/edition/capabilities/status）；`cy plugins validate <path>`（对单个 `plugin.toml` 做 schema + 词表校验，输出 OK / 具体错误行）；`cy plugins info <id>`（显示详细 manifest 字段） | `cy plugins list` 输出包含全部 7 个 community 插件；非规范 manifest 用 `cy plugins validate` 给出明确错误行号 |
| **NS4-P4 `cy plugin new` 脚手架**（F 阶段后做） | `cy plugin new <kind> <name>`：按 kind 生成最小插件目录（规范化 `plugin.toml` 模板 + Python ABC stub）；生成物直接可过 conformance 测试 | 生成的目录通过 `cy plugins validate` 和 `test_plugin_conformance.py` |

---

## 8. 每阶段验收命令与标准

| 阶段 | 验收命令（示例，按最终 CLI 名调整） | 通过标准 |
|---|---|---|
| M0 | `cargo build`; `cd python && pytest -q`; `bash -n cy-llm` | workspace 可构建；测试可收集；脚本语法通过；无 `src/cy_llm`、无死 Java 代码 |
| M1 | `cy manifest hash examples/*.yaml` ×2 | 同输入两次 `runtime_id` 一致 |
| M2 | `cy-node-agent probe`；`cy analyze model Qwen/Qwen2.5-7B --hw hw.json`；`cy_exec` 无 GPU 可 import | 输出合法 Manifest；显存估算含可行/不可行标注；servicer 无 GPU 优雅降级 |
| M3 | `cy plugins list`；`cargo build -p cy-gateway [--features pro]` | 插件按 capabilities 列出/降级；community 不含 pro 符号 |
| M4 | `cy build runtime.yaml` | 产出带 digest 的镜像 + uv.lock，smoke test 通过 |
| M5 | `cy validate <runtime_id>` | 展示来源/内容/测试证据/认证等级 |
| M6 | `cy-gateway` 与 `cy_gateway_lite` 并存对拍 | 响应/TTFT 一致；Rust 版内存显著更低；SSE 真流式可用 |
| M7 | `cy bench <runtime_id> --baseline <old>` | 输出 `performance_regression` 并给推荐 |

---

## 9. 主要技术风险

1. **A4 拆分决策/执行**边界划不清 → 逻辑重复或回退。缓解：先冻结 `ai_service.proto` 契约，再拆。
2. **插件能力声明与真实支持不符** → 解析器给出错误建议。缓解：capabilities 必须由 smoke test 回填校验（M5 前标 `declared`，验证后才 `validated`）。
3. **open-core 边界泄漏**（pro 符号进 community 产物）。缓解：pro 独立 crate + CI 检查 community 产物符号。
4. **Rust 网关交叉编译**引入 C 依赖 → 用 `rustls` 而非 openssl。
5. **无 GPU 开发环境**：M2/M4/M5 的 GPU 相关验收需真实 NVIDIA Linux 机；Windows/无 GPU 上只能跑到 import/契约级。

---

## 10. 需要人决策的点

1. **控制面语言**：Rust（axum+sqlx，与网关同栈、性能足）还是 Python（迭代快、与执行层同语言、便于共享 manifest 逻辑）？——执行层保持 Python 是前提，其余二选一。
2. **coordinator_lite**：保留为简单场景独立组件，还是并入 `cy-proxy`？
3. **Kotlin 企业网关**：达到 Rust pro 对标前保留运行；对标后是退役，还是作为 pro-only 备选长期保留？
4. **首个 POC 目标机**：是否有可跑 NVIDIA GPU 的 Linux 机用于 M2/M4/M5 落地验收？
5. **专业版授权机制**：license 校验用离线签名许可证，还是在线校验？影响 B5 实现。
