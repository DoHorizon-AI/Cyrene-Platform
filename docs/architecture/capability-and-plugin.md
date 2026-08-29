# Architecture Deep-Dive: Capabilities & Plugins

In Cyrene, a **Plugin** is defined by its **architectural role**, not by its code size or programming language.

---

## Capability-First Architecture

```text
Service (Yield / Reactor)
       │
       ▼ (Calls Abstract Contract)
Capability API (e.g. model.analyzer.v1)
       │
       ▼ (Resolved by Catalog)
Plugin Implementation (e.g. cyrene.models.hf-analyzer)
```

1. **Role, Not Size**: A Plugin can be a single Python script (e.g. `compat-rules`), an out-of-process FAISS daemon (`memory`), or a multi-threaded ASP.NET Core YARP gateway (`gateway/aspnet-core`).
2. **No Direct Service-to-Plugin Imports**: Services depend strictly on capability interfaces. The platform's resolver discovers and invokes the matching active plugin from the catalog at runtime.
