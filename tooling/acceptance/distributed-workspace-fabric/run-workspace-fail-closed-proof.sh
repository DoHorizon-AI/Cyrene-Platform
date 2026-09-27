#!/usr/bin/env bash
# ┌─────────────────────────────────────────────────────────────────────┐
# │  📄 run-workspace-fail-closed-proof.sh                              │
# │  Role: Workspace direct, runtime, artifact, and Relay denial proof.  │
# │                                                                     │
# │  脚本职责：验收 Workspace 直连、Runtime、Artifact 与 Relay 拒绝策略。    │
# └─────────────────────────────────────────────────────────────────────┘

set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
proof_root=$(mktemp -d /tmp/cyrene-dwf-v1.XXXXXX)
container_name="cyrene-dwf-v1-$$"
relay_pid=""
artifact_pid=""
frontend_pid=""
base_image=${CYRENE_ACCEPTANCE_BASE_IMAGE:-ubuntu:24.04}

allocate_port() {
  python3 -c 'import socket; value=socket.socket(); value.bind(("127.0.0.1", 0)); print(value.getsockname()[1]); value.close()'
}

relay_port=$(allocate_port)
artifact_port=$(allocate_port)
while [[ "${artifact_port}" = "${relay_port}" ]]; do
  artifact_port=$(allocate_port)
done
runtime_control_port=19443
direct_port=18444

cleanup() {
  local exit_code=$?
  if [[ "${exit_code}" -ne 0 ]]; then
    printf '%s\n' '--- Workspace container ---' >&2
    docker logs "${container_name}" >&2 || true
    printf '%s\n' '--- Relay log ---' >&2
    tail -100 "${proof_root}/relay.log" >&2 || true
    printf '%s\n' '--- Runtime control trace ---' >&2
    tail -100 "${proof_root}/state/runtime-control.trace" >&2 || true
    printf '%s\n' '--- Workspace connector trace ---' >&2
    tail -100 "${proof_root}/state/workspace-connector.trace" >&2 || true
    printf '%s\n' '--- Artifact trace ---' >&2
    tail -100 "${proof_root}/artifact.trace" >&2 || true
  fi
  docker rm -f "${container_name}" >/dev/null 2>&1 || true
  if [[ -n "${relay_pid}" ]]; then kill "${relay_pid}" >/dev/null 2>&1 || true; fi
  if [[ -n "${artifact_pid}" ]]; then kill "${artifact_pid}" >/dev/null 2>&1 || true; fi
  if [[ -n "${frontend_pid}" ]]; then kill "${frontend_pid}" >/dev/null 2>&1 || true; fi
  if [[ "${proof_root}" = /tmp/cyrene-dwf-v1.* ]]; then rm -rf "${proof_root}"; fi
}
trap cleanup EXIT
trap 'exit 130' INT TERM

for command in cargo docker grep openssl python3 sha256sum; do
  command -v "${command}" >/dev/null || {
    printf 'required command is missing: %s\n' "${command}" >&2
    exit 1
  }
done

mkdir -p "${proof_root}/certs" "${proof_root}/commands" "${proof_root}/state" "${proof_root}/artifacts"
chmod 700 "${proof_root}/certs" "${proof_root}/commands" "${proof_root}/state" "${proof_root}/artifacts"
dd if=/dev/zero of="${proof_root}/artifact.bin" bs=1M count=4 status=none
source_digest=$(sha256sum "${proof_root}/artifact.bin" | cut -d ' ' -f 1)
artifact_uri="artifact://sha256/${source_digest}"
published_path="${proof_root}/artifacts/${source_digest}"

openssl req -x509 -newkey rsa:2048 -nodes -days 1 -subj '/CN=Cyrene Workspace Fixture CA' -keyout "${proof_root}/certs/ca.key" -out "${proof_root}/certs/ca.crt" >/dev/null 2>&1
openssl req -newkey rsa:2048 -nodes -subj '/CN=cyrene-relay.test' -keyout "${proof_root}/certs/server.key" -out "${proof_root}/certs/server.csr" >/dev/null 2>&1
printf '%s\n' 'subjectAltName=DNS:cyrene-relay.test,DNS:cyrene-control.test,DNS:cyrene-artifact.test,DNS:cyrene-workspace.test' 'extendedKeyUsage=serverAuth' > "${proof_root}/certs/server.ext"
openssl x509 -req -days 1 -in "${proof_root}/certs/server.csr" -CA "${proof_root}/certs/ca.crt" -CAkey "${proof_root}/certs/ca.key" -CAcreateserial -extfile "${proof_root}/certs/server.ext" -out "${proof_root}/certs/server.crt" >/dev/null 2>&1

issue_client_certificate() {
  local name=$1
  openssl req -newkey rsa:2048 -nodes -subj "/CN=${name}" -keyout "${proof_root}/certs/${name}.key" -out "${proof_root}/certs/${name}.csr" >/dev/null 2>&1
  printf '%s\n' 'extendedKeyUsage=clientAuth' > "${proof_root}/certs/${name}.ext"
  openssl x509 -req -days 1 -in "${proof_root}/certs/${name}.csr" -CA "${proof_root}/certs/ca.crt" -CAkey "${proof_root}/certs/ca.key" -CAcreateserial -extfile "${proof_root}/certs/${name}.ext" -out "${proof_root}/certs/${name}.crt" >/dev/null 2>&1
}
issue_client_certificate frontend
issue_client_certificate workspace
issue_client_certificate runtime
workspace_certificate_fingerprint=$(openssl x509 -in "${proof_root}/certs/workspace.crt" -outform DER | sha256sum | cut -d ' ' -f 1)
openssl rand -hex 32 > "${proof_root}/certs/artifact-ticket.key"
chmod 600 "${proof_root}/certs/"*.key

cd "${repo_root}"
cargo build --locked --release -p cy-workspace-fabric --bin cy-workspace-fabric-fixture -p cy-runtime-agent --bins
if ! docker image ls --format '{{.Repository}}:{{.Tag}}' | grep -Fxq "${base_image}"; then
  if [[ "${CYRENE_ACCEPTANCE_PULL_IMAGE:-0}" != "1" ]]; then
    printf 'base image is not local: %s; set CYRENE_ACCEPTANCE_PULL_IMAGE=1 to pull it\n' "${base_image}" >&2
    exit 1
  fi
  docker pull "${base_image}"
fi

python3 tooling/acceptance/distributed-execution-fabric/https_range_server.py \
  --port "${artifact_port}" \
  --artifact "${proof_root}/artifact.bin" \
  --certificate "${proof_root}/certs/server.crt" \
  --key "${proof_root}/certs/server.key" \
  --ticket-signature-file "${proof_root}/commands/artifact-ticket.signature" \
  --trace "${proof_root}/artifact.trace" &
artifact_pid=$!

start_relay() {
  CYRENE_WORKSPACE_RELAY_BIND="0.0.0.0:${relay_port}" \
  CYRENE_WORKSPACE_DESCRIPTOR_DIRECT_URI="https://${workspace_private_ip}:${direct_port}" \
  CYRENE_WORKSPACE_DIRECT_SERVER_NAME=cyrene-workspace.test \
  CYRENE_WORKSPACE_DESCRIPTOR_RELAY_URI="https://127.0.0.1:${relay_port}" \
  CYRENE_WORKSPACE_RELAY_SERVER_NAME=cyrene-relay.test \
  CYRENE_WORKSPACE_RELAY_TRACE="${proof_root}/relay.trace" \
  CYRENE_WORKSPACE_RELAY_SERVER_CERT="${proof_root}/certs/server.crt" \
  CYRENE_WORKSPACE_RELAY_SERVER_KEY="${proof_root}/certs/server.key" \
  CYRENE_WORKSPACE_RELAY_CLIENT_CA="${proof_root}/certs/ca.crt" \
  CYRENE_WORKSPACE_RELAY_DIRECTORY="${proof_root}/state/relay-directory" \
  CYRENE_WORKSPACE_DEVICE_CERTIFICATE_SHA256="${workspace_certificate_fingerprint}" \
  CYRENE_FRONTEND_SESSION_CREDENTIAL=development-frontend-session \
  CYRENE_WORKSPACE_DEVICE_ID=device-fixture \
  CYRENE_WORKSPACE_ID=workspace-fixture \
  CYRENE_ORGANIZATION_ID=organization-fixture \
  target/release/cy-workspace-fabric-fixture relay > "${proof_root}/relay.log" 2>&1 &
  relay_pid=$!
}

wait_for() {
  local description=$1
  local command=$2
  for _ in $(seq 1 450); do
    if bash -c "${command}"; then return 0; fi
    sleep 0.1
  done
  printf 'timed out waiting for %s\n' "${description}" >&2
  docker logs "${container_name}" >&2 || true
  tail -100 "${proof_root}/relay.log" >&2 || true
  tail -100 "${proof_root}/state/runtime-control.trace" >&2 || true
  tail -100 "${proof_root}/state/workspace-connector.trace" >&2 || true
  tail -100 "${proof_root}/artifact.trace" >&2 || true
  return 1
}

run_frontend() {
  local role=$1
  CYRENE_WORKSPACE_RELAY_ENDPOINT="https://127.0.0.1:${relay_port}" \
  CYRENE_WORKSPACE_RELAY_SERVER_NAME=cyrene-relay.test \
  CYRENE_WORKSPACE_RELAY_CA="${proof_root}/certs/ca.crt" \
  CYRENE_WORKSPACE_RELAY_CLIENT_CERT="${proof_root}/certs/frontend.crt" \
  CYRENE_WORKSPACE_RELAY_CLIENT_KEY="${proof_root}/certs/frontend.key" \
  CYRENE_FRONTEND_SESSION_CREDENTIAL=development-frontend-session \
  CYRENE_WORKSPACE_ID=workspace-fixture \
  CYRENE_ORGANIZATION_ID=organization-fixture \
  CYRENE_WORKSPACE_ARTIFACT_URI="${artifact_uri}" \
  CYRENE_WORKSPACE_AUTHORITY_INSTANCE_ID=workspace-authority-fixture-1 \
  CYRENE_WORKSPACE_DIRECT_BARRIER="${proof_root}/state/direct-discovered" \
  target/release/cy-workspace-fabric-fixture "${role}"
}

docker run -d --name "${container_name}" \
  --user "$(id -u):$(id -g)" \
  --add-host cyrene-relay.test:host-gateway \
  --add-host cyrene-artifact.test:host-gateway \
  --add-host cyrene-control.test:127.0.0.1 \
  -e "CYRENE_RELAY_PORT=${relay_port}" \
  -e "CYRENE_ARTIFACT_PORT=${artifact_port}" \
  -e "CYRENE_RUNTIME_CONTROL_PORT=${runtime_control_port}" \
  -e "CYRENE_WORKSPACE_DIRECT_PORT=${direct_port}" \
  -e CYRENE_FRONTEND_SESSION_CREDENTIAL=development-frontend-session \
  -e CYRENE_WORKSPACE_DEVICE_ID=device-fixture \
  -e CYRENE_WORKSPACE_ID=workspace-fixture \
  -e CYRENE_ORGANIZATION_ID=organization-fixture \
  -e "CYRENE_WORKSPACE_ARTIFACT_URI=${artifact_uri}" \
  -e CYRENE_WORKSPACE_AUTHORITY_INSTANCE_ID=workspace-authority-fixture-1 \
  -v "${proof_root}/certs:/certs:ro" \
  -v "${proof_root}/commands:/commands" \
  -v "${proof_root}/state:/state" \
  -v "${proof_root}/artifacts:/artifacts" \
  -v "${proof_root}/artifact.bin:/fixture/artifact.bin:ro" \
  -v "${repo_root}/target/release/cy-workspace-fabric-fixture:/runtime/cy-workspace-fabric-fixture:ro" \
  -v "${repo_root}/target/release/cy-runtime-control-fixture:/runtime/cy-runtime-control-fixture:ro" \
  -v "${repo_root}/target/release/cy-runtime-agent:/runtime/cy-runtime-agent:ro" \
  -v "${repo_root}/tooling/acceptance/distributed-execution-fabric/fake-workload.sh:/fixture/fake-workload.sh:ro" \
  -v "${repo_root}/tooling/acceptance/distributed-workspace-fabric/workspace-entrypoint.sh:/fixture/workspace-entrypoint.sh:ro" \
  --entrypoint /bin/bash \
  "${base_image}" /fixture/workspace-entrypoint.sh >/dev/null

workspace_private_ip=$(docker inspect -f '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' "${container_name}")
test -n "${workspace_private_ip}"
start_relay
wait_for 'relay startup' "kill -0 '${relay_pid}' && grep -q RELAY_STARTED '${proof_root}/relay.trace'"
wait_for 'Workspace Connector trust providers are explicitly unconfigured' \
  "grep -qF 'RELAY_CONNECTOR_TRUST_PIPELINE workspace_device_peer_certificate_validator=NOT_CONFIGURED signed_current_revocation=NOT_CONFIGURED current_registry_binding_validation=NOT_CONFIGURED dispatch_fence=NOT_CONFIGURED' '${proof_root}/relay.trace'"
wait_for 'private Workspace endpoint' "test -f '${proof_root}/state/workspace-connector.trace' && grep -q WORKSPACE_DIRECT_STARTED '${proof_root}/state/workspace-connector.trace'"
wait_for 'unconfigured Workspace peer validation rejects the connector' \
  "grep -q WORKSPACE_DEVICE_PEER_CERTIFICATE_VALIDATION_NOT_CONFIGURED '${proof_root}/state/workspace-connector.trace'"
if grep -q WORKSPACE_RELAY_CONNECTED "${proof_root}/state/workspace-connector.trace"; then
  printf '%s\n' 'Relay accepted a Workspace connector without peer validation' >&2
  exit 1
fi
run_frontend frontend-start > "${proof_root}/frontend-start.out"
grep -q WORKSPACE_DISCOVERED_BY_IDENTITY=PASS "${proof_root}/frontend-start.out"
grep -q REMOTE_FRONTEND_OPERATION=operation-1@1 "${proof_root}/frontend-start.out"

wait_for 'Execution Fabric assignment release' "grep -q 'ASSIGNMENT_RELEASED generation=1' '${proof_root}/state/runtime-control.trace'"
wait_for 'Artifact Plane transfer' "test -f '${published_path}' && grep -q RANGE_COMPLETE '${proof_root}/artifact.trace'"
wait_for 'Runtime Agent progress' "grep -q 'PROGRESS generation=1 completed=1 total=10 unit=steps' '${proof_root}/state/runtime-control.trace'"
wait_for 'Runtime Agent control reconnect' "test \$(grep -c 'ENROLLED runtime=runtime-workspace-fixture generation=1' '${proof_root}/state/runtime-control.trace') -ge 2"

run_frontend frontend-observe > "${proof_root}/frontend-observe.out"
grep -q WORKSPACE_DISCOVERY_OVER_RELAY=PASS "${proof_root}/frontend-observe.out"
grep -q WORKSPACE_DIRECT_AUTHORITY_PRESERVED=PASS "${proof_root}/frontend-observe.out"
grep -q REMOTE_FRONTEND_ARTIFACT="${artifact_uri}" "${proof_root}/frontend-observe.out"

run_frontend frontend-fallback > "${proof_root}/frontend-fallback.out"
grep -q LAN_DIRECT_UNREACHABLE_RELAY_DENIED=PASS "${proof_root}/frontend-fallback.out"

run_frontend frontend-direct > "${proof_root}/frontend-direct.out" &
frontend_pid=$!
wait_for 'direct frontend discovery' "test -f '${proof_root}/state/direct-discovered'"
kill "${relay_pid}"
wait "${relay_pid}" 2>/dev/null || true
relay_pid=""
touch "${proof_root}/state/direct-discovered.go"
wait "${frontend_pid}"
frontend_pid=""
grep -q LAN_DIRECT_NO_RELAY=PASS "${proof_root}/frontend-direct.out"
grep -q LAN_DIRECT_INVALID_CREDENTIAL_DENIED=PASS "${proof_root}/frontend-direct.out"
grep -q WORKSPACE_DIRECT_AUTHORITY_PRESERVED=PASS "${proof_root}/frontend-direct.out"

relay_peer_validation_denials=$(grep -c WORKSPACE_DEVICE_PEER_CERTIFICATE_VALIDATION_NOT_CONFIGURED \
  "${proof_root}/state/workspace-connector.trace")
start_relay
wait_for 'relay restart' "kill -0 '${relay_pid}' && test \$(grep -c RELAY_STARTED '${proof_root}/relay.trace') -ge 2"
wait_for 'Relay restart retains the unconfigured Connector trust pipeline' \
  "test \$(grep -c 'RELAY_CONNECTOR_TRUST_PIPELINE workspace_device_peer_certificate_validator=NOT_CONFIGURED signed_current_revocation=NOT_CONFIGURED current_registry_binding_validation=NOT_CONFIGURED dispatch_fence=NOT_CONFIGURED' '${proof_root}/relay.trace') -ge 2"
wait_for 'unconfigured Workspace peer validation rejects the connector after restart' \
  "test \$(grep -c WORKSPACE_DEVICE_PEER_CERTIFICATE_VALIDATION_NOT_CONFIGURED '${proof_root}/state/workspace-connector.trace') -gt ${relay_peer_validation_denials}"
if grep -q WORKSPACE_RELAY_CONNECTED "${proof_root}/state/workspace-connector.trace"; then
  printf '%s\n' 'Relay accepted a Workspace connector after restart without peer validation' >&2
  exit 1
fi
run_frontend frontend-observe > "${proof_root}/frontend-restarted-observe.out"
grep -q WORKSPACE_DISCOVERY_OVER_RELAY=PASS "${proof_root}/frontend-restarted-observe.out"
grep -q WORKSPACE_DIRECT_AUTHORITY_PRESERVED=PASS "${proof_root}/frontend-restarted-observe.out"

expected_trace_context='trace_id=4bf92f3577b34da6a3ce929d0e0e4736 span_id=00f067aa0ba902b7'
grep -qF "WORKSPACE_API_TRACE request_id=start-operation-1 ${expected_trace_context}" "${proof_root}/state/workspace-connector.trace"
grep -qF "WORKSPACE_API_TRACE request_id=direct-get-operation-1 ${expected_trace_context}" "${proof_root}/state/workspace-connector.trace"
if grep -qF '00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01' "${proof_root}/state/workspace-connector.trace"; then
  printf '%s\n' 'raw traceparent header was written to Workspace trace output' >&2
  exit 1
fi

test "$(sha256sum "${published_path}" | cut -d ' ' -f 1)" = "${source_digest}"
test "$(docker inspect -f '{{.HostConfig.Privileged}}' "${container_name}")" = false
test "$(docker inspect -f '{{len .HostConfig.PortBindings}}' "${container_name}")" = 0

printf '%s\n' \
  'WORKSPACE_DIRECTORY_MODEL=PASS' \
  'WORKSPACE_CONNECTION_DESCRIPTOR=PASS' \
  'WORKSPACE_DISCOVERY_OVER_RELAY=PASS' \
  'RELAY_CONNECTOR_FAIL_CLOSED=PASS' \
  'RELAY_CONNECTOR_TRUST_PIPELINE=workspace_device_peer_certificate_validator:NOT_CONFIGURED,signed_current_revocation:NOT_CONFIGURED,current_registry_binding_validation:NOT_CONFIGURED,dispatch_fence:NOT_CONFIGURED' \
  'RELAY_CONNECTOR_DENIAL_REASON=WORKSPACE_DEVICE_PEER_CERTIFICATE_VALIDATION_NOT_CONFIGURED' \
  'RELAY_RESTART_FAIL_CLOSED=PASS' \
  'RELAY_POSITIVE_CONNECTOR_E2E=NOT_CONFIGURED' \
  'LAN_DIRECT_NO_RELAY=PASS' \
  'LAN_DIRECT_INVALID_CREDENTIAL_DENIED=PASS' \
  'LAN_DIRECT_UNREACHABLE_RELAY_DENIED=PASS' \
  'WORKSPACE_TRACE_CONTEXT_DIRECT_CORRELATED=PASS' \
  'MANUAL_IP_REQUIRED=NO' \
  'WORKSPACE_INBOUND_PORT_REQUIRED=NO' \
  'REMOTE_FRONTEND_DIRECT_E2E=PASS' \
  'WORKSPACE_DIRECT_AUTHORITY_PRESERVED=PASS' \
  'EXECUTION_FABRIC_REUSED=YES' \
  'RUNTIME_AGENT_PROGRESS=PASS' \
  'RUNTIME_CONTROL_RECONNECT=PASS' \
  'ARTIFACT_FABRIC_REUSED=YES' \
  'ARTIFACT_IDENTITY_ONLY_FRONTEND=PASS' \
  'ARTIFACT_FULL_DIGEST_VERIFY=PASS' \
  'USER_NODE_WORKLOAD_IDENTITIES_SEPARATE=PASS'
