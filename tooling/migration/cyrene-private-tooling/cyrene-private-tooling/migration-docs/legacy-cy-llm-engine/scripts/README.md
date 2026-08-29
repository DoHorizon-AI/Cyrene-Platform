# Scripts 目录

本目录包含 CY-LLM Engine 的辅助脚本。

## 可用脚本

### 测试与验证

- **benchmark.sh** - 使用 k6 进行压力测试
  ```bash
  ./scripts/benchmark.sh [endpoint] [duration] [vus]
  # 示例: ./scripts/benchmark.sh http://localhost:8000 30s 10
  ```

- **e2e_lite.sh** - Lite 版本端到端测试
  ```bash
  ./scripts/e2e_lite.sh
  ```

- **verify-deploy.sh** - 验证 Docker Compose 部署
  ```bash
  ./scripts/verify-deploy.sh
  ```

### 工具脚本

- **convert_trt.py** - 转换模型为 TensorRT-LLM 引擎
  ```bash
  python scripts/convert_trt.py --model <model> --output <dir>
  # 或使用 CLI: ./cy-llm convert-trt --model <model> --output <dir>
  ```

- **diagnose_env.py** - 环境诊断工具
  ```bash
  python scripts/diagnose_env.py
  # 或使用 CLI: ./cy-llm diagnose [model]
  ```

### 维护脚本

- **clean.sh** - 清理构建产物和缓存
  ```bash
  ./scripts/clean.sh [-y] [--venv] [--git-clean]
  ```

- **check-ci-refs.sh** - 检查 CI 配置中的旧引用
  ```bash
  ./scripts/check-ci-refs.sh
  ```

- **find-ew-references.sh** - 查找代码中的旧引用
  ```bash
  ./scripts/find-ew-references.sh
  ```

## 使用建议

大多数功能已集成到 `./cy-llm` CLI 工具中，推荐优先使用 CLI：

```bash
# 推荐使用 CLI
./cy-llm diagnose qwen2.5-7b
./cy-llm convert-trt --model <model> --output <dir>
./cy-llm test integration

# 而不是直接调用脚本
python scripts/diagnose_env.py
python scripts/convert_trt.py ...
```

## 注意事项

- 所有脚本都应该从项目根目录执行
- 某些脚本需要特定的依赖（如 k6、Docker）
- 使用 `-h` 或 `--help` 查看脚本的详细帮助信息
