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

The kernel is a stateless host and enforcement layer. It supervises processes,
applies resource and filesystem policy, exposes transport primitives, records
lifecycle events, and maintains the extension registry boundary.

GPU support is intentionally shallow. The kernel may:

- read `/proc`, `/sys/class/drm`, NVML-style telemetry, or vendor CLI output;
- map approved `/dev/*` nodes into a sandbox;
- inject `CUDA_VISIBLE_DEVICES`, `HIP_VISIBLE_DEVICES`, or equivalent values;
- observe exit, OOM, health, and resource events.

It does not link model runtimes or implement CUDA/ROCm kernels. Discovery and
visibility rules are vendor adapters because device layouts and telemetry APIs
can change even when the kernel contract remains stable.

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
Statically compiled Rust code is allowed inside Core only for audited host,
device, transport, and sandbox adapters; it is not an installable plugin
runtime. The Core repository does not link PyO3 or AI compute runtimes.

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
