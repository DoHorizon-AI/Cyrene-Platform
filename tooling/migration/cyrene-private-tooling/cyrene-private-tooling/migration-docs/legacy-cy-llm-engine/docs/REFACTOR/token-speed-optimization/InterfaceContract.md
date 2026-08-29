> ⚠️ **旧设计文档（已过时）**：本文件描述已废弃的架构，可能与当前实现不符。当前权威文档见 [README 文档导航](../../../README.md#-文档导航)。请勿据此判断现状。

# Token Speed Optimization - Interface Contract

## 外部接口清单

### API接口（冻结 - 不允许修改）

#### vLLM引擎接口
所有引擎必须实现以下抽象接口（来自abstract_engine.BaseEngine）：

```python
class BaseEngine(ABC):
    @abstractmethod
    def load_model(self, model_path: str, adapter_path: Optional[str] = None, **kwargs) -> None
    
    @abstractmethod
    def infer(self, prompt: str, **kwargs) -> Generator[str, None, None]
    
    @abstractmethod
    def unload_model(self) -> None
    
    @abstractmethod
    def get_memory_usage(self) -> Dict[str, float]
```

**约束**:
- `infer()` 必须返回 `Generator[str, None, None]`
- 输出必须为字符串类型
- 允许内部实现优化，但外部行为不变

### 引擎工厂接口（允许优化）

#### EngineFactory.create()
```python
@staticmethod
def create(engine_type: str, *args, **kwargs) -> BaseEngine
```

**允许变更**:
- 可修改默认引擎类型
- 可添加新引擎类型
- 必须保持向后兼容（旧引擎类型仍可用）

#### create_engine()
```python
def create_engine(engine_type: str, **kwargs) -> BaseEngine
```

**允许变更**:
- 可优化引擎初始化参数
- 可添加性能相关配置

### gRPC服务接口（冻结）

#### grpc_servicer.py
- `StreamPredict` 方法签名不变
- 请求/响应消息格式不变
- **允许**: 内部优化消息批处理
- **禁止**: 修改protobuf定义

### 配置接口（允许扩展）

#### 引擎配置（允许新增字段）
```python
# vllm_cuda_engine.py __init__ 参数
VllmCudaEngine(
    tensor_parallel_size: int = 1,
    gpu_memory_utilization: float = 0.75,
    # 允许新增: 流式配置参数
    stream_chunk_size: int = 4,  # 新增：每个yield的token数
    enable_true_streaming: bool = False,  # 新增：是否使用AsyncLLMEngine
)
```

## 接口变更审批

### 本次重构允许的变更

| 变更类型 | 位置 | 状态 | 说明 |
|----------|------|------|------|
| 优化yield粒度 | vllm_cuda_engine.py:500 | ✅ 已批准 | 逐字符→按token块 |
| 新增配置参数 | vllm_cuda_engine.__init__ | ✅ 已批准 | stream_chunk_size等 |
| 修改默认引擎 | engine_factory.py | ✅ 已批准 | cuda-vllm→cuda-vllm-async |
| 内部批处理 | grpc_servicer.py | ✅ 已批准 | 消息缓冲优化 |

### 冻结的接口（禁止修改）

| 接口 | 位置 | 原因 |
|------|------|------|
| infer()签名 | abstract_engine.py | API契约 |
| gRPC protobuf | proto/ | 协议兼容性 |
| 环境变量名 | 所有config文件 | 配置兼容性 |

## 版本策略

### 向后兼容承诺
- 所有现有代码无需修改即可运行
- 默认行为优化但保持兼容
- 新特性通过参数开关控制（默认关闭或自动检测）

### API.md更新要求
所有接口变更必须在docs/API.md中标注：
- 变更日期
- 变更内容
- 向后兼容说明
- 迁移指南（如需要）
