# Platform clean boundary

Status: **Normative**
Baseline: the canonical merge commit produced by this remediation

Platform owns generic Kernel semantics, Node and Runtime supervision,
Lease/Fence enforcement, Artifact identity/transfer, workspace execution, and
the control-plane portion of Plugin installation, compatibility, activation,
health, permissions, and endpoint discovery.

Platform never owns capability payload schemas, Product lifecycle/state,
Product manifests, ecosystem catalogs, protocol adapters, deployment templates,
compatibility source snapshots, or business examples. It may return an opaque
`connection_ref`; it may not proxy, parse, route, persist, or transform the
business request or response carried by that endpoint.

## Zero-change extension rule

A Product or Plugin consumes the published Platform contracts without a
Platform source change. A proposed Platform addition must identify at least two
independent consumers or a Kernel-level invariant and must include a generic
contract test. One Product's convenience, language, protocol, deployment shape,
or vocabulary is insufficient.

Repository-owned `plugin.manifest.json` files define Plugin identity,
capabilities, methods, payload schema references, runtime language, and
protocol. Platform normalizes only the fields needed for generic compatibility
selection. For a managed service package, the package supplies a language-
neutral launch command. Platform starts that verified process, validates the
readiness identity, and returns its opaque endpoint.

## Removed compatibility surfaces

The following implemented legacy surfaces were removed after reverse-dependency
checks and owner migration:

- ten named v0 capability SPI Protobuf contracts and their Rust adapters;
- `cy-extension-registry`, `cy-local-transport`, and `BuiltinInMemoryStorage`;
- Capability Execution Service, its client SDK/TCK, stdio worker protocol, and
  worker SDK/shim;
- model-provider and message-connector payload contracts;
- Product run/environment/model-version implementations;
- AI model, hardware, training, runtime, checkpoint, validation, planning, and
  Artifact-lineage manifests;
- v0 `plugin.toml`, Product service manifests/catalogs, and the Platform copy of
  `media.processor.v1`.

`tooling/ci/check-no-legacy-surface.sh` prevents those paths, symbols, Product
identities, and package data-plane operations from returning.

## Evidence boundary

A local or Hosted build proves only the checks it actually ran. GPU, external
provider, Kubernetes, and complete Product lifecycle evidence remain separate
and must not be inferred from Platform compilation.
