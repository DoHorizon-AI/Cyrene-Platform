> ⚠️ **旧设计文档（已过时）**：本文件描述已废弃的架构，可能与当前实现不符。当前权威文档见 [README 文档导航](../../../README.md#-文档导航)。请勿据此判断现状。

# CY-LLM Engine - 代码基线报告
**生成时间**: 2026-02-10  
**Git Commit**: $(cd /home/baijin/Dev/CY-LLM-Engine && git rev-parse HEAD)  
**分支**: $(cd /home/baijin/Dev/CY-LLM-Engine && git branch --show-current)

## 1. 项目结构概览

### 目录统计
- **Python文件数**: $(find /home/baijin/Dev/CY-LLM-Engine -name "*.py" -type f | wc -l)
- **Python代码行数**: $(find /home/baijin/Dev/CY-LLM-Engine -name "*.py" -type f -exec wc -l {} + 2>/dev/null | tail -1 | awk '{print $1}')
- **Requirements文件数**: 9
- **Dockerfile数**: 12

### 主要入口点
1. **Worker**: 
   - `src/cy_llm/worker/main.py`
   - `CY_LLM_Backend/worker/main.py` (重复)

2. **Gateway**:
   - `gateway/gateway_lite/app/main.py`
   - `CY_LLM_Backend/gateway_lite/app/main.py` (重复)

3. **CLI工具**:
   - `./cy-llm` (主脚本)

### 目录重复问题
存在两个平行的代码目录：
- `src/cy_llm/` - 较老的代码结构
- `CY_LLM_Backend/` - 较新的活跃代码

两目录内容高度重复，需要合并。

## 2. 依赖冲突分析

### Critical (P0)

#### 冲突1: protobuf版本冲突
- **requirements-base.txt**: `protobuf>=4.0.0,<6.0.0`
- **requirements-vllm.txt**: `protobuf==6.33.4`
- **vLLM 0.12.0要求**: `protobuf<6.0.0,>=4.0.0`
- **后果**: 安装requirements-vllm.txt会导致vLLM无法运行
- **解决方案**: 统一使用protobuf==4.25.3

#### 冲突2: CUDA版本不匹配
- **requirements-nvidia.txt**: PyTorch使用 `cu118` (CUDA 11.8)
- **requirements-vllm.txt**: PyTorch使用 `cu124` (CUDA 12.4)
- **后果**: 混合安装会导致CUDA库冲突，运行时找不到libcudart.so
- **解决方案**: 统一使用cu124

### High (P1)

#### 冲突3: grpcio版本不一致
- **requirements-base.txt**: `grpcio>=1.60.0,<2.0.0`
- **requirements-vllm.txt**: `grpcio==1.76.0`
- **风险**: 版本范围过大可能引入不兼容版本
- **解决方案**: 锁定grpcio==1.76.0

#### 冲突4: numpy版本差异
- **requirements-base.txt**: `numpy>=1.24.0,<2.0.0`
- **requirements-vllm.txt**: `numpy==1.26.4`
- **风险**: 小版本差异，通常兼容但建议统一
- **解决方案**: 统一使用numpy==1.26.4

#### 冲突5: torch版本差异
- **requirements-nvidia.txt**: `torch>=2.1.0,<2.5.0`
- **requirements-vllm.txt**: `torch==2.9.0`
- **风险**: 版本跨度大，API可能有差异
- **解决方案**: 统一使用torch==2.9.0+cu124

## 3. 构建命令文档

### Worker启动
```bash
# 使用CY_LLM_Backend版本
python -m CY_LLM_Backend.worker.main

# 使用src版本（较老）
python -m src.cy_llm.worker.main
```

### Gateway启动
```bash
# Lite版本
python -m CY_LLM_Backend.gateway_lite.app.main
```

### 使用CLI工具
```bash
# 设置环境
./cy-llm setup --engine cuda-vllm

# 启动Lite版本
./cy-llm lite --engine cuda-vllm --model qwen2.5-7b

# 查看状态
./cy-llm status

# 停止服务
./cy-llm stop
```

### Docker构建
```bash
# Worker (NVIDIA)
docker build -f CY_LLM_Backend/deploy/Dockerfile.worker.nvidia -t cy-llm-worker:nvidia .

# Worker (Ascend)
docker build -f CY_LLM_Backend/deploy/Dockerfile.worker.ascend -t cy-llm-worker:ascend .

# Gateway
docker build -f CY_LLM_Backend/deploy/Dockerfile.gateway -t cy-llm-gateway .

# Coordinator
docker build -f CY_LLM_Backend/deploy/Dockerfile.coordinator -t cy-llm-coordinator .
```

## 4. 导入测试结果

### 基础导入测试
由于依赖未安装，当前无法完整导入模块。

**主要ImportError预期**:
- `torch` - 未安装
- `vllm` - 未安装
- `transformers` - 未安装
- `grpc` - 可能已安装（基础开发环境）

### 推荐的修复后导入路径
```python
# 重构后的统一导入路径
from cy_llm.worker.engines import BaseEngine, EngineFactory
from cy_llm.worker.core import TaskScheduler, MemoryManager
from cy_llm.deps import HardwareDetector, DependencyResolver
```

## 5. 已知问题总结

### 环境问题
1. CUDA版本混乱（cu118 vs cu124）
2. protobuf版本冲突
3. 重复目录结构需要合并

### 功能问题（用户报告）
1. 模型生成速度异常（6187 tokens/s，过快）
2. 输出内容重复
3. 缺少首Token延迟(TTFT)数据
4. 40系显卡显存瓶颈

## 6. 重构优先级

### P0 - Critical
- [ ] 修复protobuf版本冲突
- [ ] 统一CUDA版本到cu124
- [ ] 合并重复目录

### P1 - High
- [ ] 建立Dependency Registry
- [ ] 实现Hardware Detector
- [ ] 修复推理重复问题

### P2 - Medium
- [ ] 优化性能基准
- [ ] 添加国内镜像支持
- [ ] 完善文档

## 7. 回滚策略

如需回滚到基线状态：
```bash
cd /home/baijin/Dev/CY-LLM-Engine
git checkout $(git rev-parse HEAD)
git clean -fd  # 注意：会删除未跟踪文件
```

## 附录: 完整目录树

详见: `tree_snapshot.txt`
