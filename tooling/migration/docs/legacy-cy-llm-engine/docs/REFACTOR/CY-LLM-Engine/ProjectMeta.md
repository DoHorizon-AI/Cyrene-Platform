> ⚠️ **旧设计文档（已过时）**：本文件描述已废弃的架构，可能与当前实现不符。当前权威文档见 [README 文档导航](../../../README.md#-文档导航)。请勿据此判断现状。

# CY-LLM Engine Refactor - Project Meta

## 项目基本信息
- **项目名称**: CY-LLM Engine
- **仓库路径**: /home/baijin/Dev/CY-LLM-Engine
- **项目类型**: AI推理后端系统（支持vLLM/TensorRT/MindIE多引擎）
- **目标平台**: NVIDIA GPU / 华为Ascend NPU / Intel Arc GPU（新增）
- **分支策略**: main分支重构，保留tag备份

## 当前痛点
1. 依赖地狱：protobuf版本冲突（requirements-vllm.txt使用6.33.4，vLLM 0.12.0要求<6.0.0）
2. CUDA版本混乱：requirements-nvidia.txt用cu118，requirements-vllm.txt用cu124
3. 目录重复：src/和CY_LLM_Backend/存在大量重复代码
4. 硬件适配差：无法自动识别硬件并推荐依赖组合
5. 推理异常：模型生成速度异常、重复内容

## 重构愿景
建立四层架构：
- L1: 硬件检测层（自动识别GPU/NPU类型和驱动）
- L2: 环境适配层（自动选择CUDA/CANN/Docker镜像）
- L3: 依赖解析层（智能依赖矩阵+自动推荐）
- L4: 服务运行时（统一的Worker/Coordinator/Gateway）

## 非目标
- 不修改核心推理算法（vLLM/TRT/MindIE调用逻辑保持不变）
- 不破坏现有API接口（保持OpenAI兼容接口不变）
- 不强制升级模型版本（保持现有模型支持）

## 目标平台扩展
- NVIDIA: CUDA 11.8/12.1/12.4
- Ascend: CANN 8.0+
- Intel: oneAPI + OpenVINO/vLLM Intel分支（新增）
