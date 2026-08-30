#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
repo_root="$(cd -- "${script_dir}/../.." && pwd -P)"
proto_root="${repo_root}/contracts/proto"
dotnet_project="${script_dir}/dotnet/ModelProviderEmbeddingContractTck.csproj"

dotnet restore "${dotnet_project}" --nologo >/dev/null
package_root="$(dotnet nuget locals global-packages --list | sed -n 's/^global-packages: //p')"
case "$(uname -m)" in
    x86_64) grpc_platform="linux_x64" ;;
    aarch64|arm64) grpc_platform="linux_arm64" ;;
    *) echo "unsupported protoc architecture: $(uname -m)" >&2; exit 1 ;;
esac
protoc_path="${package_root}grpc.tools/2.67.0/tools/${grpc_platform}/protoc"
wkt_path="${package_root}grpc.tools/2.67.0/build/native/include"
if [[ ! -x "${protoc_path}" ]]; then
    echo "Grpc.Tools protoc not found at ${protoc_path}" >&2
    exit 1
fi

temp_parent="${TMPDIR:-/tmp}"
generation_root="$(mktemp -d "${temp_parent%/}/cyrene-model-provider-XXXXXX")"
cleanup() {
    case "${generation_root}" in
        "${temp_parent%/}"/cyrene-model-provider-*) rm -rf -- "${generation_root}" ;;
        *) echo "refusing to clean unexpected path: ${generation_root}" >&2; exit 1 ;;
    esac
}
trap cleanup EXIT

for language in csharp java kotlin python; do
    mkdir -p "${generation_root}/${language}"
done

"${protoc_path}" \
    "--proto_path=${proto_root}" \
    "--proto_path=${wkt_path}" \
    "--csharp_out=${generation_root}/csharp" \
    "--java_out=${generation_root}/java" \
    "--kotlin_out=${generation_root}/kotlin" \
    "--python_out=${generation_root}/python" \
    "${proto_root}/cyrene/capability/v1/capability_execution.proto" \
    "${proto_root}/cyrene/model/provider/v1/model_provider.proto"

test -n "$(find "${generation_root}/csharp" -type f -name '*.cs' -print -quit)"
test -n "$(find "${generation_root}/java" -type f -name '*.java' -print -quit)"
test -n "$(find "${generation_root}/kotlin" -type f -name '*.kt' -print -quit)"
test -n "$(find "${generation_root}/python" -type f -name '*.py' -print -quit)"
python3 -m compileall -q "${generation_root}/python"
tck_uv_cache="${UV_CACHE_DIR:-${temp_parent%/}/cyrene-uv-cache/model-provider-embedding-tck}"
UV_CACHE_DIR="${tck_uv_cache}" \
UV_PROJECT_ENVIRONMENT="${generation_root}/python-venv" \
uv run \
    --project "${script_dir}/python" \
    --locked \
    python "${script_dir}/python/run_tck.py" "${generation_root}/python"

echo "model.provider.v1 C#/Java/Kotlin/Python generation: PASS"
