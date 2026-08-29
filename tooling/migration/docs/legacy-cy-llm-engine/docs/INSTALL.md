> ⚠️ **旧设计文档（已过时）**：本文件描述已废弃的架构，可能与当前实现不符。当前权威文档见 [README 文档导航](../README.md#-文档导航)。请勿据此判断现状。

# 安装与运行指南

## 环境要求

- Python 3.12
- `uv`
- Linux/WSL for the worker UDS path; TCP mode also works in containers
- Docker Compose 2.x for the community stack
- GPU runtimes are optional and remain outside the M4 baseline images

## 本地安装

```bash
git clone https://github.com/Baijin64/CY-LLM-Engine.git
cd CY-LLM-Engine

# CPU-safe workspace and test dependencies
uv sync --locked --group dev --all-packages

# Or create a Conda Python 3.12 environment and install an engine profile.
./cy-llm setup --engine cuda-vllm
```

`setup` passes each requirements file to pip as a complete file, so nested
`-r` includes and `--extra-index-url` entries remain active. It also installs
`cy_exec`, `cy_gateway_lite`, and `cy_control_plane` editable.

## Local Lite Services

The default local topology uses UDS. Set the TCP variables when running the
services in separate containers or hosts:

```bash
./cy-llm lite --engine cuda-vllm --model default

# Equivalent package entrypoints:
uv run python -m cy_exec.main --serve --port 50052
uv run uvicorn cy_gateway_lite.app.main:app --host 0.0.0.0 --port 8000
uv run python -m cy_gateway_lite.coordinator
```

Relevant variables are `COORDINATOR_GRPC_ADDR`, `COORDINATOR_GRPC_BIND`, and
`WORKER_GRPC_ADDRS`. Without those TCP variables, the coordinator and worker
retain their UDS defaults.

## Community Compose

The Compose file is a development/insecure Python baseline. It explicitly
sets `CY_LLM_ENV=development` and allows an empty internal token for local
use. Set `CY_LLM_INTERNAL_TOKEN` before using it in a shared environment.

```bash
docker compose -f deploy/compose/community.yml config
docker compose -f deploy/compose/community.yml up -d
docker compose -f deploy/compose/community.yml ps
docker compose -f deploy/compose/community.yml down
```

Models and runtime configuration are in `deploy/models.json` and
`deploy/config.json`. Observability assets are under
`deploy/observability/`.

## Verification

```bash
uv run pytest -q
uv run python scripts/generate_proto.py --check
cargo fmt --all -- --check
cargo build --workspace --locked
cargo test --workspace --locked
```

The commands above do not install GPU packages and do not claim GPU E2E
validation.
