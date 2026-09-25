# End-to-End Flow: Model Serving & Inference

This document traces how model deployments are created, scaled, and served.

---

## Serving Architecture Flow

```mermaid
sequenceDiagram
    autonumber
    actor Client
    participant Reactor as Cyrene-Reactor (Product)
    participant Exchange as Cyrene-Exchange (Gateway)
    participant Kernel as Cyrene-Kernel
    participant Engine as ServingEngine Worker (vLLM/SGLang)

    Reactor->>Kernel: Acquire GPU Allocation & Lease
    Kernel-->>Reactor: Lease Granted
    Reactor->>Kernel: Start Serving Worker via WorkerControl
    Kernel->>Engine: Spawn Worker inside Sandbox with Fencing
    loop Readiness Probing
        Reactor->>Engine: Probe /healthz
        Engine-->>Reactor: Ready (Model Weights Loaded)
    end
    Reactor->>Exchange: Register Endpoint in Routing Table
    Client->>Exchange: POST /v1/chat/completions
    Exchange->>Engine: Stream Inference Request
    Engine-->>Exchange: SSE Tokens Stream
    Exchange-->>Client: Return Generated Response
```
---

<!-- Chinese Translation / 中文翻译 -->

# 端到端流程：模型服务与推理

本文跟踪模型部署的创建、扩缩容和服务过程。

---

## 服务架构流程

```mermaid
sequenceDiagram
    autonumber
    actor Client
    participant Reactor as Cyrene-Reactor (Product)
    participant Exchange as Cyrene-Exchange (Gateway)
    participant Kernel as Cyrene-Kernel
    participant Engine as ServingEngine Worker (vLLM/SGLang)

    Reactor->>Kernel: 获取 GPU Allocation 与 Lease
    Kernel-->>Reactor: 授予 Lease
    Reactor->>Kernel: 通过 WorkerControl 启动 Serving Worker
    Kernel->>Engine: 在带 fencing 的 Sandbox 中启动 Worker
    loop 就绪探测
        Reactor->>Engine: 探测 /healthz
        Engine-->>Reactor: 就绪（模型权重已加载）
    end
    Reactor->>Exchange: 在路由表中注册 Endpoint
    Client->>Exchange: POST /v1/chat/completions
    Exchange->>Engine: 流式发送推理请求
    Engine-->>Exchange: SSE token 流
    Exchange-->>Client: 返回生成的响应
```
