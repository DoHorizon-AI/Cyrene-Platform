> ⚠️ **部分内容可能过时**：本文件部分基于旧设计，现状以 [README 文档导航](README.md#-文档导航) 指向的权威文档为准。

# 快速开始指令速查

## 🚀 基础启动

```bash
# 初始化环境
./cy-llm setup --engine cuda-vllm

# 启动服务
./cy-llm lite --engine cuda-vllm --model qwen2.5-7b

# 测试
curl http://localhost:8000/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{"model":"qwen2.5-7b","messages":[{"role":"user","content":"你好"}]}'
```

## 🐳 Docker 启动

```bash
# 启动
docker compose -f deploy/compose/community.yml up -d

# 查看状态
docker compose -f deploy/compose/community.yml ps

# 停止
docker compose -f deploy/compose/community.yml down
```

## 🔧 常用命令

```bash
# 环境诊断
./cy-llm doctor

# 模型显存诊断
./cy-llm diagnose qwen2.5-7b

# 验证配置文件
./cy-llm config validate

# 查看服务状态
./cy-llm status

# 停止服务
./cy-llm stop

# 查看可用模型
./cy-llm models list
```

## ⚡ TensorRT-LLM 加速

```bash
# 转换模型
./cy-llm convert-trt \
  --model Qwen/Qwen2.5-7B-Instruct \
  --output /models/qwen2.5-7b-trt

# 使用 TRT 引擎启动
./cy-llm lite --engine cuda-trt --model qwen2.5-7b-trt
```

## 🎓 训练相关

```bash
# 数据预处理
./cy-llm prepare --raw ./raw_data --out ./data/train.jsonl --char 芙宁娜

# 启动训练
./cy-llm train \
  --dataset ./data/train.jsonl \
  --output ./checkpoints/lora_v1 \
  --model facebook/opt-2.7b

# 交互式测试
./cy-llm chat --model facebook/opt-2.7b --lora ./checkpoints/lora_v1
```

## 🧪 测试

```bash
# 运行集成测试
./cy-llm test integration

# 运行所有测试
./cy-llm test all
```

## 🧩 直接运行 workspace 入口

```bash
uv run python -m cy_exec.main --serve --port 50052
uv run uvicorn cy_gateway_lite.app.main:app --host 0.0.0.0 --port 8000
uv run python -m cy_gateway_lite.coordinator
```

## 📚 更多信息

详细文档请参考：
- [README.md](./README.md) - 完整项目介绍
- [docs/INSTALL.md](./docs/INSTALL.md) - 详细安装指南
- [docs/TRT_GUIDE.md](./docs/TRT_GUIDE.md) - TensorRT-LLM 完整指南
- [docs/FAQ.md](./docs/FAQ.md) - 常见问题解答
