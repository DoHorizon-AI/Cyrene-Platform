#!/usr/bin/env bash
# check-no-legacy-surface.sh
#
# Legacy-reintroduction guard (see ADR-LEGACY-CY-LLM-CUTOVER.md).
# Fails if committed source reintroduces legacy/archive/backup surface patterns.
#
# Exit 0 = clean; Exit 1 = forbidden pattern found.
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
cd "$repo_root"

# Patterns that must never reappear as committed paths.
# Format: <glob>|<human reason>
PATTERNS=(
  "legacy/*|legacy/ directory (dead/obsolete source)"
  "archive/*|top-level archive/ directory (archived stubs)"
  "*/test_legacy_*.py|dead legacy test"
  "*/plugin.legacy.toml|legacy plugin manifest"
  "*/LEGACY_PLUGIN.md|legacy plugin marker"
  "*.legacy.toml|legacy toml"
)

status=0
while IFS='|' read -r glob reason; do
  [ -z "$glob" ] && continue
  matches=$(git ls-files --error-unmatch "$glob" 2>/dev/null || true)
  if [ -n "$matches" ]; then
    echo "FORBIDDEN legacy surface reintroduced: $glob ($reason)"
    echo "$matches" | sed 's/^/  - /'
    status=1
  fi
done < <(printf '%s\n' "${PATTERNS[@]}")

# legacy-requirements/ is allowed ONLY inside the two enterprise Dockerfiles.
# Flag it anywhere else.
lr=$(git ls-files --error-unmatch 'legacy-requirements/*' 2>/dev/null || true)
if [ -n "$lr" ]; then
  echo "FORBIDDEN: legacy-requirements/ found outside enterprise Dockerfiles:"
  echo "$lr" | sed 's/^/  - /'
  status=1
fi

if [ "$status" -eq 0 ]; then
  echo "OK: no legacy/archive/backup surface patterns in committed source."
fi
exit "$status"
