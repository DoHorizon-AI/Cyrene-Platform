#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

failed=0

if [[ -d kernel/crates/cy-hardware-discovery ]]; then
  echo "vendor hardware discovery must not live under kernel/crates" >&2
  failed=1
fi

if [[ -f agents/node/cy-node-agent/src/probe.rs ]]; then
  echo "Node Agent must not contain a vendor hardware probe" >&2
  failed=1
fi

for legacy_path in \
  kernel/crates/cy-local-transport \
  kernel/crates/cy-node-agent; do
  if [[ -e "$legacy_path" ]]; then
    echo "non-Kernel component remains under kernel/: $legacy_path" >&2
    failed=1
  fi
done

if rg -n -i 'nvidia-smi|rocm-smi|/dev/nvidia|libnvidia|libloading|dlopen|ascend-cli' kernel; then
  echo "Kernel contains vendor command, device scan, or dynamic-library loading" >&2
  failed=1
fi

if rg -n 'pub enum Accelerator|pub struct AcceleratorDevice|trait AcceleratorProvider' \
  kernel --glob '*.rs'; then
  echo "Kernel public Rust ports must use Resource/Capability, not accelerator vendor types" >&2
  failed=1
fi

while IFS= read -r rust_file; do
  first_test_line="$(rg -n '^#\[cfg\(test\)\]' "$rust_file" | head -n 1 | cut -d: -f1 || true)"
  if [[ -n "$first_test_line" ]]; then
    production_end=$((first_test_line - 1))
  else
    production_end="$(wc -l < "$rust_file")"
  fi

  if head -n "$production_end" "$rust_file" | rg -n \
    'unsafe\s*\{|unsafe extern|libc::|\bCommand\b|/sys/fs/cgroup|/proc/self/cgroup|BPF_'; then
    echo "Kernel production code contains privileged sandbox implementation details: $rust_file" >&2
    failed=1
  fi
done < <(rg --files kernel --glob '*.rs' --glob '!*tests.rs' --glob '!**/tests/**')

if rg -n 'cy-hardware-discovery' kernel Cargo.toml; then
  echo "Kernel workspace still depends on the retired discovery crate" >&2
  failed=1
fi

if rg -n 'framework/crates|cy-local-transport|cy-installation-resolver|cy-node-agent' \
  kernel --glob 'Cargo.toml'; then
  echo "Kernel crate manifest depends on a framework, legacy runner, installer, or node-agent crate" >&2
  failed=1
fi

required_files=(
  "contracts/proto/cyrene/hardware/v1/hardware_adapter.proto"
  "kernel/crates/cy-adapter-client/src/lib.rs"
  "kernel/crates/cy-sandbox-client/src/lib.rs"
  "adapters/hardware/nvidia/src/main.rs"
  "adapters/execution/sandboxd/src/main.rs"
  "infrastructure/systemd/cyrene-nvidia-adapter.service"
  "infrastructure/systemd/cyrene-sandboxd.service"
  "docs/adr/ADR-HARDWARE-ADAPTER-BOUNDARY.md"
  "docs/adr/ADR-SANDBOX-ADAPTER-BOUNDARY.md"
)

for path in "${required_files[@]}"; do
  if [[ ! -f "$path" ]]; then
    echo "missing hardware adapter boundary file: $path" >&2
    failed=1
  fi
done

if ! rg -q '^package cyrene\.hardware\.v1;' contracts/proto/cyrene/hardware/v1/hardware_adapter.proto; then
  echo "hardware adapter protocol package is missing or changed" >&2
  failed=1
fi

if ! rg -q '^package cyrene\.sandbox\.v1;' contracts/proto/cyrene/sandbox/v1/sandbox_adapter.proto; then
  echo "sandbox adapter protocol package is missing or changed" >&2
  failed=1
fi

if [[ "$failed" -ne 0 ]]; then
  exit 1
fi

echo "Kernel hardware boundary checks passed"
