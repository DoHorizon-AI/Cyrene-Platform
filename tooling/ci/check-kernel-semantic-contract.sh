#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

semantic_proto="contracts/proto/cyrene/semantic/v1/kernel_contract.proto"
semantic_rust="contracts/rust/cy-kernel-contract/src/lib.rs"
descriptor="contracts/descriptors/cyrene-kernel-semantic-v1.pb"
stable_descriptor="contracts/descriptors/cyrene-kernel-semantic-v1-stable.pb"
fixture_manifest="contracts/fixtures/semantic/v1/manifest.json"
tck_root="contracts/tck/kernel-semantic/v1"
tmp_descriptor="$(mktemp)"
tmp_base_descriptor="$(mktemp)"
tmp_kotlin_dir="$(mktemp -d)"
trap 'rm -f "$tmp_descriptor" "$tmp_base_descriptor"; rm -rf "$tmp_kotlin_dir"' EXIT

for path in "$semantic_proto" "$semantic_rust" \
  "docs/contracts/kernel-semantic-contract-v1.md" "$fixture_manifest" \
  "$tck_root/limits.tsv" "$tck_root/identifiers.tsv" \
  "$tck_root/negotiation.tsv" "$tck_root/transitions.tsv" \
  "$tck_root/matching.tsv" "$tck_root/authority.tsv" \
  "$tck_root/replay.tsv" "$tck_root/renewal.tsv" \
  "$tck_root/python/kernel_semantic_tck.py" \
  "$tck_root/kotlin/KernelSemanticTck.kt"; do
  if [[ ! -f "$path" ]]; then
    echo "missing Kernel semantic contract artifact: $path" >&2
    exit 1
  fi
done

if ! rg -q '^Status: \*\*Frozen v1\.0\*\*' \
  docs/contracts/kernel-semantic-contract-v1.md; then
  echo "Kernel semantic contract must declare the frozen v1.0 status" >&2
  exit 1
fi

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

for value_object in ContractRevision Rejection EventCursor EventPage; do
  if ! rg -q "^(message|enum) ${value_object}( |\\{)" "$semantic_proto"; then
    echo "Kernel semantic value object is missing: $value_object" >&2
    exit 1
  fi
done

if ! rg -q '^#!\[forbid\(unsafe_code\)\]' "$semantic_rust"; then
  echo "Rust semantic projection must forbid unsafe code" >&2
  exit 1
fi

python_bin="${PYTHON_BIN:-}"
if [[ -z "$python_bin" ]]; then
  python_bin="$(command -v python3 || command -v python || true)"
fi
if [[ -z "$python_bin" ]]; then
  echo "Python is required for the Kernel semantic TCK" >&2
  exit 127
fi
"$python_bin" "$tck_root/python/kernel_semantic_tck.py"

if command -v kotlinc >/dev/null 2>&1 && command -v java >/dev/null 2>&1; then
  kotlinc "$tck_root/kotlin/KernelSemanticTck.kt" \
    -include-runtime -d "$tmp_kotlin_dir/kernel-semantic-tck.jar"
  java -jar "$tmp_kotlin_dir/kernel-semantic-tck.jar" "$tck_root"
elif [[ "${REQUIRE_KOTLIN_TCK:-0}" == "1" ]]; then
  echo "Kotlin and Java are required for the Kernel semantic TCK in CI" >&2
  exit 127
else
  echo "Kotlin TCK skipped locally because kotlinc/java are unavailable"
fi

cargo test -p cy-kernel-contract --locked

buf_bin="${BUF_BIN:-buf}"
if ! command -v "$buf_bin" >/dev/null 2>&1; then
  echo "Buf is required; CI installs the pinned version before this check" >&2
  exit 127
fi

"$buf_bin" lint contracts/proto

# Buf's breaking checker requires source_code_info in the against image.  The
# stable descriptor is intentionally source-info-free for deterministic
# runtime hashing, so compare against the full descriptor from the target
# branch and keep the stable descriptor for the drift/hash check below.
# Buf 的 breaking 检查要求 against 镜像包含 source_code_info；stable descriptor
# 为确定性运行时哈希而刻意去掉 source info，因此这里使用目标分支的完整
# descriptor 做兼容性基线，并继续用 stable descriptor 做漂移/哈希校验。
breaking_baseline="$descriptor"
if [[ -n "${GITHUB_BASE_REF:-}" ]] \
  && git cat-file -e "origin/${GITHUB_BASE_REF}:${descriptor}" 2>/dev/null; then
  git show "origin/${GITHUB_BASE_REF}:${descriptor}" > "$tmp_base_descriptor"
  breaking_baseline="$tmp_base_descriptor"
fi
"$buf_bin" breaking contracts/proto --against "$breaking_baseline#format=binpb"
(
  cd contracts/proto
  "$buf_bin" build . --path cyrene/semantic/v1 \
    --as-file-descriptor-set --exclude-source-info --output "$tmp_descriptor"
)

if [[ ! -f "$stable_descriptor" ]] || ! cmp -s "$tmp_descriptor" "$stable_descriptor"; then
  echo "Kernel semantic v1 descriptor drift detected; regenerate the checked-in baseline" >&2
  exit 1
fi

expected_descriptor_sha="$(rg -o '"descriptor_sha256": "[A-F0-9]+' "$fixture_manifest" | sed 's/.*"//')"
actual_descriptor_sha="$(sha256sum "$stable_descriptor" | awk '{print toupper($1)}')"
if [[ "$expected_descriptor_sha" != "$actual_descriptor_sha" ]]; then
  echo "Kernel semantic fixture manifest descriptor SHA-256 does not match baseline" >&2
  exit 1
fi

echo "Kernel semantic contract checks passed"
