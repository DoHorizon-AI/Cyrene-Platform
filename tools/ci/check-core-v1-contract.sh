#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

buf_bin="${BUF_BIN:-buf}"
if ! command -v "$buf_bin" >/dev/null 2>&1; then
  echo "Buf is required; CI installs the pinned version before this check" >&2
  exit 127
fi

core_root="contracts/proto/cyrene/core/v1"
descriptor="contracts/descriptors/cyrene-core-v1.pb"
stable_descriptor="contracts/descriptors/cyrene-core-v1-stable.pb"
fixture_manifest="contracts/fixtures/core/v1/manifest.json"
tmp_descriptor="$(mktemp)"
trap 'rm -f "$tmp_descriptor"' EXIT

"$buf_bin" lint contracts/proto
"$buf_bin" breaking contracts/proto --against "$descriptor#format=binpb"
(
  cd contracts/proto
  "$buf_bin" build . --path cyrene/core/v1 \
    --as-file-descriptor-set --exclude-source-info --output "$tmp_descriptor"
)

if ! cmp -s "$tmp_descriptor" "$stable_descriptor"; then
  echo "Core v1 stable descriptor drift detected; regenerate the checked-in baseline" >&2
  exit 1
fi

expected_descriptor_sha="$(rg -o '"descriptor_sha256": "[A-F0-9]+' "$fixture_manifest" | sed 's/.*"//')"
actual_descriptor_sha="$(sha256sum "$stable_descriptor" | awk '{print toupper($1)}')"
if [[ "$expected_descriptor_sha" != "$actual_descriptor_sha" ]]; then
  echo "Core v1 fixture manifest descriptor SHA-256 does not match baseline" >&2
  exit 1
fi

if [[ "$(rg -c '^service ' "$core_root/cyrene_core.proto")" -ne 3 ]]; then
  echo "Core v1 must expose exactly three service entry points" >&2
  exit 1
fi

if ! rg -q 'rpc Connect\(stream NodeToControlPlane\)' "$core_root/cyrene_core.proto"; then
  echo "NodeControlService.Connect must remain the outbound bidirectional stream" >&2
  exit 1
fi

if ! rg -q 'idempotency_key|expected_generation' "$core_root" \
    || ! rg -q 'fence_token|generation' "$core_root"; then
  echo "Core v1 mutation, generation, and fencing fields are incomplete" >&2
  exit 1
fi

if ! rg -q 'InstalledPluginRef plugin' "$core_root/kernel_runtime.proto" \
    || ! rg -q 'ResourceRequirements resource_claim|ResourceLeaseRef existing_lease' \
      "$core_root/kernel_runtime.proto"; then
  echo "LaunchPluginRequest must use an installed plugin and opaque resource refs" >&2
  exit 1
fi

if rg -n -i '^[[:space:]]*(string|bytes|repeated[[:space:]]+(string|bytes))[[:space:]]+' \
    "$core_root" | rg -i 'shell|argv|env|model|dataset|checkpoint|stdout|stderr|payload'; then
  echo "Core v1 exposes a forbidden shell, process, environment, or large-artifact field" >&2
  exit 1
fi

echo "Core v1 contract checks passed"
