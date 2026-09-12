#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Contract/unit gate for Distributed Workspace Fabric v1.
# Distributed Workspace Fabric v1 的 Contract 与单元测试门禁。

set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "${repo_root}"

workspace_proto=contracts/proto/cyrene/workspace/v1/workspace_fabric.proto
workspace_crate=framework/crates/cy-workspace-fabric
tck_root=contracts/tck/distributed-workspace-fabric/v1
acceptance_root=tooling/acceptance/distributed-workspace-fabric

for path in \
  "${workspace_proto}" \
  "${workspace_crate}/README.md" \
  "${workspace_crate}/src/README.md" \
  "${workspace_crate}/src/bin/README.md" \
  "${tck_root}/README.md" \
  "${tck_root}/scenarios.tsv" \
  "${acceptance_root}/README.md"; do
  test -f "${path}" || {
    printf 'missing Workspace Fabric artifact: %s\n' "${path}" >&2
    exit 1
  }
done

for message in \
  UserIdentityRef \
  DeviceEnrollmentRef \
  WorkspaceConnectionCandidate \
  WorkspaceConnectionDescriptor \
  RelayHello \
  RelayReady \
  DiscoverWorkspacesRequest \
  DiscoverWorkspacesResponse \
  WorkspaceApiRequest \
  WorkspaceApiResponse \
  WorkspaceOperationView \
  RelayForwardedRequest \
  RelayForwardedResponse \
  RelayFrame; do
  rg -q "message ${message}" "${workspace_proto}" || {
    printf 'missing Workspace Fabric wire message: %s\n' "${message}" >&2
    exit 1
  }
done

for mode in LOCAL LAN_DIRECT DIRECT OVERLAY RELAY; do
  rg -q "CONNECTIVITY_MODE_${mode}" contracts/proto/cyrene/core/v1/node_control.proto || {
    printf 'missing transport-neutral connectivity mode: %s\n' "${mode}" >&2
    exit 1
  }
done

service_count=$(rg -c '^service ' "${workspace_proto}" || true)
if [[ "${service_count}" != 1 ]]; then
  printf 'Workspace wire contract must define exactly one service; found %s\n' \
    "${service_count:-0}" >&2
  exit 1
fi
rg -q '^service WorkspaceRelayService' "${workspace_proto}"
rg -q 'rpc Connect\(stream RelayFrame\) returns \(stream RelayFrame\)' "${workspace_proto}"
rg -q 'trait WorkspaceDirectory' "${workspace_crate}/src/directory.rs"
rg -q 'trait RelayAuthenticator' "${workspace_crate}/src/auth.rs"
rg -q 'trait WorkspaceApi' "${workspace_crate}/src/api.rs"
rg -q 'LocalWorkspaceClient' "${workspace_crate}/src/api.rs"
if rg -n 'RelayConnectivityProvider|cy-execution-fabric' \
  "${workspace_crate}/src/bin/cy-workspace-fabric-fixture.rs" \
  "${workspace_crate}/Cargo.toml"; then
  printf 'Workspace Fabric public/client surface depends on execution-fabric implementation\n' >&2
  exit 1
fi
rg -q 'env -u CYRENE_WORKSPACE_SESSION_CREDENTIAL CYRENE_RUNTIME_GENERATION=1' \
  "${acceptance_root}/workspace-entrypoint.sh"

if rg -n -i 'host_ip|docker_ip|tailscale_ip|container_id|local_path|artifact_peer' "${workspace_proto}"; then
  printf 'Workspace contract exposes deployment-local addressing or transfer details\n' >&2
  exit 1
fi
if rg -n 'message (Lease|Fence|Event|Capability|Artifact|Runtime|Operation)[[:space:]]*\{' "${workspace_proto}"; then
  printf 'Workspace contract duplicates an existing canonical authority identity\n' >&2
  exit 1
fi
if rg -n 'struct (Lease|Fence|Event|Capability|ArtifactIdentity|ArtifactId|Runtime|Operation)\b' "${workspace_crate}/src"; then
  printf 'Workspace implementation duplicates an existing canonical authority type\n' >&2
  exit 1
fi
if rg -n 'Dataset|TrainingRun|ModelVersion|EvaluationRun|Deployment' "${workspace_proto}"; then
  printf 'Workspace wire contract contains Product-domain authority\n' >&2
  exit 1
fi
if rg -n 'WorkspaceOperationView|WorkspaceApiRequest|WorkspaceApiResponse' \
  "${workspace_crate}/src/directory.rs" "${workspace_crate}/src/auth.rs"; then
  printf 'Directory or authentication boundary contains Workspace/Product state\n' >&2
  exit 1
fi
if rg -n 'Mutex<.*WorkspaceOperation|BTreeMap<.*WorkspaceOperation' "${workspace_crate}/src/relay.rs"; then
  printf 'Relay must not become a Workspace operation state authority\n' >&2
  exit 1
fi

bash -n \
  "${acceptance_root}/run-relay-proof.sh" \
  "${acceptance_root}/workspace-entrypoint.sh"

buf_bin=${BUF_BIN:-buf}
command -v "${buf_bin}" >/dev/null 2>&1 || {
  printf 'Buf is required for the Workspace wire contract check\n' >&2
  exit 127
}
(cd contracts/proto && "${buf_bin}" lint . --path cyrene/workspace/v1)

cargo test --locked -p cy-workspace-fabric -p cy-runtime-agent --no-fail-fast
cargo clippy --locked -p cy-workspace-fabric -p cy-runtime-agent --all-targets -- -D warnings
printf 'Distributed Workspace Fabric v1 contract checks passed\n'
