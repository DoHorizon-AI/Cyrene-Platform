# ADR-PLUGIN-RUNTIME: CYRENE Local Plugin Runtime Architecture

- **Status:** Superseded / Historical
- **Date:** 2026-07-24
- **Authors:** CYRENE Platform Team
- **Context:** Modularizing CYRENE into a platform architecture with zero-port local process isolation and compile-time Rust extensions.

> This ADR is preserved as historical context. Installable business-plugin
> runtime decisions are superseded by
> [ADR-PLUGIN-EXECUTION-BOUNDARY](ADR-PLUGIN-EXECUTION-BOUNDARY.md).
> In-process Rust remains valid only for audited, statically compiled Core
> platform adapters and is not an installable manifest runtime.

---

## 1. Context and Problem Statement

CYRENE is evolving into a modular architecture with a small host and
out-of-process advanced services.
The system requires:
1. Low-latency, high-performance execution on hot paths (e.g., HTTP Gateway, proxy routing).
2. Out-of-process isolation for third-party or Python/JVM logic (e.g., vLLM engines, LLaMA-Factory training, hardware probes) so that runtime failures, dependencies, or memory crashes in plugins never crash the main Rust host.
3. Zero TCP port footprint for local plugins to avoid port exhaustion, permissions issues, and local firewall blocks.
4. Clear versioning, explicit error classification, cancellation, and crash-loop quarantine.

---

## 2. Architectural Decisions

### 2.1 Dual Plugin Execution Models

#### A. Rust In-Process Plugins (`in-proc-rust`)
- **Compilation:** Static linking via Cargo features and explicit composition root (`composition_root.rs`).
- **ABI:** Pure Rust traits (`Plugin`, `Probe`, `ExecutionEngine`, etc.). No C-ABI, `.so`/`.dll` dynamic loading, or unsafe dynamic symbol loading.
- **Use Case:** High-throughput, zero-vtable-overhead hot paths (Gateway auth, rate limiters, local storage).
- **Lifecycle:** Married to binary lifecycle. Changes require binary re-compilation.
- **Open-Core Barrier:** Community builds exclude Pro feature crates at compile-time (`#[cfg(feature = "pro")]`).

#### B. External Subprocess Plugins (`subprocess-python`, `subprocess-jvm`)
- **Isolation:** Executed as standalone OS child processes spawned by Rust `PluginSupervisor`.
- **Transport:** Standard input/output (`stdio`) framing by default.
  - Rust Host writes request frames to child `stdin`.
  - Child writes response/stream frames to `stdout`.
  - `stdout` is strictly reserved for binary protocol messages. Logs MUST be routed to `stderr` or structured log events.
- **Process Boundaries:** Windows Named Pipe and Linux/macOS Unix Domain Sockets (UDS) are supported optional transports, but `stdio` is the mandatory baseline transport.
- **Network Boundaries:** TCP/gRPC is reserved exclusively for external APIs, remote SSH Node Agents, and multi-node clusters. Local out-of-process plugins MUST NOT listen on TCP ports.

---

### 2.2 Protocol Versioning & Host API Compatibility

- **`protocol_version` (uint32, e.g. `1`):** Governs the wire format and framing (`plugin_protocol.proto`). Negotiated during `Hello` / `HelloAck` handshake.
- **`api_version` (string, e.g. `"1.0"`):** Governs domain extension-point semantic contracts.

A host and plugin are compatible iff:
1. `protocol_version` is supported by both.
2. `api_version` satisfies host semver constraints.

---

### 2.3 Extension-Point Invocation Strategies

Each of the 10 extension points follows a strictly defined invocation strategy:

| Extension Point | Strategy | Description |
|---|---|---|
| `Probe` | `CollectAndRank` / `FirstAvailable` | Gathers hardware facts from active probes; ranks by evidence confidence. |
| `ModelAnalyzer` | `CollectAndRank` | Evaluates model requirements against analyzer capabilities; ranks best estimates. |
| `CompatRule` | `FanOut` + `Merge` | Executes rule evaluations concurrently; merges `WhyReport` decision items. |
| `RuntimeBuilder` | `FirstMatch` | Selects the first healthy builder plugin covering the target runtime stack. |
| `ExecutionEngine` | `FirstMatch` | Routes inference requests to the active, healthy engine covering model/precision/quant. |
| `TrainingBackend` | `FirstMatch` | Routes training jobs to the specified backend plugin. |
| `Quantization` | `FirstMatch` | Selects matching quantization engine plugin. |
| `GatewayFilter` | `OrderedChain` | Executes middleware filters sequentially in configured priority order; short-circuits on rejection. |
| `Notification` | `FanOut` | Broadcasts alert messages to all registered notification plugins. |
| `Storage` | `NamedSingleOwner` | Directs artifact storage operations to the specific declared storage plugin provider. |

---

### 2.4 Lifecycle, Error Classification & Quarantine

#### Lifecycle States
```text
Discovered -> Resolved -> Starting -> Handshaking -> Healthy -> Degraded -> Stopping -> Stopped
                                                            \-> Unavailable / Incompatible / Crashed / Quarantined / Disabled
```

- **`Healthy`:** Plugin process is running, handshake completed, health check passes.
- **`Quarantined`:** Plugin crashed repeatedly (crash-loop). Supervisor stops restarting until manual intervention or backoff reset.

#### Error Classification
`PluginError` includes 10 typed categories:
1. `Unavailable`: Dependent binaries or services missing.
2. `Incompatible`: Protocol version or API version mismatch.
3. `InvalidInput`: Malformed payload or validation error.
4. `PermissionDenied`: Unauthorized capability access.
5. `Timeout`: Operation exceeded deadline.
6. `Cancelled`: Request cancelled by host.
7. `Retryable`: Temporary transient failure.
8. `ExecutionFailed`: Internal plugin logic exception.
9. `ProtocolError`: Framing corruption, invalid stdout content, or JSON/protobuf parse error.
10. `Fatal`: Subprocess crashed or exited abnormally.

#### Subprocess Crash Handling & Restart Policy
- `never`: Process will not be restarted if terminated.
- `on-failure` (default): Restarted with exponential backoff (`min=500ms`, `max=30s`, `factor=2.0`).
- `always`: Always restarted regardless of exit code.
- **Crash Loop Detection:** If a plugin crashes >3 times within a 60-second window, its state transitions to `Quarantined`.

---

## 3. Mandatory Q&A Resolutions

1. **Who launches external plugins?**
   The Rust `PluginSupervisor` component using explicit command/argument arrays (never shell strings).

2. **Who performs version negotiation?**
   The Rust `PluginSupervisor` during initial `Hello` / `HelloAck` RPC handshake over `stdio`.

3. **Are logs allowed on stdout?**
   **No.** `stdout` is strictly reserved for binary/framed protocol messages. Any unformatted non-protocol output on `stdout` causes an immediate `ProtocolError` and quarantine trigger. Logs must go to `stderr`.

4. **What happens when a plugin crashes?**
   The host supervisor catches sub-process EOF / SIGCHLD, flags active requests as `Fatal` / `Unavailable`, and applies the restart policy without crashing the core Rust host.

5. **How are multiple implementations for an extension point chosen?**
   Based on the extension point's invocation strategy (§2.3) combined with declared plugin priority and capability matching.

6. **Which communications are allowed to occupy TCP ports?**
   Only the public OpenAPI REST gateway, gRPC server for external client access, and SSH/gRPC communication to remote Node Agents. Local plugins are strictly zero-port.

---

## 4. Consequences and Compliance

- Core stability is guaranteed against third-party plugin crashes or memory leaks.
- Zero local port allocation removes port conflict risks on developer/production machines.
- Protocol buffer generation tools ensure synchronized cross-language Rust, Python, and JVM SDKs.
