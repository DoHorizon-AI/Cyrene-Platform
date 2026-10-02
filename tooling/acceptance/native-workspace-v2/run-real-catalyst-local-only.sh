#!/usr/bin/env bash
# ┌─────────────────────────────────────────────────────────────────────┐
# │  📄 run-real-catalyst-local-only.sh                                 │
# │  Role: Start and probe the real Catalyst Product API locally.       │
# │                                                                     │
# │  脚本职责：启动并探测本机真实 Catalyst Product API。                    │
# └─────────────────────────────────────────────────────────────────────┘

set -euo pipefail
umask 077

CATALYST_ROOT="${CYRENE_CATALYST_ROOT:-/home/baijin/Dev/Cyrene/Cyrene-Services/Cyrene-Catalyst}"
ACCEPTANCE_ROOT="${CYRENE_NATIVE_ACCEPTANCE_DIR:-/tmp/cyrene-components-v2-acceptance/native-relay}"
PRODUCT_ROOT="$ACCEPTANCE_ROOT/catalyst"
PRODUCT_HOME="$PRODUCT_ROOT/service"
ARTIFACT_ROOT="$PRODUCT_ROOT/artifacts"
AUTH_FILE="$PRODUCT_ROOT/service-auth.json"
TOKEN_FILE="$PRODUCT_ROOT/product-api-token"
LOG_FILE="$PRODUCT_ROOT/catalyst.log"
PID_FILE="$PRODUCT_ROOT/catalyst.pid"
PRODUCT_HOST="${CYRENE_CATALYST_BIND:-127.0.0.1}"
PRODUCT_PORT="${CYRENE_CATALYST_PORT:-18014}"
ORG_ID="${CYRENE_CATALYST_ORGANIZATION_ID:-org-native-acceptance-testscope}"
WORKSPACE_ID="${CYRENE_CATALYST_WORKSPACE_ID:-workspace-native-acceptance-testscope}"
REPORT_FILE="$PRODUCT_ROOT/real-product-api-local-report.json"

# ── Phase 1: Validate paths and preserve private acceptance inputs ─────
# 第一阶段：检查路径并保护本地验收输入
if [[ ! -x "$CATALYST_ROOT/.venv/bin/python" ]]; then
  printf 'Catalyst virtual environment is unavailable at the configured repository.\n' >&2
  exit 2
fi
if [[ "$PRODUCT_HOST" != "127.0.0.1" ]]; then
  printf 'This local-only smoke must bind Catalyst to 127.0.0.1.\n' >&2
  exit 2
fi

install -d -m 0700 "$PRODUCT_ROOT" "$PRODUCT_HOME" "$ARTIFACT_ROOT"
chmod 0700 "$PRODUCT_ROOT" "$PRODUCT_HOME" "$ARTIFACT_ROOT"

if [[ ! -s "$TOKEN_FILE" ]]; then
  openssl rand -hex 32 >"$TOKEN_FILE"
fi
chmod 0600 "$TOKEN_FILE"

if [[ -f "$PID_FILE" ]]; then
  old_pid="$(<"$PID_FILE")"
  if [[ "$old_pid" =~ ^[0-9]+$ ]] && kill -0 "$old_pid" 2>/dev/null; then
    printf 'A Catalyst process is already recorded for this acceptance directory.\n' >&2
    exit 2
  fi
  rm -f "$PID_FILE"
fi

export CYRENE_NATIVE_AUTH_FILE="$AUTH_FILE"
export CYRENE_NATIVE_TOKEN_FILE="$TOKEN_FILE"
export CYRENE_NATIVE_ORGANIZATION_ID="$ORG_ID"
export CYRENE_NATIVE_WORKSPACE_ID="$WORKSPACE_ID"
"$CATALYST_ROOT/.venv/bin/python" - <<'PY'
import hashlib
import json
import os
from pathlib import Path

token_path = Path(os.environ["CYRENE_NATIVE_TOKEN_FILE"])
token = token_path.read_text(encoding="ascii").strip()
if len(token) < 32 or not token.isascii():
    raise SystemExit("Catalyst Product bearer has an invalid shape")

auth = [{
    "tokenSha256": hashlib.sha256(token.encode("ascii")).hexdigest(),
    "organizationId": os.environ["CYRENE_NATIVE_ORGANIZATION_ID"],
    "workspaceId": os.environ["CYRENE_NATIVE_WORKSPACE_ID"],
}]
Path(os.environ["CYRENE_NATIVE_AUTH_FILE"]).write_text(
    json.dumps(auth, separators=(",", ":")), encoding="utf-8"
)
PY
chmod 0600 "$AUTH_FILE"

# ── Phase 2: Launch the actual SQLite-backed Catalyst service ──────────
# 第二阶段：启动真实 SQLite-backed Catalyst service
cd "$CATALYST_ROOT"
CYRENE_WORKSPACE_SERVICE_AUTH_JSON="$(<"$AUTH_FILE")" \
PYTHONPATH="$CATALYST_ROOT/src${PYTHONPATH:+:$PYTHONPATH}" \
  "$CATALYST_ROOT/.venv/bin/python" -m cyrene_catalyst.cli serve \
    --home "$PRODUCT_HOME" \
    --artifact-root "$ARTIFACT_ROOT" \
    --host "$PRODUCT_HOST" \
    --port "$PRODUCT_PORT" \
    >>"$LOG_FILE" 2>&1 </dev/null &
PRODUCT_PID=$!
printf '%s\n' "$PRODUCT_PID" >"$PID_FILE"

stop_product() {
  if kill -0 "$PRODUCT_PID" 2>/dev/null; then
    kill "$PRODUCT_PID" 2>/dev/null || true
    wait "$PRODUCT_PID" 2>/dev/null || true
  fi
  rm -f "$PID_FILE"
}

stop_product_on_failure() {
  local status=$?
  if [[ "$status" -ne 0 ]]; then
    stop_product
  fi
}

stop_product_on_signal() {
  local status="$1"
  trap - INT TERM HUP
  stop_product
  exit "$status"
}

trap stop_product_on_failure EXIT
trap 'stop_product_on_signal 130' INT
trap 'stop_product_on_signal 143' TERM
trap 'stop_product_on_signal 129' HUP

# ── Phase 3: Call the actual owner endpoint without exposing its bearer ─
# 第三阶段：调用真实 owner endpoint，且不暴露 bearer
export CYRENE_NATIVE_PRODUCT_URL="http://$PRODUCT_HOST:$PRODUCT_PORT/internal/workspace/v1/datasets"
export CYRENE_NATIVE_REPORT_FILE="$REPORT_FILE"
export CYRENE_NATIVE_CATALYST_ROOT="$CATALYST_ROOT"
for attempt in $(seq 1 30); do
  if "$CATALYST_ROOT/.venv/bin/python" - <<'PY'
import json
import os
from pathlib import Path

import httpx

token = Path(os.environ["CYRENE_NATIVE_TOKEN_FILE"]).read_text(encoding="ascii").strip()
try:
    response = httpx.get(
        os.environ["CYRENE_NATIVE_PRODUCT_URL"],
        headers={"Authorization": f"Bearer {token}"},
        timeout=2.0,
    )
except httpx.HTTPError:
    raise SystemExit(1)

if response.status_code != 200:
    raise SystemExit(1)
try:
    payload = response.json()
except ValueError:
    raise SystemExit(1)
if not isinstance(payload, list):
    raise SystemExit(1)

catalyst_root = Path(os.environ["CYRENE_NATIVE_CATALYST_ROOT"])
import subprocess

def git_value(path: Path, *args: str) -> str:
    return subprocess.check_output(
        ["git", "-C", str(path), *args], text=True
    ).strip()

dirty_paths = git_value(
    catalyst_root, "status", "--short", "--untracked-files=all"
).splitlines()
report = {
    "status": "PASS",
    "category": "REAL_PRODUCT_API_LOCAL_ONLY",
    "owner": "catalyst",
    "operationId": "workspaceListDatasets",
    "method": "GET",
    "path": "/internal/workspace/v1/datasets",
    "endpoint": os.environ["CYRENE_NATIVE_PRODUCT_URL"],
    "httpStatus": response.status_code,
    "responseKind": "json-array",
    "responseItemCount": len(payload),
    "scopeKind": "TEST_SCOPE",
    "identityEvidence": "isolated Catalyst bearer mapping; not a Directory principal or human approval",
    "organizationId": os.environ["CYRENE_NATIVE_ORGANIZATION_ID"],
    "workspaceId": os.environ["CYRENE_NATIVE_WORKSPACE_ID"],
    "catalystHead": git_value(catalyst_root, "rev-parse", "HEAD"),
    "catalystBranch": git_value(catalyst_root, "branch", "--show-current"),
    "catalystDirtyPaths": dirty_paths,
}
Path(os.environ["CYRENE_NATIVE_REPORT_FILE"]).write_text(
    json.dumps(report, indent=2) + "\n", encoding="utf-8"
)
PY
  then
    chmod 0600 "$REPORT_FILE"
    stop_product
    trap - EXIT
    printf 'REAL_PRODUCT_API_LOCAL_ONLY PASS (TEST_SCOPE): Catalyst workspaceListDatasets; report=%s\n' "$REPORT_FILE"
    printf 'Catalyst stopped after the smoke; endpoint=%s:%s\n' "$PRODUCT_HOST" "$PRODUCT_PORT"
    exit 0
  fi
  if ! kill -0 "$PRODUCT_PID" 2>/dev/null; then
    printf 'Catalyst exited before the real Product API probe succeeded; private log=%s\n' "$LOG_FILE" >&2
    exit 1
  fi
  sleep 1
done

printf 'Catalyst did not answer the real Product API probe within 30 seconds; private log=%s\n' "$LOG_FILE" >&2
exit 1
