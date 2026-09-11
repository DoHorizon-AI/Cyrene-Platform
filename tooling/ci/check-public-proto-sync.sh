#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
canonical="$repo_root/contracts/proto"
mirror="$repo_root/contracts/rust/cy-proto/proto"
canonical_fixture="$repo_root/contracts/fixtures/core/v1/manifest.json"
mirror_fixture="$repo_root/contracts/rust/cy-proto/tests/fixtures/core/v1/manifest.json"

mapfile -t canonical_files < <(cd "$canonical" && find . -type f -name '*.proto' -print | sort)
mapfile -t mirror_files < <(cd "$mirror" && find . -type f -name '*.proto' -print | sort)

if [[ "${canonical_files[*]}" != "${mirror_files[*]}" ]]; then
    echo "public proto mirror file set differs from contracts/proto" >&2
    diff -u <(printf '%s\n' "${canonical_files[@]}") <(printf '%s\n' "${mirror_files[@]}") || true
    exit 1
fi

for relative in "${canonical_files[@]}"; do
    if ! cmp -s "$canonical/${relative#./}" "$mirror/${relative#./}"; then
        echo "public proto mirror is stale: ${relative#./}" >&2
        exit 1
    fi
done

if ! cmp -s "$canonical_fixture" "$mirror_fixture"; then
    echo "public proto fixture mirror is stale: core/v1/manifest.json" >&2
    exit 1
fi

echo "public proto mirror: PASS"
