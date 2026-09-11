#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
consumer_root="$repo_root/tooling/acceptance/licensing-boundary"
cd "$consumer_root"

cargo check --locked --workspace
tree=$(cargo tree --locked --workspace --edges normal,build)
boundary_config="$repo_root/tooling/architecture/license-boundaries.toml"
mapfile -t core_crates < <(
  python3 - "$boundary_config" <<'PY'
import sys
import tomllib

with open(sys.argv[1], "rb") as handle:
    for name in tomllib.load(handle)["core_copyleft"]:
        print(name)
PY
)
for core in "${core_crates[@]}"; do
  if grep -Eq "(^|[[:space:]]|\\))${core}([[:space:]]|v|$)" <<<"${tree}"; then
    echo "external consumer pulled core implementation crate: ${core}" >&2
    exit 1
  fi
done

echo "external consumer dependency closure: PASS"
