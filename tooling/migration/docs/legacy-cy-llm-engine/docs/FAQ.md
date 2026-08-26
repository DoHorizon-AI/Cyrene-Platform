> ⚠️ **部分内容可能过时**：本文件部分基于旧设计，现状以 [README 文档导航](../README.md#-文档导航) 指向的权威文档为准。

# 常见问题 (FAQ)

本文档收集了 CY-LLM Engine 的常见问题和解决方案。

## 目录

- [安装问题](#安装问题)
- [运行问题](#运行问题)
- [性能问题](#性能问题)
- [模型相关](#模型相关)
- [错误排查](#错误排查)
- [其他问题](#其他问题)

---

## 安装问题

### Q1: Python 版本要求是什么？

**A**: CY-LLM Engine 要求 Python 3.10 或更高版本。

```bash
# 检查 Python 版本
python --version

# 如需升级 (Ubuntu/Debian)
sudo apt-get update
sudo apt-get install python3.11

# 或使用 pyenv
pyenv install 3.11
pyenv local 3.11
```

---

### Q2: Java 21 安装失败怎么办？

**A**: 按照以下步骤安装：

```bash
# Ubuntu/Debian
sudo apt-get update
sudo apt-get install -y openjdk-21-jdk

# 验证安装
java -version

# 设置 JAVA_HOME
export JAVA_HOME=/usr/lib/jvm/java-21-openjdk-amd64
echo "export JAVA_HOME=/usr/lib/jvm/java-21-openjdk-amd64" >> ~/.bashrc
```

如果仍有 issues，使用 SDKMAN：

```bash
curl -s "https://get.sdkman.io" | bash
source ~/.sdkman/bin/sdkman-init.sh
sdk install java 21.0.2-tem
```

---

### Q3: pip 安装依赖失败？

**A**: 尝试以下解决方案：

```bash
# 1. 升级 pip
pip install --upgrade pip

# 2. 使用镜像源
pip install -r requirements.txt -i https://pypi.tuna.tsinghua.edu.cn/simple/

# 3. 单独安装冲突依赖
pip install package-name --force-reinstall

# 4. 创建干净的虚拟环境
python -m venv new_venv
source new_venv/bin/activate
pip install -r requirements.txt
```

---

### Q4: CUDA 版本不兼容？

**A**: 首先检查你的 CUDA 版本：

```bash
nvcc --version
nvidia-smi
```

如果版本低于 12.0，需要升级 CUDA：

```bash
# Ubuntu/Debian
wget https://developer.download.nvidia.com/compute/cuda/repos/ubuntu2204/x86_64/cuda-keyring_1.1-1_all.deb
sudo dpkg -i cuda-keyring_1.1-1_all.deb
sudo apt-get update
sudo apt-get install cuda-toolkit-12-0
```

或者使用 Docker 容器（推荐）：

```bash
docker pull nvcr.io/nvidia/cuda:12.4-devel-ubuntu22.04
```

---

## 运行问题

### Q5: gRPC 端口连接失败？

**A**: 按以下步骤排查：

```bash
# 1. 检查端口是否被占用
lsof -i :50051
lsof -i :50050

# 2. 检查服务是否启动
curl http://localhost:50050/actuator/health
curl http://localhost:50051/health

# 3. 检查防火墙设置
sudo ufw status

# 4. 查看日志
./cy-llm logs
docker compose logs coordinator
```

---

### Q6: Redis 连接失败？

**A**: 验证 Redis 配置：

```bash
# 1. 检查 Redis 是否运行
redis-cli ping
# 应返回 PONG

# 2. 启动 Redis (如未运行)
redis-server --daemonize yes

# 3. 检查连接配置
redis-cli info | grep tcp

# 4. Docker 方式启动
docker run -d --name redis -p 6379:6379 redis:7-alpine
```

---

### Q7: 端口被占用怎么办？

**A**: 找到并终止占用端口的进程：

```bash
# 查找占用端口的进程
lsof -i :8080
netstat -tlnp | grep 8080

# 终止进程
kill <PID>

# 或使用其他端口启动
./cy-llm lite --port 8081
```

---

### Q8: Docker 容器启动失败？

**A**: 按以下步骤排查：

```bash
# 1. 检查 Docker 运行状态
sudo systemctl status docker

# 2. 查看容器日志
docker compose logs

# 3. 检查资源限制
docker stats

# 4. 重新构建镜像
docker compose build --no-cache
docker compose up -d

# 5. 检查磁盘空间
df -h
docker system df
```

---

## 性能问题

### Q9: 推理速度慢怎么办？

**A**: 优化建议：

```bash
# 1. 调整 GPU 显存使用率
export VLLM_GPU_MEMORY_UTILIZATION=0.95

# 2. 启用张量并行 (多 GPU)
export VLLM_TENSOR_PARALLEL_SIZE=2

# 3. 增大批处理大小
export VLLM_MAX_NUM_BATCHED_TOKENS=65536

# 4. 监控 GPU 使用
watch -n 1 nvidia-smi

# 5. 使用 TensorRT 引擎 (NVIDIA)
./cy-llm lite --engine cuda-trt --model <model>
```

---

### Q10: 显存不足 OOM？

**A**: 解决方案：

```bash
# 1. 降低显存使用率
VLLM_GPU_MEMORY_UTILIZATION=0.5 ./cy-llm lite --model <model>

# 2. 减少最大序列长度
export VLLM_MAX_MODEL_LEN=4096

# 3. 使用量化模型
# 在 deploy/models.json 中为模型设置 "quantization": "awq"，然后启动：
./cy-llm lite --model <model>

# 4. 使用 4-bit 量化
export VLLM_USE_4BIT=True

# 5. 检查显存占用
nvidia-smi
```

---

### Q11: 批量请求吞吐量低？

**A**: 调整批处理配置：

```json
{
  "models": {
    "qwen2.5-7b": {
      "engine": "cuda-vllm",
      "model_path": "Qwen/Qwen2.5-7B-Instruct",
      "max_num_batched_tokens": 65536,
      "max_num_seqs": 256,
      "gpu_memory_utilization": 0.9
    }
  }
}
```

或者使用连续批处理：

```bash
export VLLM_ENABLE_AUTOCHOOSE_BATCH_SIZE=True
```

---

## 模型相关

### Q12: 模型下载失败？

**A**: 解决方案：

```bash
# 1. 设置 HuggingFace 镜像
export HF_ENDPOINT=https://hf-mirror.com
huggingface-cli download Qwen/Qwen2.5-7B-Instruct

# 2. 手动下载模型
git lfs install
git clone https://huggingface.co/Qwen/Qwen2.5-7B-Instruct

# 3. 在 deploy/models.json 中设置本地 model_path，再按逻辑模型名启动
./cy-llm lite --model <model>

# 4. 检查网络连接
ping huggingface.co
```

---

### Q13: 如何添加自定义模型？

**A**: 编辑 `deploy/config.json`：

```json
{
  "models": {
    "my-model": {
      "engine": "cuda-vllm",
      "model_path": "path/to/model",
      "max_model_len": 8192,
      "gpu_memory_utilization": 0.9
    }
  }
}
```

然后重新加载配置：

```bash
./cy-llm reload
# 或
docker compose restart worker
```

---

### Q14: LoRA 适配器不生效？

**A**: 检查以下配置：

```bash
# 1. 验证适配器路径
ls -la /checkpoints/my_lora/

# 2. 检查配置文件
./cy-llm models list

# 3. 验证适配器文件
cat /checkpoints/my_lora/adapter_config.json

# 4. 推理时指定适配器
curl -X POST http://localhost:8080/api/v1/inference \
  -d '{"modelId": "qwen2.5-7b", "prompt": "Hello", "adapter": "/checkpoints/my_lora"}'
```

---

### Q15: TensorRT 模型转换失败？

**A**: 解决方案：

```bash
# 1. 安装 TensorRT-LLM
pip install tensorrt_llm -U --pre --extra-index-url https://pypi.nvidia.com

# 2. 验证安装
python -c "import tensorrt_llm; print(tensorrt_llm.__version__)"

# 3. 检查模型格式
ls -la /path/to/model/
cat /path/to/model/config.json | head

# 4. 使用更多显存
python scripts/convert_trt.py --model /path/to/model --dtype float16

# 5. 查看详细错误
python scripts/convert_trt.py --model /path/to/model --verbose
```

---

## 错误排查

### Q16: 常见错误码及解决方案

| 错误码 | 描述 | 解决方案 |
|--------|------|----------|
| `MODEL_NOT_FOUND` | 模型不存在 | 检查 config.json 模型 ID |
| `MODEL_NOT_READY` | 模型未加载 | 等待模型加载完成 |
| `INSUFFICIENT_RESOURCES` | 资源不足 | 减少显存占用或使用量化 |
| `RATE_LIMITED` | 请求过频 | 添加请求间隔 |
| `CONTEXT_TOO_LONG` | 上下文过长 | 减少输入长度 |

---

### Q17: 如何查看详细日志？

```bash
# Worker 日志
./cy-llm logs worker

# 实时日志
tail -f logs/worker.log

# Docker 日志
docker compose logs -f worker

# 前台启动并直接观察日志
./cy-llm lite
```

---

### Q18: 服务崩溃如何调试？

```bash
# 1. 查看崩溃日志
dmesg | tail -100

# 2. 查看系统日志
journalctl -xe

# 3. 检查核心转储
coredumpctl list
coredumpctl info <PID>

# 4. 前台运行并观察组件日志
./cy-llm lite
```

---

## 其他问题

### Q19: 如何升级到新版本？

```bash
# 1. 备份配置
cp -r deploy/config.json /backup/

# 2. 拉取最新代码
git fetch upstream
git checkout main
git merge upstream/main

# 3. 更新依赖
pip install -r requirements.txt

# 4. 迁移数据 (如需要)
# 查看 CHANGELOG.md 中的迁移说明

# 5. 重启服务
./cy-llm stop
./cy-llm lite
```

---

### Q20: 如何回滚版本？

```bash
# 1. 查看历史版本
git log --oneline

# 2. 回滚到指定版本
git checkout <commit-hash>

# 3. 如使用 Docker
docker compose down
docker pull your-registry/cy-llm:previous-version
docker compose up -d
```

---

### Q21: 性能监控怎么做？

```bash
# 1. 查看指标端点
curl http://localhost:8080/actuator/prometheus

# 2. 使用 Grafana 仪表板
# 导入项目中的 dashboard.json

# 3. GPU 监控
nvidia-smi -l 1

# 4. 内存监控
free -h

# 5. 网络监控
iftop -i eth0
```

---

### Q22: 多租户配置如何设置？

在 `config.json` 中配置租户隔离：

```json
{
  "tenants": {
    "tenant-1": {
      "models": ["model-a", "model-b"],
      "rateLimit": 100,
      "quota": 10000
    },
    "tenant-2": {
      "models": ["model-c"],
      "rateLimit": 50,
      "quota": 5000
    }
  }
}
```

---

### Q23: 如何贡献代码？

请参考 [CONTRIBUTING.md](./CONTRIBUTING.md) 了解详细流程。

---

### Q24: 遇到本文档未解决的问题？

**A**: 请按以下步骤处理：

1. 查看日志文件寻找线索
2. 搜索 [GitHub Issues](https://github.com/Baijin64/CY-LLM-Engine/issues)
3. 创建新的 Issue，包含以下信息：
   - 操作系统和版本
   - Python/Java 版本
   - CUDA/cuDNN 版本
   - 完整的错误日志
   - 重现步骤

---

## 快速故障排除清单

```
[ ] 1. 检查服务状态: ./cy-llm status
[ ] 2. 检查端口占用: lsof -i :8080
[ ] 3. 检查 GPU: nvidia-smi
[ ] 4. 检查 Redis: redis-cli ping
[ ] 5. 查看日志: ./cy-llm logs
[ ] 6. 验证配置: cat config.json
[ ] 7. 测试网络: curl http://localhost:8080/api/v1/health
```
