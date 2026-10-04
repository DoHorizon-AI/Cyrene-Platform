#!/bin/sh
# ╔══════════════════════════════════════════════════════════════════════╗
# ║ File: framework/crates/cy-workspace-web-bff/entrypoint.sh           ║
# ║ Role: Stage fixed ACA secret-volume files before dropping privileges.║
# ║                                                                    ║
# ║ 脚本职责：复制固定 ACA secret-volume 文件并降权启动 BFF。             ║
# ╚══════════════════════════════════════════════════════════════════════╝

set -eu
umask 077

readonly SOURCE_DIR=/mnt/cyrene-secret-input
readonly RUNTIME_DIR=/run/cyrene/workspace-web-bff-secrets
readonly APP_UID=10001
readonly APP_GID=10001
readonly BUNDLE_READER_GID=${CYRENE_PRODUCT_BUNDLE_READER_GID:-}

# Exit with a fixed message so secret contents and source paths never reach logs.
fail_closed() {
    printf '%s\n' 'Workspace Web BFF secret staging failed closed' >&2
    exit 1
}

stage_secret() {
    source_name=$1
    target_name=$2
    maximum_bytes=$3
    exact_bytes=$4
    source_path="$SOURCE_DIR/$source_name"

    [ -e "$source_path" ] || fail_closed
    resolved_path=$(realpath -e -- "$source_path") || fail_closed
    case "$resolved_path" in
        "$RESOLVED_SOURCE_DIR"/*) ;;
        *) fail_closed ;;
    esac
    [ -f "$resolved_path" ] || fail_closed

    file_bytes=$(wc -c < "$resolved_path") || fail_closed
    case "$file_bytes" in
        ''|*[!0-9]*) fail_closed ;;
    esac
    [ "$file_bytes" -gt 0 ] || fail_closed
    [ "$file_bytes" -le "$maximum_bytes" ] || fail_closed
    if [ "$exact_bytes" -gt 0 ]; then
        [ "$file_bytes" -eq "$exact_bytes" ] || fail_closed
    fi

    install -o "$APP_UID" -g "$APP_GID" -m 0400 -- \
        "$resolved_path" "$RUNTIME_DIR/$target_name" || fail_closed
}

[ "$(id -u)" -eq 0 ] || fail_closed
[ -n "$BUNDLE_READER_GID" ] || fail_closed
case "$BUNDLE_READER_GID" in
    *[!0-9]*) fail_closed ;;
esac
[ -d "$SOURCE_DIR" ] && [ ! -L "$SOURCE_DIR" ] || fail_closed
RESOLVED_SOURCE_DIR=$(realpath -e -- "$SOURCE_DIR") || fail_closed
[ -d "$RESOLVED_SOURCE_DIR" ] || fail_closed

install -d -o 0 -g 0 -m 0755 /run/cyrene || fail_closed
[ ! -L /run/cyrene ] || fail_closed
install -d -o 0 -g "$APP_GID" -m 0710 "$RUNTIME_DIR" || fail_closed
[ -d "$RUNTIME_DIR" ] && [ ! -L "$RUNTIME_DIR" ] || fail_closed
chmod 0710 "$RUNTIME_DIR" || fail_closed
chown "0:$APP_GID" "$RUNTIME_DIR" || fail_closed

# Remove only the fixed output names from the private runtime directory before staging.
for staged_name in csrf-mac-key relay-ca.pem relay-client.crt relay-client.key handoff-signing-seed user-code-hmac-key device-ca-signing-key.pem device-ca-cert.pem; do
    rm -f -- "$RUNTIME_DIR/$staged_name" || fail_closed
done

# The host requires owner-only regular files and exact 32-byte signing secrets.
stage_secret csrf-mac-key csrf-mac-key 256 32
stage_secret relay-ca.pem relay-ca.pem 262144 0
stage_secret relay-client.crt relay-client.crt 262144 0
stage_secret relay-client.key relay-client.key 262144 0
stage_secret handoff-signing-seed handoff-signing-seed 256 32
stage_secret user-code-hmac-key user-code-hmac-key 256 32
stage_secret device-ca-signing-key.pem device-ca-signing-key.pem 1048576 0
stage_secret device-ca-cert.pem device-ca-cert.pem 1048576 0

export CYRENE_WORKSPACE_WEB_BFF_CSRF_KEY_FILE="$RUNTIME_DIR/csrf-mac-key"
export CYRENE_WORKSPACE_WEB_BFF_RELAY_CA_FILE="$RUNTIME_DIR/relay-ca.pem"
export CYRENE_WORKSPACE_WEB_BFF_RELAY_CLIENT_CERT_FILE="$RUNTIME_DIR/relay-client.crt"
export CYRENE_WORKSPACE_WEB_BFF_RELAY_CLIENT_KEY_FILE="$RUNTIME_DIR/relay-client.key"
export CYRENE_WORKSPACE_WEB_BFF_HANDOFF_SIGNING_SEED_FILE="$RUNTIME_DIR/handoff-signing-seed"
export CYRENE_WORKSPACE_DEVICE_AUTHORIZATION_USER_CODE_HMAC_KEY_FILE="$RUNTIME_DIR/user-code-hmac-key"
export CYRENE_WORKSPACE_DEVICE_CA_SIGNING_KEY_FILE="$RUNTIME_DIR/device-ca-signing-key.pem"
export CYRENE_WORKSPACE_DEVICE_CA_CERTIFICATE_FILE="$RUNTIME_DIR/device-ca-cert.pem"
export CYRENE_WORKSPACE_WEB_BFF_BIND="${CYRENE_WORKSPACE_WEB_BFF_BIND:-0.0.0.0:8080}"

exec /usr/bin/setpriv \
    --reuid="$APP_UID" \
    --regid="$APP_GID" \
    --groups="$BUNDLE_READER_GID" \
    --no-new-privs \
    --bounding-set=-all \
    /usr/local/bin/cy-workspace-web-bff
