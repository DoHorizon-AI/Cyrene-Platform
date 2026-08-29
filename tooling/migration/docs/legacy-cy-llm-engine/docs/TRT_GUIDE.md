> ⚠️ **旧设计文档（已过时）**：本文件描述已废弃的架构，可能与当前实现不符。当前权威文档见 [README 文档导航](../README.md#-文档导航)。请勿据此判断现状。

# TensorRT-LLM 引擎使用指南

## 1. 安装 TensorRT-LLM

### 方法 1: 使用 NGC 容器（推荐）

```bash
docker pull nvcr.io/nvidia/tensorrt-llm:latest
```

### 方法 2: pip 安装

```bash
pip install tensorrt_llm -U --pre --extra-index-url https://pypi.nvidia.com
```

### 方法 3: 从源码编译

参考: https://github.com/NVIDIA/TensorRT-LLM

## 2. 转换模型

使用内置工具转换 HuggingFace 模型：

```bash
./cy convert-trt \
  --model Qwen/Qwen2.5-7B-Instruct \
  --output /models/qwen2.5-7b-trt \
  --max-batch-size 64 \
  --max-input-len 4096
```

### 支持的模型

- **Llama** 系列 (Meta Llama 3, Llama 2 等)
- **Qwen** 系列 (Qwen2.5, Qwen2 等)
- **Mistral** (Mistral-7B 等)
- **Baichuan** (Baichuan2 等)
- **ChatGLM** 系列
- **Yi** 模型

### 转换参数

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `--model` | - | HuggingFace 模型路径或本地路径 |
| `--output` | - | TRT 引擎输出目录 |
| `--model-type` | llama | 模型类型 (llama, qwen, mistral 等) |
| `--dtype` | float16 | 精度 (float16, bfloat16) |
| `--tp-size` | 1 | 张量并行度 (多 GPU 使用) |
| `--max-batch-size` | 64 | 最大批处理大小 |
| `--max-input-len` | 4096 | 最大输入长度 |
| `--max-output-len` | 2048 | 最大输出长度 |

## 3. 配置使用

在 `deploy/config.json` 中添加：

```json
{
  "models": {
    "qwen2.5-7b-trt": {
      "engine": "cuda-trt",
      "model_path": "/models/qwen2.5-7b-trt",
./cy convert-trt \
      "max_input_len": 4096,
      "max_output_len": 2048,
      "dtype": "float16",
      "tensor_parallel_size": 1
    }
  }
}
```

## 4. 启动 TRT 引擎

./cy setup --engine cuda-trt

```bash
./cy setup --engine cuda-trt
```

./cy lite --engine cuda-trt --model qwen2.5-7b-trt

```bash
./cy lite --engine cuda-trt --model qwen2.5-7b-trt
```

./cy convert-trt \

```bash
curl -X POST http://localhost:8080/api/v1/inference/stream \
  -H "Content-Type: application/json" \
  -H "X-API-Key: YOUR_API_KEY" \
  -d '{
    "modelId": "qwen2.5-7b-trt",
    "prompt": "你好，请介绍一下自己"
  }'
```

## 5. 性能调优

### 批处理大小
./cy setup --engine cuda-vllm --env cy-vllm
根据显存调整 `max_batch_size`：

./cy setup --engine cuda-trt --env cy-trt
- **GPU 显存 12-24GB**: max_batch_size = 32-64
- **GPU 显存 > 24GB**: max_batch_size = 64-128
./cy lite --engine cuda-vllm --env cy-vllm --model my-model
./cy lite --engine cuda-trt --env cy-trt --model my-model-trt

`max_input_len` 和 `max_output_len` 在编译时固化，修改后需要重新转换模型：

```bash
./cy convert-trt \
./cy status
  --output /models/qwen2.5-7b-trt-8k \
  --max-input-len 8192 \
  --max-output-len 4096
```

./cy stop

在多 GPU 系统上使用张量并行加速：

```bash
./cy convert-trt \
  --model Qwen/Qwen2.5-70B \
  --output /models/qwen2.5-70b-trt \
  --tp-size 2  # 使用 2 个 GPU
```

然后在 config.json 中配置：

```json
{
  "models": {
    "qwen2.5-70b-trt": {
      "engine": "cuda-trt",
      "model_path": "/models/qwen2.5-70b-trt",
      "tensor_parallel_size": 2
    }
  }
}
```

### 精度选择

根据显存和精度需求选择：

- **float16** (默认): 较低精度，显存占用少，速度快
- **bfloat16**: 改进的精度，显存占用相同，某些 GPU 速度更快

```bash
./cy convert-trt \
  --model Qwen/Qwen2.5-7B-Instruct \
  --output /models/qwen2.5-7b-trt-bf16 \
  --dtype bfloat16
```

## 6. 常见问题

### Q: TRT 和 vLLM 版本冲突？

A: 使用不同的 Conda 环境隔离：

```bash
# 创建 vLLM 环境
./cy setup --engine cuda-vllm --env cy-vllm

# 创建 TRT 环境
./cy setup --engine cuda-trt --env cy-trt

# 分别启动
./cy lite --engine cuda-vllm --env cy-vllm --model my-model
./cy lite --engine cuda-trt --env cy-trt --model my-model-trt
```

### Q: 流式输出不工作？

A: 检查 TRT-LLM 版本是否支持 `streaming=True`。如果使用旧版本 TRT-LLM，引擎会自动回退到伪流式输出（逐字符返回）。

### Q: 转换模型失败？

A: 常见原因：

1. **内存不足**: 模型转换需要大量内存，使用 GPU 内存减少 CPU 内存占用：
   ```bash
   python scripts/convert_trt.py --model ... --dtype float16
   ```

2. **模型不支持**: 检查模型是否为 TRT-LLM 支持的类型

3. **依赖缺失**: 确保 tensorrt_llm 正确安装：
   ```bash
   pip install tensorrt_llm -U --pre --extra-index-url https://pypi.nvidia.com
   ```

### Q: 推理速度慢？

A: 检查以下几点：

1. 确认启用了正确的 `max_batch_size` 和序列长度
2. 使用 `nvidia-smi` 检查 GPU 利用率
3. 启用张量并行（多 GPU）以加速大模型
4. 使用 float16 而非 float32

### Q: 显存不足？

A: TRT 引擎显存占用通常大于 vLLM。如果显存不足：

1. 降低 `max_batch_size`
2. 降低 `max_input_len` 和 `max_output_len`
3. 使用量化（但 TRT 的量化支持有限）
4. 切换到较小的模型

## 7. 监控和日志

### 查看日志

```bash
# vLLM 日志
tail -f logs/worker.log

# 查看服务状态
./cy status
```

### 性能监控

```bash
# 实时 GPU 使用情况
watch -n 1 nvidia-smi

# 推理延迟统计
# 通过 API 响应头的 X-Response-Time 查看
```

## 8. 卸载模型

```bash
# Worker 会自动管理模型卸载
# 手动清理显存：
./cy stop
nvidia-smi  # 验证显存已释放
```

## 9. 最佳实践

1. **使用 float16 为默认精度**: 通常性能最好，精度足够
2. **预编译常用配置**: 提前转换经常使用的模型和参数组合
3. **监控显存**: 避免显存溢出导致的错误
4. **测试转换**: 转换后先测试小批量推理，确认质量
5. **使用 Docker**: 在生产环境中使用 Docker 确保一致性

## 10. 参考资源

- TensorRT-LLM GitHub: https://github.com/NVIDIA/TensorRT-LLM
- 官方文档: https://nvidia.github.io/TensorRT-LLM/
- 模型支持列表: https://github.com/NVIDIA/TensorRT-LLM/tree/main/examples
