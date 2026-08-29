> ⚠️ **旧设计文档（已过时）**：本文件描述已废弃的架构，可能与当前实现不符。当前权威文档见 [README 文档导航](../../../README.md#-文档导航)。请勿据此判断现状。

# CY-LLM Engine Refactor - Environment Plan

## 环境配置状态
- **状态**: ✅ 已完成
- **配置日期**: 2026-02-10
- **环境名称**: cy-llm-refactor
- **Python版本**: 3.11.14

## 已安装工具链

| 工具 | 版本 | 用途 | 验证命令 |
|------|------|------|----------|
| pytest | 9.0.2 | 测试框架 | `pytest --version` |
| pytest-cov | 7.0.0 | 覆盖率 | `pytest --cov` |
| black | 26.1.0 | 代码格式化 | `black --version` |
| ruff | 0.15.0 | Linter | `ruff --version` |
| mypy | 1.19.1 | 类型检查 | `mypy --version` |

## 环境激活命令

```bash
# 激活环境
conda activate cy-llm-refactor

# 验证
which python  # 应显示 /home/baijin/miniforge3/envs/cy-llm-refactor/bin/python
python --version  # 应显示 Python 3.11.14
```

## 执行的命令记录

### 1. 创建Conda环境
```bash
conda create -n cy-llm-refactor python=3.11 -y
conda activate cy-llm-refactor
```

### 2. 安装开发工具
```bash
pip install pytest pytest-cov black ruff mypy
```

### 3. 验证安装
```bash
pytest --version
black --version
ruff --version
mypy --version
```

## 依赖冲突分析结果

### P0 - Critical
1. **protobuf版本冲突**
   - requirements-vllm.txt: `protobuf==6.33.4`
   - vLLM 0.12.0 要求: `protobuf<6.0.0,>=4.0.0`
   - **影响**: vLLM无法运行
   - **解决方案**: 统一使用protobuf==4.25.3

2. **CUDA版本不匹配**
   - requirements-nvidia.txt: `cu118` (PyTorch CUDA 11.8)
   - requirements-vllm.txt: `cu124` (PyTorch CUDA 12.4)
   - **影响**: 混合安装导致CUDA库冲突
   - **解决方案**: 统一使用cu124

### P1 - High
3. **grpcio版本范围过大**
   - requirements-base.txt: `grpcio>=1.60.0,<2.0.0`
   - requirements-vllm.txt: `grpcio==1.76.0`
   - **影响**: 可能引入不兼容版本
   - **解决方案**: 锁定grpcio==1.76.0

### P2 - Medium
4. **numpy版本冲突**
   - requirements-base.txt: `numpy>=1.24.0,<2.0.0`
   - requirements-vllm.txt: `numpy==1.26.4`
   - **影响**: 小版本差异，通常兼容
   - **解决方案**: 统一使用numpy==1.26.4

## 项目基线快照

- **目录数**: 56
- **文件数**: 117
- **主要代码目录**:
  - src/cy_llm/worker/
  - CY_LLM_Backend/worker/
  - gateway/
  - CY_LLM_Training/

## 回滚点

### 回滚到环境创建前
```bash
conda deactivate
conda remove -n cy-llm-refactor --all -y
```

### 重新创建环境
```bash
conda create -n cy-llm-refactor python=3.11 -y
conda activate cy-llm-refactor
pip install pytest pytest-cov black ruff mypy
```

## 后续环境需求

### Phase 5-6 需要
- 保持当前开发环境
- 为不同引擎创建独立环境（按需）
- vLLM环境: protobuf==4.25.3, torch==2.9.0+cu124

### Phase 7 测试需要
- 可能需要GPU环境进行性能测试
- 或使用mock引擎进行CI测试

## 已知限制

1. **CPU-only**: 当前环境无GPU，无法测试CUDA功能
2. **无大型依赖**: torch/vLLM未安装，需单独环境
3. **网络依赖**: pip安装依赖网络，国内建议使用镜像

## 故障排除

### 问题: conda命令未找到
**解决**: 
```bash
source ~/miniforge3/etc/profile.d/conda.sh
```

### 问题: pip安装超时
**解决**: 
```bash
pip install --index-url https://pypi.tuna.tsinghua.edu.cn/simple <package>
```

### 问题: 环境激活失败
**解决**: 
```bash
conda init bash
source ~/.bashrc
conda activate cy-llm-refactor
```
