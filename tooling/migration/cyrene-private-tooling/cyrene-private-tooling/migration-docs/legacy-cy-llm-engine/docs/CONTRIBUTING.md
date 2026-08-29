> ⚠️ **部分内容可能过时**：本文件部分基于旧设计，现状以 [README 文档导航](../README.md#-文档导航) 指向的权威文档为准。

# 贡献者规范

欢迎提交问题、文档和代码改进。当前有效实现位于 Python workspace 和
Rust workspace；旧后端目录不再是开发入口。

## 开发环境

```bash
git clone https://github.com/Baijin64/CY-LLM-Engine.git
cd CY-LLM-Engine
uv sync --locked --group dev --all-packages
```

Python 包边界如下：

- `python/cy_exec`：Worker、协议绑定和 legacy training
- `python/cy_gateway_lite`：Lite HTTP Gateway 和 Coordinator
- `python/cy_control_plane`：Control-plane workspace boundary

Proto 的唯一源文件是 `proto/ai_service.proto`。Python 生成物已提交到
`python/cy_exec/src/cy_exec/proto`（导入包名为 `cy_exec.proto`），修改协议后运行生成检查并提交生成物：

```bash
uv run python scripts/generate_proto.py --check
```

## 代码质量

```bash
uv run ruff check python tests scripts
uv run ruff format --check python tests scripts
uv run mypy python/cy_exec/src python/cy_gateway_lite/src
```

## 测试

```bash
uv run pytest -q
cargo fmt --all -- --check
cargo build --workspace --locked
cargo test --workspace --locked
```

默认测试不安装 GPU 包，也不运行 GPU、UDS 或集成标记测试。需要对应
运行时的开发者可以显式运行：

```bash
uv run pytest tests/integration -m integration -o addopts=""
```

## 入口与部署

```bash
uv run python -m cy_exec.main --serve --port 50052
uv run uvicorn cy_gateway_lite.app.main:app --host 0.0.0.0 --port 8000
uv run python -m cy_gateway_lite.coordinator
docker compose -f deploy/compose/community.yml config
```

社区 Compose 明确是 development/insecure 基线。共享或生产环境必须设置
`CY_LLM_INTERNAL_TOKEN`，并单独配置安全策略；M4 之前不宣称 GPU E2E。

## Pull Request

- 保持变更聚焦，避免提交模型、缓存和凭据。
- 协议变更必须包含生成物和 Proto 检查结果。
- 入口、配置或部署路径变更应同步更新当前 README 和 QUICK_START。
- 在描述中列出实际运行的验证命令和未验证的可选运行时。
