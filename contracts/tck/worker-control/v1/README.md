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
pulling a Python/JVM runtime into the Kernel.

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
