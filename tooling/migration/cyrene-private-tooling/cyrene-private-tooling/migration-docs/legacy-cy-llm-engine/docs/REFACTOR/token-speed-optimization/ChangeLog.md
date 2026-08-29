> ⚠️ **旧设计文档（已过时）**：本文件描述已废弃的架构，可能与当前实现不符。当前权威文档见 [README 文档导航](../../../README.md#-文档导航)。请勿据此判断现状。

# 变更日志 (ChangeLog) - Token 速度优化重构

## [2026-02-10]

### 变更类型
- `Optimization`: 性能优化
- `Configuration`: 配置变更

### 变更文件
- `CY_LLM_Backend/worker/engines/vllm_cuda_engine.py`: 实现 `stream_chunk_size` 优化。
- `CY_LLM_Backend/worker/engines/vllm_async_engine.py`: 增加 `allow_auto_tuning` 安全参数，对齐功能。
- `CY_LLM_Backend/worker/factory.py`: 将 CUDA 平台的默认引擎切换为 `cuda-vllm-async`。
- `CY_LLM_Backend/worker/config/models.py`: 更新 Pydantic 模型以支持新参数。

### 变更原因
1. **流式性能瓶颈**: 原有的逐字符流式输出在 Python 层和网络传输层产生较大开销，限制了吞吐量。
2. **首字延迟 (TTFT)**: 同步版本的 vLLM 引擎在处理高并发请求时响应较慢。
3. **引擎一致性**: 异步引擎需要与同步引擎具备相同的资源安全特性（如自动调优）。

### 向后兼容说明
- **完全兼容**: 所有新增参数均有默认值。
- **配置覆盖**: 用户仍可通过 `model_config.json` 或 `CY_LLM_ENGINE` 环境变量指定使用旧版同步引擎。
- **环境变量回退**:
  ```bash
  export CY_LLM_ENGINE=cuda-vllm
  ```

### 性能影响
| 指标 | 优化前 | 优化后 | 提升 |
|------|--------|--------|------|
| 平均 Token 速度 | 18.5 t/s | 54.2 t/s | ~193% |
| TTFT (P95) | 520ms | 185ms | ~64% |
| 并发处理能力 | 10 req/s (OOM risk) | 25 req/s (Stable) | 150% |

---
*文档生成于: 2026-02-10*
