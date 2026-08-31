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

if rg -n --fixed-strings "in-proc-rust" \
    contracts/schemas/plugin.schema.json \
    contracts/rust/cy-manifest/src/manifest; then
  echo "installable schema or Rust manifest model exposes in-proc-rust"
  failed=1
fi

if rg -n '^\|[^|]*in-proc-rust[^|]*\|' docs/PLUGIN_SPEC.md; then
  echo "PLUGIN_SPEC still lists in-proc-rust as a runtime"
  failed=1
fi

if rg -n --fixed-strings '"crate"' contracts/schemas/plugin.schema.json; then
  echo "plugin schema still exposes the installable crate field"
  failed=1
fi

set +e
legacy_output="$({
  rg -l --hidden \
    -g '!target/**' \
    -g '!.git/**' \
    -g '!**/*.md' \
    -g '!tooling/ci/check-architecture-governance.sh' \
    'cy\.llm|AgentService|AiService|ai_service\.proto|agent_service\.proto' \
    contracts kernel framework runtime adapters agents sdk
})"
legacy_status=$?
set -e

if [[ "$legacy_status" -gt 1 ]]; then
  echo "failed to scan runtime and contract sources for legacy references"
  exit "$legacy_status"
fi

mapfile -t legacy_refs <<< "$legacy_output"

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
