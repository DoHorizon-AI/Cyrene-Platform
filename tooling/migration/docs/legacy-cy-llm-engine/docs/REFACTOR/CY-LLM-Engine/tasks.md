> ⚠️ **旧设计文档（已过时）**：本文件描述已废弃的架构，可能与当前实现不符。当前权威文档见 [README 文档导航](../../../README.md#-文档导航)。请勿据此判断现状。

# Task Breakdown: CY-LLM Engine Refactoring
- **Mode**: REFACTOR  
- **Project**: CY-LLM Engine  
- **Version**: 1.5.2.0 → 2.0.0  
- **Target Python**: 3.10+
- **Sources**:
  - Requirements: `docs/refactor/cy-llm-engine/RefactorGoals.md`
  - Design: `docs/REFACTOR/CY-LLM-Engine/design.md`
  - Interfaces: `docs/REFACTOR/CY-LLM-Engine/interfaces.md`

---

## Assumptions
- 必须保持OpenAI兼容HTTP API和gRPC接口不变（NG2）
- 必须保持对Qwen2.5/Llama3等现有模型支持（NG3）
- 不修改推理核心逻辑（NG1）
- 支持硬件: NVIDIA CUDA, Ascend NPU, CPU-only

---

## Risks (Top 5)

| 优先级 | 风险 | 影响 | 缓解措施 |
|--------|------|------|----------|
| P0 | protobuf版本冲突 | vLLM无法运行 | Registry按引擎隔离 |
| P0 | 目录合并丢失代码 | 功能回退 | 完整diff+基线测试 |
| P1 | MindIE/Ascend无本地测试环境 | 适配错误 | CI环境+mock |
| P1 | 依赖下载失败(国内网络) | 安装失败 | 镜像源+预下载wheel |
| P2 | 重构期间主分支冲突 | 合并困难 | 特性分支+频繁rebase |

---

## Milestones

### M0: Baseline & Prep (重构前必须完成)
- **目标**: 建立可验证的基线，确保重构后可检测回归
- **成功标准**: 
  - 现有构建命令100%可执行
  - 至少1个E2E测试通过
  - 代码覆盖率达到基线值

### M1: Directory Consolidation (Phase 1)
- **目标**: 消除 `src/cy_llm/` 和 `CY_LLM_Backend/` 重复
- **成功标准**:
  - 目录重复率降至0%
  - 所有import路径统一
  - 单元测试通过率100%

### M2: Dependency System (Phase 2)
- **目标**: 实现智能依赖管理
- **成功标准**:
  - Registry JSON Schema有效
  - 硬件检测准确率>95%
  - `cy-llm setup`命令可用

### M3: Engine Refactoring (Phase 3)
- **目标**: 引擎继承BaseEngine，消除重复代码
- **成功标准**:
  - 所有引擎通过ABC验证
  - 引擎工厂统一调度
  - 性能无退化(TTFT差异<5%)

### M4: Testing & Delivery
- **目标**: 完整测试覆盖和文档
- **成功标准**:
  - 测试覆盖率>70%
  - 集成测试通过率100%
  - 文档完整性>90%

---

## Dependency Overview

```
关键路径: T0 -> T1 -> Phase1 -> Phase2 -> Phase3

可并行:
- T0 和 T1 部分可并行
- Phase2的Registry/Detector/Resolver设计可并行
- Phase3各引擎适配可并行

强依赖:
- T023必须在T022之后
- T032必须在T030之后
- T041/T042/T043必须在T040之后
```

---

## Task List (WBS)

---

### T-000 建立代码基线

**Goal**: 捕获重构前的完整代码状态，建立可比较的基线

**Scope**:
- 记录当前目录结构快照
- 记录所有requirements.txt的依赖冲突
- 创建基线测试报告

**Out of Scope**: 不修改任何代码

**Priority**: P0-Critical

**Test Cases**:
- [ ] 目录树导出完整
- [ ] Git状态干净（无未提交变更）
- [ ] 基线报告生成成功

**Suggested Owner**: Python Agent

**Deliverables**:
- `docs/refactor/cy-llm-engine/baseline/tree_snapshot.txt`
- `docs/refactor/cy-llm-engine/baseline/requirements_conflict_report.md`
- `docs/refactor/cy-llm-engine/baseline/test_baseline_report.md`

**Acceptance Checks**:
- [ ] `tree -I '__pycache__|*.pyc|.git' > tree_snapshot.txt`执行成功
- [ ] 识别所有requirements.txt并记录冲突
- [ ] 记录当前可通过的测试列表

**Links**: FR-G2 (消除目录混乱)

**Risk**: 低 | **Rollback**: 无需回滚 | **Frozen**: 否

---

### T-001 构建命令文档化

**Goal**: 记录并验证当前所有构建/运行命令

**Scope**:
- Worker启动命令
- Gateway/Coordinator启动命令
- Docker构建命令
- 测试执行命令

**Priority**: P0-Critical

**Test Cases**:
- [ ] Worker可以当前方式启动
- [ ] 至少一种引擎可以加载模型
- [ ] Docker镜像可以构建

**Suggested Owner**: Python/Shell Agent

**Deliverables**:
- `docs/refactor/cy-llm-engine/baseline/build_commands.md`

**Acceptance Checks**:
- [ ] 记录`python -m worker.main`启动方式
- [ ] 记录Docker构建命令
- [ ] 记录测试执行命令

**Links**: FR-G2

**Risk**: 低 | **Rollback**: N/A | **Frozen**: 否

---

### T-002 基线行为验证

**Goal**: 确保至少一个端到端测试可以通过

**Scope**:
- 选择最小的模型(如sshleifer/tiny-gpt2)
- 记录推理延迟基准
- 记录显存使用基准

**Priority**: P0-Critical

**Test Cases**:
- [ ] 模型可以加载
- [ ] 推理可以执行并返回结果
- [ ] gRPC接口可以响应

**Suggested Owner**: Python Agent

**Deliverables**:
- `docs/refactor/cy-llm-engine/baseline/e2e_baseline.json`

**Acceptance Checks**:
- [ ] 记录模型加载时间
- [ ] 记录首Token延迟(TTFT)
- [ ] 记录显存使用峰值

**Links**: FR-G5 (性能基准)

**Risk**: 中(需GPU) | **Rollback**: 使用CPU/mock | **Frozen**: 否

---

### T-010 单元测试增强

**Goal**: 提高核心模块单元测试覆盖率到70%+

**Scope**:
- abstract_engine测试
- engine_factory测试
- config_loader测试
- memory_manager测试
- task_scheduler测试

**Priority**: P1-High

**Test Cases**:
- [ ] test_abstract_engine.py 覆盖所有ABC方法
- [ ] test_engine_factory.py 覆盖注册/创建流程
- [ ] test_config_loader.py 覆盖配置加载

**Suggested Owner**: Python Agent

**Deliverables**:
- `src/cy_llm/worker/tests/` 新增/增强测试文件

**Acceptance Checks**:
- [ ] `pytest tests/ --cov=worker --cov-report=term` 显示coverage >= 70%
- [ ] 所有单元测试通过

**Links**: FR-G5, NFR-覆盖率

**Risk**: 低 | **Rollback**: 保留原测试 | **Frozen**: 否

---

### T-011 集成测试套件

**Goal**: 创建Gateway-Coordinator-Worker链的集成测试

**Scope**:
- Mock引擎测试完整链路
- 测试gRPC通信
- 测试HTTP API调用

**Priority**: P1-High

**Test Cases**:
- [ ] Gateway可以转发请求到Coordinator
- [ ] Coordinator可以分发到Worker
- [ ] Worker可以返回mock响应

**Suggested Owner**: Python Agent

**Deliverables**:
- `tests/integration/test_gateway_coordinator_worker.py`
- `tests/integration/test_grpc_api.py`

**Acceptance Checks**:
- [ ] 集成测试不依赖外部服务可运行
- [ ] 使用Mock引擎验证链路

**Links**: FR-G5, NFR-接口兼容

**Risk**: 中 | **Rollback**: 保留原tests/ | **Frozen**: 是(gRPC/HTTP)

---

### T-012 E2E测试框架

**Goal**: 创建可重复执行的E2E测试脚本

**Scope**:
- 自动化启动完整服务栈
- 执行端到端推理测试
- 验证响应格式符合OpenAI API规范

**Priority**: P1-High

**Test Cases**:
- [ ] 脚本可以启动gateway+coordinator+worker
- [ ] 可以发送/chat/completions请求
- [ ] 可以验证SSE流式响应

**Suggested Owner**: Python/Shell Agent

**Deliverables**:
- `tests/e2e/test_e2e_inference.py`
- `scripts/run_e2e_test.sh`

**Acceptance Checks**:
- [ ] E2E测试脚本可独立运行
- [ ] 使用tiny模型可在<5分钟完成

**Links**: FR-G5, NFR-可用性

**Risk**: 中(需服务栈) | **Rollback**: 保留原test_integration.py | **Frozen**: 是(OpenAI API)

---

### T-013 API兼容性测试

**Goal**: 验证重构前后API响应一致

**Scope**:
- HTTP API响应字段对比
- gRPC消息格式验证
- 错误码一致性

**Priority**: P1-High

**Test Cases**:
- [ ] 比较重构前后/chat/completions响应
- [ ] 验证错误响应格式
- [ ] 验证流式响应格式

**Suggested Owner**: Python Agent

**Deliverables**:
- `tests/api/test_api_compatibility.py`
- `tests/api/api_contract.json`

**Acceptance Checks**:
- [ ] API契约文档化
- [ ] 响应字段100%匹配

**Links**: FR-NG2 (不破坏外部接口)

**Risk**: 高(触及冻结接口) | **Rollback**: 必须保证兼容 | **Frozen**: 是

---

### T-020 目录重复分析

**Goal**: 分析 `src/cy_llm/` 和 `CY_LLM_Backend/` 的重复情况

**Scope**:
- 文件级diff对比
- 识别差异文件
- 标记活跃版本

**Priority**: P0-Critical

**Test Cases**:
- [ ] 生成完整的文件对比报告
- [ ] 识别哪些文件有差异
- [ ] 确定保留哪个版本

**Suggested Owner**: Python Agent

**Deliverables**:
- `docs/refactor/cy-llm-engine/phase1/diff_analysis.md`
- `docs/refactor/cy-llm-engine/phase1/file_mapping.csv`

**Acceptance Checks**:
- [ ] 列出所有重复文件
- [ ] 标记建议保留版本及理由

**Links**: FR-G2

**Risk**: 中 | **Rollback**: 基于git历史 | **Frozen**: 否

---

### T-021 迁移计划制定

**Goal**: 制定详细的文件迁移计划

**Scope**:
- 确定最终目录结构
- 规划import路径变更
- 制定回滚策略

**Priority**: P0-Critical

**Test Cases**:
- [ ] 最终目录结构图
- [ ] 迁移顺序清单
- [ ] 风险点标记

**Suggested Owner**: Python Agent

**Deliverables**:
- `docs/refactor/cy-llm-engine/phase1/migration_plan.md`
- `docs/refactor/cy-llm-engine/phase1/target_structure.md`

**Acceptance Checks**:
- [ ] 目录结构获批准
- [ ] 迁移顺序合理

**Links**: FR-G2

**Risk**: 中 | **Rollback**: git revert | **Frozen**: 否

---

### T-022 合并核心模块 (worker/core)

**Goal**: 合并核心模块文件

**Scope**:
- memory_manager.py
- task_scheduler.py
- telemetry.py
- server.py

**Priority**: P1-High

**Test Cases**:
- [ ] 合并后文件可以导入
- [ ] 单元测试通过
- [ ] 无功能丢失

**Suggested Owner**: Python Agent

**Deliverables**:
- `src/cy_llm/worker/core/` 更新文件

**Acceptance Checks**:
- [ ] `from worker.core.memory_manager import GPUMemoryManager` 工作正常
- [ ] 所有core测试通过

**Links**: FR-G2

**Risk**: 高(核心逻辑) | **Rollback**: git分支保留原代码 | **Frozen**: 否

---

### T-023 合并引擎模块 (worker/engines)

**Goal**: 合并引擎相关文件，保留最新版本

**Scope**:
- abstract_engine.py (统一接口)
- engine_factory.py (统一工厂)
- vllm_cuda_engine.py
- trt_engine.py
- mindie_engine.py
- ascend_engine.py
- nvidia_engine.py

**Priority**: P1-High

**Test Cases**:
- [ ] 所有引擎可以导入
- [ ] engine_factory可以列出所有引擎
- [ ] 无重复定义

**Suggested Owner**: Python Agent

**Deliverables**:
- `src/cy_llm/worker/engines/` 统一文件

**Acceptance Checks**:
- [ ] `list_available_engines()` 返回完整列表
- [ ] 无ImportError

**Links**: FR-G2, G3

**Risk**: 高 | **Rollback**: git revert | **Frozen**: 部分

---

### T-024 合并配置和工具

**Goal**: 统一配置管理和工具函数

**Scope**:
- config/ 目录合并
- utils/ 目录合并
- requirements.txt 合并规划

**Priority**: P1-High

**Test Cases**:
- [ ] 配置加载统一
- [ ] 工具函数无重复

**Suggested Owner**: Python Agent

**Deliverables**:
- `src/cy_llm/worker/config/` 统一
- `src/cy_llm/worker/utils/` 统一

**Acceptance Checks**:
- [ ] `load_worker_config()` 工作正常
- [ ] 无重复工具函数

**Links**: FR-G2

**Risk**: 中 | **Rollback**: git revert | **Frozen**: 否

---

### T-025 清理废弃目录

**Goal**: 删除合并后的CY_LLM_Backend目录

**Scope**:
- 删除CY_LLM_Backend/worker/（确认已迁移）
- 删除CY_LLM_Backend/deploy/（迁移到项目根deploy/）
- 更新所有import路径

**Priority**: P1-High

**Test Cases**:
- [ ] CY_LLM_Backend目录已删除
- [ ] 所有import指向新位置
- [ ] 测试可以通过

**Suggested Owner**: Python Agent

**Deliverables**:
- 删除确认清单

**Acceptance Checks**:
- [ ] 无`CY_LLM_Backend`残留
- [ ] `grep -r "CY_LLM_Backend" src/` 无结果

**Links**: FR-G2

**Risk**: 高(破坏性) | **Rollback**: git历史保留可恢复 | **Frozen**: 否

---

### T-030 Dependency Registry Schema设计

**Goal**: 设计并验证dependency_registry.json的JSON Schema

**Scope**:
- 设计硬件profile定义
- 设计引擎定义
- 设计兼容性矩阵
- 设计wheel URL映射

**Priority**: P0-Critical

**Test Cases**:
- [ ] Schema可以通过JSON Schema验证
- [ ] 包含所有硬件类型
- [ ] 包含所有引擎类型

**Suggested Owner**: Python/JSON Agent

**Deliverables**:
- `deploy/dependency_registry.json` (模板)
- `src/cy_llm/deps/schemas/registry_schema.json`

**Acceptance Checks**:
- [ ] Schema定义完整
- [ ] 示例数据可以通过验证

**Links**: FR-G1, G4 | API-DEPS-1

**Risk**: 中 | **Rollback**: 版本控制 | **Frozen**: 否

**Schema结构**:
```json
{
  "version": "1.0.0",
  "hardware_profiles": {...},
  "engines": {...},
  "compatibility_matrix": [...],
  "wheels": {...},
  "mirrors": {...}
}
```

---

### T-031 硬件检测模块 (Hardware Detector)

**Goal**: 实现自动硬件检测

**Scope**:
- NVIDIA GPU检测（CUDA版本，计算能力）
- Ascend NPU检测（CANN版本）
- CPU-only检测
- 驱动版本检测

**Priority**: P0-Critical

**Test Cases**:
- [ ] 可以检测NVIDIA GPU
- [ ] 可以检测Ascend NPU
- [ ] 可以检测CPU-only环境

**Suggested Owner**: Python Agent

**Deliverables**:
- `src/cy_llm/deps/hardware_detector.py`
- `tests/deps/test_hardware_detector.py`

**Acceptance Checks**:
- [ ] `detect_hardware()` 返回HardwareProfile
- [ ] 准确率>95%
- [ ] 支持mock模式测试

**Links**: FR-G1, G3 | API: detect_hardware()

**Risk**: 中(需多硬件) | **Rollback**: 保留原检测逻辑 | **Frozen**: 否

---

### T-032 依赖解析模块 (Dependency Resolver)

**Goal**: 实现依赖兼容性解析

**Scope**:
- 解析Registry JSON
- 根据硬件+引擎查找兼容依赖
- 解决版本冲突（如protobuf）
- 生成requirements.lock

**Priority**: P0-Critical

**Test Cases**:
- [ ] 给定NVIDIA+vLLM返回正确依赖列表
- [ ] 给定Ascend+MindIE返回正确依赖列表
- [ ] 正确处理protobuf版本冲突

**Suggested Owner**: Python Agent

**Deliverables**:
- `src/cy_llm/deps/resolver.py`
- `tests/deps/test_resolver.py`

**Acceptance Checks**:
- [ ] `resolve_dependencies(profile, engine_type)` 工作正常
- [ ] 支持vllm-0.12.0的protobuf==6.33.4
- [ ] 支持基础环境的protobuf<6.0.0

**Links**: FR-G1, G4 | API: resolve_dependencies()

**Risk**: 高(解决依赖地狱) | **Rollback**: 保留原requirements | **Frozen**: 否

---

### T-033 CLI框架搭建

**Goal**: 创建cy-llm CLI框架

**Scope**:
- 使用click/argparse创建CLI入口
- 子命令注册机制
- 帮助文档生成

**Priority**: P1-High

**Test Cases**:
- [ ] `python -m cy_llm --help` 工作
- [ ] 子命令可以注册

**Suggested Owner**: Python Agent

**Deliverables**:
- `src/cy_llm/cli/__init__.py`
- `src/cy_llm/cli/main.py`

**Acceptance Checks**:
- [ ] CLI入口可用
- [ ] 帮助信息完整

**Links**: FR-G1

**Risk**: 低 | **Rollback**: 删除cli目录 | **Frozen**: 否

---

### T-034 Setup命令实现

**Goal**: 实现 `cy-llm setup` 命令

**Scope**:
- 交互式硬件检测
- 引擎选择提示
- 依赖生成与安装
- 支持--auto/--dry-run/--engine等参数

**Priority**: P0-Critical

**Test Cases**:
- [ ] `cy-llm setup --auto` 自动检测并安装
- [ ] `cy-llm setup --dry-run` 仅生成requirements
- [ ] `cy-llm setup --engine vllm` 强制选择引擎

**Suggested Owner**: Python Agent

**Deliverables**:
- `src/cy_llm/cli/commands/setup.py`
- `tests/cli/test_setup.py`

**Acceptance Checks**:
- [ ] 命令可用且稳定
- [ ] 生成的requirements.lock可安装

**Links**: FR-G1, G3 | API-CLI-1

**Risk**: 中 | **Rollback**: 保留pip list输出 | **Frozen**: 否

---

### T-035 Verify命令实现

**Goal**: 实现 `cy-llm verify` 环境验证命令

**Scope**:
- 检查Python版本
- 检查硬件可用性
- 检查引擎安装状态
- 输出诊断报告

**Priority**: P1-High

**Test Cases**:
- [ ] `cy-llm verify` 输出完整报告
- [ ] 检测缺失的依赖
- [ ] 检测硬件驱动问题

**Suggested Owner**: Python Agent

**Deliverables**:
- `src/cy_llm/cli/commands/verify.py`
- `tests/cli/test_verify.py`

**Acceptance Checks**:
- [ ] 可以检测所有引擎可用性
- [ ] 输出JSON格式支持`--json`

**Links**: FR-G1

**Risk**: 低 | **Rollback**: N/A | **Frozen**: 否

---

### T-036 虚拟环境生成

**Goal**: 支持生成隔离的虚拟环境

**Scope**:
- 创建venv/conda环境
- 安装解析后的依赖
- 引擎隔离（多个引擎各自独立环境）

**Priority**: P2-Medium

**Test Cases**:
- [ ] 可以为vLLM创建独立venv
- [ ] 可以为TRT创建独立venv
- [ ] 环境可以激活并使用

**Suggested Owner**: Python Agent

**Deliverables**:
- `src/cy_llm/deps/env_manager.py`
- `tests/deps/test_env_manager.py`

**Acceptance Checks**:
- [ ] 环境创建成功
- [ ] pip list显示正确依赖

**Links**: FR-G1, G3

**Risk**: 中 | **Rollback**: 删除venv目录 | **Frozen**: 否

---

### T-037 镜像源支持

**Goal**: 支持国内镜像源和离线安装

**Scope**:
- 清华/阿里PyPI镜像
- HuggingFace镜像
- 预下载wheel支持

**Priority**: P1-High

**Test Cases**:
- [ ] 可以使用清华镜像安装
- [ ] 可以指定本地wheel目录
- [ ] 镜像选择可配置

**Suggested Owner**: Python Agent

**Deliverables**:
- `src/cy_llm/deps/mirror_config.py`
- 镜像配置文件

**Acceptance Checks**:
- [ ] 国内环境安装成功率>95%
- [ ] 离线安装模式可用

**Links**: FR-G4 | NFR-新用户<15分钟运行

**Risk**: 中(网络依赖) | **Rollback**: 使用官方源 | **Frozen**: 否

---

### T-040 BaseEngine抽象类重构

**Goal**: 设计并完善BaseEngine ABC

**Scope**:
- 统一load_model接口
- 统一stream_predict接口
- 统一内存监控接口
- 支持上下文管理器

**Priority**: P0-Critical

**Test Cases**:
- [ ] BaseEngine不能实例化
- [ ] 子类必须实现所有抽象方法
- [ ] 上下文管理器可用

**Suggested Owner**: Python Agent

**Deliverables**:
- `src/cy_llm/worker/engines/base_engine.py` (重命名自abstract_engine)
- `tests/engines/test_base_engine.py`

**Acceptance Checks**:
- [ ] `issubclass(VllmCudaEngine, BaseEngine)` 为True
- [ ] 所有引擎可以实例化

**Links**: FR-G3 | API-ENGINE-1

**Risk**: 高(核心接口) | **Rollback**: 保留原abstract_engine | **Frozen**: 是(BaseEngine)

---

### T-041 vLLM引擎适配

**Goal**: 重构vLLM引擎继承BaseEngine

**Scope**:
- VllmCudaEngine适配
- VllmAsyncEngine适配
- VllmAscendEngine适配

**Priority**: P0-Critical

**Test Cases**:
- [ ] 可以加载模型
- [ ] 可以流式推理
- [ ] 内存监控准确

**Suggested Owner**: Python Agent

**Deliverables**:
- `src/cy_llm/worker/engines/vllm_cuda_engine.py` (重构)
- `src/cy_llm/worker/engines/vllm_async_engine.py` (重构)
- `src/cy_llm/worker/engines/vllm_ascend_engine.py` (重构)

**Acceptance Checks**:
- [ ] 继承BaseEngine
- [ ] 通过引擎工厂创建
- [ ] 性能无退化

**Links**: FR-G3

**Risk**: 高(核心引擎) | **Rollback**: 保留原实现分支 | **Frozen**: 否

---

### T-042 TensorRT引擎适配

**Goal**: 重构TensorRT引擎继承BaseEngine

**Scope**:
- TensorRTEngine适配

**Priority**: P1-High

**Test Cases**:
- [ ] 可以加载TRT引擎
- [ ] 可以推理

**Suggested Owner**: Python Agent

**Deliverables**:
- `src/cy_llm/worker/engines/trt_engine.py` (重构)

**Acceptance Checks**:
- [ ] 继承BaseEngine
- [ ] 通过引擎工厂创建

**Links**: FR-G3

**Risk**: 中(需TRT环境) | **Rollback**: 保留原实现 | **Frozen**: 否

---

### T-043 MindIE引擎适配

**Goal**: 重构MindIE引擎继承BaseEngine

**Scope**:
- MindIEEngine适配
- AscendEngine适配

**Priority**: P1-High

**Test Cases**:
- [ ] 可以加载MindIE引擎
- [ ] 可以推理

**Suggested Owner**: Python Agent

**Deliverables**:
- `src/cy_llm/worker/engines/mindie_engine.py` (重构)
- `src/cy_llm/worker/engines/ascend_engine.py` (重构)

**Acceptance Checks**:
- [ ] 继承BaseEngine
- [ ] 通过引擎工厂创建

**Links**: FR-G3

**Risk**: 高(无本地环境) | **Rollback**: 保留原实现 | **Frozen**: 否
**Note**: 需要华为Ascend环境CI测试

---

### T-044 引擎工厂重构

**Goal**: 统一EngineFactory接口

**Scope**:
- 基于BaseEngine创建引擎
- 延迟导入优化
- 引擎注册表统一

**Priority**: P0-Critical

**Test Cases**:
- [ ] 可以创建所有引擎类型
- [ ] 延迟导入正确
- [ ] 自动检测推荐引擎

**Suggested Owner**: Python Agent

**Deliverables**:
- `src/cy_llm/worker/engines/engine_factory.py` (重构)

**Acceptance Checks**:
- [ ] `EngineFactory.create("cuda-vllm")` 返回BaseEngine
- [ ] `EngineFactory.auto_detect()` 工作正常

**Links**: FR-G3 | API-ENGINE-2

**Risk**: 高(核心组件) | **Rollback**: 保留旧工厂别名 | **Frozen**: 部分(create接口)

---

### T-045 Memory Manager集成

**Goal**: 引擎与Memory Manager集成

**Scope**:
- 引擎加载时注册到Memory Manager
- 引擎卸载时通知Memory Manager
- 统一显存监控

**Priority**: P1-High

**Test Cases**:
- [ ] 引擎加载时注册成功
- [ ] LRU淘汰时正确卸载引擎
- [ ] 显存报告准确

**Suggested Owner**: Python Agent

**Deliverables**:
- `src/cy_llm/worker/engines/base_engine.py` (更新)
- `src/cy_llm/worker/core/memory_manager.py` (更新)

**Acceptance Checks**:
- [ ] 引擎生命周期与内存管理同步
- [ ] OOM时正确触发GC

**Links**: FR-G3, G5

**Risk**: 中 | **Rollback**: 保留原集成逻辑 | **Frozen**: 否

---

### T-046 Telemetry集成

**Goal**: 引擎与Telemetry系统集成

**Scope**:
- 推理延迟上报
- Token生成速率上报
- 错误率上报

**Priority**: P2-Medium

**Test Cases**:
- [ ] 推理延迟被记录
- [ ] Token速率计算正确
- [ ] 错误被正确分类上报

**Suggested Owner**: Python Agent

**Deliverables**:
- `src/cy_llm/worker/engines/base_engine.py` (telemetry hooks)
- `tests/engines/test_telemetry.py`

**Acceptance Checks**:
- [ ] 指标可以被Prometheus抓取
- [ ] 日志格式统一

**Links**: FR-G5

**Risk**: 低 | **Rollback**: 保留原telemetry | **Frozen**: 否

---

## Test Plan Pointers

### 单元测试新增
- `tests/deps/test_hardware_detector.py` - 硬件检测(支持mock)
- `tests/deps/test_resolver.py` - 依赖解析
- `tests/deps/test_env_manager.py` - 环境管理
- `tests/cli/test_setup.py` - setup命令
- `tests/cli/test_verify.py` - verify命令
- `tests/engines/test_base_engine.py` - BaseEngine ABC
- `tests/integration/test_gateway_coordinator_worker.py` - 链路测试

### 集成测试场景
1. **依赖安装流程**: setup -> verify -> 运行
2. **引擎切换**: 同一环境切换vLLM/TRT
3. **多硬件检测**: NVIDIA/Ascend/CPU自动识别
4. **API兼容性**: 重构前后响应对比

### 所需Fixtures/Mock
- MockHardwareDetector - 模拟各种硬件配置
- MockEngine - 用于链路测试的虚拟引擎
- TinyGPT2 - 用于E2E测试的小模型

---

## Completion Checklist

- [x] Task granularity actionable and verifiable
- [x] Dependencies clear (text format)
- [x] Each task contains acceptance points and links
- [x] User has explicitly replied (via question)
- [x] Written `docs/REFACTOR/CY-LLM-Engine/tasks.md`

---

## Task Dependency Table

| Task | Depends On | Blocks | Can Parallel With |
|------|------------|--------|-------------------|
| T-000 | - | T-001,T-002 | T-001 |
| T-001 | T-000 | T-002 | T-000 |
| T-002 | T-000,T-001 | T-010+ | - |
| T-010+ | T-002 | T-020 | T-020 |
| T-020 | T-010 | T-021 | - |
| T-021 | T-020 | T-022+ | - |
| T-022 | T-021 | T-023 | - |
| T-023 | T-022 | T-024 | - |
| T-024 | T-023 | T-025 | - |
| T-025 | T-024 | T-030,T-040 | - |
| T-030 | T-025 | T-031,T-032 | T-040 |
| T-031 | T-030 | T-034 | T-032,T-033 |
| T-032 | T-030 | T-034 | T-031,T-033 |
| T-033 | T-025 | T-034,T-035 | T-030 |
| T-034 | T-031,T-032,T-033 | T-036 | T-035 |
| T-035 | T-033 | T-037 | T-034,T-036 |
| T-040 | T-025 | T-041,T-042,T-043 | T-030 |
| T-041 | T-040 | T-044 | T-042,T-043 |
| T-042 | T-040 | T-044 | T-041,T-043 |
| T-043 | T-040 | T-044 | T-041,T-042 |
| T-044 | T-041,T-042,T-043 | T-045 | - |
| T-045 | T-044 | T-046 | - |
| T-046 | T-045 | - | - |

---

**文档版本**: 1.0  
**创建日期**: 2026-02-10  
**最后更新**: 2026-02-10  
**状态**: 已批准执行任务分解
