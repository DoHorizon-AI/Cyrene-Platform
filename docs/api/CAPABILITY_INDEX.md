# Cyrene-Platform Contract Index

This index covers contracts physically owned by Cyrene-Platform. It is not a
catalog of Products, plugin packages, repository versions, or implementation
readiness. Those mutable facts belong to the implementing repository and to
[Cyrene-Workspace](https://github.com/DoHorizon-AI/Cyrene-Workspace).

## Contract surfaces

| Surface | Platform source | Status in this repository | Boundary |
| --- | --- | --- | --- |
| Kernel semantic authority | `contracts/proto/cyrene/semantic/v1/kernel_contract.proto`, `docs/contracts/kernel-semantic-contract-v1.md` | Normative v1 | Generic node-local identity, lifecycle, lease, fence, endpoint, and event semantics |
| Kernel and node control wire contracts | `contracts/proto/cyrene/core/v1/`, `contracts/proto/cyrene/core/v2/` | Versioned | Generic runtime, resource, supervision, and node-control messages |
| Capability Execution Service | `contracts/proto/cyrene/capability/v1/capability_execution.proto` | Versioned v1 | Capability invocation and application-event transport; no capability business policy |
| Worker/package protocol | `contracts/proto/plugin/v1/plugin_protocol.proto`, `contracts/tck/worker-control/` | Versioned v1 | Generic worker lifecycle, invocation, cancellation, and event flow |
| `model.provider.v1` | `contracts/proto/cyrene/model/provider/v1/model_provider.proto` | **EXPERIMENTAL** | Stateless model computation payloads; no routing, memory, retrieval, or persistence policy |
| `message.connector.v1` | `contracts/proto/cyrene/message/connector/v1/message_connector.proto` | **EXPERIMENTAL** | Vendor-neutral message facts and delivery requests; no Product session or reply policy |
| `media.processor.v1` | `contracts/schemas/media-processor-v1.schema.json` | **STABLE** | Wire schema only; implementations and typed language adapters belong to consumer or plugin repositories |
| Execution and Artifact Fabric | `contracts/proto/cyrene/core/v1/node_control.proto`, `contracts/schemas/artifact_transfer.schema.json` | Versioned v1 | Generic placement, attachment, transfer, and failure facts |
| Artifact and runtime manifests | `contracts/schemas/manifests/` | Versioned schemas | Immutable references, manifests, validation facts, and workload requests |
| Advanced service manifest shape | `contracts/schemas/advanced-service.schema.json` | Schema v1 | Generic manifest syntax only; service names and lifecycle state are consumer-owned |

## Rules

1. A contract entry does not prove that any plugin or Product implementation is
   available, deployed, or production-ready.
2. Platform tests may use neutral fixtures to prove wire and schema semantics.
   Concrete vendors, Product identities, adapter bindings, and business state
   belong outside this repository.
3. Adding or changing a Product, plugin, repository, distribution profile, or
   connector vendor must not require a Platform source change.
4. Contract maturity changes require contract-local evidence and compatibility
   review. Consumer readiness is recorded by the consumer.
