# Kernel Semantic TCK v1

This directory is the executable compatibility baseline for the frozen
`cyrene.kernel.semantic/v1` contract. The normative specification remains
`docs/contracts/kernel-semantic-contract-v1.md`; these TSV fixtures are its
machine-readable acceptance vectors. Python, Kotlin and Rust independently
evaluate the same decisions.

The suite freezes four areas that must not drift between projections:

- identifier grammar, timestamp range and numeric limits;
- complete Lease, Worker and Operation transition matrices;
- semantic revision negotiation and exact Capability/Quantity matching;
- Lease and EndpointGrant fencing/expiry authority.
- Event source, replay-gap and bounded page behavior.

Run the dependency-free reference runners from this directory:

```text
python python/kernel_semantic_tck.py
kotlinc kotlin/KernelSemanticTck.kt -include-runtime -d kernel-semantic-tck.jar
java -jar kernel-semantic-tck.jar .
```

The Rust projection consumes these files from its unit tests. A language SDK
conforms only when its public types and transport decoders pass these same
vectors, reject missing/unknown required values, and do not add weaker local
defaults. Passing this suite does not certify a transport, authentication
policy, scheduler or Provider implementation.

Frozen-v1 change rule: existing accepted inputs, state transitions, authority
decisions, field meanings and numeric limits cannot change in place. A change
to any of them requires semantic v2. New optional projection fields are only
allowed when ignoring them preserves every v1 decision.
