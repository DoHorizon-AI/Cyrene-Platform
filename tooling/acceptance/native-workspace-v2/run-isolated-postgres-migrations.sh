#!/usr/bin/env bash
# Isolated Native Workspace V2 PostgreSQL migration runner.
# Runs only the official storage migrator against the dedicated local acceptance database.
# 专用验收库的 operator URL 只从 0600 环境文件读取，不放入命令参数。

set -euo pipefail
set +x

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
acceptance_root="/tmp/cyrene-components-v2-acceptance/native-relay"
env_file="${CYRENE_NATIVE_ACCEPTANCE_MIGRATOR_ENV:-${acceptance_root}/migrator.env}"
target_dir="/tmp/cyrene-components-target"
migration="${1:-migrate-all}"

case "${migration}" in
  validate-config|migrate-all|migrate-directory|migrate-device-authorization|migrate-device-registry|migrate-webauthn|migrate-webauthn-http-binding|migrate-device-ca)
    ;;
  *)
    printf '%s\n' 'Unsupported migration selector.' >&2
    exit 2
    ;;
esac

if [[ ! -r "${env_file}" ]]; then
  printf '%s\n' 'Private migration environment file is unavailable.' >&2
  exit 2
fi
if [[ "$(stat -c '%a' "${env_file}")" != '600' ]]; then
  printf '%s\n' 'Private migration environment file must have mode 0600.' >&2
  exit 2
fi

# Keep operator URLs in the process environment; never place them in command arguments.
set -a
source "${env_file}"
set +a

export CYRENE_WORKSPACE_DIRECTORY_MIGRATION_DATABASE_URL
export CYRENE_WORKSPACE_DEVICE_AUTHORIZATION_MIGRATION_DATABASE_URL
export CYRENE_WORKSPACE_DEVICE_REGISTRY_MIGRATION_DATABASE_URL
export CYRENE_WORKSPACE_WEBAUTHN_MIGRATION_DATABASE_URL
export CYRENE_WORKSPACE_WEBAUTHN_HTTP_SESSION_BINDING_MIGRATION_DATABASE_URL
export CYRENE_WORKSPACE_DEVICE_CA_MIGRATION_DATABASE_URL

# Confirm all adapters target the dedicated loopback database with TLS verification.
python3 - <<'PY'
import os
import sys
from urllib.parse import unquote, urlsplit

names = (
    "CYRENE_WORKSPACE_DIRECTORY_MIGRATION_DATABASE_URL",
    "CYRENE_WORKSPACE_DEVICE_AUTHORIZATION_MIGRATION_DATABASE_URL",
    "CYRENE_WORKSPACE_DEVICE_REGISTRY_MIGRATION_DATABASE_URL",
    "CYRENE_WORKSPACE_WEBAUTHN_MIGRATION_DATABASE_URL",
    "CYRENE_WORKSPACE_WEBAUTHN_HTTP_SESSION_BINDING_MIGRATION_DATABASE_URL",
    "CYRENE_WORKSPACE_DEVICE_CA_MIGRATION_DATABASE_URL",
)
urls = [os.environ.get(name, "") for name in names]
if not urls[0] or any(value != urls[0] for value in urls):
    sys.exit("Migration URLs must all target the one isolated acceptance database.")

parsed = urlsplit(urls[0])
query = dict(part.split("=", 1) for part in parsed.query.split("&") if "=" in part)
root_certificate = unquote(query.get("sslrootcert", ""))
if (
    parsed.scheme not in ("postgres", "postgresql")
    or parsed.hostname != "127.0.0.1"
    or parsed.port != 44626
    or parsed.path != "/cyrene_native_workspace_acceptance"
    or unquote(parsed.username or "") != "cyrene_acceptance_operator"
    or query.get("sslmode") != "verify-full"
    or not root_certificate.startswith("/tmp/cyrene-components-v2-acceptance/")
    or not os.path.isfile(root_certificate)
):
    sys.exit("Migration URL is outside the approved isolated PostgreSQL target.")
PY

if [[ "${migration}" == 'validate-config' ]]; then
  printf '%s\n' 'Private migration configuration targets the dedicated TLS-verified acceptance database.'
  exit 0
fi

cd "${repo_root}"
CARGO_TARGET_DIR="${target_dir}" cargo run --locked --offline \
  -p cy-workspace-postgres-storage \
  --bin cy-workspace-storage-migrator \
  -- "${migration}"
