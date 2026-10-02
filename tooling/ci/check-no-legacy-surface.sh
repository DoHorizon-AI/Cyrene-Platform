#!/usr/bin/env bash
# check-no-legacy-surface.sh
#
# Legacy and repository-boundary guard (see ADR-LEGACY-CY-LLM-CUTOVER.md).
# Fails if committed source reintroduces retired surfaces or consumer-owned work.
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

product_name_candidates=$(
  git grep -n -E '\b(AstrBot|Catalyst|Yield|Reactor|Exchange|Navigator|Echo)\b' -- \
    'contracts/proto/**' \
    'contracts/rust/**' \
    'contracts/schemas/**' \
    'examples/**' \
    'framework/**' \
    'kernel/**' \
    'sdk/**' || true
)

workspace_app_host_product_name_paths=(
  "framework/crates/cy-workspace-control-plane/src/caller.rs"
  "framework/crates/cy-workspace-control-plane/src/product_authorization.rs"
  "framework/crates/cy-workspace-control-plane/src/product_projection.rs"
  "framework/crates/cy-workspace-product-adapters/src/endpoint_manifest.rs"
  "framework/crates/cy-workspace-product-adapters/src/lib.rs"
  "framework/crates/cy-workspace-web-bff/README.md"
  "framework/crates/cy-workspace-web-bff/src/catalog_loader.rs"
)

# Product names are allowed only in Workspace caller validation, the v2 wire
# projection, Product endpoint catalog metadata, and direct catalog docs.
# This exact-file list does not exempt any source from the semantic checks below.
is_workspace_app_host_product_name_path() {
  local candidate="$1"
  local allowed_path
  for allowed_path in "${workspace_app_host_product_name_paths[@]}"; do
    if [[ "$candidate" == "$allowed_path" ]]; then
      return 0
    fi
  done
  return 1
}

product_name_matches=""
while IFS= read -r match; do
  [ -z "$match" ] && continue
  candidate_path="${match%%:*}"
  if is_workspace_app_host_product_name_path "$candidate_path"; then
    continue
  fi
  if [ -n "$product_name_matches" ]; then
    product_name_matches+=$'\n'
  fi
  product_name_matches+="$match"
done <<< "$product_name_candidates"

if [ -n "$product_name_matches" ]; then
  echo "FORBIDDEN: Product name leaked into Platform source or contracts:"
  echo "$product_name_matches" | sed 's/^/  - /'
  status=1
fi

# The retained Workspace hosts may not add Product-owned state or persistence.
# Product v2 operation identifiers remain data from pinned owner catalogs.
# 保留的 Workspace host 不得新增 Product 自有状态或持久化。
product_private_state_pattern='(CREATE[[:space:]]+TABLE|ALTER[[:space:]]+TABLE|sqlx::|rusqlite::|diesel::|sea_orm::|Product[A-Z][[:alnum:]]*(State|Store|Repository|Persistence|Database|Migration|Lifecycle|Workflow|Entity)|((Catalyst|Echo|Exchange|Navigator|Reactor|Yield)[A-Z][[:alnum:]]*(State|Store|Repository|Persistence|Database|Migration|Lifecycle|Workflow|Entity)|(^|[^[:alnum:]_])(product|catalyst|echo|exchange|navigator|reactor|yield)_[[:alnum:]_]*(runs?|attempts?|workflows?|lifecycles?|states?|stores?|repositories?|persistence|databases?|migrations?|events?)($|[^[:alnum:]_]))'
mapfile -t workspace_product_adapter_sources < <(
  git ls-files 'framework/crates/cy-workspace-product-adapters/src/*.rs'
)
mapfile -t workspace_bff_sources < <(
  git ls-files 'framework/crates/cy-workspace-web-bff/src' | grep '\.rs$' || true
)
workspace_host_semantic_sources=(
  "framework/crates/cy-workspace-control-plane/src/caller.rs"
  "framework/crates/cy-workspace-control-plane/src/product_authorization.rs"
  "framework/crates/cy-workspace-control-plane/src/product_projection.rs"
  "framework/crates/cy-workspace-product-contracts/src/bundle.rs"
  "framework/crates/cy-workspace-product-contracts/src/invocation.rs"
  "framework/crates/cy-workspace-product-contracts/src/policy.rs"
  "${workspace_product_adapter_sources[@]}"
  "framework/crates/cy-workspace-connector-host/src/main.rs"
  "${workspace_bff_sources[@]}"
)
product_private_state_matches=$(
  git grep -n -i -E "$product_private_state_pattern" -- \
    "${workspace_host_semantic_sources[@]}" 2>/dev/null || true
)
if [ -n "$product_private_state_matches" ]; then
  echo "FORBIDDEN: Workspace host glue contains Product-private state or persistence:"
  echo "$product_private_state_matches" | sed 's/^/  - /'
  status=1
fi

# Reject new Product-shaped data declarations even when they use neutral names
# such as `NavigatorSession` instead of a `Product*State` suffix. The listed
# symbols are the existing bounded Workspace wire/HTTP contracts and read-only
# Navigator view types; adding another Product DTO/schema requires review.
workspace_product_private_type_pattern='^[[:space:]]*(pub([[:space:]]*\([^)]*\))?[[:space:]]+)?(struct|enum|type|trait)[[:space:]]+((Catalyst|Echo|Exchange|Navigator|Reactor|Yield)[[:upper:]][[:alnum:]_]*|[[:alnum:]_]*Product[[:alnum:]_]*(State|Session|Store|Repository|Persistence|Database|Migration|Lifecycle|Workflow|Entity|Dto|DTO|Schema|Record|Event|Snapshot|Request|Response|Model|Payload|Document|Cache|Summary|View|Metadata))([[:space:]<{(:;=]|$)'
workspace_product_private_type_candidates=$(
  git grep -n -E "$workspace_product_private_type_pattern" -- \
    "${workspace_host_semantic_sources[@]}" 2>/dev/null || true
)
is_existing_workspace_host_product_type() {
  case "$1" in
    CatalystEchoProductApiAdapter| \
    ProductHttpRequest|ProductHttpResponse| \
    ProductInvocationRequest|ProductInvocationResponse| \
    ProductJsonSchema|ProductMetadata|ProductView|WorkspaceProductRequest| \
    CompiledProductSchema)
      return 0
      ;;
    *)
      return 1
      ;;
  esac
}
workspace_product_private_type_matches=""
while IFS= read -r match; do
  [ -z "$match" ] && continue
  declared_type=$(printf '%s\n' "$match" | sed -E \
    's/^[^:]+:[0-9]+:[[:space:]]*(pub([[:space:]]*\([^)]*\))?[[:space:]]+)?(struct|enum|type|trait)[[:space:]]+([[:alnum:]_]+).*/\4/')
  if is_existing_workspace_host_product_type "$declared_type"; then
    continue
  fi
  workspace_product_private_type_matches+="$match"$'\n'
done <<< "$workspace_product_private_type_candidates"
if [ -n "$workspace_product_private_type_matches" ]; then
  echo "FORBIDDEN: Workspace host glue declares new Product-specific state, DTO, or schema types:"
  printf '%s' "$workspace_product_private_type_matches" | sed 's/^/  - /'
  status=1
fi

# Product v2 must remain catalog- and policy-driven. Platform may bind the
# verified caller and Workspace scope, but it may not duplicate owner grants,
# business routes, or operation dispatch in this host crate.
workspace_product_auth_source="framework/crates/cy-workspace-control-plane/src/product_authorization.rs"
workspace_product_projection_source="framework/crates/cy-workspace-control-plane/src/product_projection.rs"
workspace_product_http_source="framework/crates/cy-workspace-product-adapters/src/http.rs"
if ! rg -q '[.]authorize\(bundle, invocation, &principal, scope\)' "$workspace_product_auth_source" \
  || ! rg -q 'TrustedProductPolicy' "$workspace_product_auth_source" \
  || ! rg -q 'ProductApiInvocationV2' "$workspace_product_projection_source" \
  || ! rg -q 'request[.]route\(\)' "$workspace_product_http_source" \
  || ! rg -q 'route[.]path_template\(\)' "$workspace_product_http_source"; then
  echo "FORBIDDEN: Workspace Product v2 must resolve authorization and routes from the pinned contract bundle."
  status=1
fi

hardcoded_workspace_product_dispatch=$(
  git grep -n -E 'WorkspaceProductApiOperation[0-9]+|Owner::(Catalyst|Yield|Reactor|Exchange|Echo|Navigator)|workspace[.]product[.]command[.]' -- \
    "$workspace_product_auth_source" "$workspace_product_projection_source" \
    "framework/crates/cy-workspace-product-adapters/src" 2>/dev/null || true
)
if [ -n "$hardcoded_workspace_product_dispatch" ]; then
  echo "FORBIDDEN: Workspace Product owner or operation policy is hard-coded in Platform dispatch:"
  echo "$hardcoded_workspace_product_dispatch" | sed 's/^/  - /'
  status=1
fi

# Keep the current BFF routes explicit; a new endpoint requires a boundary review.
# BFF 路由固定为当前 identity、Directory、审批、health 与闭合 Product operation 面。
expected_workspace_bff_routes=(
  "/api/workspace/v1/session"
  "/api/workspace/v1/workspaces"
  "/api/workspace/v2/workspaces/:workspace_id/products/invocations"
  "/api/workspace/v1/device-authorizations/approval-challenges"
  "/api/workspace/v1/device-authorizations/approval-challenges/:approval_id/complete"
  "/api/workspace/v1/device-authorizations/denials"
  "/credential-registration"
  "/healthz"
  "/readyz"
)
expected_workspace_bff_routes_text=$(
  printf '%s\n' "${expected_workspace_bff_routes[@]}" | LC_ALL=C sort
)
actual_workspace_bff_routes=$(
  rg --no-filename --no-line-number --only-matching --multiline \
    --replace '$2' '\.(route|route_service|nest|nest_service)\([[:space:]]*"([^"]+)"' \
    framework/crates/cy-workspace-web-bff/src -g '*.rs' | LC_ALL=C sort || true
)
if [ "$actual_workspace_bff_routes" != "$expected_workspace_bff_routes_text" ]; then
  echo "FORBIDDEN: Workspace BFF route surface changed without an explicit boundary review."
  echo "  Expected routes:"
  printf '%s\n' "$expected_workspace_bff_routes_text" | sed 's/^/  - /'
  echo "  Tracked routes:"
  printf '%s\n' "$actual_workspace_bff_routes" | sed 's/^/  - /'
  status=1
fi

if [ "$status" -eq 0 ]; then
  echo "OK: no legacy surface or unapproved consumer-owned Platform adaptation is tracked."
fi
exit "$status"
