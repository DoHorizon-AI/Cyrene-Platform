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

# Platform owns only units for its own daemons under infrastructure/. Product
# deployment templates, edge configuration, observability dashboards and
# compatibility snapshots must stay with the consuming repository.
non_platform_infrastructure=$(
  git ls-files 'infrastructure/**' | grep -v '^infrastructure/systemd/' || true
)
if [ -n "$non_platform_infrastructure" ]; then
  echo "FORBIDDEN: consumer-owned infrastructure tracked by Platform:"
  echo "$non_platform_infrastructure" | sed 's/^/  - /'
  status=1
fi

forbidden_owned_paths=(
  "contracts/registries/component-catalog.v1.json"
  "contracts/tck/astrbot-capability-worker/**"
  "kernel/crates/cy-kernel-daemon/tests/*astrbot*"
  "tooling/workspace/**"
  "tooling/ci/integration.py"
  "tooling/ci/tests/test_integration.py"
  "tooling/ci/check_api_documentation.py"
  "tooling/ci/test_check_api_documentation.py"
  ".github/workflows/cross-repo.yml"
  "framework/crates/cy-platform-api/tests/real_media_worker_test.rs"
  "framework/crates/cy-capability-execution-service/tests/plugins_model_provider_tck.rs"
  "framework/crates/cy-platform-api/src/media.rs"
  "sdk/python/cyrene_control_plane/**"
  "sdk/python/cyrene_preflight/src/cyrene_preflight/reference.py"
  "framework/jvm/application/**"
  "framework/jvm/domain/**"
  "framework/jvm/bootstrap/**"
)
for glob in "${forbidden_owned_paths[@]}"; do
  matches=$(git ls-files "$glob")
  if [ -n "$matches" ]; then
    echo "FORBIDDEN: consumer-owned catalog, adapter, or integration test:"
    echo "$matches" | sed 's/^/  - /'
    status=1
  fi
done

# The implemented v0 typed registry is migration-only. Keep it buildable, but
# prevent it from becoming a dependency of another production crate.
registry_consumers=$(
  git grep -n 'cy-extension-registry' -- \
    'framework/crates/*/Cargo.toml' \
    'kernel/crates/*/Cargo.toml' 2>/dev/null \
    | grep -v '^framework/crates/cy-extension-registry/Cargo.toml:' || true
)
if [ -n "$registry_consumers" ]; then
  echo "FORBIDDEN: new production dependency on MIGRATING cy-extension-registry:"
  echo "$registry_consumers" | sed 's/^/  - /'
  status=1
fi

local_transport_consumers=$(
  git grep -n 'cy-local-transport' -- \
    'framework/crates/*/Cargo.toml' \
    'kernel/crates/*/Cargo.toml' 2>/dev/null \
    | grep -v '^framework/crates/cy-local-transport/Cargo.toml:' || true
)
if [ -n "$local_transport_consumers" ]; then
  echo "FORBIDDEN: new production dependency on MIGRATING cy-local-transport:"
  echo "$local_transport_consumers" | sed 's/^/  - /'
  status=1
fi

capability_specific_adapters=$(
  git grep -n -E 'WorkerMediaProcessor|def _build_request_object' -- \
    'framework/**' 'sdk/**' 2>/dev/null || true
)
if [ -n "$capability_specific_adapters" ]; then
  echo "FORBIDDEN: capability-specific execution adapter tracked by Platform:"
  echo "$capability_specific_adapters" | sed 's/^/  - /'
  status=1
fi

product_preflight_contracts=$(
  git grep -n -E 'class (ModelAnalyzer|CompatibilityEvaluator|ModelAnalysisRequest|ModelFacts|VramEstimate|CompatibilityRequest)' -- \
    'sdk/python/cyrene_preflight/**' 2>/dev/null || true
)
if [ -n "$product_preflight_contracts" ]; then
  echo "FORBIDDEN: Product model/preflight contract tracked by Platform Python SDK:"
  echo "$product_preflight_contracts" | sed 's/^/  - /'
  status=1
fi

frozen_v0_contracts=(
  "contracts/schemas/plugin.schema.json"
  "contracts/schemas/manifests/artifact_manifest.schema.json"
  "contracts/schemas/manifests/checkpoint_metadata.schema.json"
  "contracts/schemas/manifests/hardware_manifest.schema.json"
  "contracts/schemas/manifests/model_manifest.schema.json"
  "contracts/schemas/manifests/runtime_manifest.schema.json"
  "contracts/schemas/manifests/training_revision.schema.json"
  "contracts/schemas/manifests/validation_result.schema.json"
  "contracts/schemas/manifests/why_report.schema.json"
  "contracts/schemas/manifests/workload_request.schema.json"
)
for file in "${frozen_v0_contracts[@]}"; do
  if ! rg -q 'MIGRATING_COMPATIBILITY' "$file"; then
    echo "FORBIDDEN: frozen v0 contract lost its migration marker: $file"
    status=1
  fi
done

# Platform contracts and examples must remain consumer-neutral. These markers
# identify concrete Product or connector bindings that have their own owners.
consumer_markers='cyrene\.astrbot|onebot\.v11|CYRENE_TEXT_LIFECYCLE|com\.cyrene\.service\.(catalyst|yield|reactor|exchange|navigator|echo)'
consumer_marker_matches=$(
  git grep -n -i -E "$consumer_markers" -- \
    'contracts/proto/**' \
    'contracts/rust/**' \
    'contracts/schemas/**' \
    'examples/**' \
    'framework/**' \
    'kernel/**' \
    'sdk/**' \
    'tooling/**' \
    ':(exclude)tooling/ci/check-no-legacy-surface.sh' || true
)
if [ -n "$consumer_marker_matches" ]; then
  echo "FORBIDDEN: consumer-specific identity leaked into Platform source or contracts:"
  echo "$consumer_marker_matches" | sed 's/^/  - /'
  status=1
fi

if [ "$status" -eq 0 ]; then
  echo "OK: no legacy surface or consumer-owned Platform adaptation is tracked."
fi
exit "$status"
