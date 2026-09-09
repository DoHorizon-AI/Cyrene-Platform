# CYRENE Platform architecture

## Boundary

Platform supplies a generic distributed execution substrate. Products own
application workflows and durable business state. Plugin repositories own
versioned capability payloads, SDKs, runtime launchers, implementations, and
TCKs.

## Layers

### Kernel

The Rust Kernel is the node-local authority for identities, Leases, fences,
resource and lifecycle transitions, and bounded events. It does not contain
global Product policy, AI runtimes, vendor libraries, or business protocols.

### Adapter hosts

Sandbox and hardware adapters run as separately authenticated processes. They
translate privileged or vendor-specific mechanisms into generic resource facts
and actions. Kernel never loads their libraries in-process.

### Framework and agents

Rust framework crates and agents implement generic execution placement,
workspace transport, Artifact transfer, verified Plugin package lifecycle, and
process supervision. A package process publishes an opaque `connection_ref`.
Platform does not handle the endpoint's business payload.

### Products and Plugins

A Product asks Platform to select/start a compatible Plugin, receives the exact
binding and endpoint, then invokes that endpoint using the Plugin-owned client
contract. Product lifecycle state remains in the Product repository.

```text
Product ──control──► Platform ──authority──► Kernel/Adapter
   │                    │
   └──business data────►Plugin endpoint
                        ▲
                  process supervision
```

## Communication planes

- Control plane: identity, admission, lifecycle, health, permissions, and
  endpoint discovery.
- Artifact Plane: content identity, verified transfer, and staging.
- Capability data plane: direct Product-to-Plugin protocol outside Platform.

See `docs/governance/platform-clean-boundary.md` for the extension rule.
