> ⚠️ **旧设计文档（已过时）**：本文件描述已废弃的架构，可能与当前实现不符。当前权威文档见 [README 文档导航](../../../README.md#-文档导航)。请勿据此判断现状。

# CY-LLM Engine - Token Speed Optimization Refactor

## 项目基本信息
- **项目名称**: CY-LLM Engine Token Speed Optimization
- **父项目**: CY-LLM Engine Refactor
- **仓库路径**: /home/baijin/Dev/CY-LLM-Engine
- **目标平台**: NVIDIA GPU (CUDA)
- **分支策略**: main分支，直接优化

## 当前性能基线
- **当前速度**: 15-20 tokens/s
- **目标速度**: ≥50 tokens/s (vLLM标准性能)
- **性能差距**: 2.5-3.3x

## 问题定位
1. **致命瓶颈**: `vllm_cuda_engine.py` 逐字符yield (第500行)
2. **架构问题**: 使用同步LLM而非AsyncLLMEngine
3. **传输开销**: gRPC逐字符消息传输

## 非目标
- 不修改模型加载逻辑
- 不修改KV Cache管理
- 不修改gRPC协议定义
- 不引入新依赖（仅使用现有vLLM功能）

## 成功指标
- [ ] Token速度 ≥50/s (DeepSeek 7B模型)
- [ ] TTFT (首Token延迟) <200ms
- [ ] API完全向后兼容
- [ ] 无功能回归
