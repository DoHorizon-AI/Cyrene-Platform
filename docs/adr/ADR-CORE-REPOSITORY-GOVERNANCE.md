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

The advanced-services repository owns first-party service implementations,
service bundles, deployment assets, and preserved legacy source. Each service
consumes a released Core contract and records the exact dependency in its
core.lock.

service.json is service-bundle metadata. plugin.toml is the manifest of an
installable component. They describe different levels of the same signed
artifact and are not separate installation channels.

## Branch and release policy

- develop is the daily development and integration branch.
- main is the release branch and accepts only pull requests whose head branch
  is develop.
- Both branches require pull requests, one independent approval, current
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
