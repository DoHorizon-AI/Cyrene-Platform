#!/usr/bin/env bash
# List all occurrences of legacy EW_* references that need to be checked
set -euo pipefail

echo "Searching for legacy EW_* references in repo (excluding common binary or cache dirs)..."
# Skip binary, build, and test artifacts. Use git grep to keep it fast.
git grep -n --full-name -I -- "\bEW_\w+\b" || true

echo "Done."
