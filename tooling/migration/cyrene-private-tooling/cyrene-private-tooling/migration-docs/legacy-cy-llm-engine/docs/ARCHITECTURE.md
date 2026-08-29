> ⚠️ **旧设计文档（已过时）**：本文件描述已废弃的架构，可能与当前实现不符。当前权威文档见 [README 文档导航](../README.md#-文档导航)。请勿据此判断现状。

# Architecture

## Runtime Topology

The active community path is a Python Lite gateway, a Python coordinator, and
one or more Python execution workers. Rust crates provide the workspace
boundaries and sidecar/data-plane work; they are not a replacement entrypoint
for the Lite stack yet.

```text
HTTP client
    |
    v
cy_gateway_lite.app.main:app  -- gRPC -->  cy_gateway_lite.coordinator
                                             |
                                             +-- gRPC --> cy_exec.main
```

The default local transport is UDS:

- Coordinator: `unix:///tmp/cy_coordinator.sock`
- Worker: `unix:///tmp/cy_worker.sock`

Set `COORDINATOR_GRPC_ADDR`, `COORDINATOR_GRPC_BIND`, and
`WORKER_GRPC_ADDRS` for TCP deployments. The community Compose file uses
`coordinator-lite:50051` and `worker:50052`.

## Packages

### `cy_exec`

`python/cy_exec` owns the worker entrypoint, model registry, inference engine
plugins, gRPC service implementation, health endpoint, and legacy training
modules. The supported worker command is:

```bash
python -m cy_exec.main --serve
```

### `cy_gateway_lite`

`python/cy_gateway_lite` exposes the OpenAI-compatible FastAPI endpoint and
proxies inference/control/health RPCs through the coordinator:

```bash
uvicorn cy_gateway_lite.app.main:app --host 0.0.0.0 --port 8000
python -m cy_gateway_lite.coordinator
```

### Protocol

`proto/ai_service.proto` is the canonical schema. Checked-in Python bindings
live under `python/cy_exec/src/cy_exec/proto`; verify them with:

```bash
uv run python scripts/generate_proto.py --check
```

Docker images consume those checked-in bindings and do not run protocol
generation during image builds.

## Request Flow

1. The gateway authenticates the external HTTP request when
   `GATEWAY_API_TOKEN` is configured.
2. The gateway builds a `StreamPredictRequest` and sends it to the configured
   coordinator address.
3. If `CY_LLM_INTERNAL_TOKEN` is set, the gateway sends
   `authorization: Bearer <token>` gRPC metadata.
4. The coordinator selects a worker round-robin and forwards the authorization
   metadata to that worker.
5. The worker resolves the model from `deploy/models.json`, creates the lazy
   engine, and streams response chunks back through the same path.

## Configuration

Runtime model and service configuration lives under `deploy/`:

- `deploy/models.json`: worker model registry
- `deploy/config.json`: richer model and backend configuration
- `deploy/compose/community.yml`: development TCP Compose topology
- `deploy/observability/`: Prometheus, Alertmanager, and Grafana assets

The community Compose file explicitly uses `CY_LLM_ENV=development` and
`CY_LLM_ALLOW_INSECURE_INTERNAL_TOKEN=true`. This is not a production security
profile; set a strong internal token and use a hardened deployment before
sharing the services.

## Workspace Boundaries

Rust workspace crates are under `crates/` and are built from the repository
root:

```bash
cargo fmt --all -- --check
cargo build --workspace --locked
cargo test --workspace --locked
```

GPU runtime images and GPU E2E validation are deferred to M4.
