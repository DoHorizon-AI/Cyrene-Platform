#!/usr/bin/env bash
# Real Docker acceptance for the Distributed Execution Fabric v1 reference vertical.
# Distributed Execution Fabric v1 真实 Docker 纵向验收。

set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
proof_root=$(mktemp -d /tmp/cyrene-def-v1.XXXXXX)
container_prefix="cyrene-def-v1-$$"
control_port=$((40000 + ($$ % 10000)))
artifact_port=$((control_port + 1))
base_image=${CYRENE_ACCEPTANCE_BASE_IMAGE:-ubuntu:24.04}
control_pid=""
artifact_pid=""

cleanup() {
    for generation in 1 2 3 4; do
        docker rm -f "${container_prefix}-g${generation}" >/dev/null 2>&1 || true
    done
    if [[ -n "${control_pid}" ]]; then kill "${control_pid}" >/dev/null 2>&1 || true; fi
    if [[ -n "${artifact_pid}" ]]; then kill "${artifact_pid}" >/dev/null 2>&1 || true; fi
    rm -rf "${proof_root}"
}
trap cleanup EXIT

for command in cargo docker openssl python3 sha256sum; do
    command -v "${command}" >/dev/null || { printf 'required command is missing: %s\n' "${command}" >&2; exit 1; }
done

mkdir -p "${proof_root}/certs" "${proof_root}/commands" "${proof_root}/state" "${proof_root}/artifacts"
chmod 700 "${proof_root}/certs" "${proof_root}/commands" "${proof_root}/state" "${proof_root}/artifacts"
dd if=/dev/zero of="${proof_root}/artifact.bin" bs=1M count=64 status=none

openssl req -x509 -newkey rsa:2048 -nodes -days 1 -subj '/CN=Cyrene Fixture CA' -keyout "${proof_root}/certs/ca.key" -out "${proof_root}/certs/ca.crt" >/dev/null 2>&1
openssl req -newkey rsa:2048 -nodes -subj '/CN=cyrene-control.test' -keyout "${proof_root}/certs/server.key" -out "${proof_root}/certs/server.csr" >/dev/null 2>&1
printf '%s\n' 'subjectAltName=DNS:cyrene-control.test,DNS:cyrene-artifact.test' 'extendedKeyUsage=serverAuth' > "${proof_root}/certs/server.ext"
openssl x509 -req -days 1 -in "${proof_root}/certs/server.csr" -CA "${proof_root}/certs/ca.crt" -CAkey "${proof_root}/certs/ca.key" -CAcreateserial -extfile "${proof_root}/certs/server.ext" -out "${proof_root}/certs/server.crt" >/dev/null 2>&1
openssl req -newkey rsa:2048 -nodes -subj '/CN=cyrene-runtime-agent-fixture' -keyout "${proof_root}/certs/client.key" -out "${proof_root}/certs/client.csr" >/dev/null 2>&1
printf '%s\n' 'extendedKeyUsage=clientAuth' > "${proof_root}/certs/client.ext"
openssl x509 -req -days 1 -in "${proof_root}/certs/client.csr" -CA "${proof_root}/certs/ca.crt" -CAkey "${proof_root}/certs/ca.key" -CAcreateserial -extfile "${proof_root}/certs/client.ext" -out "${proof_root}/certs/client.crt" >/dev/null 2>&1
chmod 600 "${proof_root}/certs/client.key" "${proof_root}/certs/server.key"

cd "${repo_root}"
cargo build --locked --release -p cy-runtime-agent --bins
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
    --trace "${proof_root}/artifact.trace" &
artifact_pid=$!

CYRENE_FIXTURE_BIND="0.0.0.0:${control_port}" \
CYRENE_FIXTURE_TRACE="${proof_root}/control.trace" \
CYRENE_FIXTURE_COMMAND_DIR="${proof_root}/commands" \
CYRENE_FIXTURE_ARTIFACT_FILE="${proof_root}/artifact.bin" \
CYRENE_FIXTURE_ARTIFACT_URL="https://cyrene-artifact.test:${artifact_port}/artifact.bin" \
CYRENE_FIXTURE_SERVER_CERT="${proof_root}/certs/server.crt" \
CYRENE_FIXTURE_SERVER_KEY="${proof_root}/certs/server.key" \
CYRENE_FIXTURE_CLIENT_CA="${proof_root}/certs/ca.crt" \
CYRENE_FIXTURE_DISCONNECT_GENERATION=2 \
target/release/cy-runtime-control-fixture &
control_pid=$!

wait_for() {
    local description=$1
    local command=$2
    for _ in $(seq 1 300); do
        if bash -c "${command}"; then return 0; fi
        sleep 0.1
    done
    printf 'timed out waiting for %s\n' "${description}" >&2
    printf '%s\n' '--- control trace ---' >&2
    tail -100 "${proof_root}/control.trace" >&2 || true
    printf '%s\n' '--- artifact trace ---' >&2
    tail -100 "${proof_root}/artifact.trace" >&2 || true
    return 1
}

run_agent() {
    local generation=$1
    local token=$2
    docker run -d --name "${container_prefix}-g${generation}" \
        --user "$(id -u):$(id -g)" \
        --add-host cyrene-control.test:host-gateway \
        --add-host cyrene-artifact.test:host-gateway \
        -e "CYRENE_RUNTIME_GENERATION=${generation}" \
        -v "${proof_root}/certs:/certs:ro" \
        -v "${proof_root}/state:/state" \
        -v "${proof_root}/artifacts:/artifacts" \
        -v "${repo_root}/target/release/cy-runtime-agent:/runtime/cy-runtime-agent:ro" \
        -v "${repo_root}/tooling/acceptance/distributed-execution-fabric/fake-workload.sh:/fixture/fake-workload.sh:ro" \
        --entrypoint /runtime/cy-runtime-agent \
        "${base_image}" run \
        --control-plane "https://cyrene-control.test:${control_port}" \
        --control-plane-server-name cyrene-control.test \
        --control-plane-ca /certs/ca.crt \
        --client-certificate /certs/client.crt \
        --client-key /certs/client.key \
        --artifact-ca /certs/ca.crt \
        --organization-id organization-fixture \
        --workspace-id workspace-fixture \
        --runtime-id runtime-fixture \
        --runtime-generation "${generation}" \
        --enrollment-proof "${token}" \
        --state-dir /state \
        --artifact-root /artifacts \
        -- /fixture/fake-workload.sh /state >/dev/null
}

wait_for 'fixture startup' "grep -q FIXTURE_STARTED '${proof_root}/control.trace'"

# Interrupt a real multipart transfer by killing the first Agent container.
# 通过 kill 第一代 Agent 容器中断真实 multipart 传输。
run_agent 1 development-token-1
wait_for 'capability and inventory advertisement' "grep -q 'generation=1.*capabilities=2' '${proof_root}/control.trace' && grep -q 'INVENTORY generation=1' '${proof_root}/control.trace'"
wait_for 'durable Artifact checkpoint' "grep -q '\"index\"' '${proof_root}/state/'*.checkpoint.json"
docker kill "${container_prefix}-g1" >/dev/null
docker wait "${container_prefix}-g1" >/dev/null

# A new Runtime generation resumes verified parts, reconnects, renews, and stops gracefully.
# 新 Runtime generation 恢复已验证 part，随后完成重连、续约与优雅停止。
run_agent 2 development-token-2
wait_for 'Artifact resume evidence' "grep -q 'generation=2.*reason=ARTIFACT_RESUMED' '${proof_root}/control.trace'"
wait_for 'replacement Attempt identity' "grep -q 'ASSIGNMENT generation=1 logical_run=logical-run-1 attempt=attempt-1' '${proof_root}/control.trace' && grep -q 'ASSIGNMENT generation=2 logical_run=logical-run-1 attempt=attempt-2' '${proof_root}/control.trace'"
wait_for 'running observation' "grep -q 'generation=2.*reason=WORKLOAD_RUNNING' '${proof_root}/control.trace'"
wait_for 'forced control disconnect' "grep -q 'CONTROL_CHANNEL_FORCED_DISCONNECT generation=2' '${proof_root}/control.trace'"
wait_for 'authenticated reconnect' "test \$(grep -c 'ENROLLED runtime=runtime-fixture generation=2' '${proof_root}/control.trace') -ge 2"
wait_for 'Lease renewal' "grep -q 'LEASE_RENEWED generation=2' '${proof_root}/control.trace'"
touch "${proof_root}/commands/stop-2"
wait_for 'StopAck' "grep -q 'STOP_ACK generation=2' '${proof_root}/control.trace'"
wait_for 'graceful terminal observation' "grep -q 'generation=2.*reason=GRACEFUL_TERMINATION' '${proof_root}/control.trace'"
stop_ack_line=$(grep -n -m1 'STOP_ACK generation=2' "${proof_root}/control.trace" | cut -d: -f1)
terminal_line=$(grep -n -m1 'generation=2.*reason=GRACEFUL_TERMINATION' "${proof_root}/control.trace" | cut -d: -f1)
test "${stop_ack_line}" -lt "${terminal_line}"
docker wait "${container_prefix}-g2" >/dev/null

source_digest=$(sha256sum "${proof_root}/artifact.bin" | cut -d ' ' -f 1)
published_path="${proof_root}/artifacts/${source_digest}"
test -f "${published_path}"
test "$(sha256sum "${published_path}" | cut -d ' ' -f 1)" = "${source_digest}"
test -f "${proof_root}/state/workload-graceful-2"
test -z "$(find "${proof_root}/artifacts" -name '*.publish-*' -print -quit)"

# Kill a running generation and let canonical Lease disappearance classify loss.
# kill 运行中的 generation，并由 canonical Lease 消失判定非预期丢失。
run_agent 3 development-token-3
wait_for 'generation 3 running' "grep -q 'generation=3.*reason=WORKLOAD_RUNNING' '${proof_root}/control.trace'"
docker kill "${container_prefix}-g3" >/dev/null
docker wait "${container_prefix}-g3" >/dev/null
wait_for 'Lease expiry unexpected loss' "grep -q 'LEASE_EXPIRED generation=3 classification=UNEXPECTED_LOSS' '${proof_root}/control.trace'"

# The old generation can connect physically but cannot become authority again.
# 旧 generation 即使能建立物理连接，也不能恢复权威。
docker rm "${container_prefix}-g2" >/dev/null
run_agent 2 development-token-12
wait_for 'stale generation rejection' "grep -q 'STALE_GENERATION_REJECTED runtime=runtime-fixture generation=2 authority=3' '${proof_root}/control.trace'"
docker kill "${container_prefix}-g2" >/dev/null
docker wait "${container_prefix}-g2" >/dev/null

# Docker stop reaches PID 1 and the Agent propagates SIGTERM to the child group.
# docker stop 到达 PID 1 后，Agent 将 SIGTERM 传递给 workload 进程组。
run_agent 4 development-token-4
wait_for 'generation 4 running' "grep -q 'generation=4.*reason=WORKLOAD_RUNNING' '${proof_root}/control.trace'"
docker stop "${container_prefix}-g4" >/dev/null
test -f "${proof_root}/state/workload-graceful-4"
test "$(cat "${proof_root}/state/last-termination")" = GRACEFUL_TERMINATION

privileged=$(docker inspect -f '{{.HostConfig.Privileged}}' "${container_prefix}-g4")
ports=$(docker inspect -f '{{json .HostConfig.PortBindings}}' "${container_prefix}-g4")
test "${privileged}" = false
test "${ports}" = null

printf '%s\n' \
    'CONTAINER_AGENT_REAL_E2E=PASS' \
    'CONTROL_CHANNEL_RECONNECT=PASS' \
    'LEASE_RENEWAL=PASS' \
    'LEASE_EXPIRY=PASS' \
    'GRACEFUL_TERMINATION=PASS' \
    'UNEXPECTED_CONTAINER_KILL=PASS' \
    'RUNTIME_GENERATION_FENCING=PASS' \
    'STALE_GENERATION_REJECTED=PASS' \
    'ARTIFACT_RANGE_TRANSFER=PASS' \
    'ARTIFACT_INTERRUPTION_RESUME=PASS' \
    'ARTIFACT_FULL_DIGEST_VERIFY=PASS' \
    'CONTAINER_MODE_REQUIRES_HOST_PRIVILEGE=NO' \
    'CONTAINER_MODE_REQUIRES_INBOUND_PORT=NO'
