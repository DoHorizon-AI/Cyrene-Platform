# Source-preserving split inventory

## Decision

The clean workspace contains two independent repositories:

- `cyrene-core`, intended for private-first development in a GitHub organization;
- `cyrene-advanced-services`, intended for private Azure DevOps storage.

The old `Cyrene-Platform` and `CY-Navigator` checkouts remain migration sources
and are not used as the new repositories' Git history.

Source selection was copied into clean-history snapshots. Generated build
output, dependency caches, local IDE/assistant state, and archived third-party
research bundles were deliberately excluded. Preserved source files were not
deleted from the migration checkouts.

## CY-LLM Engine mapping

| Old source | New ownership |
| --- | --- |
| `CY_LLM_Training` | Catalyst legacy data corpus |
| `python/cy_exec` inference/runtime code | Reactor legacy worker |
| `python/cy_exec` training subtree | Yield legacy training worker |
| `cy-gateway`, `cy-proxy`, gateway lite, control plane | Exchange legacy services |
| former Rust control plane and its dependent CLI | Exchange compatibility archive for later Yield/Reactor/Exchange extraction |
| LLaMA-Factory plugin | Yield |
| vLLM and quantization code | Reactor |
| Hugging Face analyzer | Catalyst for the first migration checkpoint |
| NVIDIA probe, Docker+uv builder, compatibility rules | Shared plugins |
| `plugins/pro` | Preserved enterprise bundle |
| old product documents and Python workspace files | `docs/legacy-cy-llm-engine` and `tooling/legacy-python-workspace` |

## Dh / Navigator mapping

The old desktop, mobile, daemon, control-plane, CLI, `dh-core`, research, and
pipeline files are preserved under `services/navigator/legacy-dh`. Existing
uncommitted edits were moved with those files and were not rewritten.

The standalone desktop feedback command and analytics panel were moved to
`services/echo/legacy/navigator-feedback`. Feedback persistence embedded in the
large legacy session store remains in Navigator until a protocol is extracted;
duplicating or partially rewriting that file in this checkpoint would be less
safe than recording the coupling.

## Known cross-boundary couplings

- Yield training code still imports helper modules from the Reactor-era
  `cy_exec` package.
- Navigator still references the two files moved to Echo.
- The enterprise bundle contains mixed Exchange, Yield, Reactor, and Echo code
  and stays intact until its internal contracts are versioned.
- Old Cargo, Gradle, Python, Tauri, Flutter, and pipeline paths still describe
  the pre-split layout.
- The Exchange compatibility archive retains broken original relative workspace
  paths on purpose; it is source for extraction, not an active workspace member.

These are expected migration inputs, not evidence that the split is complete or
currently buildable.

## Next gates

1. Publish versioned core contract artifacts from a private GitHub repository.
2. Replace legacy direct imports with control/event/data-plane clients.
3. Give each service an independent build and contract test.
4. Split the enterprise bundle only after each extracted component has a test.
5. Remove legacy folders only after byte inventory, behavior parity, and owner
   sign-off.

## Submission exclusions

- `target`, `node_modules`, Gradle/Python caches, and compiled classes;
- local `.idea`, `.claude`, and `.workbuddy` state;
- `services/navigator/legacy-dh/research/*.7z` third-party research archives.

These exclusions remove no product source from the migration checkouts.
