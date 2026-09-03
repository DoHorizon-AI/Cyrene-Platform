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
    contracts/rust/cy-manifest/src/manifest/plugin.rs; then
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

# Only canonical runtime and contract source participates in the cutover guard.
# Historical migration snapshots and compatibility-only enterprise images retain
# the frozen v0 names by design and are audited by their own migration checks.
# 仅扫描当前权威运行时与契约源码；迁移快照及兼容性企业镜像按设计保留冻结
# 的 v0 名称，并由各自迁移检查负责审计。
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
    -g '!infrastructure/**/enterprise/**' \
    -g '!infrastructure/**/enterprise-dockerfiles/**' \
    -g '!infrastructure/**/compatibility/**' \
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
