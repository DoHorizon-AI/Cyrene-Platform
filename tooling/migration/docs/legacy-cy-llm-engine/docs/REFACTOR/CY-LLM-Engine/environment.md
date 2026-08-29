> ⚠️ **旧设计文档（已过时）**：本文件描述已废弃的架构，可能与当前实现不符。当前权威文档见 [README 文档导航](../../../README.md#-文档导航)。请勿据此判断现状。

# CY-LLM Engine 重构环境配置

**文档版本**: 1.0.0
**创建日期**: 2026-02-10
**环境名称**: cy-llm-refactor
**目标**: CPU-only 开发与重构

---

## 1. 环境概览

### 1.1 Conda 环境

```bash
环境名称: cy-llm-refactor
Python 版本: 3.11.14
位置: /home/baijin/miniforge3/envs/cy-llm-refactor
激活命令: conda activate cy-llm-refactor
```

### 1.2 开发工具链

| 工具 | 版本 | 用途 |
|------|------|------|
| pytest | 9.0.2 | 测试框架 |
| pytest-cov | 7.0.0 | 覆盖率报告 |
| black | 26.1.0 | 代码格式化 |
| ruff | 0.15.0 | Linter & Formatter |
| mypy | 1.19.1 | 静态类型检查 |

---

## 2. 安装步骤

### 2.1 环境创建

```bash
# 创建 Conda 环境
conda create -n cy-llm-refactor python=3.11 -y

# 激活环境
conda activate cy-llm-refactor

# 验证 Python 版本
python --version
# 输出: Python 3.11.14
```

### 2.2 开发依赖安装

```bash
# 安装核心开发工具
pip install pytest pytest-cov black ruff mypy

# 验证安装
pytest --version
black --version
ruff --version
mypy --version
```

### 2.3 项目依赖 (CPU-only)

```bash
# 创建 CPU-only 依赖文件 (可选)
cat > requirements-cpu.txt << 'EOF'
-r requirements-base.txt

# CPU-only PyTorch (无 CUDA 依赖)
torch==2.4.0
torchvision==0.19.0
torchaudio==2.9.0

# 统一版本以避免冲突
grpcio==1.76.0
protobuf==5.28.3
accelerate==0.32.0
transformers==4.42.4
EOF

# 安装 CPU-only 依赖
pip install -r requirements-cpu.txt
```

---

## 3. 项目结构

```
CY-LLM-Engine/
├── CY_LLM_Backend/        # 主后端服务
│   ├── coordinator_lite/  # 协调器
│   ├── gateway_lite/      # 网关
│   ├── worker/           # 工作节点 (重构重点)
│   │   ├── engines/      # 推理引擎
│   │   ├── core/         # 核心逻辑
│   │   └── tests/        # 测试
│   └── deploy/           # 部署配置
├── gateway/              # API 网关
├── crates/cy-proxy/      # Rust 数据面代理
├── CY_LLM_Training/      # 训练模块
├── tests/                # 测试套件
├── scripts/              # 工具脚本
└── docs/                 # 文档
    └── refactor/
        └── cy-llm-engine/
            └── environment.md (本文档)
```

---

## 4. 开发工作流

### 4.1 每日开发

```bash
# 1. 激活环境
conda activate cy-llm-refactor

# 2. 进入项目目录
cd /home/baijin/Dev/CY-LLM-Engine

# 3. 运行测试
pytest tests/unit/ -v

# 4. 代码检查
black --check .
ruff check .
mypy .

# 5. 格式化代码
black .
ruff check --fix .
```

### 4.2 重构任务

```bash
# 运行特定模块测试
pytest CY_LLM_Backend/worker/tests/ -v

# 类型检查特定路径
mypy CY_LLM_Backend/worker/core/

# 生成覆盖率报告
pytest --cov=CY_LLM_Backend/worker/ --cov-report=html
```

---

## 5. 已知问题与解决方案

### 5.1 依赖冲突

#### Protobuf 版本冲突
**问题**: requirements-vllm.txt 中 `protobuf==6.33.4` 与 vLLM 0.12.0 要求 `protobuf<6.0.0` 冲突

**解决方案** (CPU-only 环境不受影响):
```bash
# 临时解决 (如需使用 vLLM)
pip install protobuf==5.28.3
```

#### CUDA 版本不匹配
**问题**: 不同 requirements 文件使用不同 CUDA 版本

**解决方案**: CPU-only 开发环境使用统一依赖
```bash
# 使用 CPU-only 依赖
pip install -r requirements-cpu.txt
```

---

## 6. 测试配置

### 6.1 pytest 配置

```ini
# pytest.ini
[tool:pytest]
testpaths = tests
python_files = test_*.py
python_classes = Test*
python_functions = test_*
addopts = -v --tb=short
```

### 6.2 mypy 配置

```ini
# mypy.ini
[mypy]
python_version = 3.11
warn_return_any = True
warn_unused_configs = True
ignore_missing_imports = True
```

### 6.3 black 配置

```toml
# pyproject.toml
[tool.black]
line-length = 88
target-version = ['py311']
include = '\.pyi?$'
```

---

## 7. 验证清单

启动新开发会话前，运行以下检查:

```bash
# 环境验证
conda env list | grep cy-llm-refactor
python --version  # 应显示 3.11.x
pytest --version
black --version

# 代码质量
black --check .
ruff check .
mypy .

# 基础测试
pytest tests/unit/ -v --tb=short
```

---

## 8. 故障排除

### 8.1 Conda 环境问题

```bash
# 环境损坏时重新创建
conda deactivate
conda env remove -n cy-llm-refactor
conda create -n cy-llm-refactor python=3.11 -y
conda activate cy-llm-refactor
pip install pytest pytest-cov black ruff mypy
```

### 8.2 依赖冲突

```bash
# 清除 pip 缓存
pip cache purge

# 重新安装
pip install --force-reinstall -r requirements-cpu.txt
```

### 8.3 权限问题

```bash
# 确保项目目录可写
chmod -R u+w /home/baijin/Dev/CY-LLM-Engine
```

---

## 9. 资源与文档

### 9.1 相关文档

- [依赖冲突分析报告](../dependency-conflict-report.md)
- [项目基线快照](../project-baseline-snapshot.md)
- [代码标准](../refactor/code_standards.md)
- [重构计划](../refactor/refactor_plan.md)

### 9.2 外部资源

- [Python 3.11 文档](https://docs.python.org/3.11/)
- [pytest 文档](https://docs.pytest.org/)
- [black 文档](https://black.readthedocs.io/)
- [ruff 文档](https://docs.astral.sh/ruff/)
- [mypy 文档](https://mypy.readthedocs.io/)

---

## 10. 安全注意事项

### 10.1 环境隔离

- 所有开发活动在 `cy-llm-refactor` 环境中进行
- 不修改系统 Python 或 base 环境
- 大型 ML 框架 (torch, vLLM) 仅按需安装到专用环境

### 10.2 代码安全

- 使用 `black` 确保代码一致性
- 使用 `ruff` 进行静态分析
- 使用 `mypy` 捕获类型错误
- 定期运行完整测试套件

### 10.3 依赖安全

```bash
# 检查已知漏洞
pip install safety
safety check -r requirements-base.txt
```

---

## 11. 维护日志

| 日期 | 操作 | 执行者 |
|------|------|--------|
| 2026-02-10 | 初始环境配置 | DevOps Setup Engineer |
| 2026-02-10 | 安装开发工具链 | DevOps Setup Engineer |
| 2026-02-10 | 生成依赖冲突报告 | DevOps Setup Engineer |
| 2026-02-10 | 生成项目基线快照 | DevOps Setup Engineer |

---

**维护者**: Environment & DevOps Setup Engineer
**下次审查**: 2026-03-10
