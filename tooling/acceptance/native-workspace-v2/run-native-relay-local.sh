#!/usr/bin/env bash
# Stop a legacy Platform Relay process recorded by an earlier acceptance run.
# 仅停止旧验收记录的 Platform Relay 进程，避免删除后遗留进程继续运行。

set -euo pipefail
set +x

acceptance_root="/tmp/cyrene-components-v2-acceptance/native-relay"
pid_file="${acceptance_root}/native-relay-host.pid"
log_file="${acceptance_root}/native-relay-host.log"
binary="/tmp/cyrene-components-target/debug/cy-workspace-relay-host"
action="${1:-stop}"

if [[ "$#" -gt 1 || "${action}" != 'stop' ]]; then
  printf '%s\n' \
    'RETIRED: the Platform Relay host cannot be started or status-checked from this script.' \
    'Only [stop] remains to clean up a legacy process; current Relay runtime belongs to Cyrene-Plugins-Official.' \
    '已退役：本脚本不能启动或检查 Platform Relay；仅保留 [stop] 用于清理旧进程。' >&2
  exit 2
fi

process_is_ours() {
  local recorded_pid="$1"
  local executable
  [[ "${recorded_pid}" =~ ^[0-9]+$ && -r "/proc/${recorded_pid}/exe" ]] || return 1
  executable="$(readlink "/proc/${recorded_pid}/exe")"
  [[ "${executable}" == "${binary}" || "${executable}" == "${binary} (deleted)" ]]
}

if [[ ! -e "${pid_file}" ]]; then
  printf '%s\n' 'No recorded legacy Platform Relay process to stop.'
  exit 0
fi
if [[ -L "${pid_file}" || ! -f "${pid_file}" ]]; then
  printf '%s\n' 'Refusing to read an unsafe legacy Relay PID record.' >&2
  exit 2
fi

pid="$(cat "${pid_file}")"
if ! process_is_ours "${pid}"; then
  if ! kill -0 "${pid}" 2>/dev/null; then
    unlink "${pid_file}"
    printf '%s\n' 'Removed the stale legacy Relay PID record; no process was signaled.'
    exit 0
  fi
  printf '%s\n' 'Refusing to signal a process that does not match the legacy Relay binary.' >&2
  exit 2
fi

kill "${pid}"
for _ in $(seq 1 50); do
  if ! process_is_ours "${pid}"; then
    unlink "${pid_file}"
    printf 'Stopped the legacy Platform Relay. Private log: %s\n' "${log_file}"
    exit 0
  fi
  sleep 0.1
done

printf 'The legacy Relay did not stop promptly; inspect private log %s\n' "${log_file}" >&2
exit 1
