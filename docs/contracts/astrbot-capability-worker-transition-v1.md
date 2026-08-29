# AstrBot Capability Worker Transition Contract v1

Status: **Transition contract**. This document is normative only for the
AstrBot V2 capability-worker admission seam. It is not a CYRENE Kernel semantic
contract, a C ABI, or a JVM SPI.

## Scope and authority

The contract identifier is `cyrene.astrbot.capability-worker` at API version
`1.0`. It defines how the AstrBot Host admits an out-of-process Python
capability worker. It does not define the business meaning of a model, memory,
skill, channel, or Agent capability. Each such capability needs its own
versioned extension contract and TCK before it can be installed as a CYRENE
capability plugin.

The existing V2 MessagePack/stdin-stdout protocol is its temporary wire
projection. The CYRENE Kernel semantic contract remains the authority for the
nine Kernel nouns; C ABI and JVM SPI remain independent client bindings of that
Kernel contract. Neither is a business-plugin API.

## Admission identity

A schema-version `2` AstrBot package must contain exactly this declaration:

```json
"cyrene": {
  "componentClass": "capability-plugin",
  "extensionContract": "cyrene.astrbot.capability-worker",
  "apiVersion": "1.0"
}
```

The Host sends the equivalent snake-case object in
`hello.payload.cyrene_contract`. The worker must echo the same
`component_class`, `extension_contract`, and `api_version` values in a
successful hello response. Any missing field or mismatch rejects activation.

Schema version `1` is legacy AstrBot compatibility metadata. It may be read in
the Development migration path, but a Production Host that enables Python
capabilities must require this V2 declaration. A manifest alone does not make
a compatibility snapshot discoverable or installable by the CYRENE Plugin Host.

## Mediated interaction boundary

| Concern | Transition rule |
| --- | --- |
| Invocation | Host assigns request ID, trace ID, plugin ID/version, deadline and operation. The worker cannot replace these identities. |
| Operations | `event` and `invoke` are the SDK-plugin request operations. `host_call` is a worker-to-Host callback and retains caller identity. |
| Effects | A worker affects Host state only through a declared `host_call` capability allowed by active manifest permissions. |
| Cancellation | Host uses V2 `cancel`. Timeout or cancellation is an outcome, not permission to leave a background action running. |
| Errors | V2 errors are `{code, message, transient}`. Protocol mismatch invalidates the active worker process generation. |
| Forbidden access | Workers receive no direct database, Kernel, GPU, secret, or host-object access. The Host mediates every allowed capability. |

Business payloads remain opaque at this transition layer. A concrete capability
contract must define its request, response, stream, error, configuration,
authorization, cancellation, and event semantics before installation.

## Contract matrix

| Layer | Identifier/version | Responsibility | Must not be confused with |
| --- | --- | --- | --- |
| Kernel semantics | `cyrene.semantic.v1` | Nine Kernel nouns and authority | Plugin package behavior |
| Kernel client binding | C ABI / JVM SPI version | Native and JDK-only client projection | Worker wire protocol |
| Plugin worker wire | `cy.plugin.v1` protocol `2` | Framing, correlation, lifecycle control | Capability business semantics |
| AstrBot transition extension | `cyrene.astrbot.capability-worker@1.0` | V2 admission identity and mediated envelope | A generic installable plugin contract |
| Business extension | capability-specific | Product request/effect/error/event meaning | Compatibility snapshot metadata |

The [admission-vector verifier](../../contracts/tck/astrbot-capability-worker/v1)
checks the V2 declaration, hello echo, and Production legacy rejection. It is
not a complete plugin-admission TCK. The Platform component-boundary admission
gates still apply before any compatibility snapshot becomes installable.
