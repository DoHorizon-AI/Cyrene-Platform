> ⚠️ **部分内容可能过时**：本文件部分基于旧设计，现状以 [README 文档导航](../README.md#-文档导航) 指向的权威文档为准。

# Testing

The repository uses Python 3.12 and Rust workspace checks. GPU packages are
optional and are not installed by CI.

## Python

```bash
uv sync --locked --group dev --all-packages
uv run pytest -q
uv run python scripts/generate_proto.py --check
```

The default pytest configuration excludes `gpu`, `integration`, and `uds`
markers. Run the optional integration tests explicitly when the required
runtime is available:

```bash
uv run pytest tests/integration -m integration -o addopts=""
```

## Rust

```bash
cargo fmt --all -- --check
cargo build --workspace --locked
cargo test --workspace --locked
```

## Lite Smoke Checks

The local Python entrypoints are:

```bash
uv run python -m cy_exec.main --serve --port 50052
uv run uvicorn cy_gateway_lite.app.main:app --host 0.0.0.0 --port 8000
uv run python -m cy_gateway_lite.coordinator
```

For the development TCP topology:

```bash
docker compose -f deploy/compose/community.yml up -d
curl http://localhost:8000/health
docker compose -f deploy/compose/community.yml down
```

The community images are Python 3.12 baselines. GPU runtime and GPU E2E
validation are deferred to M4.
