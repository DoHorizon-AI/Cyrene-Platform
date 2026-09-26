#!/usr/bin/env bash
# Real Docker acceptance for the Distributed Execution Fabric v1 reference vertical.
# Distributed Execution Fabric v1 真实 Docker 纵向验收。

set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
proof_root=$(mktemp -d /tmp/cyrene-def-v1.XXXXXX)
container_prefix="cyrene-def-v1-$$"
allocate_port() {
    python3 -c 'import socket; value=socket.socket(); value.bind(("127.0.0.1", 0)); print(value.getsockname()[1]); value.close()'
}
control_port=$(allocate_port)
artifact_port=$(allocate_port)
while [[ "${artifact_port}" = "${control_port}" ]]; do artifact_port=$(allocate_port); done
base_image=${CYRENE_ACCEPTANCE_BASE_IMAGE:-ubuntu:24.04}
control_pid=""
artifact_pid=""
proof_outcomes=()
proof_pass=0
proof_fail=0
proof_skip=0

record_outcome() {
    local name=$1
    local status=$2
    proof_outcomes+=("${name}=${status}")
    case "${status}" in
        PASS) ((proof_pass += 1)) ;;
        FAIL) ((proof_fail += 1)) ;;
        SKIP) ((proof_skip += 1)) ;;
    esac
}

print_outcomes() {
    printf '%s\n' "${proof_outcomes[@]}"
    printf 'NAMED_OUTCOMES_PASS=%s\nNAMED_OUTCOMES_FAIL=%s\nNAMED_OUTCOMES_SKIP=%s\n' \
        "${proof_pass}" "${proof_fail}" "${proof_skip}"
}

assert_named() {
    local name=$1
    local command=$2
    if bash -c "${command}"; then
        record_outcome "${name}" PASS
        return 0
    fi
    record_outcome "${name}" FAIL
    print_outcomes >&2
    return 1
}

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
source_digest=$(sha256sum "${proof_root}/artifact.bin" | cut -d ' ' -f 1)
published_path="${proof_root}/artifacts/${source_digest}"

openssl req -x509 -newkey rsa:2048 -nodes -days 1 -subj '/CN=Cyrene Fixture CA' -keyout "${proof_root}/certs/ca.key" -out "${proof_root}/certs/ca.crt" >/dev/null 2>&1
openssl req -newkey rsa:2048 -nodes -subj '/CN=cyrene-control.test' -keyout "${proof_root}/certs/server.key" -out "${proof_root}/certs/server.csr" >/dev/null 2>&1
printf '%s\n' 'subjectAltName=DNS:cyrene-control.test,DNS:cyrene-artifact.test' 'extendedKeyUsage=serverAuth' > "${proof_root}/certs/server.ext"
openssl x509 -req -days 1 -in "${proof_root}/certs/server.csr" -CA "${proof_root}/certs/ca.crt" -CAkey "${proof_root}/certs/ca.key" -CAcreateserial -extfile "${proof_root}/certs/server.ext" -out "${proof_root}/certs/server.crt" >/dev/null 2>&1
openssl req -newkey rsa:2048 -nodes -subj '/CN=cyrene-runtime-agent-fixture' -keyout "${proof_root}/certs/client.key" -out "${proof_root}/certs/client.csr" >/dev/null 2>&1
printf '%s\n' 'extendedKeyUsage=clientAuth' > "${proof_root}/certs/client.ext"
openssl x509 -req -days 1 -in "${proof_root}/certs/client.csr" -CA "${proof_root}/certs/ca.crt" -CAkey "${proof_root}/certs/ca.key" -CAcreateserial -extfile "${proof_root}/certs/client.ext" -out "${proof_root}/certs/client.crt" >/dev/null 2>&1
openssl rand -hex 32 > "${proof_root}/certs/artifact-ticket.key"
chmod 600 "${proof_root}/certs/client.key" "${proof_root}/certs/server.key" "${proof_root}/certs/artifact-ticket.key"

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
    --ticket-signature-file "${proof_root}/commands/artifact-ticket.signature" \
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
CYRENE_FIXTURE_ARTIFACT_TICKET_KEY="${proof_root}/certs/artifact-ticket.key" \
CYRENE_FIXTURE_TICKET_SIGNATURE="${proof_root}/commands/artifact-ticket.signature" \
CYRENE_FIXTURE_DISCONNECT_GENERATION=2 \
target/release/cy-runtime-control-fixture &
control_pid=$!

wait_for() {
    local description=$1
    local command=$2
    local outcome_name=${3:-}
    for _ in $(seq 1 300); do
        if bash -c "${command}"; then
            if [[ -n "${outcome_name}" ]]; then record_outcome "${outcome_name}" PASS; fi
            return 0
        fi
        sleep 0.1
    done
    if [[ -n "${outcome_name}" ]]; then record_outcome "${outcome_name}" FAIL; fi
    printf 'timed out waiting for %s\n' "${description}" >&2
    printf '%s\n' '--- runtime Agent logs ---' >&2
    for generation in 1 2 3 4; do
        if docker inspect "${container_prefix}-g${generation}" >/dev/null 2>&1; then
            printf '%s\n' "--- generation ${generation} ---" >&2
            docker logs "${container_prefix}-g${generation}" >&2 || true
        fi
    done
    printf '%s\n' '--- control trace ---' >&2
    tail -100 "${proof_root}/control.trace" >&2 || true
    printf '%s\n' '--- artifact trace ---' >&2
    tail -100 "${proof_root}/artifact.trace" >&2 || true
    print_outcomes >&2
    return 1
}

run_agent() {
    local generation=$1
    local token=$2
    local node_id=$3
    local persistent=$4
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
        --artifact-ticket-key /certs/artifact-ticket.key \
        --organization-id organization-fixture \
        --workspace-id workspace-fixture \
        --node-id "${node_id}" \
        --node-epoch 1 \
        --node-type container \
        --persistent "${persistent}" \
        --runtime-id runtime-fixture \
        --runtime-generation "${generation}" \
        --enrollment-proof "${token}" \
        --state-dir /state \
        --artifact-root /artifacts \
        -- /bin/sh /fixture/fake-workload.sh /state "${generation}" >/dev/null
}

wait_for 'fixture startup' "kill -0 '${control_pid}' && kill -0 '${artifact_pid}' && grep -q FIXTURE_STARTED '${proof_root}/control.trace'" FIXTURE_STARTUP

# Interrupt a real multipart transfer by killing the first Agent container.
# 通过 kill 第一代 Agent 容器中断真实 multipart 传输。
run_agent 1 development-token-1 node-ephemeral-1 false
wait_for 'capability and inventory advertisement' "grep -q 'generation=1.*node=node-ephemeral-1.*capabilities=2' '${proof_root}/control.trace' && grep -q 'INVENTORY generation=1' '${proof_root}/control.trace'" CAPABILITY_INVENTORY_ADVERTISEMENT
wait_for 'durable Artifact checkpoint' "grep -q '\"index\"' '${proof_root}/state/'*.checkpoint.json" ARTIFACT_CHECKPOINT_CREATED
docker kill "${container_prefix}-g1" >/dev/null
docker wait "${container_prefix}-g1" >/dev/null
assert_named ARTIFACT_PARTIAL_NOT_PUBLISHED "test ! -e '${published_path}'"

# A new Runtime generation resumes verified parts, reconnects, renews, and stops gracefully.
# 新 Runtime generation 恢复已验证 part，随后完成重连、续约与优雅停止。
run_agent 2 development-token-2 node-persistent-1 true
wait_for 'Artifact resume evidence' "grep -Eq 'generation=2.*reason=ARTIFACT_RESUMED.*summary=reused [1-9][0-9]* verified parts and downloaded [1-9][0-9]*' '${proof_root}/control.trace'" ARTIFACT_INTERRUPTION_RESUME
wait_for 'replacement Attempt identity' "grep -q 'ASSIGNMENT generation=1 logical_run=logical-run-1 attempt=attempt-1' '${proof_root}/control.trace' && grep -q 'ASSIGNMENT generation=2 logical_run=logical-run-1 attempt=attempt-2' '${proof_root}/control.trace'" REPLACEMENT_ATTEMPT_IDENTITY
wait_for 'running observation' "grep -q 'generation=2.*reason=WORKLOAD_RUNNING' '${proof_root}/control.trace'" WORKLOAD_RUNNING
wait_for 'Node heartbeat' "grep -q 'NODE_HEARTBEAT generation=2 node=node-persistent-1 epoch=1' '${proof_root}/control.trace'" NODE_HEARTBEAT
wait_for 'structured workload event' "grep -q 'STRUCTURED_EVENT generation=2 kind=workload.log schema=cyrene.workload.log.v1' '${proof_root}/control.trace'" STRUCTURED_EVENT_FORWARDING
wait_for 'workload progress' "grep -q 'PROGRESS generation=2 completed=1 total=10 unit=steps' '${proof_root}/control.trace'" PROGRESS_FORWARDING
wait_for 'forced control disconnect' "grep -q 'CONTROL_CHANNEL_FORCED_DISCONNECT generation=2' '${proof_root}/control.trace'" FORCED_CONTROL_DISCONNECT
wait_for 'authenticated reconnect' "test \$(grep -c 'ENROLLED runtime=runtime-fixture generation=2' '${proof_root}/control.trace') -ge 2" CONTROL_CHANNEL_RECONNECT
wait_for 'persistent Node reconnect' "grep -q 'PERSISTENT_NODE_RECONNECTED node=node-persistent-1 state=ONLINE' '${proof_root}/control.trace'" PERSISTENT_NODE_RECONNECT
wait_for 'original assignment replay acknowledgment' "grep -q 'ASSIGNMENT_ACK generation=2 disposition=2' '${proof_root}/control.trace'" ORIGINAL_ASSIGNMENT_REPLAY
wait_for 'Lease renewal' "grep -q 'LEASE_RENEWED generation=2' '${proof_root}/control.trace'" LEASE_RENEWAL
touch "${proof_root}/commands/stop-2"
wait_for 'StopAck' "grep -q 'STOP_ACK generation=2' '${proof_root}/control.trace'" STOP_ACK
wait_for 'graceful terminal observation' "grep -q 'generation=2.*reason=GRACEFUL_TERMINATION' '${proof_root}/control.trace'" GRACEFUL_TERMINATION
wait_for 'digest-addressed log reference' "grep -q 'LOG_REFERENCE generation=2 artifact=artifact://sha256/.*digest=sha256:.*size=' '${proof_root}/control.trace'" LOG_REFERENCE_FORWARDING
stop_ack_line=$(grep -n -m1 'STOP_ACK generation=2' "${proof_root}/control.trace" | cut -d: -f1)
terminal_line=$(grep -n -m1 'generation=2.*reason=GRACEFUL_TERMINATION' "${proof_root}/control.trace" | cut -d: -f1)
assert_named STOP_ACK_PRECEDES_TERMINAL "test '${stop_ack_line}' -lt '${terminal_line}'"
assert_named GRACEFUL_CONTAINER_EXIT "docker wait '${container_prefix}-g2' >/dev/null"

assert_named ARTIFACT_ATOMIC_PUBLISH "test -f '${published_path}'"
assert_named ARTIFACT_FULL_DIGEST_VERIFY "test \"\$(sha256sum '${published_path}' | cut -d ' ' -f 1)\" = '${source_digest}'"
assert_named WORKLOAD_GRACEFUL_MARKER "test -f '${proof_root}/state/workload-graceful-2'"
assert_named ARTIFACT_TEMPFILE_CLEAN "test -z \"\$(find '${proof_root}/artifacts' -name '*.publish-*' -print -quit)\""
assert_named ARTIFACT_RANGE_TRANSFER "grep -q RANGE_COMPLETE '${proof_root}/artifact.trace'"
assert_named ARTIFACT_SIGNED_TICKET_ACCEPTED "! grep -q TICKET_REJECTED '${proof_root}/artifact.trace'"

# Kill a running generation and let canonical Lease disappearance classify loss.
# kill 运行中的 generation，并由 canonical Lease 消失判定非预期丢失。
run_agent 3 development-token-3 node-ephemeral-3 false
wait_for 'generation 3 running' "grep -q 'generation=3.*reason=WORKLOAD_RUNNING' '${proof_root}/control.trace'" GENERATION_3_RUNNING
docker kill "${container_prefix}-g3" >/dev/null
docker wait "${container_prefix}-g3" >/dev/null
wait_for 'Lease expiry unexpected loss' "grep -q 'LEASE_EXPIRED generation=3 classification=UNEXPECTED_LOSS' '${proof_root}/control.trace'" LEASE_EXPIRY_UNEXPECTED_LOSS
wait_for 'ephemeral Node termination' "grep -q 'EPHEMERAL_NODE_TERMINATED node=node-ephemeral-3 state=TERMINATED' '${proof_root}/control.trace'" EPHEMERAL_NODE_TERMINATION

# The old generation can connect physically but cannot become authority again.
# 旧 generation 即使能建立物理连接，也不能恢复权威。
docker rm "${container_prefix}-g2" >/dev/null
run_agent 2 development-token-12 node-stale-2 false
wait_for 'stale generation rejection' "grep -q 'STALE_GENERATION_REJECTED runtime=runtime-fixture generation=2 authority=3' '${proof_root}/control.trace'" STALE_GENERATION_REJECTED
docker kill "${container_prefix}-g2" >/dev/null
docker wait "${container_prefix}-g2" >/dev/null

# Docker stop reaches PID 1 and the Agent propagates SIGTERM to the child group.
# docker stop 到达 PID 1 后，Agent 将 SIGTERM 传递给 workload 进程组。
run_agent 4 development-token-4 node-ephemeral-4 false
wait_for 'generation 4 running' "grep -q 'generation=4.*reason=WORKLOAD_RUNNING' '${proof_root}/control.trace'" GENERATION_4_RUNNING
docker stop "${container_prefix}-g4" >/dev/null
assert_named DOCKER_STOP_PROPAGATES_GRACEFULLY "test -f '${proof_root}/state/workload-graceful-4'"
assert_named LOCAL_TERMINATION_CLASSIFICATION "test \"\$(cat '${proof_root}/state/last-termination')\" = GRACEFUL_TERMINATION"

privileged=$(docker inspect -f '{{.HostConfig.Privileged}}' "${container_prefix}-g4")
port_binding_count=$(docker inspect -f '{{len .HostConfig.PortBindings}}' "${container_prefix}-g4")
assert_named CONTAINER_MODE_HOST_PRIVILEGE_ABSENT "test '${privileged}' = false"
assert_named CONTAINER_MODE_INBOUND_PORT_ABSENT "test '${port_binding_count}' = 0"
print_outcomes
