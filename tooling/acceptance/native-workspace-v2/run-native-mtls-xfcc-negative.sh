#!/usr/bin/env bash

set -euo pipefail
umask 077

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ACCEPTANCE_ROOT="${CYRENE_NATIVE_ACCEPTANCE_DIR:-/tmp/cyrene-components-v2-acceptance/native-relay}"
TLS_DIR="$ACCEPTANCE_ROOT/tls"
TLS_REPORT="$ACCEPTANCE_ROOT/native-mtls-negative-report.json"

export CYRENE_NATIVE_RELAY_HOST="${CYRENE_NATIVE_RELAY_HOST:-127.0.0.1}"
export CYRENE_NATIVE_RELAY_PORT="${CYRENE_NATIVE_RELAY_PORT:-18080}"
export CYRENE_NATIVE_RELAY_SERVER_NAME="${CYRENE_NATIVE_RELAY_SERVER_NAME:-localhost}"
export CYRENE_NATIVE_RELAY_SERVER_CA_FILE="${CYRENE_NATIVE_RELAY_SERVER_CA_FILE:-$TLS_DIR/relay-server-ca.crt}"
export CYRENE_NATIVE_RELAY_CLIENT_CERT_FILE="${CYRENE_NATIVE_RELAY_CLIENT_CERT_FILE:-$TLS_DIR/bff-client.crt}"
export CYRENE_NATIVE_RELAY_CLIENT_KEY_FILE="${CYRENE_NATIVE_RELAY_CLIENT_KEY_FILE:-$TLS_DIR/bff-client.key}"

for certificate_file in \
  "$CYRENE_NATIVE_RELAY_SERVER_CA_FILE" \
  "$CYRENE_NATIVE_RELAY_CLIENT_CERT_FILE" \
  "$CYRENE_NATIVE_RELAY_CLIENT_KEY_FILE"; do
  if [[ ! -r "$certificate_file" || ! -f "$certificate_file" ]]; then
    printf 'Required local TLS file is unavailable: %s\n' "$certificate_file" >&2
    exit 2
  fi
done

install -d -m 0700 "$ACCEPTANCE_ROOT"
chmod 0700 "$ACCEPTANCE_ROOT"
export CYRENE_NATIVE_ACCEPTANCE_DIR="$ACCEPTANCE_ROOT"
export CYRENE_NATIVE_RELAY_TLS_NEGATIVE_REPORT_FILE="$TLS_REPORT"

"$SCRIPT_DIR/probe-native-mtls-negative.py"
CARGO_TARGET_DIR="$ACCEPTANCE_ROOT/cargo-target" \
  cargo run --locked --offline \
    --manifest-path "$SCRIPT_DIR/native-mtls-xfcc-probe/Cargo.toml"
