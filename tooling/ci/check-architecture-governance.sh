#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

failed=0

required_files=(
  "docs/adr/ADR-CORE-REPOSITORY-GOVERNANCE.md"
  "docs/adr/ADR-HARDWARE-ADAPTER-BOUNDARY.md"
  "docs/adr/ADR-PLUGIN-EXECUTION-BOUNDARY.md"
  "docs/adr/ADR-LEGACY-CY-LLM-CUTOVER.md"
)

for path in "${required_files[@]}"; do
  if [[ ! -f "$path" ]]; then
    echo "missing required governance file: $path"
    failed=1
  fi
done

if ! grep -q "Superseded / Historical" docs/adr/ADR-PLUGIN-RUNTIME.md; then
  echo "ADR-PLUGIN-RUNTIME must remain historical and superseded"
  failed=1
fi

if rg -n --fixed-strings "in-proc-rust" contracts/rust/cy-manifest/src/manifest/plugin.rs; then
  echo "Platform manifest model exposes in-proc-rust"
  failed=1
fi

# Only canonical runtime and contract source participates in the cutover guard.
# Product migration snapshots and deployment images are no longer owned by this
# repository, so there is no Platform-side compatibility exclusion.
# 仅扫描当前权威运行时与契约源码；产品迁移快照和部署镜像已不归本仓库所有，
# 因此 Platform 侧不再保留 compatibility 排除项。
source_roots=(contracts kernel framework runtime sdk adapters agents)
if [[ -d infrastructure ]]; then
  source_roots+=(infrastructure)
fi

mapfile -t legacy_refs < <(
  rg -l --hidden \
    -g '!target/**' \
    -g '!.git/**' \
    -g '!**/*.md' \
    -g '!tooling/ci/check-architecture-governance.sh' \
    -g '!tooling/migration/**' \
    'cy\.llm|AgentService|AiService|ai_service\.proto|agent_service\.proto' \
    "${source_roots[@]}" || true
)

while IFS= read -r path; do
  [[ -z "$path" ]] && continue
  normalized="${path//\\//}"
  normalized="${normalized#./}"
  echo "legacy cy.llm reference in runtime or contract source: $normalized"
  failed=1
done < <(printf '%s\n' "${legacy_refs[@]}")

if [[ "$failed" -ne 0 ]]; then
  exit 1
fi

echo "architecture governance checks passed"
