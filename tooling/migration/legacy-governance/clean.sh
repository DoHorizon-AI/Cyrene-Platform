#!/usr/bin/env bash
# Clean intermediate build artifacts and caches for the repo
# Usage: ./scripts/clean.sh [-y] [--venv] [--git-clean]
#   -y         : run without confirmation
#   --venv     : remove .venv directory (dangerous)
#   --git-clean: run 'git clean -fdX' to remove all ignored files (dangerous)

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DRY_RUN=false
FORCE=false
REMOVE_VENV=false
GIT_CLEAN=false

if [ "$#" -gt 0 ]; then
  while (( "$#" )); do
    case "$1" in
      -y|--yes)
        FORCE=true; shift;;
      --venv)
        REMOVE_VENV=true; shift;;
      --git-clean)
        GIT_CLEAN=true; shift;;
      *) echo "Unknown option: $1"; exit 1;;
    esac
  done
fi

# Paths to remove (safe defaults)
TARGETS=(
  "$ROOT_DIR/**/__pycache__"
  "$ROOT_DIR/**/*.pyc"
  "$ROOT_DIR/__pycache__"
  "$ROOT_DIR/.pytest_cache"
  "$ROOT_DIR/.mypy_cache"
  "$ROOT_DIR/**/build"
  "$ROOT_DIR/**/out"
  "$ROOT_DIR/**/.ipynb_checkpoints"
  "$ROOT_DIR/**/*.egg-info"
  "$ROOT_DIR/.DS_Store"
  "$ROOT_DIR/.pytest_cache"
)

# Non-recursive cleanup for node_modules or pip caches (if present)
NODE_DIRS=("$ROOT_DIR/node_modules"
           "$ROOT_DIR/**/node_modules")

if [ "$REMOVE_VENV" = true ]; then
  TARGETS+=("$ROOT_DIR/.venv")
fi

echo "Cleaning build artifacts and caches in: $ROOT_DIR"
if [ "$GIT_CLEAN" = true ]; then
  echo "Will run 'git clean -fdX' to remove all ignored files (including .venv if present)."
fi

if [ "$FORCE" = false ]; then
  echo "The following patterns will be removed (globs expanded):"
  for p in "${TARGETS[@]}"; do
    echo "  - $p"
  done
  echo "Enter 'yes' to proceed:"
  read -r RESP
  if [ "$RESP" != "yes" ]; then
    echo "Aborted by user."; exit 0
  fi
fi

# Expand and delete
shopt -s globstar nullglob
DELETED_COUNT=0
for p in "${TARGETS[@]}"; do
  # shellcheck disable=SC2046
  for f in $p; do
    if [ -e "$f" ]; then
      echo "Removing: $f"
      rm -rf "$f"
      ((DELETED_COUNT++))
    fi
  done
done

# node_modules
for nd in "${NODE_DIRS[@]}"; do
  for f in $nd; do
    if [ -e "$f" ]; then
      echo "Removing node_modules: $f"
      rm -rf "$f"
      ((DELETED_COUNT++))
    fi
  done
done

if [ "$GIT_CLEAN" = true ]; then
  echo "Running: git clean -fdX"
  git -C "$ROOT_DIR" clean -fdX
fi

if [ "$DELETED_COUNT" -eq 0 ]; then
  echo "No intermediate files found to remove."
else
  echo "Removed $DELETED_COUNT items."
fi

echo "Done. You can now restart VSCode and run your build commands."
