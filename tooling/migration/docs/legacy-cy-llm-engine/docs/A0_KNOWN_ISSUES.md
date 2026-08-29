# A0 Known Issues

This file tracks defects intentionally deferred while the architecture is being rebuilt. Severity reflects operational impact, not implementation order.

## P0 - Critical

### A0-KI-001: Rust proxy cannot forward requests

- Status: deferred
- Impact: `cy-proxy` starts, but its Worker client is never initialized, so inference forwarding fails.
- Locations:
  - `crates/cy-proxy/src/bin/sidecar.rs:35`
  - `crates/cy-proxy/src/proxy.rs:79`
  - `crates/cy-proxy/src/proxy.rs:141`
- Required work: initialize the Worker connection and implement a tonic UDS connector with reconnect handling.

### A0-KI-002: Rust proxy health callback can panic

- Status: deferred
- Impact: the background health task calls `blocking_write` inside a Tokio runtime and can terminate when the Worker state changes.
- Locations:
  - `crates/cy-proxy/src/health.rs:102`
  - `crates/cy-proxy/src/proxy.rs:250`
- Required work: make health status propagation asynchronous and remove blocking lock acquisition.

## P1 - High

### A0-KI-003: Rust proxy drops internal authentication metadata

- Status: deferred
- Impact: production Worker requests through `cy-proxy` are rejected by the execution adapter.
- Locations:
  - `crates/cy-proxy/src/proxy.rs:157`
  - `python/cy_exec/src/cy_exec/grpc_servicer.py:90`
- Required work: copy approved metadata, especially `authorization`, into the upstream tonic request.

### A0-KI-004: Async Python gRPC implementation is incomplete

- Status: deferred
- Impact: the async server references undefined variables and does not enforce the internal token on inference requests.
- Location: `python/cy_exec/src/cy_exec/grpc_servicer_async.py`
- Required work: either repair and test the async implementation or remove it when the A4 adapter replaces both server variants.

### A0-KI-005: A0 execution image has no model runtime

- Status: accepted A0 limitation
- Impact: the image can start the control/adapter process but cannot execute model inference without Torch and an engine extra.
- Locations:
  - `deploy/docker/Dockerfile.cy-exec`
  - `python/cy_exec/pyproject.toml`
- Required work: M4 must build per-runtime Docker images with uv locks and verified engine dependencies.

### A0-KI-006: CLI examples and model registry can select different model IDs

- Status: deferred
- Impact: a model name accepted by `cy-llm lite --model` can be absent from `deploy/models.json`, causing `NOT_FOUND` at inference time.
- Locations:
  - `cy-llm`
  - `deploy/models.json`
  - `python/cy_exec/src/cy_exec/grpc_servicer.py:143`
- Required work: validate the requested logical model before starting Lite services and generate examples from the active registry.

### A0-KI-007: `cuda-vllm-async` is missing from Worker configuration validation

- Status: deferred
- Impact: the CLI accepts the engine while configuration loading can silently select another backend.
- Locations:
  - `cy-llm`
  - `python/cy_exec/src/cy_exec/config/config_loader.py:72`
  - `python/cy_exec/src/cy_exec/engines/engine_factory.py:107`
- Required work: make engine identifiers come from the plugin capability registry rather than duplicate static lists.

### A0-KI-008: Compose Worker scaling conflicts with fixed host ports

- Status: deferred
- Impact: `cy-llm docker up --scale 2` cannot bind the same Worker and metrics ports for multiple containers.
- Locations:
  - `cy-llm`
  - `deploy/compose/community.yml`
- Required work: remove fixed Worker host ports for scaled deployments or assign ports dynamically.

## P2 - Medium

### A0-KI-009: Dependency registry lookup still targets an obsolete package path

- Status: deferred
- Impact: legacy dependency resolver code can raise `FileNotFoundError`.
- Location: `python/cy_exec/src/cy_exec/deps/__init__.py`
- Required work: replace the legacy registry with versioned CompatibilityResolver rule bundles.

### A0-KI-010: Rust proxy metering uses inconsistent session IDs

- Status: deferred
- Impact: per-session token accounting is silently discarded and sessions are never closed.
- Locations:
  - `crates/cy-proxy/src/proxy.rs:193`
  - `crates/cy-proxy/src/metering.rs:62`
- Required work: retain the ID returned by `start_session`, use it for updates, and close the session on every terminal path.

### A0-KI-011: `cy-llm diagnose` does not use the selected Conda environment

- Status: deferred
- Impact: diagnosis can fail to import packages that `cy-llm setup` installed into the named environment.
- Location: `cy-llm`
- Required work: run diagnosis through the selected Conda/Mamba environment or replace the Bash command with the cross-platform Python CLI.

### A0-KI-012: uv and legacy requirements resolve different GPU runtime versions

- Status: deferred
- Impact: uv and `cy-llm setup` can produce different Torch/vLLM environments.
- Locations:
  - `uv.lock`
  - `requirements-vllm.txt`
- Required work: M4 must replace the legacy requirements files with per-runtime uv locks.

## Migrated Pro Issues

### A0-KI-013: Kotlin enterprise capabilities are not on the main inference chain

- Severity: P1
- Status: deferred
- Impact: the Pro Kotlin gateway has its own inference, tenant, billing, audit, and failover path, but it is not wired into the Community gateway/control-plane inference chain. Enabling the optional module does not by itself make the enterprise path the repository default.
- Locations:
  - `plugins/pro/kotlin/gateway/src/main/kotlin/com/cy/llm/service/InferenceService.kt:39`
  - `plugins/pro/kotlin/gateway/src/main/kotlin/com/cy/llm/service/WorkerStreamClient.kt:40`
  - `plugins/pro/kotlin/gateway/src/main/kotlin/com/cy/llm/service/CoordinatorStreamClient.kt:29`
- Required work: define and validate the deliberate Pro-to-Community integration boundary before enabling this path in a product deployment.

### A0-KI-014: Pro DatabaseConfig fails when database mode is enabled

- Severity: P1
- Status: deferred
- Impact: `DatabaseConfig.connectionFactory()` throws `UnsupportedOperationException` while the class claims that Spring Boot auto-configuration supplies the connection factory. Enabling the Pro database feature can therefore fail during application startup or repository initialization.
- Location: `plugins/pro/kotlin/gateway/src/main/kotlin/com/cy/llm/gateway/config/DatabaseConfig.kt:17-24`
- Required work: provide a tested R2DBC connection-factory configuration and verify migration/repository startup against the target PostgreSQL deployment.

### A0-KI-015: In-memory tenant fallback has a default API key

- Severity: P1
- Status: deferred
- Impact: `InMemoryTenantRepository` creates `default-api-key-12345` when no API key is configured. This is a credential risk and can make an apparently protected development configuration reachable with a known key.
- Location: `plugins/pro/kotlin/gateway/src/main/kotlin/com/cy/llm/gateway/tenant/TenantRepository.kt:33-55`
- Required work: fail closed or require explicit provisioning for any default tenant/API key; no credential is added by this archive task.

### A0-KI-016: Coordinator inference queue is not consumed

- Severity: P1
- Status: deferred
- Impact: the Coordinator exposes a Redis-backed `TaskQueueService`, but the inference service selects a Worker and calls it directly without polling or taking the queued task. Queue depth and task state therefore do not represent the main inference path.
- Locations:
  - `plugins/pro/kotlin/coordinator/src/main/kotlin/com/cy/llm/coordinator/queue/TaskQueueService.kt:98-134`
  - `plugins/pro/kotlin/coordinator/src/main/kotlin/com/cy/llm/coordinator/grpc/CoordinatorInferenceService.kt:116-151`
- Required work: add a bounded queue consumer/dispatcher and define cancellation, retry, and state ownership before relying on queue metrics for production scheduling.

### A0-KI-017: Scheduler bridge API does not match the Rust scheduler API

- Severity: P2
- Status: deferred
- Impact: the Python bridge reads `cache_affinity` and sends telemetry fields that do not match the Rust `ScheduleResult` and `WorkerTelemetry` bindings. A Rust-enabled bridge can fail at runtime or silently lose scheduling telemetry.
- Locations:
  - `plugins/pro/python/cy_exec_pro/src/cy_exec_pro/coordinator/scheduler_bridge.py:114-158`
  - `plugins/pro/rust/scheduler-rs/src/scheduler.rs:61-140`
- Required work: version and test the Python/Rust DTO contract, including tenant, affinity, inflight, cache, and timestamp fields, before enabling the bridge.

### A0-KI-018: Pro sidecar UDS is not connected to the Worker

- Severity: P1
- Status: deferred
- Impact: the Rust sidecar opens its configured UDS client, while the current Worker starts its own UDS endpoint and the Pro Python `UDSServer` is not registered in the Worker serve path. Starting both components does not establish a usable sidecar-to-Worker inference route.
- Locations:
  - `plugins/pro/rust/sidecar/src/main.rs:53-59`
  - `plugins/pro/python/cy_exec_pro/src/cy_exec_pro/core/uds_server.py:112-145`
  - `python/cy_exec/src/cy_exec/main.py:120-137`
- Required work: define one socket owner and wire the Pro protocol into the Worker lifecycle with reconnect and health handling before deploying the sidecar.

## Verification Boundary

- CPU Python tests and import checks are part of A0 verification.
- Rust workspace build and unit tests are part of A0 verification.
- Compose syntax is checked, but GPU image inference is not an A0 acceptance criterion.
- NVIDIA Linux inference, training, checkpoint recovery, and performance validation remain pending dedicated hardware.
