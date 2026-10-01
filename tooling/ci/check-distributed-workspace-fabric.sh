#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Contract/unit gate for the Workspace Fabric v1 transport and v2 Product API.
# Workspace Fabric v1 transport 与 v2 Product API 的 Contract 和单元测试门禁。

set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "${repo_root}"

workspace_proto=contracts/proto/cyrene/workspace/v1/workspace_fabric.proto
product_v2_proto=contracts/proto/cyrene/workspace/product/v2/product_api.proto
workspace_crate=framework/crates/cy-workspace-fabric
client_sdk_crate=framework/crates/cy-workspace-client-sdk
control_plane_crate=framework/crates/cy-workspace-control-plane
relay_runtime_crate=framework/crates/cy-workspace-relay-runtime
storage_crate=framework/crates/cy-workspace-postgres-storage
sidecar_crate=framework/crates/cy-workspace-sidecar
tck_root=contracts/tck/distributed-workspace-fabric/v1
acceptance_root=tooling/acceptance/distributed-workspace-fabric
projection_manifest=${tck_root}/product-projections.tsv

for path in \
  "${workspace_proto}" \
  "${product_v2_proto}" \
  "${workspace_crate}/README.md" \
  "${workspace_crate}/src/README.md" \
  "${client_sdk_crate}/Cargo.toml" \
  "${client_sdk_crate}/src/lib.rs" \
  "${control_plane_crate}/src/README.md" \
  "${relay_runtime_crate}/src/README.md" \
  "${storage_crate}/src/README.md" \
  "${sidecar_crate}/src/README.md" \
  "${workspace_crate}/src/bin/README.md" \
  "${tck_root}/README.md" \
  "${tck_root}/scenarios.tsv" \
  "${projection_manifest}" \
  "${acceptance_root}/README.md"; do
  test -f "${path}" || {
    printf 'missing Workspace Fabric artifact: %s\n' "${path}" >&2
    exit 1
  }
done

for message in ProductApiInvocationV2 ProductApiResponseV2; do
  rg -q "message ${message}" "${product_v2_proto}" || {
    printf 'missing generic Workspace Product API v2 wire message: %s\n' "${message}" >&2
    exit 1
  }
done
rg -q 'string owner_id = 1;' "${product_v2_proto}"
rg -q 'string operation_id = 2;' "${product_v2_proto}"
rg -q 'bytes json_body = 3;' "${product_v2_proto}"
rg -q 'ProductApiInvocationV2 product_api_v2 = 13;' "${workspace_proto}"
rg -q 'ProductApiResponseV2 product_api_v2 = 13;' "${workspace_proto}"

# The v1 enum assertions below preserve its closed compatibility contract. New Product operations
# use the generic owner/operation IDs in v2 and are checked against pinned catalogs and policy.
rg -q 'WORKSPACE_PRODUCT_API_V1_UNSUPPORTED' \
  "${control_plane_crate}/src/control_plane.rs"
if rg -n 'sqlx|webauthn-rs|axum' "${workspace_crate}/Cargo.toml"; then
  printf 'Fabric facade must not contain database, WebAuthn, or HTTP server implementations\n' >&2
  exit 1
fi
if rg -n 'sqlx|webauthn-rs|axum|cy-workspace-fabric|cy-workspace-control-plane|cy-workspace-relay-runtime|cy-workspace-postgres-storage' \
  "${sidecar_crate}/Cargo.toml"; then
  printf 'Sidecar client dependency closure contains a Platform host implementation crate\n' >&2
  exit 1
fi
rg -q 'cy-workspace-client-sdk' "${sidecar_crate}/Cargo.toml"
rg -q 'default-features = false' "${client_sdk_crate}/Cargo.toml"
rg -q 'features = \["channel", "tls", "codegen", "prost"\]' \
  "${client_sdk_crate}/Cargo.toml"
if rg -n 'tonic = .*server|cy-proto = .*server' "${client_sdk_crate}/Cargo.toml"; then
  printf 'Workspace client SDK must not enable tonic server support\n' >&2
  exit 1
fi

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
  WorkspaceProductApiRequest \
  WorkspaceProductApiResponse \
  WorkspaceDirectRequest \
  WorkspaceOperationView \
  RelayForwardedRequest \
  RelayForwardedResponse \
  RelayFrame; do
  rg -q "message ${message}" "${workspace_proto}" || {
    printf 'missing Workspace Fabric wire message: %s\n' "${message}" >&2
    exit 1
  }
done

for enum in WorkspaceProductApiOwner WorkspaceProductApiOperation WorkspaceProductApiRequestKind; do
  rg -q "enum ${enum}" "${workspace_proto}" || {
    printf 'missing Workspace Product API closed enum: %s\n' "${enum}" >&2
    exit 1
  }
done
rg -q 'enum WorkspaceProductApiContentType' "${workspace_proto}"

python3 - "${workspace_proto}" "${projection_manifest}" <<'PY'
import csv
import re
import sys
from collections import Counter, defaultdict
from pathlib import Path

proto_path, manifest_path = map(Path, sys.argv[1:])
proto = proto_path.read_text(encoding="utf-8")


def enum_values(name: str) -> dict[str, int]:
    match = re.search(rf"enum\s+{name}\s*\{{(.*?)\}}", proto, re.DOTALL)
    if match is None:
        raise SystemExit(f"missing enum definition: {name}")
    return {
        value: int(number)
        for value, number in re.findall(r"^\s*(\w+)\s*=\s*(\d+)\s*;", match.group(1), re.MULTILINE)
    }


expected_owners = {
    f"WORKSPACE_PRODUCT_API_OWNER_{name}": number
    for number, name in enumerate(("UNSPECIFIED", "CATALYST", "YIELD", "REACTOR", "EXCHANGE", "ECHO", "NAVIGATOR"))
}
if enum_values("WorkspaceProductApiOwner") != expected_owners:
    raise SystemExit("Workspace Product API owner enum does not match the TCK owner set")

expected_operations = {
    "WORKSPACE_PRODUCT_API_OPERATION_UNSPECIFIED": 0,
    **{f"WORKSPACE_PRODUCT_API_OPERATION_{number:02d}": number for number in range(1, 14)},
}
if enum_values("WorkspaceProductApiOperation") != expected_operations:
    raise SystemExit("Workspace Product API operation enum does not match the closed TCK operation set")

expected_kinds = {
    "WORKSPACE_PRODUCT_API_REQUEST_KIND_UNSPECIFIED": 0,
    "WORKSPACE_PRODUCT_API_REQUEST_KIND_READ": 1,
    "WORKSPACE_PRODUCT_API_REQUEST_KIND_COMMAND": 2,
}
if enum_values("WorkspaceProductApiRequestKind") != expected_kinds:
    raise SystemExit("Workspace Product API request-kind enum has an unexpected value set")
expected_content_types = {
    "WORKSPACE_PRODUCT_API_CONTENT_TYPE_UNSPECIFIED": 0,
    "WORKSPACE_PRODUCT_API_CONTENT_TYPE_APPLICATION_JSON": 1,
    "WORKSPACE_PRODUCT_API_CONTENT_TYPE_APPLICATION_PROBLEM_JSON": 2,
}
if enum_values("WorkspaceProductApiContentType") != expected_content_types:
    raise SystemExit("Workspace Product API content type is not restricted to JSON")

expected_header = ["owner", "wire_operation", "product_operation_id", "kind", "product_contract"]
with manifest_path.open(encoding="utf-8", newline="") as manifest_file:
    reader = csv.DictReader(manifest_file, delimiter="\t")
    if reader.fieldnames != expected_header:
        raise SystemExit("Product projection TCK has an unexpected header")
    rows = list(reader)

if len(rows) != 13:
    raise SystemExit(f"Product projection TCK must contain 13 operations; found {len(rows)}")

owner_repositories = {
    "CATALYST": "Cyrene-Catalyst",
    "YIELD": "Cyrene-Yield",
    "REACTOR": "Cyrene-Reactor",
    "EXCHANGE": "Cyrene-Exchange",
    "ECHO": "Cyrene-Echo",
    "NAVIGATOR": "Cyrene-Navigator",
}
owner_product_contracts = {
    "CATALYST": {"workspace-internal.openapi.yaml"},
    "YIELD": {"workspace-private.openapi.yaml"},
    "REACTOR": {"workspace-private.openapi.yaml"},
    "EXCHANGE": {"openapi.yaml"},
    "ECHO": {"workspace-internal.openapi.yaml"},
    "NAVIGATOR": {"openapi.yaml", "persistence.openapi.yaml"},
}
owner_kinds: dict[str, Counter[str]] = defaultdict(Counter)
wire_operations: set[str] = set()
owner_operation_pairs: set[tuple[str, str]] = set()
for row in rows:
    owner = row["owner"]
    operation = row["wire_operation"]
    product_operation = row["product_operation_id"]
    kind = row["kind"]
    contract = row["product_contract"]
    if owner not in owner_repositories:
        raise SystemExit(f"unknown Product API owner in TCK: {owner}")
    if operation not in expected_operations or operation.endswith("UNSPECIFIED"):
        raise SystemExit(f"unknown Product API wire operation in TCK: {operation}")
    if operation in wire_operations:
        raise SystemExit(f"duplicate Product API wire operation in TCK: {operation}")
    if not re.fullmatch(r"[A-Za-z][A-Za-z0-9_]*", product_operation):
        raise SystemExit(f"invalid Product OpenAPI operationId in TCK: {product_operation}")
    if (owner, product_operation) in owner_operation_pairs:
        raise SystemExit(f"duplicate Product operation for owner {owner}: {product_operation}")
    if kind not in {"READ", "COMMAND"}:
        raise SystemExit(f"invalid Product API request kind in TCK: {kind}")
    expected_contracts = {
        f"{owner_repositories[owner]}/contracts/product/v1/{contract_name}"
        for contract_name in owner_product_contracts[owner]
    }
    if contract not in expected_contracts:
        raise SystemExit(f"unexpected Product contract pointer for {owner}: {contract}")
    wire_operations.add(operation)
    owner_operation_pairs.add((owner, product_operation))
    owner_kinds[owner][kind] += 1

if wire_operations != set(expected_operations) - {"WORKSPACE_PRODUCT_API_OPERATION_UNSPECIFIED"}:
    raise SystemExit("Product projection TCK must map every admitted wire operation exactly once")
for owner in ("CATALYST", "YIELD", "REACTOR", "EXCHANGE", "ECHO"):
    if owner_kinds[owner] != Counter({"READ": 1, "COMMAND": 1}):
        raise SystemExit(f"Product projection TCK must contain one read and one command for {owner}")
if owner_kinds["NAVIGATOR"] != Counter({"READ": 2, "COMMAND": 1}):
    raise SystemExit("Navigator projection must contain snapshot/session reads and one Harness command")
PY

rg -q 'Valid application/json body, at most 4 MiB' "${workspace_proto}"
rg -q 'Valid JSON body, at most 4 MiB' "${workspace_proto}"
rg -q 'navigator_harness_append_frontend_denied' "${tck_root}/scenarios.tsv"
rg -q 'WRITER_TOKEN_NEVER_TO_BROWSER' "${tck_root}/scenarios.tsv"

rg -q 'WorkspaceProductApiRequest product_api = 12;' "${workspace_proto}"
rg -q 'WorkspaceProductApiResponse product_api = 12;' "${workspace_proto}"

for mode in LOCAL LAN_DIRECT DIRECT OVERLAY RELAY; do
  rg -q "CONNECTIVITY_MODE_${mode}" contracts/proto/cyrene/core/v1/node_control.proto || {
    printf 'missing transport-neutral connectivity mode: %s\n' "${mode}" >&2
    exit 1
  }
done

service_count=$(rg -c '^service ' "${workspace_proto}" || true)
if [[ "${service_count}" != 2 ]]; then
  printf 'Workspace wire contract must define Relay and Direct services; found %s\n' \
    "${service_count:-0}" >&2
  exit 1
fi
rg -q '^service WorkspaceRelayService' "${workspace_proto}"
rg -q 'rpc Connect\(stream RelayFrame\) returns \(stream RelayFrame\)' "${workspace_proto}"
rg -q '^service WorkspaceDirectService' "${workspace_proto}"
rg -q 'rpc Execute\(WorkspaceDirectRequest\) returns \(WorkspaceApiResponse\)' "${workspace_proto}"
rg -q 'trait WorkspaceDirectory' "${control_plane_crate}/src/directory.rs"
rg -q 'trait RelayAuthenticator' "${control_plane_crate}/src/auth.rs"
rg -q 'trait WorkspaceApi' "${control_plane_crate}/src/api.rs"
rg -q 'LocalWorkspaceClient' "${control_plane_crate}/src/api.rs"
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
  "${control_plane_crate}/src/directory.rs" "${control_plane_crate}/src/auth.rs"; then
  printf 'Directory or authentication boundary contains Workspace/Product state\n' >&2
  exit 1
fi
if rg -n 'Mutex<.*WorkspaceOperation|BTreeMap<.*WorkspaceOperation' "${relay_runtime_crate}/src/relay.rs"; then
  printf 'Relay must not become a Workspace operation state authority\n' >&2
  exit 1
fi

bash -n \
  "${acceptance_root}/run-workspace-fail-closed-proof.sh" \
  "${acceptance_root}/workspace-entrypoint.sh"

buf_bin=${BUF_BIN:-buf}
command -v "${buf_bin}" >/dev/null 2>&1 || {
  printf 'Buf is required for the Workspace wire contract check\n' >&2
  exit 127
}
(cd contracts/proto && "${buf_bin}" lint . --path cyrene/workspace/v1)

workspace_packages=(
  cy-workspace-client-sdk
  cy-workspace-sidecar
  cy-workspace-control-plane
  cy-workspace-relay-runtime
  cy-workspace-postgres-storage
  cy-workspace-product-contracts
  cy-workspace-product-adapters
  cy-workspace-fabric
  cy-runtime-agent
)
package_args=()
for package in "${workspace_packages[@]}"; do
  package_args+=(-p "${package}")
done
cargo test --locked "${package_args[@]}" --no-fail-fast
cargo clippy --locked "${package_args[@]}" --all-targets -- -D warnings
printf 'Distributed Workspace Fabric v1 contract checks passed\n'
