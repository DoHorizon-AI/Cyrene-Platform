# Worker-control TCK v1

This is the language-neutral conformance suite for both the Core v1 legacy
`PluginLifecycleService.ConnectWorker` compatibility protocol and the canonical
`WorkerControlService` protocol. It covers the Worker-side control state
machine used by Python, Kotlin/JVM, and native Workers:

```text
WorkerHello -> WorkerWelcome -> WorkerHeartbeat/WorkerHeartbeatAck
                                     -> WorkerShutdown -> WorkerShutdownAck
```

`vectors.tsv` and `scenarios.tsv` cover the legacy compatibility envelope.
`semantic_scenarios.tsv` covers the canonical Worker control state machine:
the Hello, Heartbeat and ShutdownAck frames must all preserve the Kernel-issued
Worker generation, Lease generation and fence token. The included Python and
Kotlin runners independently validate the same bounded scenarios without
pulling a Python/JVM runtime into the Kernel. The `HEARTBEAT_WORKER` action is
covered by `heartbeat_worker_accepted` (accept path) and the existing
`semantic_heartbeat_fence_mismatch` (deny path).

Run the reference runners from this directory:

```text
python python/worker_control_tck.py
kotlinc kotlin/WorkerControlTck.kt -include-runtime -d worker-control-tck.jar
java -jar worker-control-tck.jar .
```

SDK integration rule: a generated Proto/gRPC Worker client must decode the
legacy wire vectors with its own generated `WorkerToKernel`/`KernelToWorker`
classes and drive the canonical `WorkerControlToKernel`/
`KernelToWorkerControl` sequence through the semantic scenarios. A TCK pass
does not authorize access to a Worker command channel: the UDS endpoint,
instance generation, lease, fence and sandbox policy remain Kernel-controlled.
---

<!-- Chinese Translation / 中文翻译 -->

# Worker-control TCK v1

这是针对 Core v1 旧版兼容协议 `PluginLifecycleService.ConnectWorker` 和规范 `WorkerControlService` 协议的跨语言符合性测试套件。它覆盖 Python、Kotlin/JVM 和 native Worker 使用的 Worker 侧 control 状态机：

```text
WorkerHello -> WorkerWelcome -> WorkerHeartbeat/WorkerHeartbeatAck
                                     -> WorkerShutdown -> WorkerShutdownAck
```

`vectors.tsv` 和 `scenarios.tsv` 覆盖旧版兼容 envelope。`semantic_scenarios.tsv` 覆盖规范 Worker control 状态机：Hello、Heartbeat 和 ShutdownAck frame 都必须保留 Kernel 签发的 Worker generation、Lease generation 和 fence token。附带的 Python 与 Kotlin runner 独立验证相同的有界场景，不会把 Python/JVM runtime 引入 Kernel。`HEARTBEAT_WORKER` action 由 `heartbeat_worker_accepted`（接受路径）和既有的 `semantic_heartbeat_fence_mismatch`（拒绝路径）覆盖。

在本目录运行参考 runner：

```text
python python/worker_control_tck.py
kotlinc kotlin/WorkerControlTck.kt -include-runtime -d worker-control-tck.jar
java -jar worker-control-tck.jar .
```

SDK 集成规则：生成的 Proto/gRPC Worker client 必须使用自身生成的 `WorkerToKernel`/`KernelToWorker` class 解码旧 wire vector，并根据 semantic scenario 驱动规范的 `WorkerControlToKernel`/`KernelToWorkerControl` 序列。TCK 通过并不授权访问 Worker command channel：UDS endpoint、instance generation、lease、fence 和 sandbox policy 仍由 Kernel 控制。
