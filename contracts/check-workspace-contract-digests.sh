#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
(
  cd "$repo_root/contracts/proto"
  sha256sum --check "$repo_root/contracts/workspace-contract-digests.sha256"
)
