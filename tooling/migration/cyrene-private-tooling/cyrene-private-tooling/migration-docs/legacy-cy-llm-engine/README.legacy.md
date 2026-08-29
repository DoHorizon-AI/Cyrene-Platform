# CY-LLM Engine

> 🚀 **Runtime Control Plane** · **Docker + uv** · **可解释兼容规划** 的现代 AI 运行控制面系统
> 
> `CY-LLM-Engine` 为**唯一主仓库**。系统致力于：自动识别目标硬件与模型需求 → 解析并构建 Docker+uv 运行环境 → 给出可解释的兼容性/安全/性能证据 → 驱动稳健的推理与训练。Pro 企业级能力已归档集中于 [plugins/pro](plugins/pro/MIGRATION_INVENTORY.md)。

---

## 📊 能力真实状态矩阵 (Capability Matrix)

> ⚠️ **只信代码与真实测试**：为防止文档与代码实现漂移，以下为当前代码库的真实验证状态划分。

| 状态等级 | 涵盖模块 / 功能 | 当前说明与验证方式 |
|---|---|---|
| **✅ 已验证 (Verified)** | Cargo workspace (9个测试)、Python CPU 测试 (262 passed)、Pro 仓迁入、跨语言 canonical hash | `cargo test --workspace --locked` 通行；`uv run --frozen pytest` CPU 测试通行；Rust/Python Manifest hash 对拍通行；Pro 已合入 `plugins/pro` |
| **💻 仅 CPU 验证 (CPU-only)** | `cy_gateway_lite` FastAPI 服务、Worker 基础 lifecycle & servicer | 能够在无 GPU 环境完成模块导入与 gRPC servicer 服务启动 |
| **🦴 仅代码存在/骨架 (Skeleton)** | `cy-node-agent` CLI 桩、`cy-control-plane` 边界 | 存在模块定义与 CLI 桩代码，功能有待 M2 充实 |
| **🚧 尚未实现 (Not Implemented)** | 完整 strict validation、硬件真实探测、`CompatibilityResolver` 规划器、BuildKit Builder | 属于 M1~M4 阶段核心任务 |
| **⏳ 等待 GPU 验证 (Pending GPU)** | NVIDIA CUDA vLLM 真实推理 e2e、LLaMA-Factory 微调训练 e2e | 基础代码存在，待 Linux NVIDIA GPU 硬件就位后进行端到端验证 |

---

## ⚡ 架构决策与过渡兼容说明 (Transition Notice)

1. **唯一主仓**：`CY-LLM-Engine` 为唯一开发仓；`CY-LLM-Engine-Pro` 仓库已冻结为历史只读备份。
2. **默认 Runtime**：`Docker + uv` 为系统默认构建与交付方式。
3. **过渡路径 (Legacy Transition)**：现有 `requirements-*.txt` 及 Conda 相关 CLI 仅作过渡兼容保留，后续将全面迁移至基于 uv 的按需 Runtime 生成。
4. **技术栈聚焦**：第一版核心聚焦于 Linux NVIDIA CUDA + vLLM 推理 + LLaMA-Factory 训练；Ascend/MindIE/TRT 等推迟至后续阶段插件化扩展。

---

## 📚 文档导航

> 本仓库文档遵循**单一事实来源（SSOT）**：每个事实只在一份权威文档中定义，其它文档交叉引用它，不重复描述。旧设计文档保留作历史参考，但已在文件顶部加注「过时」横幅，请勿据此判断现状。

### 权威文档（以此为准）

| 文档 | SSOT 职责（唯一权威） |
|------|------|
| [README.md](./README.md) | 导航中枢 + 能力真实状态矩阵 + 关键决策速览 |
| [REBUILD_PLAN.md](./REBUILD_PLAN.md) | **唯一权威执行序列**：里程碑 M0–M8 / Epic E0–E8、Ticket、实施顺序与验收门 |
| [docs/PRODUCT_REQUIREMENTS.md](./docs/PRODUCT_REQUIREMENTS.md) | 产品需求与用户交互流程（WHAT / WHY） |
| [docs/TARGET_ARCHITECTURE.md](./docs/TARGET_ARCHITECTURE.md) | 目标架构：组件 / 边界 / 插件与语言模型 / 关键架构决策（HOW） |
| [docs/A0_KNOWN_ISSUES.md](./docs/A0_KNOWN_ISSUES.md) | 延期缺陷追踪清单（含迁入的 Pro 缺陷） |
| [plugins/pro/MIGRATION_INVENTORY.md](./plugins/pro/MIGRATION_INVENTORY.md) | Pro 企业版归档与迁移清单 |

### 旧文档（请勿据此判断现状）

以下文档描述的是已废弃的旧设计，仅作历史参考，均已在文件顶部加注**过时横幅**：

| 旧文档 | 状态 |
|------|------|
| [docs/ARCHITECTURE.md](./docs/ARCHITECTURE.md) | 已被 `docs/TARGET_ARCHITECTURE.md` 取代 |
| [docs/README.md](./docs/README.md) | 已被本 README 取代 |
| [docs/API.md](./docs/API.md) | 旧 API 说明，已过时 |
| [docs/INSTALL.md](./docs/INSTALL.md) | 旧安装说明，已过时 |
| [docs/TRT_GUIDE.md](./docs/TRT_GUIDE.md) | 旧 TensorRT 指南，已过时 |
| [docs/HISTORY/](./docs/HISTORY/) | 历史迁移 / 升级报告存档 |
| [docs/REFACTOR/](./docs/REFACTOR/) | 历史重构过程存档 |

以下文档**部分内容可能过时**，现状以上方权威文档为准：

| 文档 | 说明 |
|------|------|
| [QUICK_START.md](./QUICK_START.md) | 快速开始速查（部分基于旧设计） |
| [docs/FAQ.md](./docs/FAQ.md) | 常见问题（部分基于旧设计） |
| [docs/CONTRIBUTING.md](./docs/CONTRIBUTING.md) | 贡献指南（部分基于旧设计） |
| [docs/TESTING.md](./docs/TESTING.md) | 测试说明（部分基于旧设计） |

### 关键决策速览

- **平台核心语言 = Rust**：`cy-control-plane` 作为插件宿主 + 编排 + 契约 + 状态机 + Controller↔Agent gRPC。
- **Worker / 执行层 = Python**：进程外插件（vLLM / LLaMA-Factory / model-analyzer / 量化），Rust 不重写 AI 执行生态。
- **数据面网关 = Rust**：`cy-proxy`→`cy-gateway`，推理热路径唯一吃语言性能之处。
- **插件 = 语言中立的协议契约**：Rust 可进程内一起编译（Cargo feature + tower Layer）；Python / Java 走进程外 gRPC + `plugin.toml` 发现。
- **专业版 / 普通版 = 同一核心 + 不同插件组合**：`plugins/pro` = license 门控，community 构建不编译 pro 符号。

---

## 🚀 快速开始 (Community Lite 过渡路径)

### 本地启动 (Python Lite Gateway + Worker)

```bash
# 1. 简单体验安装 (过渡兼容 Conda / pip)
./cy-llm setup --engine cuda-vllm

# 2. 一键启动 (Lite Gateway + Lite Coordinator + Worker)
./cy-llm lite --engine cuda-vllm --model qwen2.5-7b

# 3. 测试 (OpenAI 兼容)
curl http://localhost:8000/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{"model":"qwen2.5-7b","messages":[{"role":"user","content":"你好"}]}'
```

> Lite 版本默认端口为 8000（Gateway），Coordinator 为 50051，Worker 为 50052。

### Docker Compose 启动

```bash
# 启动
docker compose -f deploy/compose/community.yml up -d

# 查看状态
docker compose -f deploy/compose/community.yml ps

# 停止
docker compose -f deploy/compose/community.yml down
```

> Community 镜像是 Python 3.12 开发基线；GPU runtime 留给 M4，本配置不表示 GPU E2E 已验证。

---

## 📁 项目结构

```
CY-LLM-Engine/
├── Cargo.toml                  # Rust Workspace 根
├── crates/                     # ── Rust ──
│   ├── cy-proto/               # 由 proto/ai_service.proto 生成（唯一契约源）
│   ├── cy-manifest/            # Manifest 类型 + canonical hash（共享）
│   ├── cy-proxy/               # 数据面代理
│   ├── cy-control-plane/       # 控制面 API：分析/解析/构建/验证/训练控制
│   └── cy-node-agent/          # 硬件探测静态二进制
├── python/                     # ── Python ──
│   ├── cy_exec/                # 执行层（原 worker 重塑）
│   ├── cy_gateway_lite/        # 简版网关 FastAPI
│   └── cy_control_plane/       # 控制面 Python Workspace
├── plugins/
│   └── pro/                    # 【专业版归档】已迁入 Enterprise 模块
├── proto/ai_service.proto      # 唯一契约源
├── deploy/                     # Docker Compose、模型与运行配置
├── docs/                       # 架构与历史文档
├── REBUILD_PLAN.md             # 权威重构执行方案
└── requirements*.txt           # (Legacy) 过渡依赖清单
```

---

## 🧪 基线测试验证

```bash
# 运行 Rust Workspace 单元测试
cargo test --workspace --locked

# 运行 Python CPU 单元测试
uv run --frozen pytest -q -p no:cacheprovider
```

---

## 📄 许可证与贡献

- 本项目采用 MIT 许可证，详见 [LICENSE](./LICENSE) 文件。
- 详细贡献指南请参考 [docs/CONTRIBUTING.md](./docs/CONTRIBUTING.md)。
