#!/usr/bin/env bash
# Search for legacy EW_AI_* references that should have been migrated
set -euo pipefail

echo "Searching common CI configs for legacy EW_AI_* references..."

CI_FILES=(".github/workflows" ".circleci" ".gitlab-ci.yml" "Jenkinsfile" "ci" "docker-compose.yml")
for f in "${CI_FILES[@]}"; do
  if [ -e "$f" ]; then
    echo "-- Scanning: $f"
    git grep -n --full-name -I -- "\bEW_AI_\w+\b" -- $f || true
  fi
done
echo "Done. All EW_AI_* references should be replaced with CY_LLM_* variables."
