> ⚠️ **旧设计文档（已过时）**：本文件描述已废弃的架构，可能与当前实现不符。当前权威文档见 [README 文档导航](../../../README.md#-文档导航)。请勿据此判断现状。

# CY-LLM Engine Refactor - Task Board

## 当前状态
- **Phase**: 3 → 4 (环境配置阶段)
- **已完成**: Phase 2 架构设计
- **进行中**: Phase 3 任务分解已完成，准备环境配置
- **总体进度**: 15% (设计完成，准备实施)

## 任务清单

### M0: 基线建立 (P0-Critical) - 必须先完成

| ID | 任务 | 状态 | 优先级 | 负责人 | DoD | 验证命令 |
|----|------|------|--------|--------|-----|----------|
| T-000 | 建立代码基线 | 🔵 Pending | P0 | Python Agent | 目录树快照、依赖冲突报告 | `tree -I '__pycache__' > baseline/tree.txt` |
| T-001 | 构建命令文档化 | 🔵 Pending | P0 | Shell Agent | 记录所有启动/构建命令 | `cat baseline/build_commands.md` |
| T-002 | 基线行为验证 | 🔵 Pending | P0 | Python Agent | 至少1个E2E测试通过 | `python -m pytest tests/e2e/test_baseline.py -v` |

### M4: 回归测试套件 (P1-High)

| ID | 任务 | 状态 | 优先级 | 负责人 | DoD | 验证命令 |
|----|------|------|--------|--------|-----|----------|
| T-010 | 单元测试增强 | 🔵 Pending | P1 | Python Agent | 覆盖率>=70% | `pytest --cov=worker --cov-report=term` |
| T-011 | 集成测试套件 | 🔵 Pending | P1 | Python Agent | Gateway-Coordinator-Worker链路测试 | `pytest tests/integration/ -v` |
| T-012 | E2E测试框架 | 🔵 Pending | P1 | Python Agent | 可重复执行的端到端测试 | `./scripts/run_e2e_test.sh` |
| T-013 | API兼容性测试 | 🔵 Pending | P1 | Python Agent | API响应字段100%匹配 | `pytest tests/api/test_compatibility.py` |

### M1: 目录合并 Phase 1 (P0-Critical)

| ID | 任务 | 状态 | 优先级 | 负责人 | DoD | 验证命令 |
|----|------|------|--------|--------|-----|----------|
| T-020 | 目录重复分析 | 🔵 Pending | P0 | Python Agent | 完整diff报告 | `cat phase1/diff_analysis.md` |
| T-021 | 迁移计划制定 | 🔵 Pending | P0 | Python Agent | 迁移计划获批 | `cat phase1/migration_plan.md` |
| T-022 | 合并核心模块 | 🔵 Pending | P1 | Python Agent | core/模块统一 | `python -c "from worker.core import *"` |
| T-023 | 合并引擎模块 | 🔵 Pending | P1 | Python Agent | engines/模块统一 | `python -c "from worker.engines import list_engines"` |
| T-024 | 合并配置和工具 | 🔵 Pending | P1 | Python Agent | config/, utils/统一 | `python -c "from worker.config import load_config"` |
| T-025 | 清理废弃目录 | 🔵 Pending | P1 | Python Agent | CY_LLM_Backend/删除 | `test -d CY_LLM_Backend && echo "FAIL" || echo "PASS"` |

### M2: 依赖系统 Phase 2 (P1-High)

| ID | 任务 | 状态 | 优先级 | 负责人 | DoD | 验证命令 |
|----|------|------|--------|--------|-----|----------|
| T-030 | Dependency Registry设计 | 🔵 Pending | P1 | Python Agent | JSON Schema定义 | `jsonschema -i registry.json schema.json` |
| T-031 | Hardware Detector实现 | 🔵 Pending | P1 | Python Agent | 支持NVIDIA/Ascend/CPU检测 | `python -m cy_llm.deps.detect --test` |
| T-032 | Dependency Resolver实现 | 🔵 Pending | P1 | Python Agent | 根据硬件+引擎解析依赖 | `python -m cy_llm.deps.resolve --engine vllm` |
| T-033 | CLI setup命令实现 | 🔵 Pending | P1 | Python Agent | `./cy-llm setup`可用 | `./cy-llm setup --dry-run` |
| T-034 | 修复protobuf冲突 | 🔵 Pending | P0 | Python Agent | vLLM与base protobuf一致 | `pip check`无冲突 |
| T-035 | 统一CUDA版本 | 🔵 Pending | P1 | Python Agent | 所有requirements用cu124 | `grep -r "cu118" requirements*.txt`无结果 |
| T-036 | requirements合并 | 🔵 Pending | P1 | Python Agent | 单一requirements来源 | `ls requirements*.txt`符合设计 |
| T-037 | 镜像源支持 | 🔵 Pending | P2 | Python Agent | 支持国内镜像 | `cy-llm setup --mirror tsinghua` |

### M3: 引擎重构 Phase 3 (P1-High)

| ID | 任务 | 状态 | 优先级 | 负责人 | DoD | 验证命令 |
|----|------|------|--------|--------|-----|----------|
| T-040 | BaseEngine ABC设计 | 🔵 Pending | P1 | Python Agent | 抽象基类定义 | `python -c "from worker.engines.base import BaseEngine; import inspect; inspect.isabstract(BaseEngine)"` |
| T-041 | vLLM引擎适配 | 🔵 Pending | P1 | Python Agent | 继承BaseEngine | `pytest tests/engines/test_vllm.py` |
| T-042 | TensorRT引擎适配 | 🔵 Pending | P1 | Python Agent | 继承BaseEngine | `pytest tests/engines/test_trt.py` |
| T-043 | MindIE引擎适配 | 🔵 Pending | P1 | Python Agent | 继承BaseEngine | `pytest tests/engines/test_mindie.py` |
| T-044 | Engine Factory统一 | 🔵 Pending | P1 | Python Agent | 工厂模式创建引擎 | `python -c "from worker.engines import EngineFactory; f = EngineFactory(); e = f.create('vllm')"` |
| T-045 | 引擎性能基准 | 🔵 Pending | P2 | Python Agent | TTFT差异<5% | `pytest tests/perf/test_engine_perf.py` |
| T-046 | 修复推理重复问题 | 🔵 Pending | P1 | Python Agent | 重复率<5% | 人工测试验证 |

### 依赖关系图

```
关键路径:
T-000 → T-001 → T-002 → T-020 → T-021 → T-030 → T-040 → T-041

可并行组:
- T-022/T-023/T-024 (核心/引擎/配置合并)
- T-031/T-032/T-033 (检测器/解析器/CLI)
- T-041/T-042/T-043 (三引擎适配)

阻塞关系:
- T-021 → T-022/T-023/T-024
- T-030 → T-032
- T-040 → T-041/T-042/T-043
```

## 风险追踪

| ID | 风险描述 | 等级 | 状态 | 缓解措施 |
|----|----------|------|------|----------|
| R-001 | protobuf版本冲突导致vLLM无法运行 | 🔴 P0 | 开放 | Registry按引擎隔离，统一使用4.x版本 |
| R-002 | 目录合并丢失代码 | 🔴 P0 | 开放 | 完整diff分析 + git历史保留 + 基线测试 |
| R-003 | MindIE/Ascend无测试环境 | 🟡 P1 | 开放 | CI环境 + mock测试 + 华为云资源 |
| R-004 | 国内网络下载失败 | 🟡 P1 | 开放 | 清华/阿里镜像 + 预下载wheel支持 |
| R-005 | API接口被破坏 | 🔴 P0 | 开放 | T-013兼容性测试 + 接口冻结清单 |

## 冻结接口清单 (变更需审批)

- [ ] HTTP API: /v1/chat/completions, /v1/models, /health
- [ ] gRPC: InferenceService/Generate, CoordinatorService/RegisterWorker
- [ ] 环境变量: CY_LLM_ENGINE, CY_LLM_DEFAULT_MODEL, COORDINATOR_GRPC_ADDR
- [ ] 配置文件: models.json 字段定义

## 变更记录

| 日期 | 变更 | 原因 | 审批状态 |
|------|------|------|----------|
| 2026-02-10 | Phase 2架构设计完成 | 用户批准全面重构 | ✅ 已批准 |
