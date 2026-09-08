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
   └── IM Bots & Chat Session UI ────────────────────────────────────► cyrene-astrbot-rev

2. A concrete, replaceable implementation of an AI engine, tool, or storage?
   ├── Training Engine (e.g. LLaMA Factory, Axolotl, Unsloth) ──────► Cyrene-Plugins (plugins/engine/...)
   ├── Serving Engine (e.g. vLLM, SGLang, TensorRT-LLM) ───────────► Cyrene-Plugins (plugins/engine/...)
   ├── Model API Connector (e.g. OpenAI, Anthropic, DeepSeek) ──────► Cyrene-Plugins (plugins/providers/...)
   ├── Tool Provider / MCP Server ──────────────────────────────────► Cyrene-Plugins (plugins/tools/...)
   ├── Storage / Database / Vector Index (e.g. SQLite, FAISS) ──────► Cyrene-Plugins (plugins/data/...)
   └── Gateway Runtime (e.g. FastAPI, ASP.NET Core, Spring) ────────► Cyrene-Plugins (plugins/gateway/...)

3. Common platform contracts, execution primitives, or shared tooling?
   ├── Node Agent, Process Sandbox, Hardware Discovery ─────────────► Cyrene-Platform (kernel/)
   ├── Orchestration Reconciler, PlanStep, Attempt, Lease ──────────► Cyrene-Platform (framework/ or sdk/)
   ├── Shared Python SDKs (artifacts, environment, preflight) ──────► Cyrene-Platform (sdk/python/)
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
| *"I want to add support for a new training framework (e.g. Axolotl)"* | **`Cyrene-Plugins`** (`plugins/engine/axolotl`) | Implement `training.engine.v1` capability. Do not put engine code in Platform or Yield. |
| *"I want to add a new hyperparameter field to the training UI"* | **`Cyrene-Yield`** (`training/core/`) | High-level training semantics belong to Yield. Compile it into standard `TrainingSpec`. |
| *"I want to add a new serving engine (e.g. SGLang)"* | **`Cyrene-Plugins`** (`plugins/engine/sglang`) | Implement `serving.engine.v1` capability. Reactor supervises it via generic `WorkerControl`. |
| *"I want to add autoscaling based on queue latency"* | **`Cyrene-Reactor`** (`runtime/core/`) | Autoscaling policy is a Product concern owned by Reactor. |
| *"I want to add a new Model API provider (e.g. Mistral)"* | **`Cyrene-Plugins`** (`plugins/providers/`) | Add provider adapter in `cyrene.providers.model-api-connector`. |
| *"I want to add SSO / SAML authentication"* | **`Cyrene-Enterprise`** *(Proposed)* | Organization-scale security belongs to the enterprise tier. Public code consumes it via `IdentityProvider` capability. |
| *"I want to change GPU allocation logic"* | **`Cyrene-Platform`** (`kernel/resource/`) | Node GPU allocation is governed by Platform Lease & Resource manager. |
| *"I want to add a new Kernel primitive"* | **STOP & REVIEW** | First prove that existing primitives (`Worker`, `Operation`, `Endpoint`, `Lease`, `Fence`) cannot express the requirement. Kernel changes require an approved ADR. |
| *"I want to add hardware compatibility rules"* | **`Cyrene-Plugins`** (`plugins/policy/compat-rules`) | Compatibility heuristics are pluggable business rules implementing `compatibility.evaluator.v1`. |
| *"I want to read physical GPU temperatures and VRAM"* | **`Cyrene-Platform`** (`kernel/agent/` or `NodeAgent`) | Physical hardware inspection is owned by the Platform Node Agent authority (`HardwareFacts`). |
