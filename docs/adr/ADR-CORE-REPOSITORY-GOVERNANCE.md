# ADR-CORE-REPOSITORY-GOVERNANCE: Core and Advanced-Service Boundaries

- Status: Approved / Normative
- Date: 2026-08-10
- Supersedes: repository-boundary alternatives described before the split-repository decision

## Decision

CYRENE uses one reusable Core repository and one independent repository for each
first-party service. The Core repository contains the Rust host runtime, public
contracts, framework boundary, examples, conformance tooling, and governance
documentation. It does not contain Catalyst, Yield, Reactor, Exchange,
Navigator, Echo, vendor runtimes, enterprise bundles, or product UI source.

Each Product repository owns its service implementation, Product manifest,
deployment assets, and migration source. Cyrene-Plugins-Official owns reusable
capability manifests, payload contracts, packages, SDKs, implementations, and
TCKs. Consumers record exact released Platform dependencies in their own lock
or build configuration.

## Branch and release policy

- `develop-kernel` is the Rust Kernel/Node Runtime development branch.
- `develop` is the reviewed cross-language integration branch.
- `main` is the release branch and accepts only pull requests whose head branch
  is `develop`.
- `develop` and `main` require pull requests, one independent approval, current
  required checks, resolved conversations, and no force-push or deletion.
- The main release PR additionally requires a release-evidence-verified label
  and a linked verification record.
- Administrators are subject to the same protected-branch rules.

## Consequences

Core can be built and tested without checking out any first-party service.
Cross-repository changes are coordinated through versioned contracts, TCK
artifacts, signed packages, and the service's `core.lock` rather than source
imports or Git submodules.

This ADR does not create Core v1, configure a deployment environment, or migrate
the six services. Those are later phases with their own acceptance gates.
