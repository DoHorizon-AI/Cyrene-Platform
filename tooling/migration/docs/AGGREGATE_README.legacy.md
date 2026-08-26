# CYRENE Advanced Services

Private advanced-service and plugin staging repository for the CYRENE software
matrix. This repository is intended for Azure DevOps. The reusable core,
framework contracts, and SDKs belong in the separate `cyrene-core` GitHub
organization repository.

## First-party services

| Service | Product responsibility |
| --- | --- |
| `services/catalyst` | Data preparation, labeling, vectorization, and knowledge extraction |
| `services/yield` | Training, fine-tuning, checkpoints, and model-weight production |
| `services/reactor` | Quantization, deployment, acceleration, and inference |
| `services/exchange` | Workflow orchestration, API gateway, permissions, and enterprise services |
| `services/navigator` | Cross-device UI, approvals, and user-facing secure execution |
| `services/echo` | Evaluation, scoring, feedback, and feedback enhancement |

`plugins/` contains vendor adapters, runtime builders, compatibility rules, and
the preserved enterprise bundle that do not belong to exactly one product.

Every service has a `service.json` manifest validated by tooling checked out
from the core repository. The current source layout is a preservation checkpoint;
legacy imports and build files are intentionally not rewritten yet.

The root Azure pipeline is a source-preservation and contract gate only. It
does not claim that legacy services build, deploy, or pass product acceptance.
Each `legacy` directory has an archive notice; its internal commands and status
claims are historical and must not be treated as current documentation.

This clean-history snapshot excludes generated build output, dependency caches,
local IDE/assistant state, and two third-party research archives. Those archives
remain untouched in the original migration checkout and are not product source.

See [the split inventory](docs/SPLIT_INVENTORY.md).
