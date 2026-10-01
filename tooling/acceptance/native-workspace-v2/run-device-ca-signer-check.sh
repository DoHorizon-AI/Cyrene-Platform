#!/usr/bin/env bash
# Starts the restricted local signer long enough to persist and verify its signed CRL.
# 仅在隔离验收库刷新空签名 CRL；不签发设备证书。

set -euo pipefail
set +x
umask 077

acceptance_root="/tmp/cyrene-components-v2-acceptance/native-relay"
runtime_env="${acceptance_root}/runtime-bff.env"
key_file="/tmp/cyrene-components-v2-acceptance/device-ca-pki/ca.key"
certificate_file="/tmp/cyrene-components-v2-acceptance/device-ca-pki/ca.crt"
issuer_id="cyrene-components-v2-local-acceptance-device-ca-v1"

if [[ ! -f "${runtime_env}" || -L "${runtime_env}" || "$(stat -c '%a' "${runtime_env}")" != '600' ]]; then
  printf '%s\n' 'Protected BFF runtime environment is unavailable or has unsafe permissions.' >&2
  exit 2
fi
if [[ ! -f "${key_file}" || -L "${key_file}" || "$(stat -c '%a' "${key_file}")" != '600' ]]; then
  printf '%s\n' 'The local acceptance CA key is unavailable or has unsafe permissions.' >&2
  exit 2
fi
if [[ "$(stat -c '%u:%a' "$(dirname "${key_file}")")" != "$(id -u):700" ]]; then
  printf '%s\n' 'The local acceptance CA key directory must be owner-only.' >&2
  exit 2
fi
if [[ ! -f "${certificate_file}" || -L "${certificate_file}" ]]; then
  printf '%s\n' 'The local acceptance CA certificate is unavailable.' >&2
  exit 2
fi

# Read the BFF CA database URL only from its owner-only runtime environment.
set -a
source "${runtime_env}"
set +a
export CYRENE_WORKSPACE_DEVICE_CA_SIGNING_KEY_FILE="${key_file}"
export CYRENE_WORKSPACE_DEVICE_CA_CERTIFICATE_FILE="${certificate_file}"
export CYRENE_WORKSPACE_DEVICE_CA_ISSUER_ID="${issuer_id}"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
cd "${repo_root}"
CARGO_TARGET_DIR="/tmp/cyrene-components-target" cargo run --locked --offline \
  -p cy-workspace-postgres-storage \
  --bin cy-workspace-device-ca-admin \
  -- check

printf '%s\n' 'Restricted local CA signer and current signed CRL check passed; no device certificate was issued.'
