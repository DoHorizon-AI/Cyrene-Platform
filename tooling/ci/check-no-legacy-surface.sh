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
  "astrbot/*|astrbot package namespace (legacy AstrBot source)"
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
  "framework/jvm/**"
  "framework/crates/cy-extension-registry/**"
  "framework/crates/cy-local-transport/**"
  "framework/crates/cy-platform-api/src/builtin.rs"
  "framework/crates/cy-capability-execution-service/**"
  "framework/crates/cy-platform-api/src/worker.rs"
  "sdk/python/cyrene_capability_client/**"
  "sdk/python/cyrene_worker_shim/**"
  "sdk/rust/cy-worker-sdk/**"
  "contracts/proto/cyrene/capability/**"
  "contracts/proto/plugin/**"
  "contracts/rust/cy-plugin-protocol/**"
  "tck/capability-execution-service/**"
  "contracts/proto/cyrene/model/**"
  "contracts/proto/cyrene/message/**"
  "contracts/rust/cy-proto/src/model_provider.rs"
  "contracts/rust/cy-proto/src/message_connector.rs"
  "contracts/rust/cy-proto/tests/model_provider_*"
  "contracts/rust/cy-proto/tests/message_connector_*"
  "sdk/python/cyrene_capability_client/src/cyrene_capability_client/model_provider_v1.py"
  "sdk/python/cyrene_capability_client/src/cyrene_capability_client/_generated/model_provider_pb2.py"
  "sdk/python/cyrene_environment/**"
  "contracts/schemas/manifests/model_version.schema.json"
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
  "contracts/schemas/media-processor-v1.schema.json"
  "contracts/schemas/advanced-service.schema.json"
  "contracts/schemas/component-catalog.schema.json"
  "contracts/schemas/service.schema.json"
  "contracts/rust/cy-manifest/src/manifest/hardware.rs"
  "contracts/rust/cy-manifest/src/manifest/model.rs"
  "contracts/rust/cy-manifest/src/manifest/runtime.rs"
  "contracts/rust/cy-manifest/src/manifest/training.rs"
  "docs/PLUGIN_SPEC.md"
  "docs/contracts/media-processor-v1.md"
  "docs/flows/training-end-to-end.md"
  "docs/contracts/model-version-composed-v1.md"
  "tck/model-provider-embedding-contract/**"
  "tck/message-connector-contract/**"
  "examples/plugins/jvm/poc/**"
)
for glob in "${forbidden_owned_paths[@]}"; do
  matches=$(git ls-files "$glob")
  if [ -n "$matches" ]; then
    echo "FORBIDDEN: consumer-owned catalog, adapter, or integration test:"
    echo "$matches" | sed 's/^/  - /'
    status=1
  fi
done

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

product_model_contracts=$(
  git grep -n -E 'class (ModelVersion|ArtifactManifest|ArtifactLineage)|MODEL_VERSION_SCHEMA_VERSION|MODEL_VERSION_URI_PREFIX' -- \
    'sdk/python/cyrene_artifacts/**' 2>/dev/null || true
)
if [ -n "$product_model_contracts" ]; then
  echo "FORBIDDEN: Product-owned manifest or lineage contract returned to Platform Artifact SDK:"
  echo "$product_model_contracts" | sed 's/^/  - /'
  status=1
fi

if ! python3 -c 'import json, pathlib; schema=json.loads(pathlib.Path("contracts/schemas/artifact_transfer.schema.json").read_text(encoding="utf-8")); kind=schema["$defs"]["artifactIdentity"]["properties"]["kind"]; assert kind.get("type") == "string"; assert "enum" not in kind; assert kind.get("minLength") == 1; assert kind.get("maxLength") == 128; assert kind.get("pattern")'; then
  echo "FORBIDDEN: Artifact transfer schema closed the producer-owned kind taxonomy"
  status=1
fi

product_manifest_symbols='(HardwareManifest|ModelManifest|RuntimeManifest|TrainingRevision|CheckpointMetadata|WhyReport|WorkloadRequest|ValidationResult|ArtifactManifest|ArtifactLineage)'
product_manifest_matches=$(
  git grep -n -E "$product_manifest_symbols" -- \
    'contracts/rust/cy-manifest/**' \
    'sdk/python/cyrene_artifacts/**' 2>/dev/null || true
)
if [ -n "$product_manifest_matches" ]; then
  echo "FORBIDDEN: Product-owned AI or lifecycle manifest returned to Platform contracts:"
  echo "$product_manifest_matches" | sed 's/^/  - /'
  status=1
fi

platform_owned_plugin_runtime=$(
  git grep -n -E 'cyrene_plugin_runtime|cyrene_worker(_shim)?|PythonVenvDependencyPreparer|requirements\.lock|PYTHONPATH|PYTHONDONTWRITEBYTECODE|uv (venv|pip)' -- \
    'framework/crates/cy-package-runtime/src/**' 2>/dev/null || true
)
if [ -n "$platform_owned_plugin_runtime" ]; then
  echo "FORBIDDEN: Platform Package Runtime hard-codes a language or Plugin-owned runtime:"
  echo "$platform_owned_plugin_runtime" | sed 's/^/  - /'
  status=1
fi

legacy_plugin_manifest_symbols='(PluginCapabilitiesManifest|PluginPackageManifest|PluginDependenciesManifest|PluginComponentsManifest|DescriptorImplementation.*entrypoint)'
legacy_plugin_manifest_matches=$(
  git grep -n -E "$legacy_plugin_manifest_symbols" -- \
    'contracts/rust/cy-manifest/**' \
    'framework/crates/cy-package-runtime/src/**' 2>/dev/null || true
)
if [ -n "$legacy_plugin_manifest_matches" ]; then
  echo "FORBIDDEN: legacy implementation-specific Plugin manifest model returned to Platform:"
  echo "$legacy_plugin_manifest_matches" | sed 's/^/  - /'
  status=1
fi

named_spi_protos=(
  "contracts/proto/plugin/v1/compat_rule.proto"
  "contracts/proto/plugin/v1/execution_engine.proto"
  "contracts/proto/plugin/v1/gateway_filter.proto"
  "contracts/proto/plugin/v1/model_analyzer.proto"
  "contracts/proto/plugin/v1/notification.proto"
  "contracts/proto/plugin/v1/probe.proto"
  "contracts/proto/plugin/v1/quantization.proto"
  "contracts/proto/plugin/v1/runtime_builder.proto"
  "contracts/proto/plugin/v1/storage.proto"
  "contracts/proto/plugin/v1/training_backend.proto"
)
for file in "${named_spi_protos[@]}"; do
  if [[ -e "$file" ]]; then
    echo "FORBIDDEN: removed capability payload contract returned to Platform: $file"
    status=1
  fi
done

named_spi_symbols='pub trait (Probe|ModelAnalyzer|CompatRule|RuntimeBuilder|ExecutionEngine|TrainingBackend|Quantization|GatewayFilter|Notification|Storage)|BuiltinInMemoryStorage'
named_spi_matches=$(git grep -n -E "$named_spi_symbols" -- 'framework/**' 'kernel/**' 'sdk/**' 2>/dev/null || true)
if [ -n "$named_spi_matches" ]; then
  echo "FORBIDDEN: removed named capability SPI returned to Platform:"
  echo "$named_spi_matches" | sed 's/^/  - /'
  status=1
fi

endpoint_payload_fields=$(
  sed -n '/^message Endpoint {/,/^}/p' contracts/proto/cyrene/semantic/v1/kernel_contract.proto \
    | tail -n +2 \
    | rg -n -i '^[[:space:]]*(optional[[:space:]]+)?(bytes|string)[[:space:]]+(payload|request|response|body|prompt|message)[[:space:]]*=' || true
)
if [ -n "$endpoint_payload_fields" ]; then
  echo "FORBIDDEN: control-plane Endpoint contains a business payload field:"
  echo "$endpoint_payload_fields" | sed 's/^/  - /'
  status=1
fi

if ! rg -q 'Capability payload contracts, generated capability SDKs, or capability TCKs' repository-policy.yaml; then
  echo "FORBIDDEN: repository policy no longer excludes capability payload authority"
  status=1
fi

package_runtime_data_plane=$(
  git grep -n -E 'RuntimeInvocationResult|RuntimeApplicationEvent|invoke_typed|payload_base64|payload_type_url|ControlCommand::(Invoke|Subscribe|NextEvent|Unsubscribe)' -- \
    'framework/crates/cy-package-runtime/**' \
    ':(exclude)framework/crates/cy-package-runtime/README.md' 2>/dev/null || true
)
if [ -n "$package_runtime_data_plane" ]; then
  echo "FORBIDDEN: Platform Package Runtime contains a business data-plane operation:"
  echo "$package_runtime_data_plane" | sed 's/^/  - /'
  status=1
fi

if ! rg -q 'opaque `connection_ref`' framework/crates/cy-package-runtime/README.md; then
  echo "FORBIDDEN: package runtime no longer documents its opaque connection-only boundary"
  status=1
fi

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

product_name_matches=$(
  git grep -n -E '\b(AstrBot|Catalyst|Yield|Reactor|Exchange|Navigator|Echo)\b' -- \
    'contracts/proto/**' \
    'contracts/rust/**' \
    'contracts/schemas/**' \
    'examples/**' \
    'framework/**' \
    'kernel/**' \
    'sdk/**' || true
)
if [ -n "$product_name_matches" ]; then
  echo "FORBIDDEN: Product name leaked into Platform source or contracts:"
  echo "$product_name_matches" | sed 's/^/  - /'
  status=1
fi

if [ "$status" -eq 0 ]; then
  echo "OK: no legacy surface or consumer-owned Platform adaptation is tracked."
fi
exit "$status"
