# Cyrene-Platform Canonical API & Substrate Contract Specification

Cyrene-Platform is the shared execution substrate for Products and capability
plugins. Its public surface is product-neutral: Kernel execution, isolation,
resource observations, capability resolution, Artifact references, environment
locks, and versioned wire contracts.

## Repository ownership

Platform owns:

- Kernel process execution, sandboxing, resource accounting, Lease/Fence
  enforcement, and worker lifecycle transport;
- canonical Node resource observations and immutable HardwareFacts projections;
- generic capability package resolution and language-neutral worker/service
  runtime selection;
- Artifact identity, CAS mechanisms, environment locks, and contract TCKs.

Products own their run, attempt, retry, workflow, draft, result, and persistence
state. Plugins own capability implementations and conversion between generic
wire objects and capability-specific request types. Adding a Product or plugin
must not require a Platform source change.

The former Python/Kotlin ProductRun control plane moved to Cyrene-Yield. The
former in-Platform model analyzer and compatibility evaluator moved behind
Yield-owned replaceable ports; the official Hugging Face analyzer remains in
Cyrene-Plugins-Official. These are no longer Platform APIs.

## Public Platform objects

| Object / Type | Contract | Status | Responsibility |
|---|---|---|---|
| WorkerControl | kernel_authority.proto | IMPLEMENTED | Generic supervised worker lifecycle. |
| HardwareFacts | cyrene_preflight and Node resource inventory | IMPLEMENTED | Immutable Platform resource projection. |
| ArtifactRef | cyrene_artifacts and manifest schemas | IMPLEMENTED | Immutable digest-addressed artifact reference. |
| EnvironmentLock | cyrene_environment | IMPLEMENTED | Reproducible resolved environment identity. |
| CapabilityRequirement | plugin manifest schema | IMPLEMENTED | Capability/interface and execution-mode requirement. |

cyrene_preflight exposes contracts and resource facts. It does not choose model
architecture, estimate model memory, or decide Product admission.

## Execution relationship

A Product resolves a capability requirement, persists its own lifecycle state,
and passes bounded execution intent to the Platform Kernel. The Kernel
supervises the selected plugin worker or service. The plugin returns a typed
result or ArtifactRef to the Product.

Platform never infers a Product run from a process, worker, route, or container
identity.

## Implementation status

| Subsystem | Status | Notes |
|---|---|---|
| Rust Kernel supervision and isolation | IMPLEMENTED | Workspace Cargo gates build all targets. |
| Node resource observation | IMPLEMENTED | Node Agent is the resource-fact authority. |
| Artifact Plane | IMPLEMENTED | Generic CAS and ArtifactRef contracts. |
| Environment lock | IMPLEMENTED | Product-neutral environment resolution. |
| Preflight contracts | IMPLEMENTED | Facts and replaceable capability interfaces only. |
| Capability package runtime | IMPLEMENTED | Language-neutral worker/service dispatch. |
| Product lifecycle | EXTERNAL | Owned by each Product repository. |
| Capability implementations | EXTERNAL | Owned by plugin or consuming repositories. |

## 6. Generic Service & Workload Supervision Foundation (`IMPLEMENTED_STABLE`)

The Platform Kernel provides generic, Product-neutral process hosting and supervision mechanisms capable of running long-running service workloads across any runtime (Python, Node.js, JVM, .NET, Rust, Go, or native C/C++ binaries).

> **Architectural Invariant ([ADR-010](adr/ADR-010-service-is-supervisor-orchestration-not-kernel-authority.md))**: `ServiceSpec`, `ServiceState`, `ServiceStatus`, and `ServiceSupervisor` are daemon/orchestration layer constructs. They drive existing Kernel primitives (`LaunchPlan`, `ProcessRuntime`, `SandboxBackend`, `CleanupReport`, and `semantic::Endpoint`). They do **NOT** create a new authoritative Kernel domain entity (no authoritative `ServiceId` resource, no Service ledger/table, and no independent Service repository).

```mermaid
stateDiagram-v2
    [*] --> Starting: spawn_process(LaunchPlan)
    Starting --> Ready: ReadinessProbe Passed (HTTP/TCP/Alive/WorkerControl)
    Starting --> Failed: Probe Timeout / Spawn Error
    Ready --> Running: Publish Endpoint
    Running --> Stopping: Stop / Cancel Requested
    Running --> Restarting: Unexpected Crash (RestartPolicy::OnFailure / Always)
    Restarting --> Starting: Deterministic Backoff Elapsed
    Restarting --> Quarantined: Max Retries Exhausted
    Stopping --> Stopped: Graceful Exit / Clean Stop
    Stopping --> Failed: Forced Kill on Deadline Timeout
    Failed --> [*]
    Stopped --> [*]
    Quarantined --> [*]
```

### A. State Machine & Exact Lifecycle Semantics
- **`Starting`**: Process has been launched into the sandbox/OS runtime and the supervisor is actively evaluating configured readiness probes. The service is **not yet ready** to receive consumer traffic; no endpoint is published.
- **`Ready`**: All configured readiness probes have succeeded (e.g. TCP port open, HTTP 2xx returned, or WorkerControl handshake completed). The semantic `Endpoint` is published to the registry.
- **`Running`**: The service workload is actively running and continuously supervised for liveness, crashes, and exit events.
- **`Stopping`**: Graceful shutdown has been initiated. Published endpoints are revoked immediately. The supervisor waits for the process to exit cleanly within `graceful_stop_timeout`.
- **`Stopped`**: Process has terminated cleanly (exit code 0 or successful graceful stop). This is a terminal resting state.
- **`Failed`**: Process spawn failed, readiness probe timed out, or process required forced termination after shutdown deadline expired.
- **`Restarting`**: An unexpected crash occurred under an active restart policy (`OnFailure` or `Always`). The supervisor is actively counting restart attempts and holding the deterministic backoff delay before spawning the next generation.
- **`Quarantined`**: The consecutive restart ceiling (`max_retries`) has been exhausted. Automated restarts are suspended to prevent infinite crash loops, and the service remains in isolation for investigation.

### B. Core Declarative Types (`cy-kernel-api::service`)
- **`LaunchPlan`**: Contains `executable`, `args`, `environment`, `cgroup_name`, `limits`, `working_dir: Option<PathBuf>`, and optional `transport_socket`.
- **`ReadinessProbe`**:
  - `ProcessAlive`: **Permissive shallow readiness strategy**. Confirms only that the OS/sandbox process was spawned and is alive. It does **NOT** imply application-level, framework-level, or network-level readiness.
  - `TcpSocket { host, port }`: Validates TCP connectivity to the service listening port.
  - `HttpGet { host, port, path, expected_status }`: Validates HTTP endpoint response (e.g. `GET /health/ready` $\rightarrow$ 200 OK).
  - `WorkerControl`: Validates bidirectional framing handshake (`Hello` / `HelloAck`).
- **`RestartPolicy`**:
  - `Never`: Terminal on any exit.
  - `OnFailure { max_retries, backoff }`: Automatically restarts crashed processes with bounded exponential backoff. Clean exits (code 0) transition to `Stopped`.
  - `Always { max_retries, backoff }`: Restarts on both clean exits and unexpected crashes.
- **`BackoffConfig`**: `initial_delay`, `max_delay`, `multiplier`, and `reset_after` for resetting retry counts after sustained healthy execution.
- **`ServiceEndpointSpec`**: Declares transport (e.g. `http`), schema ID, port, path, and public attributes published to the Kernel registry upon reaching `Ready`.

### C. Lifecycle Invariants, Restart Ownership & Failure Semantics
1. **Single Restart Ownership**: Exactly one component (`ServiceSupervisor`) owns restart policies, attempt counters, and backoff timing across generations. Physical execution actors (`InstanceActor` / `SandboxedProcess`) detect and report single-generation crash/exit facts upward; they do not perform restarts.
2. **Stale Endpoint Protection Across Generations**: Endpoints are strictly scoped to the active generation ($N$). When generation $N$ crashes or stops, its endpoint is immediately revoked. During backoff (`Restarting`) and `Starting`, no endpoint is exposed. When generation $N+1$ passes readiness checks, a new endpoint with generation $N+1$ is published; old generation $N$ endpoints are never re-exposed.
3. **Graceful Shutdown & Escalation**: On stop request, processes receive graceful termination. If the process does not terminate within `graceful_stop_timeout`, forced termination is executed and reported via `CleanupReport`.
4. **Deterministic Backoff**: Backoff delays follow $D_n = \min(D_{\text{initial}} \cdot \text{multiplier}^{n-1}, D_{\text{max}})$.
5. **Retry Exhaustion & Quarantine**: Exceeding `max_retries` transitions the service to `Quarantined`, isolating crashing workloads without endless crash-loops.
6. **Zero Zombie / Orphan Guarantee**: All child processes and cgroup allocations are tracked and cleaned up on termination.
