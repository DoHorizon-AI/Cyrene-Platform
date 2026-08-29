> ⚠️ **旧设计文档（已过时）**：本文件描述已废弃的架构，可能与当前实现不符。当前权威文档见 [README 文档导航](../README.md#-文档导航)。请勿据此判断现状。

# API 接口文档

本文档描述 CY-LLM Engine 的所有 API 接口，包括 REST API 和 gRPC 接口。

## 目录

- [REST API](#rest-api)
  - [推理接口](#推理接口)
  - [训练接口](#训练接口)
  - [模型管理](#模型管理)
  - [健康检查](#健康检查)
- [gRPC API](#grpc-api)
  - [推理服务](#推理服务-1)
  - [训练服务](#训练服务-1)
- [认证](#认证)
- [错误码](#错误码)

## REST API

### 基础信息

| 属性 | 值 |
|------|-----|
| 基础路径 | `/api/v1` |
| 默认端口 | 8080 |
| Content-Type | `application/json` |

### 认证方式

所有 API 需要通过 `X-API-Key` Header 进行认证：

```bash
curl -H "X-API-Key: YOUR_API_KEY" \
  http://localhost:8080/api/v1/health
```

---

## Lite API (OpenAI 兼容)

> Lite 版本使用 OpenAI 兼容接口，默认端口为 8000。

### 基础信息

| 属性 | 值 |
|------|-----|
| 基础路径 | `/v1` |
| 默认端口 | 8000 |
| Content-Type | `application/json` |

### 认证方式

若配置了 `GATEWAY_API_TOKEN`，需通过 `Authorization: Bearer <token>` 认证。

### 1. Chat Completions (非流式)

**POST** `/v1/chat/completions`

#### 请求参数

| 字段 | 类型 | 必填 | 描述 |
|------|------|------|------|
| `model` | string | 否 | 逻辑模型 ID，默认 `default` |
| `messages` | array | 是 | OpenAI messages 列表 |
| `max_tokens` | int | 否 | 最大生成 token 数（默认 256） |
| `temperature` | float | 否 | 温度（默认 0.7） |
| `top_p` | float | 否 | Top-p 采样（默认 0.9） |
| `repetition_penalty` | float | 否 | 重复惩罚（默认 1.0） |

#### 请求示例

```bash
curl -X POST http://localhost:8000/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{"model":"default","messages":[{"role":"user","content":"你好"}]}'
```

#### 响应示例

```json
{
  "id": "chatcmpl-6d3a1f7d4e024ccbb7fbbdc9b4d3f0c9",
  "object": "chat.completion",
  "created": 1704067200,
  "model": "default",
  "choices": [
    {
      "index": 0,
      "message": {
        "role": "assistant",
        "content": "你好！有什么可以帮你？"
      },
      "finish_reason": "stop"
    }
  ]
}
```

### 2. 健康检查

**GET** `/health`

#### 响应示例

```json
{"status":"ok"}
```

## 推理接口

### 1. 非流式推理

**POST** `/api/v1/inference`

#### 请求参数

| 字段 | 类型 | 必填 | 描述 |
|------|------|------|------|
| `modelId` | string | 是 | 模型 ID (对应 config.json 中的键) |
| `prompt` | string | 是 | 输入提示词 |
| `adapter` | string | 否 | LoRA 适配器路径 |
| `generation` | object | 否 | 生成参数 |

#### Generation 参数

| 字段 | 类型 | 默认值 | 描述 |
|------|------|--------|------|
| `maxNewTokens` | int | 256 | 最大生成 token 数 |
| `temperature` | float | 0.7 | 温度 (0.0-2.0) |
| `topP` | float | 0.95 | Top-p 采样 |
| `repetitionPenalty` | float | 1.0 | 重复惩罚 |

#### 请求示例

```bash
curl -X POST http://localhost:8080/api/v1/inference \
  -H "Content-Type: application/json" \
  -H "X-API-Key: YOUR_API_KEY" \
  -d '{
    "modelId": "qwen2.5-7b",
    "prompt": "你好，请介绍一下自己",
    "generation": {
      "maxNewTokens": 512,
      "temperature": 0.7,
      "topP": 0.95
    }
  }'
```

#### 响应示例

```json
{
  "id": "gen-abc123",
  "modelId": "qwen2.5-7b",
  "object": "text_completion",
  "created": 1704067200,
  "choices": [
    {
      "index": 0,
      "text": "你好！我是 Qwen2.5，一个由阿里云通义千问团队开发的大语言模型。",
      "finish_reason": "stop"
    }
  ],
  "usage": {
    "prompt_tokens": 15,
    "completion_tokens": 28,
    "total_tokens": 43
  }
}
```

---

### 2. 流式推理 (SSE)

**POST** `/api/v1/inference/stream`

#### 请求参数

同非流式推理。

#### 请求示例

```bash
curl -N -X POST http://localhost:8080/api/v1/inference/stream \
  -H "Content-Type: application/json" \
  -H "X-API-Key: YOUR_API_KEY" \
  -d '{
    "modelId": "qwen2.5-7b",
    "prompt": "讲一个关于人工智能的短故事"
  }'
```

#### 响应示例 (SSE 格式)

```
data: {"id":"gen-xyz789","object":"text_completion.chunk","choices":[{"index":0,"text":"从","offset":0}]}

data: {"id":"gen-xyz789","object":"text_completion.chunk","choices":[{"index":0,"text":"前","offset":1}]}

data: {"id":"gen-xyz789","object":"text_completion.chunk","choices":[{"index":0,"text":"有","offset":2}]}

...

data: {"id":"gen-xyz789","object":"text_completion.chunk","choices":[{"index":0,"text":"。","finish_reason":"stop","offset":156}]}

data: [DONE]
```

---

### 3. 带元数据的流式推理

**POST** `/api/v1/inference/streamWithMetadata`

#### 请求参数

| 字段 | 类型 | 必填 | 描述 |
|------|------|------|------|
| `modelId` | string | 是 | 模型 ID |
| `prompt` | string | 是 | 输入提示词 |
| `metadata` | object | 否 | 元数据 (trace_id, tenant, player_id, locale) |
| `generation` | object | 否 | 生成参数 |

#### 元数据字段

| 字段 | 类型 | 描述 |
|------|------|------|
| `traceId` | string | 追踪 ID (用于分布式追踪) |
| `tenant` | string | 租户 ID (多租户场景) |
| `playerId` | string | 玩家 ID (游戏场景) |
| `locale` | string | 语言区域 |

#### 请求示例

```bash
curl -N -X POST http://localhost:8080/api/v1/inference/streamWithMetadata \
  -H "Content-Type: application/json" \
  -H "X-API-Key: YOUR_API_KEY" \
  -d '{
    "modelId": "qwen2.5-7b",
    "prompt": "请用中文回答",
    "metadata": {
      "traceId": "trace-001",
      "tenant": "tenant-123",
      "playerId": "player-456",
      "locale": "zh-CN"
    }
  }'
```

---

## 训练接口

### 1. 启动训练任务

**POST** `/api/v1/training/start`

#### 请求参数

| 字段 | 类型 | 必填 | 描述 |
|------|------|------|------|
| `baseModel` | string | 是 | 基础模型路径或 ID |
| `outputDir` | string | 是 | 输出目录 (检查点保存路径) |
| `datasetPath` | string | 是 | 训练数据集路径 |
| `hyperParams` | object | 否 | 超参数 |
| `loraConfig` | object | 否 | LoRA 配置 |

#### HyperParams 参数

| 字段 | 类型 | 默认值 | 描述 |
|------|------|--------|------|
| `batchSize` | int | 4 | 批大小 |
| `learningRate` | float | 2e-4 | 学习率 |
| `epochs` | int | 3 | 训练轮数 |
| `maxSeqLength` | int | 2048 | 最大序列长度 |
| `loraRank` | int | 64 | LoRA 秩 |
| `loraAlpha` | int | 16 | LoRA alpha |
| `loraDropout` | float | 0.05 | LoRA dropout |
| `saveSteps` | int | 100 | 检查点保存间隔 |
| `evalSteps` | int | 100 | 评估间隔 |

#### LoRaConfig 参数

| 字段 | 类型 | 描述 |
|------|------|------|
| `r` | int | LoRA 秩 |
| `loraAlpha` | int | LoRA alpha |
| `loraDropout` | float | LoRA dropout |
| `targetModules` | string[] | 目标模块列表 |
| `bias` | string | bias 类型 ('none', 'all', 'lora_only') |

#### 请求示例

```bash
curl -X POST http://localhost:8080/api/v1/training/start \
  -H "Content-Type: application/json" \
  -H "X-API-Key: YOUR_API_KEY" \
  -d '{
    "baseModel": "Qwen/Qwen2.5-7B-Instruct",
    "outputDir": "/checkpoints/my_lora",
    "datasetPath": "/data/train.json",
    "hyperParams": {
      "batchSize": 4,
      "learningRate": 2e-4,
      "epochs": 3,
      "maxSeqLength": 2048,
      "loraRank": 64,
      "loraAlpha": 16,
      "saveSteps": 100,
      "evalSteps": 100
    },
    "loraConfig": {
      "r": 64,
      "loraAlpha": 16,
      "loraDropout": 0.05,
      "targetModules": ["q_proj", "v_proj"],
      "bias": "none"
    }
  }'
```

#### 响应示例

```json
{
  "jobId": "job-abc123",
  "status": "queued",
  "message": "Training job has been queued",
  "createdAt": "2025-01-21T10:00:00Z"
}
```

---

### 2. 查询训练状态

**GET** `/api/v1/training/status/{jobId}`

#### 响应示例

```json
{
  "jobId": "job-abc123",
  "status": "running",
  "progress": {
    "currentEpoch": 2,
    "currentStep": 150,
    "totalSteps": 1000,
    "loss": 0.5234,
    "learningRate": 1.5e-4
  },
  "elapsedTimeSeconds": 1250,
  "estimatedTimeRemainingSeconds": 4200,
  "createdAt": "2025-01-21T10:00:00Z",
  "startedAt": "2025-01-21T10:00:05Z"
}
```

#### 状态值

| 状态 | 描述 |
|------|------|
| `queued` | 排队中 |
| `running` | 运行中 |
| `paused` | 已暂停 |
| `completed` | 完成 |
| `failed` | 失败 |
| `cancelled` | 已取消 |

---

### 3. 取消训练任务

**POST** `/api/v1/training/cancel/{jobId}`

#### 响应示例

```json
{
  "jobId": "job-abc123",
  "status": "cancelled",
  "message": "Training job has been cancelled"
}
```

---

### 4. 列出训练任务

**GET** `/api/v1/training/list`

#### 查询参数

| 参数 | 类型 | 默认值 | 描述 |
|------|------|--------|------|
| `status` | string | - | 按状态筛选 |
| `limit` | int | 100 | 返回数量限制 |
| `offset` | int | 0 | 偏移量 |

#### 响应示例

```json
{
  "jobs": [
    {
      "jobId": "job-abc123",
      "baseModel": "Qwen/Qwen2.5-7B-Instruct",
      "status": "completed",
      "createdAt": "2025-01-21T10:00:00Z",
      "completedAt": "2025-01-21T12:00:00Z"
    }
  ],
  "total": 1,
  "limit": 100,
  "offset": 0
}
```

---

## 模型管理

### 1. 列出可用模型

**GET** `/api/v1/models`

#### 响应示例

```json
{
  "models": [
    {
      "id": "qwen2.5-7b",
      "name": "Qwen2.5 7B",
      "engine": "cuda-vllm",
      "status": "ready",
      "capabilities": ["inference", "stream"],
      "maxModelLen": 8192
    },
    {
      "id": "deepseek-v3",
      "name": "DeepSeek V3",
      "engine": "cuda-vllm",
      "status": "ready",
      "capabilities": ["inference", "stream", "training"],
      "maxModelLen": 16384,
      "loraAdapters": ["furina", "Character1"]
    }
  ]
}
```

---

### 2. 获取模型详情

**GET** `/api/v1/models/{modelId}`

#### 响应示例

```json
{
  "id": "qwen2.5-7b",
  "name": "Qwen2.5 7B Instruct",
  "engine": "cuda-vllm",
  "modelPath": "Qwen/Qwen2.5-7B-Instruct",
  "status": "ready",
  "capabilities": ["inference", "stream"],
  "config": {
    "maxModelLen": 8192,
    "gpuMemoryUtilization": 0.9,
    "tensorParallelSize": 1
  }
}
```

---

## 健康检查

### 1. Gateway 健康检查

**GET** `/api/v1/health`

#### 响应示例

```json
{
  "status": "UP",
  "components": {
    "gateway": "UP",
    "coordinator": "UP",
    "redis": "UP"
  },
  "timestamp": "2025-01-21T10:00:00Z"
}
```

---

### 2. 详细健康信息

**GET** `/api/v1/health/details`

#### 响应示例

```json
{
  "status": "UP",
  "components": {
    "gateway": {
      "status": "UP",
      "details": {
        "startTime": "2025-01-21T09:00:00Z",
        "uptime": "1h0m0s"
      }
    },
    "coordinator": {
      "status": "UP",
      "details": {
        "workers": {
          "total": 2,
          "healthy": 2,
          "unhealthy": 0
        },
        "queue": {
          "pendingTasks": 5,
          "maxSize": 1000
        }
      }
    },
    "redis": {
      "status": "UP",
      "details": {
        "latencyMs": 1,
        "connected": true
      }
    }
  },
  "timestamp": "2025-01-21T10:00:00Z"
}
```

---

## gRPC API

### 服务定义

gRPC 服务定义文件位于根目录唯一契约源 `proto/ai_service.proto`。

### 1. 推理服务

#### StreamPredict

```protobuf
service AIService {
  // 流式推理
  rpc StreamPredict(StreamPredictRequest) returns (stream StreamPredictResponse);
}
```

##### StreamPredictRequest

| 字段 | 类型 | 描述 |
|------|------|------|
| model_id | string | 模型 ID |
| prompt | string | 输入提示词 |
| adapter | string | LoRA 适配器路径 |
| priority | int32 | 优先级 |
| generation | GenerationParameters | 生成参数 |
| metadata | StreamMetadata | 元数据 |
| worker_hint | string | Worker 提示 |

##### StreamPredictResponse

| 字段 | 类型 | 描述 |
|------|------|------|
| trace_id | string | 追踪 ID |
| chunk | string | 文本块 |
| end_of_stream | bool | 是否结束 |
| index | int32 | 块索引 |

---

#### 2. 训练服务

```protobuf
service TrainingService {
  // 启动训练
  rpc StartTraining(TrainingRequest) returns (stream TrainingProgress);
  
  // 查询状态
  rpc GetTrainingStatus(TrainingStatusRequest) returns (TrainingStatusResponse);
  
  // 取消训练
  rpc CancelTraining(CancelTrainingRequest) returns (CancelTrainingResponse);
  
  // 列出训练任务
  rpc ListTrainingJobs(ListTrainingJobsRequest) returns (ListTrainingJobsResponse);
}
```

##### TrainingRequest

| 字段 | 类型 | 描述 |
|------|------|------|
| base_model | string | 基础模型 |
| output_dir | string | 输出目录 |
| dataset_path | string | 数据集路径 |
| hyper_params | TrainingHyperParams | 超参数 |
| lora_config | LoraConfig | LoRA 配置 |

##### TrainingProgress

| 字段 | 类型 | 描述 |
|------|------|------|
| trace_id | string | 追踪 ID |
| job_id | string | 任务 ID |
| status | TrainingStatus | 状态 |
| progress | ProgressUpdate | 进度 |
| metrics | TrainingMetrics | 指标 |

---

## 认证

### API Key 认证

所有 API 请求需要在 Header 中包含 `X-API-Key`：

```bash
curl -H "X-API-Key: your-api-key-here" \
  http://localhost:8080/api/v1/inference
```

### 环境变量配置

```bash
# Gateway 配置
CY_LLM_API_KEY=your-api-key-here
CY_LLM_INTERNAL_TOKEN=internal-service-token
```

---

## 错误码

### HTTP 状态码

| 状态码 | 描述 |
|--------|------|
| 200 | 成功 |
| 400 | 请求参数错误 |
| 401 | 未认证 |
| 403 | 无权限 |
| 404 | 资源不存在 |
| 429 | 请求过于频繁 |
| 500 | 服务器内部错误 |
| 503 | 服务不可用 |

### 错误响应格式

```json
{
  "error": {
    "code": "INVALID_REQUEST",
    "message": "Invalid model ID: model-not-found",
    "details": [
      {
        "type": "model_not_found",
        "field": "modelId",
        "description": "Model 'model-not-found' is not configured"
      }
    ]
  }
}
```

### 常见错误码

| 错误码 | HTTP 状态 | 描述 |
|--------|-----------|------|
| `INVALID_REQUEST` | 400 | 请求参数无效 |
| `MODEL_NOT_FOUND` | 404 | 模型不存在 |
| `MODEL_NOT_READY` | 503 | 模型未加载完成 |
| `INSUFFICIENT_RESOURCES` | 503 | 资源不足 |
| `TRAINING_JOB_NOT_FOUND` | 404 | 训练任务不存在 |
| `TRAINING_JOB_RUNNING` | 400 | 训练任务运行中 |
| `RATE_LIMITED` | 429 | 请求过于频繁 |
| `INTERNAL_ERROR` | 500 | 服务器内部错误 |

---

## 2026-02-10 Token速度优化更新

### 变更摘要
- 优化vLLM引擎流式输出性能
- 切换CUDA平台默认引擎为异步版本

### 引擎初始化参数更新
#### VllmCudaEngine
新增参数：
- `stream_chunk_size: int = 4` - 流式输出块大小

#### VllmAsyncEngine
新增参数：
- `allow_auto_tuning: bool = True` - 自动调整参数避免OOM

### 默认引擎变更
| 平台 | 旧默认 | 新默认 |
|------|--------|--------|
| CUDA | cuda-vllm | cuda-vllm-async |

### 向后兼容
- 所有变更向后兼容
- 可通过环境变量回退到旧引擎：
  ```bash
  export CY_LLM_ENGINE=cuda-vllm
  ```

### 性能提升
| 指标 | 优化前 | 优化后 |
|------|--------|--------|
| Token速度 | 15-20 t/s | ≥50 t/s |
| TTFT | ~500ms | ≤200ms |
