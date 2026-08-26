# How to Read the Cyrene Codebase

If you are new to Cyrene, reading raw source code top-down can be overwhelming because execution spans multiple repositories. Use this guide to trace workflows effectively.

---

## 1. Trace a Model Training Workflow

Follow this file sequence to understand how a training job is received, planned, and executed:

1. **`Cyrene-Yield: training/core/src/cy_exec/training/`**
   - Read `spec.py`: Defines user-facing `TrainingSpec` (dataset, model, hyperparameters).
   - Read `controller.py`: Receives spec and compiles it into an `ExecutionPlan`.
2. **`Cyrene-Platform: sdk/python/cyrene_preflight/`**
   - Read `preflight.py`: Performs hardware compatibility verification and VRAM estimation before scheduling.
3. **`Cyrene-Platform: framework/` & `kernel/`**
   - Read `attempt.rs` / `reconciler.rs`: Manages execution steps, creates `Attempt` instances with monotonic `Generation`.
   - Read `worker_control.rs`: Spawns and supervises worker processes inside sandboxes with Lease fencing.
4. **`Cyrene-Plugins: plugins/models/hf-model-analyzer/`**
   - Read `analyzer.py`: Concrete model weight and memory estimation logic.

---

## 2. Trace a Model Serving Workflow

1. **`Cyrene-Reactor: runtime/core/`**
   - Read `server.py`: FastAPI server managing deployment state and routing requests.
2. **`Cyrene-Platform: kernel/resource/`**
   - Read `lease.rs`: Allocates GPU memory leases and binds endpoints.
3. **`Cyrene-Plugins: plugins/providers/model-api-connector/`**
   - Read `provider.py`: Connects to upstream model APIs or local serving workers.

---

## 3. Golden Rule for Code Navigation

> **Always ask: Is this code defining *Product Intent*, *Platform Mechanism*, or *Plugin Implementation*?**
>
> - If it calculates business state or billing $ightarrow$ Service.
> - If it manages leases, processes, or sandboxes $ightarrow$ Platform / Kernel.
> - If it implements a specific library (HF, FAISS, FastMCP) $ightarrow$ Plugin.
