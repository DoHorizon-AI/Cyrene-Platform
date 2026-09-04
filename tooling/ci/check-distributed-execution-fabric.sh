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

for scenario in enrollment_welcome_replay credential_ambiguity welcome_ordering placement_assignment assignment_ack_after_start assignment_prepare_failure kernel_unavailable_unknown rollback_host_replacement verified_local_unrepresentable artifact_ticket_tamper artifact_legacy_uri_only; do
    rg -q "^${scenario}$(printf '\t')" "${tck_root}/scenarios.tsv" || { printf 'missing Fabric TCK scenario: %s\n' "${scenario}" >&2; exit 1; }
done

for message in ExecutionNodeDescriptor ExecutionAgentHello ExecutionAgentWelcome RuntimeAssignment AssignmentAck RuntimeHeartbeat LeaseRenewalRequest LeaseRenewalResult RuntimeObservation ProviderRuntimeObservation RuntimeProgress StructuredAgentEvent LogReference StopCommand StopAck ArtifactTransferSource ArtifactPartSource; do
    rg -q "message ${message}" "${core_root}/node_control.proto" || { printf 'missing wire message: %s\n' "${message}" >&2; exit 1; }
done

for value in HOST_AGENT CONTAINER_AGENT PROVIDER_MANAGED PERSISTENT EPHEMERAL NONE HOST_SUPERVISED PROVIDER_SUPERVISED LOCAL LAN_DIRECT DIRECT OVERLAY RELAY; do
    rg -q "_${value}" "${core_root}/node_control.proto" || { printf 'missing attachment value: %s\n' "${value}" >&2; exit 1; }
done
rg -q 'optional bool persistent' "${core_root}/node_control.proto"
rg -q 'cyrene.execution.fabric.v1' framework/crates/cy-execution-fabric/src/capability.rs
rg -q 'trait ConnectivityProvider' framework/crates/cy-execution-fabric/src/connectivity.rs
rg -q 'trait ProviderObservationSource' framework/crates/cy-execution-fabric/src/provider.rs
rg -q 'struct NodeLifecycleProjection' framework/crates/cy-execution-fabric/src/node.rs
rg -q 'struct ExecutionPlacementRequest' framework/crates/cy-execution-fabric/src/placement.rs
rg -q 'fn plan_execution_placement' framework/crates/cy-execution-fabric/src/placement.rs
rg -q 'struct ResourceMatchEvidence' framework/crates/cy-execution-fabric/src/placement.rs
rg -q 'struct ArtifactPlacementQuote' framework/crates/cy-execution-fabric/src/placement.rs
rg -q 'struct RuntimeAssignmentBuilder' framework/crates/cy-execution-fabric/src/assignment.rs
rg -q 'fn validate_artifact_projection' framework/crates/cy-execution-fabric/src/assignment.rs
rg -q 'struct ExecutionController' framework/crates/cy-execution-control/src/controller.rs
rg -q 'struct FileExecutionIntentStore' framework/crates/cy-execution-control/src/intent.rs
rg -q 'impl NodeControlService for ExecutionControlService' framework/crates/cy-execution-control/src/server.rs
rg -q 'struct ArtifactPeer' sdk/rust/cy-artifact-transfer/src/contract.rs
rg -q 'struct TransferEstimate' sdk/rust/cy-artifact-transfer/src/contract.rs
rg -q 'pub fn estimate' sdk/rust/cy-artifact-transfer/src/contract.rs
rg -q 'trait ArtifactSourceResolver' sdk/rust/cy-artifact-transfer/src/planner.rs
rg -q 'struct ArtifactTransferCoordinator' sdk/rust/cy-artifact-transfer/src/planner.rs
rg -q 'trait TransferTicketIssuer' sdk/rust/cy-artifact-transfer/src/ticket.rs
rg -q '\.verify\(&ticket\)' agents/runtime/cy-runtime-agent/src/agent.rs
rg -q 'pending_runtime_enrollments' framework/crates/cy-execution-control/src/server.rs
rg -q 'send_welcome' framework/crates/cy-execution-control/src/server.rs

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
if rg -n 'struct (ArtifactIdentity|Lease|Capability|Worker|Operation)\b' framework/crates/cy-execution-fabric framework/crates/cy-execution-control sdk/rust/cy-artifact-transfer; then
    printf 'Fabric implementation duplicates an existing canonical authority type\n' >&2
    exit 1
fi
if rg -n 'class NodeControlService|class NodeControlGrpcServer|activeNodes|usePlaintext\(' framework/jvm --glob '*.kt'; then
    printf 'JVM target must not contain a second NodeControl owner or plaintext Agent transport\n' >&2
    exit 1
fi
if rg -n 'NodeAgentGrpcClient|KernelOutboundAdapter|KernelCommandPort|MethodDescriptor<ByteArray|blockingUnaryCall' framework/jvm --glob '*.kt'; then
    printf 'JVM target must not expose a fake or dynamically constructed Kernel execution path\n' >&2
    exit 1
fi
if rg -n 'data class (Lease|Node|WorkerInstance|ArtifactReference)\b' framework/jvm/domain --glob '*.kt'; then
    printf 'JVM target must reuse canonical Kernel and Artifact identities instead of defining parallel models\n' >&2
    exit 1
fi
if rg -n 'legacy-local-ticket|legacy-local-compatibility|legacy-seed-peer' agents/runtime/cy-runtime-agent framework/crates/cy-execution-fabric; then
    printf 'Runtime Artifact staging contains a legacy authorization bypass\n' >&2
    exit 1
fi
rg -q 'cy-manifest' sdk/rust/cy-artifact-transfer/Cargo.toml
rg -q 'pub use cy_manifest::\{ArtifactKind, ArtifactRef\}' sdk/rust/cy-artifact-transfer/src/lib.rs

python3 -c 'import json, pathlib; schema=json.loads(pathlib.Path("contracts/schemas/artifact_transfer.schema.json").read_text()); required={"artifactPeer", "artifactReplica", "transferTicket", "transferPlan", "transferCheckpoint", "externalSource", "sourceImportJob", "sourceSnapshot"}; missing=required-set(schema["$defs"]); assert not missing, missing'
bash -n tooling/acceptance/distributed-execution-fabric/run-container-proof.sh
python3 -c 'import ast, pathlib; ast.parse(pathlib.Path("tooling/acceptance/distributed-execution-fabric/https_range_server.py").read_text())'

buf_bin=${BUF_BIN:-buf}
command -v "${buf_bin}" >/dev/null 2>&1 || { printf 'Buf is required for the frozen wire descriptor check\n' >&2; exit 127; }
(cd contracts/proto && "${buf_bin}" lint . --path cyrene/core/v1)
generated=$(mktemp)
trap 'rm -f "${generated}"' EXIT
(cd contracts/proto && "${buf_bin}" build . --path cyrene/core/v1 --as-file-descriptor-set --exclude-source-info --output "${generated}")
cmp "${generated}" "${stable_descriptor}"

expected_sha=$(rg -o '"descriptor_sha256": "[A-F0-9]+' "${fixture_manifest}" | sed 's/.*"//')
actual_sha=$(sha256sum "${stable_descriptor}" | awk '{print toupper($1)}')
test "${expected_sha}" = "${actual_sha}"

cargo fmt --all -- --check
cargo clippy --locked -p cy-execution-fabric -p cy-execution-control -p cy-artifact-transfer -p cy-runtime-agent --all-targets -- -D warnings
cargo test --locked -p cy-execution-fabric -p cy-execution-control -p cy-artifact-transfer -p cy-runtime-agent --no-fail-fast
printf 'Distributed Execution Fabric v1 contract checks passed\n'
