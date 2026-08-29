> ⚠️ **旧设计文档（已过时）**：本文件描述已废弃的架构，可能与当前实现不符。当前权威文档见 [README 文档导航](../../../README.md#-文档导航)。请勿据此判断现状。

# CY-LLM Engine Refactor - Interface Contract

## 外部接口清单

### API接口（冻结 - 不允许修改）

#### HTTP REST API (OpenAI兼容)
- `POST /v1/chat/completions` - 聊天补全（非流式）
- `POST /v1/completions` - 文本补全
- `GET  /v1/models` - 模型列表
- `GET  /health` - 健康检查

请求/响应格式严格遵循OpenAI API规范，不得修改字段名或类型。

#### gRPC接口（冻结）
- `InferenceService/Generate` - 生成请求
- `InferenceService/StreamGenerate` - 流式生成
- `CoordinatorService/RegisterWorker` - Worker注册
- `CoordinatorService/Heartbeat` - 心跳检测

proto文件位置: `proto/inference.proto`

### 环境变量接口（冻结）

#### Gateway
- `COORDINATOR_GRPC_ADDR` - Coordinator地址
- `GATEWAY_API_TOKEN` - 鉴权Token
- `GATEWAY_REQUEST_TIMEOUT` - 超时时间

#### Coordinator
- `COORDINATOR_GRPC_BIND` - 绑定地址
- `WORKER_GRPC_ADDRS` - Worker列表

#### Worker
- `CY_LLM_ENGINE` - 引擎类型
- `CY_LLM_DEFAULT_MODEL` - 默认模型
- `VLLM_GPU_MEMORY_UTILIZATION` - 显存利用率

### 配置文件接口（允许扩展）

#### 模型注册表（允许新增字段）
文件: `deploy/models.json`
```json
{
  "model_id": {
    "model_path": "str",
    "engine": "str",
    "gpu_memory_utilization": "float",
    "max_model_len": "int",
    "quantization": "str|null",
    "//新增": "可添加新字段但不能删除旧字段"
  }
}
```

#### 依赖矩阵（新增配置）
文件: `deploy/dependency_matrix.json`（新增）
```json
{
  "hardware_profiles": {...},
  "engine_profiles": {...},
  "compatibility_matrix": {...}
}
```

### CLI接口（允许扩展）

#### 现有命令（冻结行为）
- `./cy-llm setup` - 环境初始化
- `./cy-llm lite` - 启动轻量版
- `./cy-llm worker` - 启动Worker
- `./cy-llm stop` - 停止服务

#### 允许新增命令
- `./cy-llm detect` - 硬件检测（新增）
- `./cy-llm deps` - 依赖管理（新增）

## 接口变更审批流程

### 冻结接口变更
如需修改冻结接口，必须经过：
1. 在ChangeLog中记录变更提案
2. 用户明确批准
3. 提供向后兼容的迁移方案
4. 更新API版本号

### 允许变更接口
新增字段/命令可直接实现，但需：
1. 保持向后兼容
2. 更新文档
3. 添加测试用例

## 版本兼容性策略

### 当前版本: 1.5.2.0
重构期间保持次版本号递增（1.5.x.x），重大接口变更需升级到1.6.x.x。

### 兼容性承诺
- 重构后所有现有脚本无需修改即可运行
- 环境变量配置完全向后兼容
- API响应格式保持不变
