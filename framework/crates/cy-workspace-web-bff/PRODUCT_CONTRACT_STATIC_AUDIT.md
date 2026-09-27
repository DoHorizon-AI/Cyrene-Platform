# Static Product catalog audit

This is a read-only contract inventory dated 2026-09-26. It is not a test,
release approval, runtime result, or deployable bundle. The inspected smoke
artifact at `/tmp/cyrene-product-contract-bundle-pin-smoke` contains 31 files
and an older TCK digest (`7abe8405ab2da0b8b879e0d2d3122dfa9b40476c74fe6ce4d0306929d7493f6b`).
Its Product pins predate the private operation mappings below. The current BFF
loader rejects that artifact because its canonical TCK digest does not match
this branch. The bundle builder must rebuild from clean owner commits and the
current canonical TCK before catalog startup can succeed.

The current TCK has 13 required rows. Rows 01–12 must have safe callable
schemas. Row 13 is explicitly deny-only: its operation mapping remains in the
catalog for provenance, while its open body and response schemas are not
compiled or dispatchable. A missing row or source operation blocks the catalog.

| Row | Current TCK selection | Owner service contract evidence | Static smoke finding / remaining gate |
| --- | --- | --- | --- |
| 01 | Catalyst `workspaceListDatasets`, `workspace-internal.openapi.yaml` | Clean owner SHA `8aa3432fc17fb0b1de50a4e0d5eb0ca37f4f21c9`; `/internal/workspace/v1/datasets`, `WorkspaceServiceBearer`. | The smoke bundle instead selected public `listDatasets` at `/api/v1/datasets` without operation security. It contains no private alias schema; rebuild and pin the clean owner source. |
| 02 | Catalyst `workspaceCreateDataset`, `workspace-internal.openapi.yaml` | Same clean owner SHA; private POST has `WorkspaceServiceBearer` and an owner request/response schema. | The smoke bundle instead selected public `createDataset` without operation security. It cannot prove the private request and response schemas. |
| 03 | Yield `workspaceGetDraft`, `workspace-private.openapi.yaml` | Clean owner SHA `6b90c62675a0b54134512471cf0d1680989cf184`; `/internal/workspace/v1/` route uses `WorkspaceServiceBearer` and returns closed `WorkspaceTrainingDraftProjection`. | The smoke bundle selected public `get_draft...`; its `TrainingDraft` response must not be reused. The new safe schema is not in the smoke pin. |
| 04 | Yield `workspaceStartRun`, `workspace-private.openapi.yaml` | Same clean owner SHA; the private operation retains HTTP `202` and returns closed `WorkspaceTrainingRunProjection`. | The smoke bundle selected public `start_run...`; `TrainingRunResource` includes URI-bearing fields and open `modelVersion`/extension branches. The new safe `202` schema is not in the smoke pin. |
| 05 | Reactor `workspaceListModelImports`, `workspace-private.openapi.yaml` | Clean owner SHA `5f211b3a1eaec05dd14b47f5a2341816f61bbf51`; `/internal/workspace/v1/` route uses `WorkspaceServiceBearer` and returns closed `WorkspaceModelImportProjection` items. | The smoke bundle selected public `list_model_imports...`; legacy `ModelImport` exposes `modelArtifact.uri`. The new safe schema is not in the smoke pin. |
| 06 | Reactor `workspaceCreateModelImport`, `workspace-private.openapi.yaml` | Same clean owner SHA; private command uses `WorkspaceServiceBearer` and returns closed `WorkspaceModelImportProjection`. | The smoke bundle selected public `create_model_import...`; its success response exposes `modelArtifact.uri`. The new safe schema is not in the smoke pin. |
| 07 | Exchange `listWorkspaceGatewayRoutes`, `openapi.yaml` | A safe private alias is present in a dirty owner worktree, but no clean owner SHA has been supplied. It must use the closed route projection without `source.resourceUri`. | The smoke bundle selected public `listGatewayRoutes`; its source projection exposes raw `source.resourceUri`. Do not pin the dirty worktree. |
| 08 | Exchange `createWorkspaceGatewayRouteDraft`, `openapi.yaml` | The required private command body is a closed `sourceEndpoint` selector (`product: reactor`, UUID `endpointId`, positive `resourceVersion`). Exchange checks the exact scoped Reactor endpoint grant and resolves it server-side. A clean owner SHA is pending. | The smoke bundle selected public `create_draft...`, which accepts caller-controlled `source.resourceUri`; its 422 validation body also has open `ctx`. Reject the legacy request and response schemas. |
| 09 | Echo `workspaceGetEvaluationSuite`, `workspace-internal.openapi.yaml` | Clean owner SHA `fcd641832d0d154c18e5ab5811ee7232d6d91fbc` contains the private alias. | The smoke bundle selected public `getEvaluationSuite` without operation security and does not include the private contract file. Rebuild and compile its closed private schema. |
| 10 | Echo `workspaceCreateEvaluationSuite`, `workspace-internal.openapi.yaml` | Same clean owner SHA; private operation is the current TCK target. | The smoke bundle selected public `createEvaluationSuite` without operation security. Rebuild and compile its private request and response schemas. |
| 11 | Navigator `observeWorkspaceSnapshot`, `openapi.yaml` | Clean owner SHA `7d5fce9790f719ea6371338b5f3cab3b0372991c` uses `NavigatorProductBearer` and returns a closed resource summary rather than embedding arbitrary Product JSON. | The smoke pin predates that projection; its response has `views[].sourceUrl` and open `views[].resource`. Rebuild from the clean owner SHA. |
| 12 | Navigator `getWorkspaceSession`, `persistence.openapi.yaml` | Same clean owner SHA; private GET is `/internal/workspace/v1/workspaces/{workspace_id}/sessions/{session_id}` with `NavigatorWorkspaceBearer` and a closed session summary. | The smoke bundle selected the legacy `get_session...` operation, which returns `Snapshot.meta` with `additionalProperties: true`. Do not use it. |
| 13 | Navigator legacy `append_events...`, `persistence.openapi.yaml` | No trusted-writer BFF handoff exists. The row remains provenance-only and ordinary Web `COMMAND` dispatch is denied with BFF-owned 403 before reading the body. | `AppendRequest` requires `writerToken` (1–512 chars), `epoch` (integer ≥0), `batchId` (1–512 chars), and 1–10,000 `events`; each event is an open object (`additionalProperties: true`). The loader verifies the recorded Navigator POST operation but does not compile these request/response schemas. A future writer path also needs exact Workspace scope, writer-token/epoch fencing, and atomic batch replay; `writerToken` must never reach the browser. |

All Product Problem Details schemas in the smoke artifact declare RFC 9457
`type` with `format: uri`; this is accepted only through the loader's exact
closed shared Problem Details schema exception. At runtime the BFF accepts
only `type` and `instance` equal to `about:blank`, a body status equal to the
HTTP status, no `resourceRef` or extension fields, and safe public error text.
Any violation becomes a BFF-owned `502`.

The Exchange `sourceEndpoint` is a command-only, non-navigable request
selector. It is not an output link and does not replace the closed
`ProductResourceReference` required for Product-owned navigable response
references. The selector contains no URL, path, href, or caller-selected
operation; Exchange performs the scoped grant check and fixed Reactor lookup.
