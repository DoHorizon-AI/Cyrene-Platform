#!/usr/bin/env bash
# Retry the Pro self-deploy restart endpoint when a Worker becomes unhealthy.

set -euo pipefail

GATEWAY_URL="${GATEWAY_URL:-http://localhost:${CY_LLM_PORT:-8080}}"
STATUS_URL="${GATEWAY_URL%/}/api/deploy/status"
RESTART_URL="${GATEWAY_URL%/}/api/deploy/restart"
MAX_RETRIES="${MAX_RETRIES:-3}"
SLEEP_SECONDS="${SLEEP_SECONDS:-5}"

AUTH_ARGS=()
if [[ -n "${CY_LLM_INTERNAL_TOKEN:-}" ]]; then
  AUTH_ARGS=(-H "Authorization: Bearer ${CY_LLM_INTERNAL_TOKEN}")
fi

log() {
  printf '[auto_recover] %s\n' "$*"
}

check_status() {
  curl -fsS "${AUTH_ARGS[@]}" "${STATUS_URL}" \
    | grep -Eq '"enabled"[[:space:]]*:[[:space:]]*true'
}

restart_worker() {
  curl -fsS "${AUTH_ARGS[@]}" -X POST "${RESTART_URL}" >/dev/null
}

attempt=1
while [[ "${attempt}" -le "${MAX_RETRIES}" ]]; do
  if check_status; then
    log "Self-deploy is enabled; triggering restart (attempt ${attempt}/${MAX_RETRIES})."
    restart_worker || true
    sleep "${SLEEP_SECONDS}"
    if check_status; then
      log 'Restart succeeded.'
      exit 0
    fi
  else
    log 'Self-deploy is not enabled or the status endpoint is unavailable.'
  fi
  attempt=$((attempt + 1))
  sleep "${SLEEP_SECONDS}"
done

log "Recovery failed after ${MAX_RETRIES} attempts."
exit 1
