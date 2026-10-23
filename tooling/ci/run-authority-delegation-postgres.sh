#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "${repo_root}"

artifact_dir="${CYRENE_AUTHORITY_DELEGATION_ARTIFACT_DIR:-${RUNNER_TEMP:-${TMPDIR:-/tmp}}/cyrene-authority-delegation-postgres}"
mkdir -p "${artifact_dir}"
chmod 0700 "${artifact_dir}"
report_path="${artifact_dir}/authority-delegation-postgres.log"
exec > >(tee "${report_path}") 2>&1

fixture_dir="$(mktemp -d "${TMPDIR:-/tmp}/cyrene-authority-delegation-pg.XXXXXX")"
container_name="cyrene-authority-delegation-pg-$(date -u +%Y%m%dT%H%M%S)-${RANDOM}-$$"
container_id=""
cleanup() {
    local result=$?
    trap - EXIT
    if [[ -n "${container_id}" ]]; then
        if [[ "${result}" -ne 0 ]]; then
            printf 'PostgreSQL fixture logs before cleanup:\n'
            docker logs "${container_id}" 2>&1 || true
        fi
        if docker rm --force --volumes "${container_id}" >/dev/null 2>&1; then
            printf 'container_cleanup=removed container_id=%s label=cyrene.purpose=authority-delegation-postgres-test\n' "${container_id}"
        else
            printf 'container_cleanup=failed container_id=%s\n' "${container_id}"
            result=1
        fi
    fi
    rm -f "${fixture_dir}"/*
    rmdir "${fixture_dir}" 2>/dev/null || true
    printf 'validation_report=%s\n' "${report_path}"
    exit "${result}"
}
trap cleanup EXIT

export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"
export CARGO_INCREMENTAL=0
export OPENSSL_NO_VENDOR=1

postgres_image="${CYRENE_AUTHORITY_DELEGATION_POSTGRES_IMAGE:-postgres:17@sha256:d74eeac9a635390a49bc21bd49fccd973de707e2a53a76ac49b552b8712ec46f}"
database_name="cyrene_authority_delegation_test"
database_password="$(openssl rand -hex 24)"

openssl req -x509 -newkey rsa:2048 -nodes \
    -keyout "${fixture_dir}/server.key" \
    -out "${fixture_dir}/server.crt" \
    -days 2 \
    -subj "/CN=localhost" \
    -addext "subjectAltName=DNS:localhost" \
    -addext "basicConstraints=critical,CA:FALSE" \
    -addext "keyUsage=critical,digitalSignature,keyEncipherment" \
    -addext "extendedKeyUsage=serverAuth"
chmod 0600 "${fixture_dir}/server.key"
chmod 0644 "${fixture_dir}/server.crt"

container_id="$(docker run --detach \
    --name "${container_name}" \
    --label "cyrene.task=exchange-integration-v1-20261012" \
    --label "cyrene.purpose=authority-delegation-postgres-test" \
    --memory=768m \
    --cpus=1 \
    --pids-limit=256 \
    --publish 127.0.0.1::5432 \
    --env "POSTGRES_PASSWORD=${database_password}" \
    --env "POSTGRES_DB=${database_name}" \
    --mount "type=bind,source=${fixture_dir}/server.crt,target=/tmp/server.crt,readonly" \
    --mount "type=bind,source=${fixture_dir}/server.key,target=/tmp/server.key,readonly" \
    --entrypoint /bin/bash \
    "${postgres_image}" \
    -c 'set -euo pipefail; install -d -m 0700 -o postgres -g postgres /var/lib/postgresql/tls; install -m 0644 -o postgres -g postgres /tmp/server.crt /var/lib/postgresql/tls/server.crt; install -m 0600 -o postgres -g postgres /tmp/server.key /var/lib/postgresql/tls/server.key; exec /usr/local/bin/docker-entrypoint.sh postgres -c ssl=on -c ssl_cert_file=/var/lib/postgresql/tls/server.crt -c ssl_key_file=/var/lib/postgresql/tls/server.key')"

host_port="$(docker port "${container_id}" 5432/tcp | sed 's/.*://')"
memory_limit="$(docker inspect --format '{{.HostConfig.Memory}}' "${container_id}")"
printf 'container_name=%s\ncontainer_id=%s\npostgres_image=%s\nhost_bind=127.0.0.1:%s\nmemory_limit_bytes=%s\n' \
    "${container_name}" "${container_id}" "${postgres_image}" "${host_port}" "${memory_limit}"

for attempt in $(seq 1 60); do
    if docker exec "${container_id}" pg_isready --username postgres --dbname "${database_name}" >/dev/null; then
        printf 'postgres_fixture=ready\n'
        break
    fi
    if [[ "${attempt}" == 60 ]]; then
        printf 'postgres_fixture=not-ready\n'
        exit 1
    fi
    sleep 1
done

database_url="postgres://postgres:${database_password}@localhost:${host_port}/${database_name}?sslmode=verify-full&sslrootcert=${fixture_dir}/server.crt"
export CYRENE_WORKSPACE_DIRECTORY_MIGRATION_DATABASE_URL="${database_url}"
export CYRENE_WORKSPACE_DEVICE_AUTHORIZATION_MIGRATION_DATABASE_URL="${database_url}"
export CYRENE_WORKSPACE_DEVICE_REGISTRY_MIGRATION_DATABASE_URL="${database_url}"
export CYRENE_WORKSPACE_WEBAUTHN_MIGRATION_DATABASE_URL="${database_url}"
export CYRENE_WORKSPACE_WEBAUTHN_HTTP_SESSION_BINDING_MIGRATION_DATABASE_URL="${database_url}"
export CYRENE_WORKSPACE_DEVICE_CA_MIGRATION_DATABASE_URL="${database_url}"
export CYRENE_WORKSPACE_AUTHORITY_DELEGATION_TEST_DATABASE_URL="${database_url}"

printf 'migration_gate=all-workspace-storage-components\n'
cargo run --locked -p cy-workspace-postgres-storage --bin cy-workspace-storage-migrator -- migrate-all
printf 'postgres_test_gate=authority-delegation\n'
cargo test --locked -p cy-workspace-postgres-storage --test authority_delegation_postgres -- --ignored --nocapture
printf 'authority_delegation_postgres=PASS\n'

directory_operator_password="$(openssl rand -hex 24)"
directory_reader_password="$(openssl rand -hex 24)"
docker exec "${container_id}" psql \
    --username postgres \
    --dbname "${database_name}" \
    --set ON_ERROR_STOP=1 \
    --command "CREATE ROLE cyrene_test_operator LOGIN PASSWORD '${directory_operator_password}'" \
    --command "GRANT cyrene_workspace_directory_operator TO cyrene_test_operator" \
    --command "CREATE ROLE cyrene_test_reader LOGIN PASSWORD '${directory_reader_password}'" \
    --command "GRANT cyrene_workspace_directory_reader TO cyrene_test_reader"

directory_operator_url="postgres://cyrene_test_operator:${directory_operator_password}@localhost:${host_port}/${database_name}?sslmode=verify-full&sslrootcert=${fixture_dir}/server.crt"
directory_reader_url="postgres://cyrene_test_reader:${directory_reader_password}@localhost:${host_port}/${database_name}?sslmode=verify-full&sslrootcert=${fixture_dir}/server.crt"
export CYRENE_WORKSPACE_DIRECTORY_OPERATOR_DATABASE_URL="${directory_operator_url}"
export CYRENE_WORKSPACE_DIRECTORY_TEST_DATABASE_URL="${directory_reader_url}"
export CYRENE_WORKSPACE_DIRECTORY_OPERATOR_ID="directory-connector-integration-test"

printf 'postgres_test_gate=directory-connector-maintainer-and-signed-grant\n'
cargo test --locked -p cy-workspace-postgres-storage --test directory_connector_operator -- --ignored --nocapture
printf 'directory_connector_operator_postgres=PASS\n'
