#!/usr/bin/env bash
# Generate a development certificate or a Let's Encrypt certificate for Pro.
# A keystore password is always supplied by SSL_KEYSTORE_PASSWORD; there is no
# password fallback in this script.

set -euo pipefail

DOMAIN="${1:-localhost}"
SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(CDPATH= cd -- "${SCRIPT_DIR}/../../.." && pwd)"
OUTPUT_DIR="${2:-${REPO_ROOT}/infrastructure/cyrene-runtime-deployment/enterprise/certs}"
KEYSTORE_PASSWORD="${SSL_KEYSTORE_PASSWORD:-}"

GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

require_keystore_password() {
  if [[ -z "${KEYSTORE_PASSWORD}" ]]; then
    printf '%b\n' "${YELLOW}Set SSL_KEYSTORE_PASSWORD before generating a PKCS12 keystore.${NC}" >&2
    exit 2
  fi
}

mkdir -p "${OUTPUT_DIR}"
printf '%b\n' "${GREEN}Generating certificate for ${DOMAIN} in ${OUTPUT_DIR}${NC}"

generate_self_signed() {
  require_keystore_password
  openssl req -x509 -newkey rsa:4096 \
    -keyout "${OUTPUT_DIR}/privkey.pem" \
    -out "${OUTPUT_DIR}/fullchain.pem" \
    -sha256 -days 365 -nodes \
    -subj "/CN=${DOMAIN}" \
    -addext "subjectAltName=DNS:${DOMAIN},DNS:localhost,IP:127.0.0.1"

  openssl pkcs12 -export \
    -in "${OUTPUT_DIR}/fullchain.pem" \
    -inkey "${OUTPUT_DIR}/privkey.pem" \
    -out "${OUTPUT_DIR}/keystore.p12" \
    -name gateway \
    -passout "pass:${KEYSTORE_PASSWORD}"

  printf '%b\n' "${GREEN}Self-signed certificate generated.${NC}"
}

generate_letsencrypt() {
  require_keystore_password
  if ! command -v certbot >/dev/null 2>&1; then
    printf '%b\n' "${YELLOW}certbot is required for Let's Encrypt certificates.${NC}" >&2
    exit 2
  fi

  sudo certbot certonly --standalone \
    -d "${DOMAIN}" \
    --non-interactive \
    --agree-tos \
    --email "admin@${DOMAIN}"

  sudo cp "/etc/letsencrypt/live/${DOMAIN}/fullchain.pem" "${OUTPUT_DIR}/"
  sudo cp "/etc/letsencrypt/live/${DOMAIN}/privkey.pem" "${OUTPUT_DIR}/"

  if [[ -n "${SUDO_USER:-}" ]]; then
    sudo chown "${SUDO_USER}:${SUDO_USER}" "${OUTPUT_DIR}"/*.pem
  fi

  openssl pkcs12 -export \
    -in "${OUTPUT_DIR}/fullchain.pem" \
    -inkey "${OUTPUT_DIR}/privkey.pem" \
    -out "${OUTPUT_DIR}/keystore.p12" \
    -name gateway \
    -passout "pass:${KEYSTORE_PASSWORD}"

  printf '%b\n' "${GREEN}Let's Encrypt certificate generated. Renew it with: sudo certbot renew${NC}"
}

if [[ "${DOMAIN}" == "localhost" || "${DOMAIN}" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  generate_self_signed
else
  printf 'Choose certificate type for %s:\n' "${DOMAIN}"
  printf '  1) self-signed (development)\n  2) Let\x27s Encrypt (production)\n'
  read -r -p 'Choice [1/2]: ' choice
  if [[ "${choice}" == "2" ]]; then
    generate_letsencrypt
  else
    generate_self_signed
  fi
fi
