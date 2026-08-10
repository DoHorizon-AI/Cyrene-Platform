#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

semantic_proto="contracts/proto/cyrene/semantic/v1/kernel_contract.proto"
semantic_rust="contracts/rust/cy-kernel-contract/src/lib.rs"
descriptor="contracts/descriptors/cyrene-kernel-semantic-v1.pb"
stable_descriptor="contracts/descriptors/cyrene-kernel-semantic-v1-stable.pb"
tmp_descriptor="$(mktemp)"
trap 'rm -f "$tmp_descriptor"' EXIT

for path in "$semantic_proto" "$semantic_rust" \
  "docs/contracts/kernel-semantic-contract-v1.md"; do
  if [[ ! -f "$path" ]]; then
    echo "missing Kernel semantic contract artifact: $path" >&2
    exit 1
  fi
done

if rg -n -i 'cuda|rocm|nvidia|amd|docker|python|jvm|tensor|tokenizer|dataset|model|pid|cgroup|file_descriptor|native_handle' \
  "$semantic_proto"; then
  echo "Kernel semantic Proto contains an implementation, vendor, or product term" >&2
  exit 1
fi

if rg -n 'google\.protobuf\.(Any|Struct)|\b(float|double)\b|^service ' "$semantic_proto"; then
  echo "Kernel semantic Proto contains an unbounded value or transport service" >&2
  exit 1
fi

for noun in Principal Provider Resource Lease Worker Operation Capability Endpoint Event; do
  if ! rg -q "^(message|enum) ${noun}( |\\{)" "$semantic_proto"; then
    echo "Kernel semantic noun is missing: $noun" >&2
    exit 1
  fi
done

if ! rg -q '^#!\[forbid\(unsafe_code\)\]' "$semantic_rust"; then
  echo "Rust semantic projection must forbid unsafe code" >&2
  exit 1
fi

buf_bin="${BUF_BIN:-buf}"
if ! command -v "$buf_bin" >/dev/null 2>&1; then
  echo "Buf is required; CI installs the pinned version before this check" >&2
  exit 127
fi

"$buf_bin" lint contracts/proto
"$buf_bin" breaking contracts/proto --against "$descriptor#format=binpb"
(
  cd contracts/proto
  "$buf_bin" build . --path cyrene/semantic/v1 \
    --as-file-descriptor-set --exclude-source-info --output "$tmp_descriptor"
)

if [[ ! -f "$stable_descriptor" ]] || ! cmp -s "$tmp_descriptor" "$stable_descriptor"; then
  echo "Kernel semantic v1 descriptor drift detected; regenerate the checked-in baseline" >&2
  exit 1
fi

echo "Kernel semantic contract checks passed"
