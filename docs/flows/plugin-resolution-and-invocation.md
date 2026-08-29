# End-to-End Flow: Plugin Resolution & Invocation

This document explains how Capability requests from Services are dynamically resolved and bound to concrete Plugin implementations.

---

## Dynamic Resolution Flow

```text
1. Service Request (e.g. "I need model.analyzer.v1")
          │
          ▼
2. Platform Capability Resolver
          │
          ├── Reads: catalog/official/catalog.json
          ├── Matches: Active plugin providing "model.analyzer.v1"
          └── Validates: Manifest schema & Conformance level
          │
          ▼
3. Execution Binding Strategy
   ├── If Mode == "inline"  ──► Load Python module / C# DLL directly in process
   ├── If Mode == "worker"  ──► Invoke via WorkerControl (stdio / HTTP IPC)
   └── If Mode == "job"     ──► Schedule as standalone Kernel Operation
```
