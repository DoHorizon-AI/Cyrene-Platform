# JVM framework target

This directory reserves the future Kotlin/JVM framework boundary. No legacy JVM
code was moved here because the available Kotlin sources implement private
gateway and coordinator business behavior; they were preserved with Exchange in
the advanced-services repository.

The future JVM implementation should provide workflow/state orchestration,
enterprise security integration, extension routing, and public SDK ergonomics.
It must communicate with the Rust kernel and workers through `contracts/`.
