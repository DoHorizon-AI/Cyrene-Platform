# "Where Does My Code Go?" — Developer Decision Guide

When implementing a new feature, fixing a bug, or extending Cyrene, consult this guide to determine the correct repository and directory owner.

---

## Interactive Decision Tree

```text
Are you adding/modifying...

1. High-level user intent, AI lifecycle state, or business orchestration?
   ├── Model Training (e.g. LoRA runs, checkpoints, loss metrics) ──► Cyrene-Yield
   ├── Model Serving (e.g. replica scaling, deployment state) ────────► Cyrene-Reactor
   ├── Gateway Routing & Policies (e.g. model balancing) ───────────► Cyrene-Exchange
   └── IM Bots & Chat Session UI ───────────────────────────────────► Cyrene-Plugins-Official (plugins/connectors/, plugins/agents/)

2. A concrete, replaceable implementation of an AI engine, tool, or storage?
   ├── Training Engine (e.g. LLaMA Factory, Axolotl, Unsloth) ──────► Cyrene-Plugins-Official (plugins/engine/...)
   ├── Serving Engine (e.g. vLLM, SGLang, TensorRT-LLM) ───────────► Cyrene-Plugins-Official (plugins/engine/...)
   ├── Model API Connector (e.g. OpenAI, Anthropic, DeepSeek) ──────► Cyrene-Plugins-Official (plugins/providers/...)
   ├── Tool Provider / MCP Server ──────────────────────────────────► Cyrene-Plugins-Official (plugins/tools/...)
   ├── Storage / Database / Vector Index (e.g. SQLite, FAISS) ──────► Cyrene-Plugins-Official (plugins/data/...)
   └── Gateway Runtime (e.g. FastAPI, ASP.NET Core, Spring) ────────► Cyrene-Plugins-Official (plugins/gateway/...)

3. Common platform contracts, execution primitives, or shared tooling?
   ├── Node Agent, Process Sandbox, Hardware Discovery ─────────────► Cyrene-Platform (kernel/)
   ├── Orchestration Reconciler, PlanStep, Attempt, Lease ──────────► Cyrene-Platform (framework/ or sdk/)
   ├── Shared Python SDKs (artifacts, preflight) ──────► Cyrene-Platform (sdk/python/)
   ├── Platform daemon systemd units ───────────────────────────────► Cyrene-Platform (infrastructure/systemd/)
   └── Platform-local governance, CI, and codegen ──────────────────► Cyrene-Platform (tooling/)

   Multi-repository checkout, profiles, and status ─────────────────► Cyrene-Workspace

4. Enterprise Organization & Commercial features?
   ├── SSO, SAML, SCIM, Enterprise RBAC, Multi-Tenancy ───────────► Cyrene-Enterprise (Proposed)
   └── Proprietary hardware acceleration (TPU / Ascend / AMD) ──────► Cyrene-Commercial-Plugins (Proposed)
```

---

## Practical Scenarios & Guidance

| What You Want To Do | Correct Target Location | Architecture Rule & Rationale |
|---|---|---|
| *"I want to add support for a new training framework (e.g. Axolotl)"* | **`Cyrene-Plugins-Official`** (`plugins/engine/axolotl`) | Implement `training.engine.v1` capability. Do not put engine code in Platform or Yield. |
| *"I want to add a new hyperparameter field to the training UI"* | **`Cyrene-Yield`** (`training/core/`) | High-level training semantics belong to Yield. Compile it into standard `TrainingSpec`. |
| *"I want to add a new serving engine (e.g. SGLang)"* | **`Cyrene-Plugins-Official`** (`plugins/engine/sglang`) | Implement `serving.engine.v1` capability. Reactor supervises it via generic `WorkerControl`. |
| *"I want to add autoscaling based on queue latency"* | **`Cyrene-Reactor`** (`runtime/core/`) | Autoscaling policy is a Product concern owned by Reactor. |
| *"I want to add a new Model API provider (e.g. Mistral)"* | **`Cyrene-Plugins-Official`** (`plugins/providers/`) | Add provider adapter in `cyrene.providers.model-api-connector`. |
| *"I want to add SSO / SAML authentication"* | **`Cyrene-Enterprise`** *(Proposed)* | Organization-scale security belongs to the enterprise tier. Public code consumes it via `IdentityProvider` capability. |
| *"I want to change GPU allocation logic"* | **`Cyrene-Platform`** (`kernel/resource/`) | Node GPU allocation is governed by Platform Lease & Resource manager. |
| *"I want to add a new Kernel primitive"* | **STOP & REVIEW** | First prove that existing primitives (`Worker`, `Operation`, `Endpoint`, `Lease`, `Fence`) cannot express the requirement. Kernel changes require an approved ADR. |
| *"I want to add hardware compatibility rules"* | **`Cyrene-Plugins-Official`** (`plugins/policy/compat-rules`) | Compatibility heuristics are pluggable business rules implementing `compatibility.evaluator.v1`. |
| *"I want to read physical GPU temperatures and VRAM"* | **`Cyrene-Platform`** (`kernel/agent/` or `NodeAgent`) | Physical hardware inspection is owned by the Platform Node Agent authority (`HardwareFacts`). |
---

<!-- Chinese Translation / 中文翻译 -->

# “我的代码应该放在哪里？”——开发者决策指南

实现新功能、修复错误或扩展 Cyrene 时，请参照本指南确定应由哪个仓库和目录负责。

---

## 交互式决策树

```text
你要新增或修改……

1. 高层用户意图、AI 生命周期状态或业务编排？
   ├── 模型训练（例如 LoRA 运行、checkpoint、loss 指标）──► Cyrene-Yield
   ├── 模型服务（例如副本扩缩容、部署状态）──────────────► Cyrene-Reactor
   ├── 网关路由与策略（例如模型负载均衡）────────────────► Cyrene-Exchange
   └── IM Bot 与聊天会话 UI ──────────────────────────────► Cyrene-Plugins-Official（plugins/connectors/、plugins/agents/）

2. AI 引擎、工具或存储的具体可替换实现？
   ├── Training Engine（例如 LLaMA Factory、Axolotl、Unsloth）──► Cyrene-Plugins-Official（plugins/engine/...）
   ├── Serving Engine（例如 vLLM、SGLang、TensorRT-LLM）──────► Cyrene-Plugins-Official（plugins/engine/...）
   ├── Model API Connector（例如 OpenAI、Anthropic、DeepSeek）──► Cyrene-Plugins-Official（plugins/providers/...）
   ├── Tool Provider / MCP Server ────────────────────────────► Cyrene-Plugins-Official（plugins/tools/...）
   ├── Storage / Database / Vector Index（例如 SQLite、FAISS）─► Cyrene-Plugins-Official（plugins/data/...）
   └── Gateway Runtime（例如 FastAPI、ASP.NET Core、Spring）──► Cyrene-Plugins-Official（plugins/gateway/...）

3. 通用平台契约、执行基元或共享工具？
   ├── Node Agent、Process Sandbox、Hardware Discovery ───────► Cyrene-Platform（kernel/）
   ├── Orchestration Reconciler、PlanStep、Attempt、Lease ────► Cyrene-Platform（framework/ 或 sdk/）
   ├── 共享 Python SDK（artifacts、preflight）────────────────► Cyrene-Platform（sdk/python/）
   ├── Platform daemon systemd unit ──────────────────────────► Cyrene-Platform（infrastructure/systemd/）
   └── Platform 本地治理、CI 和代码生成 ──────────────────────► Cyrene-Platform（tooling/）

   多仓检出、profile 和状态 ──────────────────────────────────► Cyrene-Workspace

4. 企业组织与商业功能？
   ├── SSO、SAML、SCIM、Enterprise RBAC、多租户 ────────────► Cyrene-Enterprise（拟议）
   └── 专有硬件加速（TPU / Ascend / AMD）─────────────────────► Cyrene-Commercial-Plugins（拟议）
```

---

## 实际场景与建议

| 需求 | 正确目标位置 | 架构规则与理由 |
|---|---|---|
| “我想支持一种新的训练框架（例如 Axolotl）” | **`Cyrene-Plugins-Official`**（`plugins/engine/axolotl`） | 实现 `training.engine.v1` capability。不要把引擎代码放入 Platform 或 Yield。 |
| “我想给训练 UI 增加一个超参数字段” | **`Cyrene-Yield`**（`training/core/`） | 高层训练语义归 Yield；将其编译为标准 `TrainingSpec`。 |
| “我想新增一个 serving engine（例如 SGLang）” | **`Cyrene-Plugins-Official`**（`plugins/engine/sglang`） | 实现 `serving.engine.v1` capability；由 Reactor 使用通用 `WorkerControl` 监管。 |
| “我想根据队列延迟添加自动扩缩容” | **`Cyrene-Reactor`**（`runtime/core/`） | 自动扩缩容策略属于 Reactor 所有的 Product 关注点。 |
| “我想新增一个 Model API provider（例如 Mistral）” | **`Cyrene-Plugins-Official`**（`plugins/providers/`） | 在 `cyrene.providers.model-api-connector` 中增加 provider adapter。 |
| “我想增加 SSO / SAML 认证” | **`Cyrene-Enterprise`**（拟议） | 组织级安全属于企业层；公开代码通过 `IdentityProvider` capability 消费它。 |
| “我想修改 GPU 分配逻辑” | **`Cyrene-Platform`**（`kernel/resource/`） | 节点 GPU 分配由 Platform Lease 和 Resource manager 管理。 |
| “我想增加一个 Kernel 基元” | **暂停并评审** | 先证明现有 `Worker`、`Operation`、`Endpoint`、`Lease`、`Fence` 无法表达需求。Kernel 改动需要已批准的 ADR。 |
| “我想增加硬件兼容规则” | **`Cyrene-Plugins-Official`**（`plugins/policy/compat-rules`） | 兼容性启发规则属于可插拔业务逻辑，实现 `compatibility.evaluator.v1`。 |
| “我想读取物理 GPU 温度和 VRAM” | **`Cyrene-Platform`**（`kernel/agent/` 或 `NodeAgent`） | 物理硬件检查由 Platform Node Agent authority（`HardwareFacts`）负责。 |
