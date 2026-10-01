#!/usr/bin/env bash
# Creates isolated local-only Relay server and BFF workload certificates.
# 此脚本只在验收目录生成本机自签信任链，不访问公网或现有私钥。

set -euo pipefail
set +x
umask 077

acceptance_root="${CYRENE_NATIVE_ACCEPTANCE_DIR:-/tmp/cyrene-components-v2-acceptance/native-relay}"
tls_dir="${acceptance_root}/tls"

if [[ -e "${tls_dir}" ]]; then
  printf '%s\n' 'Local TLS directory already exists; refusing to overwrite certificate material.' >&2
  exit 2
fi

mkdir -p "${tls_dir}"
chmod 0700 "${acceptance_root}" "${tls_dir}"

server_ca_key="${tls_dir}/relay-server-ca.key"
server_ca_cert="${tls_dir}/relay-server-ca.crt"
server_key="${tls_dir}/relay-server.key"
server_csr="${tls_dir}/relay-server.csr"
server_cert="${tls_dir}/relay-server.crt"
bff_ca_key="${tls_dir}/bff-client-ca.key"
bff_ca_cert="${tls_dir}/bff-client-ca.crt"
bff_key="${tls_dir}/bff-client.key"
bff_csr="${tls_dir}/bff-client.csr"
bff_cert="${tls_dir}/bff-client.crt"

# Generate two independent acceptance roots so Relay server trust and BFF workload trust stay separate.
openssl req -x509 -newkey rsa:3072 -nodes -sha256 -days 30 \
  -keyout "${server_ca_key}" -out "${server_ca_cert}" \
  -subj '/CN=Cyrene Native Relay Local Acceptance Server CA' \
  -addext 'basicConstraints=critical,CA:TRUE,pathlen:0' \
  -addext 'keyUsage=critical,keyCertSign,cRLSign' >/dev/null 2>&1
openssl req -x509 -newkey rsa:3072 -nodes -sha256 -days 30 \
  -keyout "${bff_ca_key}" -out "${bff_ca_cert}" \
  -subj '/CN=Cyrene Web BFF Local Acceptance CA' \
  -addext 'basicConstraints=critical,CA:TRUE,pathlen:0' \
  -addext 'keyUsage=critical,keyCertSign,cRLSign' >/dev/null 2>&1

openssl req -new -newkey rsa:3072 -nodes -sha256 \
  -keyout "${server_key}" -out "${server_csr}" \
  -subj '/CN=localhost' >/dev/null 2>&1
cat >"${tls_dir}/relay-server-leaf.ext" <<'EOF'
basicConstraints=critical,CA:FALSE
keyUsage=critical,digitalSignature,keyEncipherment
extendedKeyUsage=serverAuth
subjectAltName=DNS:localhost,IP:127.0.0.1
EOF
openssl x509 -req -sha256 -days 14 \
  -in "${server_csr}" -CA "${server_ca_cert}" -CAkey "${server_ca_key}" \
  -CAcreateserial -extfile "${tls_dir}/relay-server-leaf.ext" -out "${server_cert}" >/dev/null 2>&1

openssl req -new -newkey rsa:3072 -nodes -sha256 \
  -keyout "${bff_key}" -out "${bff_csr}" \
  -subj '/CN=cyrene-web-bff-native' >/dev/null 2>&1
cat >"${tls_dir}/bff-client-leaf.ext" <<'EOF'
basicConstraints=critical,CA:FALSE
keyUsage=critical,digitalSignature,keyEncipherment
extendedKeyUsage=clientAuth
EOF
openssl x509 -req -sha256 -days 14 \
  -in "${bff_csr}" -CA "${bff_ca_cert}" -CAkey "${bff_ca_key}" \
  -CAcreateserial -extfile "${tls_dir}/bff-client-leaf.ext" -out "${bff_cert}" >/dev/null 2>&1

# Verify trust purpose and loopback name before publishing file paths to the acceptance env.
openssl verify -CAfile "${server_ca_cert}" -purpose sslserver -verify_ip 127.0.0.1 "${server_cert}"
openssl verify -CAfile "${bff_ca_cert}" -purpose sslclient "${bff_cert}"

fingerprint="$(openssl x509 -in "${bff_cert}" -outform DER | openssl dgst -sha256 | awk '{print $NF}')"
subject="$(openssl x509 -in "${bff_cert}" -noout -subject -nameopt RFC2253 | sed 's/^subject=//')"
if [[ "${subject}" != 'CN=cyrene-web-bff-native' ]]; then
  printf '%s\n' 'Generated BFF workload subject does not match the Relay pin validator.' >&2
  exit 1
fi

cat >"${tls_dir}/bff-allowlist.json" <<EOF
[
  {
    "sha256Fingerprint": "${fingerprint}",
    "subject": "${subject}",
    "revoked": false
  }
]
EOF

cat >"${tls_dir}/manifest.json" <<EOF
{
  "category": "LOCAL_ONLY_MTLS_ACCEPTANCE_TRUST",
  "relayServerCa": "${server_ca_cert}",
  "relayServerCertificate": "${server_cert}",
  "relayServerPrivateKey": "${server_key}",
  "relayServerName": "localhost",
  "relayServerSans": ["DNS:localhost", "IP:127.0.0.1"],
  "bffClientCa": "${bff_ca_cert}",
  "bffClientCertificate": "${bff_cert}",
  "bffClientPrivateKey": "${bff_key}",
  "bffClientSubject": "${subject}",
  "bffClientSha256Fingerprint": "${fingerprint}",
  "bffAllowlist": "${tls_dir}/bff-allowlist.json",
  "deviceCaBundle": "/tmp/cyrene-components-v2-acceptance/device-ca-pki/ca.crt",
  "scope": "Independent local acceptance roots; not production trust, AAD identity, or device approval"
}
EOF

chmod 0600 "${tls_dir}"/*
printf 'Local acceptance TLS trust files created: %s\n' "${tls_dir}"
