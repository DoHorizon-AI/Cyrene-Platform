#!/usr/bin/env bash
# ┌─────────────────────────────────────────────────────────────────────┐
# │  📄 workspace-entrypoint.sh                                         │
# │  Role: Runs the outbound Workspace connector and Runtime Agent.     │
# │                                                                     │
# │  脚本职责：在无入站端口容器中运行 Workspace Connector 与 Runtime Agent。 │
# └─────────────────────────────────────────────────────────────────────┘

set -euo pipefail

control_pid=""
connector_pid=""
runtime_pid=""

# ════════════════════════════════════════════════════════════════════════
# FUNCTION: shutdown_workspace
#
#   Requests canonical graceful stop, then reaps fixture processes.
#   请求 canonical 优雅停止，然后回收 fixture 进程。
#
#   Returns: Always 0 after best-effort cleanup.
# ════════════════════════════════════════════════════════════════════════
shutdown_workspace() {
  touch /commands/stop-1
  for _ in $(seq 1 100); do
    if [[ -f /state/workload-graceful-1 ]]; then
      break
    fi
    sleep 0.1
  done
  for process_id in "${connector_pid}" "${runtime_pid}" "${control_pid}"; do
    if [[ -n "${process_id}" ]]; then
      kill "${process_id}" >/dev/null 2>&1 || true
    fi
  done
  wait >/dev/null 2>&1 || true
}
trap shutdown_workspace EXIT TERM INT

CYRENE_FIXTURE_BIND="127.0.0.1:${CYRENE_RUNTIME_CONTROL_PORT}" \
CYRENE_FIXTURE_TRACE=/state/runtime-control.trace \
CYRENE_FIXTURE_COMMAND_DIR=/commands \
CYRENE_FIXTURE_ARTIFACT_FILE=/fixture/artifact.bin \
CYRENE_FIXTURE_ARTIFACT_URL="https://cyrene-artifact.test:${CYRENE_ARTIFACT_PORT}/artifact.bin" \
CYRENE_FIXTURE_SERVER_CERT=/certs/server.crt \
CYRENE_FIXTURE_SERVER_KEY=/certs/server.key \
CYRENE_FIXTURE_CLIENT_CA=/certs/ca.crt \
CYRENE_FIXTURE_DISCONNECT_GENERATION=1 \
CYRENE_FIXTURE_WAIT_FOR_START=1 \
/runtime/cy-runtime-control-fixture &
control_pid=$!

for _ in $(seq 1 200); do
  if [[ -f /state/runtime-control.trace ]] && grep -q FIXTURE_STARTED /state/runtime-control.trace; then
    break
  fi
  sleep 0.05
done
grep -q FIXTURE_STARTED /state/runtime-control.trace

CYRENE_WORKSPACE_RELAY_ENDPOINT="https://cyrene-relay.test:${CYRENE_RELAY_PORT}" \
CYRENE_WORKSPACE_RELAY_SERVER_NAME=cyrene-relay.test \
CYRENE_WORKSPACE_RELAY_CA=/certs/ca.crt \
CYRENE_WORKSPACE_RELAY_CLIENT_CERT=/certs/workspace.crt \
CYRENE_WORKSPACE_RELAY_CLIENT_KEY=/certs/workspace.key \
CYRENE_WORKSPACE_CONNECTOR_TRACE=/state/workspace-connector.trace \
CYRENE_WORKSPACE_ASSIGNMENT_TRIGGER=/commands/start-1 \
CYRENE_RUNTIME_CONTROL_TRACE=/state/runtime-control.trace \
/runtime/cy-workspace-fabric-fixture connector &
connector_pid=$!

# The Runtime Agent receives only its Node/workload enrollment scope. It does
# not inherit the frontend or Workspace relay session credentials.
# Runtime Agent 只接收 Node/workload enrollment scope，不继承用户会话凭证。
env -u CYRENE_WORKSPACE_SESSION_CREDENTIAL \
  /runtime/cy-runtime-agent run \
  --control-plane "https://cyrene-control.test:${CYRENE_RUNTIME_CONTROL_PORT}" \
  --control-plane-server-name cyrene-control.test \
  --control-plane-ca /certs/ca.crt \
  --client-certificate /certs/runtime.crt \
  --client-key /certs/runtime.key \
  --artifact-ca /certs/ca.crt \
  --organization-id "${CYRENE_ORGANIZATION_ID}" \
  --workspace-id "${CYRENE_WORKSPACE_ID}" \
  --node-id node-workspace-fixture \
  --node-epoch 1 \
  --node-type container \
  --persistent true \
  --runtime-id runtime-workspace-fixture \
  --runtime-generation 1 \
  --enrollment-proof development-token-1 \
  --state-dir /state \
  --artifact-root /artifacts \
  -- /bin/sh /fixture/fake-workload.sh /state &
runtime_pid=$!

wait "${connector_pid}"
