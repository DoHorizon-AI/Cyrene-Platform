# Repository boundaries and migration checkpoint

## Core repository - GitHub organization

The future GitHub repository contains `kernel/`, `framework/`, `contracts/`,
public SDKs, examples, and core CI. It is private during the redesign and may be
opened later after security, API stability, licensing, and documentation gates.

## Advanced-services repository - Azure DevOps

The private repository contains the six first-party services, vendor/runtime
plugins, enterprise bundles, service deployments, and preserved legacy product
sources. Its pipeline checks out this core repository and executes the validator
from `framework/tooling/validate_advanced_service.py` against all six manifests.

## Migration rule

The new repositories are clean-history snapshots assembled from audited source.
Core contains only its explicit public whitelist. Legacy service source remains
byte-preserved in the private repository; follow-up work may refactor imports
only after a service has a stable public contract and an acceptance test.

## Non-goals for this checkpoint

- publishing or making either repository public;
- configuring GitHub or Azure DevOps remotes;
- claiming GPU support from source presence;
- making every private legacy application build from its new path;
- deleting legacy bundles or generated compatibility code.
