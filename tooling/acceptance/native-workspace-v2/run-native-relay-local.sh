#!/usr/bin/env bash
# Build, start, probe, or stop the real Native Relay on loopback for acceptance.
# 本脚本只管理当前验收目录记录的本机 Relay 进程。

set -euo pipefail
set +x
umask 077

acceptance_root="/tmp/cyrene-components-v2-acceptance/native-relay"
runtime_env="${acceptance_root}/runtime-relay.env"
host_env="${acceptance_root}/relay-host.env"
pid_file="${acceptance_root}/native-relay-host.pid"
log_file="${acceptance_root}/native-relay-host.log"
health_file="${acceptance_root}/native-relay-readiness.json"
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
target_dir="/tmp/cyrene-components-target"
binary="${target_dir}/debug/cy-workspace-relay-host"
action="${1:-start}"

case "${action}" in
  start|stop|status) ;;
  *)
    printf '%s\n' 'Usage: run-native-relay-local.sh [start|stop|status]' >&2
    exit 2
    ;;
esac
if [[ "$#" -gt 1 ]]; then
  printf '%s\n' 'Usage: run-native-relay-local.sh [start|stop|status]' >&2
  exit 2
fi

process_is_ours() {
  local recorded_pid="$1"
  [[ "${recorded_pid}" =~ ^[0-9]+$ && -r "/proc/${recorded_pid}/exe" ]] || return 1
  [[ "$(readlink "/proc/${recorded_pid}/exe")" == "${binary}"* ]]
}

if [[ "${action}" == 'status' ]]; then
  if [[ ! -r "${pid_file}" ]]; then
    printf '%s\n' 'Native Relay is not recorded as running.'
    exit 1
  fi
  pid="$(cat "${pid_file}")"
  if process_is_ours "${pid}"; then
    printf 'Native Relay process is running; readiness report: %s\n' "${health_file}"
    exit 0
  fi
  printf '%s\n' 'Recorded Native Relay process is not running.' >&2
  exit 1
fi

if [[ "${action}" == 'stop' ]]; then
  if [[ ! -r "${pid_file}" ]]; then
    printf '%s\n' 'No recorded Native Relay process to stop.'
    exit 0
  fi
  pid="$(cat "${pid_file}")"
  if ! process_is_ours "${pid}"; then
    if ! kill -0 "${pid}" 2>/dev/null; then
      unlink "${pid_file}"
      printf '%s\n' 'Removed the stale Native Relay PID record; no process was signaled.'
      exit 0
    fi
    printf '%s\n' 'Refusing to signal a process that does not match this Native Relay binary.' >&2
    exit 2
  fi
  kill "${pid}"
  for _ in $(seq 1 50); do
    if ! process_is_ours "${pid}"; then
      unlink "${pid_file}"
      printf '%s\n' 'Native Relay stopped.'
      exit 0
    fi
    sleep 0.1
  done
  printf 'Native Relay did not stop promptly; inspect private log %s\n' "${log_file}" >&2
  exit 1
fi

if [[ -e "${pid_file}" ]]; then
  printf '%s\n' 'A Native Relay PID record already exists; inspect status before starting another process.' >&2
  exit 2
fi
for env_file in "${runtime_env}" "${host_env}"; do
  if [[ ! -f "${env_file}" || -L "${env_file}" || "$(stat -c '%a' "${env_file}")" != '600' ]]; then
    printf '%s\n' 'A protected Relay environment file is missing or has unsafe permissions.' >&2
    exit 2
  fi
done

# Runtime URLs stay in environment variables and never enter process arguments or logs.
set -a
source "${runtime_env}"
source "${host_env}"
set +a
unset CYRENE_WORKSPACE_RELAY_ACA_INGRESS_ASSERTION
unset CYRENE_WORKSPACE_DEVICE_CA_SIGNING_KEY_FILE

cd "${repo_root}"
CARGO_TARGET_DIR="${target_dir}" cargo build --locked --offline \
  -p cy-workspace-relay-host \
  --bin cy-workspace-relay-host

if [[ ! -x "${binary}" ]]; then
  printf '%s\n' 'Native Relay binary was not produced.' >&2
  exit 1
fi
python3 - "${binary}" "${log_file}" "${pid_file}" <<'PY'
import subprocess
import sys
from pathlib import Path

binary_path, log_path, pid_path = sys.argv[1:]
with Path(log_path).open("ab") as host_log:
    process = subprocess.Popen(
        [binary_path],
        stdin=subprocess.DEVNULL,
        stdout=host_log,
        stderr=subprocess.STDOUT,
        close_fds=True,
        start_new_session=True,
    )
Path(pid_path).write_text(f"{process.pid}\n", encoding="ascii")
PY
pid="$(cat "${pid_file}")"
chmod 0600 "${pid_file}" "${log_file}"

for _ in $(seq 1 100); do
  if ! process_is_ours "${pid}"; then
    printf 'Native Relay exited before health became available; private log: %s\n' "${log_file}" >&2
    exit 1
  fi
  status="$(curl --silent --show-error --max-time 1 --output "${health_file}" --write-out '%{http_code}' \
    http://127.0.0.1:18081/readyz 2>/dev/null || true)"
  chmod 0600 "${health_file}" 2>/dev/null || true
  if [[ "${status}" == '200' ]]; then
    printf 'Native Relay ready on loopback; pid=%s, report=%s\n' "${pid}" "${health_file}"
    exit 0
  fi
  sleep 0.2
done

printf 'Native Relay process started but readiness did not return HTTP 200; report=%s log=%s\n' \
  "${health_file}" "${log_file}" >&2
exit 1
