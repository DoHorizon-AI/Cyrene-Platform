> ⚠️ **旧设计文档（已过时）**：本文件描述已废弃的架构，可能与当前实现不符。当前权威文档见 [README 文档导航](../../../README.md#-文档导航)。请勿据此判断现状。

# CY-LLM Engine Refactor - Refactor Goals

## 重构目标 (Goals)

### G1: 建立智能依赖管理系统
- 创建依赖兼容性矩阵（Dependency Compatibility Matrix）
- 支持硬件自动检测并推荐可用依赖组合
- 提供交互式依赖选择CLI工具

### G2: 消除目录结构混乱
- 合并 src/ 和 CY_LLM_Backend/ 重复目录
- 建立清晰的模块边界：core/ engines/ adapters/ utils/
- 统一配置管理（单一config来源）

### G3: 构建多层环境适配
- 硬件检测层: 自动识别NVIDIA/Ascend/Intel/CPU-only
- 运行时选择: Conda env / Docker / Native
- 依赖安装: 根据硬件+引擎自动选择requirements

### G4: 修复现有依赖冲突
- 解决protobuf版本冲突（统一使用4.x兼容vLLM 0.12.0）
- 统一PyTorch CUDA版本（默认cu124）
- 明确vllm/trt/mindie的互斥/共存关系

### G5: 改进推理稳定性
- 添加模型推理健康检查
- 修复重复生成问题（调整sampling参数）
- 添加性能基准测试

## 非目标 (Non-Goals)

### NG1: 不修改推理核心逻辑
- vLLM/TensorRT/MindIE的调用方式保持不变
- 模型加载和推理流程不变
- KV Cache管理逻辑不变

### NG2: 不破坏外部接口
- OpenAI兼容API保持不变
- gRPC协议定义保持不变
- 环境变量命名保持不变（除非冲突）

### NG3: 不强制升级模型
- 保持对Qwen2.5/Llama3等现有模型的支持
- 不强制转换模型格式
- 不删除任何已有引擎支持

### NG4: 不引入新运行时依赖
- 不强制要求Docker（但支持）
- 不强制要求Kubernetes
- 保持Python 3.10+兼容性

## 成功指标

### 质量指标
- [ ] 依赖安装成功率 > 95%（主流平台）
- [ ] 目录重复率降至 0%（消除src/CY_LLM_Backend重复）
- [ ] 代码测试覆盖率 > 70%

### 性能指标
- [ ] 首Token延迟(TTFT)可测量且稳定
- [ ] 重复生成率 < 5%（相同prompt多次调用）
- [ ] 环境检测+推荐时间 < 30秒

### 可用性指标
- [ ] 新用户从clone到运行 < 15分钟
- [ ] 支持硬件类型 >= 3种（NVIDIA/Ascend/Intel）
- [ ] 文档完整性 > 90%（核心流程有文档）
