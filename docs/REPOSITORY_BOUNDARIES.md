# Repository boundaries and migration checkpoint

## Core repository - GitHub organization

The future GitHub repository contains `kernel/`, `framework/`, `contracts/`,
examples, and Rust-only core CI. It is private during the redesign and may be
opened later after security, API stability, licensing, and documentation gates.

## Advanced-services repository - Azure DevOps

The private repository contains the six first-party services, vendor/runtime
plugins, enterprise bundles, service deployments, and preserved legacy product
sources. Its pipeline checks out this core repository and verifies the referenced
Rust workspace before auditing all six service manifests and preserved roots.

## Migration rule

The new repositories are clean-history snapshots assembled from audited source.
Core contains only its explicit public whitelist. Legacy service source remains
byte-preserved in the private repository; follow-up work may refactor imports
only after a service has a stable public contract and an acceptance test.

Language-specific plugin SDKs, generators, and compatibility tests are private
migration tooling until the replacement API is designed. A Python process may
implement an out-of-process plugin, but Python is not part of the core runtime or
its repository toolchain.

## Non-goals for this checkpoint

- publishing or making either repository public;
- configuring GitHub or Azure DevOps remotes;
- claiming GPU support from source presence;
- making every private legacy application build from its new path;
- deleting legacy bundles or generated compatibility code.
