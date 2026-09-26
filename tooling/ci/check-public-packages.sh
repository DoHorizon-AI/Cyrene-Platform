#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "${repo_root}"

public_packages=(
  cy-kernel-contract
  cy-manifest
  cy-proto
  cy-observability
  cy-artifact-transfer
  cy-execution-fabric
  cy-workspace-fabric
)

# The public crates are intentionally versioned path dependencies. Before the
# first registry publication, Cargo's package verifier cannot resolve those
# names from crates.io. These temporary patches validate the packaged crates
# against the checked-out public sources; the version requirements remain in
# each manifest and define the real publish order.
public_dependency_patches=(
  --config 'patch.crates-io.cy-kernel-contract.path="contracts/rust/cy-kernel-contract"'
  --config 'patch.crates-io.cy-manifest.path="contracts/rust/cy-manifest"'
  --config 'patch.crates-io.cy-proto.path="contracts/rust/cy-proto"'
  --config 'patch.crates-io.cy-observability.path="framework/crates/cy-observability"'
  --config 'patch.crates-io.cy-artifact-transfer.path="sdk/rust/cy-artifact-transfer"'
)

for package in "${public_packages[@]}"; do
  cargo package --locked --allow-dirty "${public_dependency_patches[@]}" -p "${package}"
done

echo "public package verification: PASS (${#public_packages[@]} crates)"
