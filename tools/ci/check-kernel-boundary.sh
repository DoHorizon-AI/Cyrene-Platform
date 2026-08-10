#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

failed=0

if [[ -d kernel/crates/cy-hardware-discovery ]]; then
  echo "vendor hardware discovery must not live under kernel/crates" >&2
  failed=1
fi

if [[ -f kernel/crates/cy-node-agent/src/probe.rs ]]; then
  echo "Node Agent must not contain a vendor hardware probe" >&2
  failed=1
fi

if rg -n -i 'nvidia-smi|rocm-smi|/dev/nvidia|libnvidia|libloading|dlopen|ascend-cli' kernel; then
  echo "Kernel contains vendor command, device scan, or dynamic-library loading" >&2
  failed=1
fi

if rg -n 'cy-hardware-discovery' kernel Cargo.toml; then
  echo "Kernel workspace still depends on the retired discovery crate" >&2
  failed=1
fi

required_files=(
  "contracts/proto/cyrene/hardware/v1/hardware_adapter.proto"
  "kernel/crates/cy-adapter-client/src/lib.rs"
  "adapters/hardware/nvidia/src/main.rs"
  "infra/systemd/cyrene-nvidia-adapter.service"
  "docs/adr/ADR-HARDWARE-ADAPTER-BOUNDARY.md"
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

if [[ "$failed" -ne 0 ]]; then
  exit 1
fi

echo "Kernel hardware boundary checks passed"
