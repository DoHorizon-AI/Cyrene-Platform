# Worker-control TCK v1

This is the language-neutral conformance suite for the Core v1
`PluginLifecycleService.ConnectWorker` protocol. It covers the Worker-side
control state machine used by Python, Kotlin/JVM, and native Workers:

```text
WorkerHello -> WorkerWelcome -> WorkerHeartbeat/WorkerHeartbeatAck
                                     -> WorkerShutdown -> WorkerShutdownAck
```

`vectors.tsv` contains canonical protobuf wire bytes for each oneof arm and
their SHA-256. `scenarios.tsv` defines the required handshake, monotonic
heartbeat, fenced shutdown id, and rejection behavior. The included Python and
Kotlin runners independently validate the exact same bounded protobuf envelope
and state machine without pulling a Python/JVM runtime into the Kernel.

Run the reference runners from this directory:

```text
python python/worker_control_tck.py
kotlinc kotlin/WorkerControlTck.kt -include-runtime -d worker-control-tck.jar
java -jar worker-control-tck.jar .
```

SDK integration rule: a generated Proto/gRPC Worker client must decode the
wire vectors with its own generated `WorkerToKernel`/`KernelToWorker` classes,
then drive its live control loop through the same scenario order. A TCK pass
does not authorize access to a Worker command channel: the UDS endpoint,
instance generation, lease, and sandbox policy remain Kernel-controlled.
