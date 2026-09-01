#!/usr/bin/env bash
# Contract/unit gate for Distributed Execution Fabric v1.
# Distributed Execution Fabric v1 的 Contract 与单元测试门禁。

set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "${repo_root}"

core_root=contracts/proto/cyrene/core/v1
tck_root=contracts/tck/distributed-execution-fabric/v1
schema=contracts/schemas/artifact_transfer.schema.json
stable_descriptor=contracts/descriptors/cyrene-core-v1-stable.pb
fixture_manifest=contracts/fixtures/core/v1/manifest.json

for path in "${tck_root}/README.md" "${tck_root}/scenarios.tsv" "${schema}"; do
    test -f "${path}" || { printf 'missing Fabric contract artifact: %s\n' "${path}" >&2; exit 1; }
done

for message in ExecutionAgentHello ExecutionAgentWelcome RuntimeAssignment AssignmentAck RuntimeHeartbeat LeaseRenewalRequest LeaseRenewalResult RuntimeObservation StopCommand StopAck; do
    rg -q "message ${message}" "${core_root}/node_control.proto" || { printf 'missing wire message: %s\n' "${message}" >&2; exit 1; }
done

for value in HOST_AGENT CONTAINER_AGENT PROVIDER_MANAGED PERSISTENT EPHEMERAL; do
    rg -q "_${value}" "${core_root}/node_control.proto" || { printf 'missing attachment value: %s\n' "${value}" >&2; exit 1; }
done

if [[ "$(rg -c '^service ' "${core_root}/cyrene_core.proto")" -ne 3 ]]; then
    printf 'Fabric v1 must not add a second control service\n' >&2
    exit 1
fi
if rg -n 'TrainingRun|Dataset|ModelVersion|Deployment|Catalyst|Echo|Reactor|Exchange|Navigator|Yield' "${core_root}/node_control.proto"; then
    printf 'Fabric wire contract contains Product semantics\n' >&2
    exit 1
fi
if rg -n -i 'container_id|docker_ip|host_ip|tailscale_ip' "${core_root}/node_control.proto" "${schema}"; then
    printf 'Fabric identity depends on deployment-local addressing\n' >&2
    exit 1
fi

python3 -c 'import json, pathlib; json.loads(pathlib.Path("contracts/schemas/artifact_transfer.schema.json").read_text())'
bash -n tooling/acceptance/distributed-execution-fabric/run-container-proof.sh
python3 -c 'import ast, pathlib; ast.parse(pathlib.Path("tooling/acceptance/distributed-execution-fabric/https_range_server.py").read_text())'

buf_bin=${BUF_BIN:-buf}
if command -v "${buf_bin}" >/dev/null 2>&1; then
    (cd contracts/proto && "${buf_bin}" lint . --path cyrene/core/v1)
    generated=$(mktemp)
    trap 'rm -f "${generated}"' EXIT
    (cd contracts/proto && "${buf_bin}" build . --path cyrene/core/v1 --as-file-descriptor-set --exclude-source-info --output "${generated}")
    cmp "${generated}" "${stable_descriptor}"
fi

expected_sha=$(rg -o '"descriptor_sha256": "[A-F0-9]+' "${fixture_manifest}" | sed 's/.*"//')
actual_sha=$(sha256sum "${stable_descriptor}" | awk '{print toupper($1)}')
test "${expected_sha}" = "${actual_sha}"

cargo test --locked -p cy-execution-fabric -p cy-artifact-transfer -p cy-runtime-agent --no-fail-fast
printf 'Distributed Execution Fabric v1 contract checks passed\n'
