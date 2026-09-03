# CYRENE core architecture

## Product model

CYRENE is a distributed AI application runtime. The reusable host is separated
from six first-party advanced services:

1. Catalyst - data preparation and knowledge extraction;
2. Yield - training and fine-tuning;
3. Reactor - quantization, deployment, and inference;
4. Exchange - workflow, API gateway, and enterprise service coordination;
5. Navigator - cross-device UI and user-facing execution;
6. Echo - evaluation, scoring, and feedback enhancement.

The six products are plugins, not branches inside the core.

## Layers

### Rust kernel

The Kernel is a node-local decision and lifecycle layer. It owns leases/fencing,
launch authorization, generic local IPC, and lifecycle events. It does not own
global scheduling policy, a vendor driver integration, cgroup files, or a
worker process tree.

GPU support is intentionally outside the Kernel process. A separately
supervised Hardware Adapter Host may:

- read vendor `/proc` or sysfs facts, telemetry, or CLI output;
- enumerate approved device nodes and binding environment variables;
- load a vendor C ABI library when a CLI is insufficient;
- return versioned inventory, health, and binding facts over UDS.

The Kernel sends the returned binding to a separately supervised `sandboxd`
over UDS. sandboxd applies cgroup/device enforcement and reports exit, OOM,
and cgroup resource facts. Neither process links model runtimes, vendor
libraries, or vendor commands into Kernel address space. Adapter loss blocks
new dependent leases rather than crashing the Kernel; a C ABI is an internal
Adapter Host detail, not a public Kernel extension API.

### Framework and control plane

The framework owns policy routing, permissions, plugin discovery,
configuration, and extension composition. The previous Rust control plane also
implemented training, runtime construction, model acquisition, quantization,
and serving; it is therefore retained only in the private migration archive.
A Kotlin/JVM orchestration implementation is a valid future target, provided it
depends only on public contracts and does not absorb host isolation or AI
worker code.

### Worker and advanced-service plugins

Python, JVM, and third-party Rust business code run outside the kernel. They
implement concrete data, training, inference, gateway, UI, and evaluation
behavior. A worker crash must become a lifecycle event, not a core crash.
Statically compiled Rust code inside the Kernel is limited to pure-safe state
machines, contracts, and transport clients. Hardware and sandbox adapters are
Core components but run as separate processes; they are not installable plugin
runtimes. The Core repository does not link PyO3 or AI compute runtimes into
the Kernel.

## Communication planes

- Control: versioned gRPC or local sockets for commands and API calls.
- Event: asynchronous publish/subscribe for state and health changes.
- Data: shared memory or memory-mapped handles locally, streaming transfers
  remotely. Large artifacts do not travel as ordinary control RPC payloads.

## Extension contract

Every advanced service declares its identity, protocol version, required
extension points, and source roots in `service.json`. Other plugins continue to
use `plugin.toml`. The framework discovers declarations; it never imports one of
the six product packages by name.

service.json is first-party service-bundle metadata. plugin.toml is component
manifest metadata inside the same signed package; the two files are not
alternative installation protocols.

## UI boundary

Production servers are headless. Plugins may provide micro-frontend assets. A
thin cross-platform shell renders those assets and talks to the framework over
public APIs. Navigator is the first-party implementation of that shell, not a
kernel component.

## Kernel document layers

- [Distributed Execution Fabric v1](architecture/distributed-execution-fabric-v1.md): frozen attachment, identity, control, loss, and Artifact transfer model for host, container-only, and provider-managed execution.
- [Distributed Workspace Fabric v1](architecture/distributed-workspace-fabric-v1.md): identity-based Workspace discovery, transport-neutral connection candidates, and relay-first remote frontend access.
- [Kernel design goals](architecture/kernel-design-goals.md): durable design
  direction and boundaries.
- [Kernel semantic contract v1](contracts/kernel-semantic-contract-v1.md): the
  sole normative definition of current v1 semantics.
- [Kernel execution goals](operations/kernel-execution-goals.md): active P2
  implementation and Linux acceptance work.
- [Kernel runtime baseline](operations/kernel-runtime.md): deployment and
  operating guidance.
