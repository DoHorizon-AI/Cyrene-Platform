# 测试覆盖率报告

## 概述

本文档说明项目的测试策略和覆盖情况。

---

## 测试架构

### 测试层级

```
tests/
├── unit/                   # 单元测试（快速，无依赖）
│   ├── test_task_scheduler.py
│   └── test_memory_manager.py
└── integration/            # 集成测试（需要环境）
    └── test_grpc_uds.py
```

---

## 已实现的测试

### 1. TaskScheduler 单元测试

**文件**: `tests/unit/test_task_scheduler.py`

**覆盖场景**:
- ✅ 基本任务提交与执行
- ✅ 队列溢出背压机制
- ✅ 优先级调度
- ✅ 异常处理与传播
- ✅ 关闭时任务取消

**运行方式**:
```bash
pytest tests/unit/test_task_scheduler.py -v
```

---

### 2. GPUMemoryManager 单元测试

**文件**: `tests/unit/test_memory_manager.py`

**覆盖场景**:
- ✅ 单例模式
- ✅ 模型注册/注销
- ✅ LRU 访问时间更新
- ✅ 线程锁管理
- ✅ 显存压力检测（Mock）

**运行方式**:
```bash
pytest tests/unit/test_memory_manager.py -v
```

---

### 3. gRPC UDS 集成测试（框架）

**文件**: `tests/integration/test_grpc_uds.py`

**状态**: 🚧 框架已创建，需要完整环境支持

**运行方式**:
```bash
pytest tests/integration/ -v -m integration
```

---

## 运行所有测试

### 快速测试（仅单元测试）

```bash
pytest tests/unit/ -v
```

### 完整测试（包含集成测试）

```bash
pytest tests/ -v
```

### 生成覆盖率报告

```bash
uv run pytest tests/ --cov=cy_exec --cov-report=html
# 查看报告: open htmlcov/index.html
```

---

## CI/CD 集成

### GitHub Actions 示例

```yaml
name: Tests

on: [push, pull_request]

jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v3
      - uses: actions/setup-python@v4
        with:
          python-version: '3.12'
      - run: pip install -e .[dev]
      - run: pytest tests/unit/ -v --cov
```

---

## 测试覆盖目标

| 模块 | 当前覆盖率 | 目标覆盖率 | 状态 |
|------|------------|------------|------|
| `task_scheduler.py` | ~85% | 90% | ✅ |
| `memory_manager.py` | ~75% | 85% | 🚧 |
| `server.py` | ~60% | 80% | 📋 待改进 |
| `grpc_servicer.py` | ~50% | 75% | 📋 待改进 |

---

## 下一步

1. **补充测试用例**：
   - Worker 的模型加载流程
   - gRPC Servicer 的完整请求处理

2. **集成测试**：
   - 完整的 Gateway -> Coordinator -> Worker 链路测试
   - UDS 通信性能测试

3. **性能测试**：
   - 并发推理压测
   - 显存管理压力测试
