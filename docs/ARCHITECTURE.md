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

The Kernel is a node-local host and enforcement layer. It owns leases/fencing,
cgroup and process lifecycle, generic local IPC, and lifecycle events. It does
not own global scheduling policy or a vendor driver integration.

GPU support is intentionally outside the Kernel process. A separately
supervised Hardware Adapter Host may:

- read vendor `/proc` or sysfs facts, telemetry, or CLI output;
- enumerate approved device nodes and binding environment variables;
- load a vendor C ABI library when a CLI is insufficient;
- return versioned inventory, health, and binding facts over UDS.

The Kernel applies the returned binding to its sandbox and observes exit, OOM,
and cgroup resource events. It does not link model runtimes, vendor libraries,
or vendor commands. Adapter loss blocks new dependent leases rather than
crashing the Kernel; a C ABI is an internal Adapter Host detail, not a public
Kernel extension API.

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
Statically compiled Rust code inside the Kernel is limited to generic cgroup,
process, and transport mechanisms. Hardware adapters are Core components but
run as separate processes; they are not installable plugin runtimes. The Core
repository does not link PyO3 or AI compute runtimes into the Kernel.

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
