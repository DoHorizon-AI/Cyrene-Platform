# JVM framework target

This directory reserves the future Kotlin/JVM framework boundary. No legacy JVM
code was moved here because the available Kotlin sources implement private
gateway and coordinator business behavior; they were preserved with Exchange in
the advanced-services repository.

The future JVM implementation should provide workflow/state orchestration,
enterprise security integration, extension routing, and public SDK ergonomics.
It must communicate with the Rust kernel and workers through `contracts/`.

NodeControl ownership is intentionally outside this JVM target. The deployable
`NodeControlService.Connect` implementation is Rust
`framework/crates/cy-execution-control`; this target must not provide a second
inbound server, registration registry, or `activeNodes` authority.

The former raw-byte `NodeAgentGrpcClient` and always-successful
`KernelOutboundAdapter` were also removed. A Node Agent is outbound-only and
does not expose the listener those classes assumed. Any future JVM execution
client must consume a published canonical Platform contract with generated
bindings and real conformance evidence.
