#!/usr/bin/env python3
"""Create the isolated Native Relay host environment for loopback acceptance.

Module: tooling.acceptance.native_workspace_v2.prepare_local_relay_config
Role: Generate a local handoff verifier key and assemble the host's safe TLS settings.

模块职责：生成本机 Relay 验收配置与短时 handoff verifier 密钥对。
· 独立本机信任根  · 不代表 AAD 用户、设备批准或生产身份
"""

from __future__ import annotations

import base64
import hashlib
import json
import os
import subprocess
import sys
import tempfile
from datetime import datetime, timezone
from pathlib import Path


ACCEPTANCE_ROOT = Path("/tmp/cyrene-components-v2-acceptance/native-relay")
TLS_ROOT = ACCEPTANCE_ROOT / "tls"
DEVICE_CA_ROOT = Path("/tmp/cyrene-components-v2-acceptance/device-ca-pki")
ENV_FILE = ACCEPTANCE_ROOT / "relay-host.env"
SEED_FILE = TLS_ROOT / "handoff-signing-seed.bin"
REPORT_FILE = ACCEPTANCE_ROOT / "local-relay-config-report.json"
ISSUER_ID = "cyrene-components-v2-local-acceptance-device-ca-v1"
HANDOFF_ISSUER = "cyrene-native-local-acceptance-bff"
HANDOFF_AUDIENCE = "cyrene-native-relay-local-acceptance"


def require_private_file(path: Path) -> None:
    """Require a regular, non-symlink mode-0600 acceptance key or bundle."""
    if path.is_symlink() or not path.is_file() or path.stat().st_mode & 0o777 != 0o600:
        raise RuntimeError("A required local acceptance TLS file is missing or has unsafe permissions.")


def generate_ed25519_keypair() -> tuple[bytes, bytes]:
    """Generate Ed25519 raw seed/public bytes without exposing key material."""
    fd, temporary_name = tempfile.mkstemp(prefix=".handoff-key.", dir=TLS_ROOT)
    os.close(fd)
    temporary_key = Path(temporary_name)
    os.chmod(temporary_key, 0o600)
    try:
        subprocess.run(
            ["openssl", "genpkey", "-algorithm", "ED25519", "-out", str(temporary_key)],
            check=True,
            capture_output=True,
        )
        private_der = subprocess.run(
            ["openssl", "pkey", "-in", str(temporary_key), "-outform", "DER"],
            check=True,
            capture_output=True,
        ).stdout
        public_der = subprocess.run(
            [
                "openssl",
                "pkey",
                "-in",
                str(temporary_key),
                "-pubout",
                "-outform",
                "DER",
            ],
            check=True,
            capture_output=True,
        ).stdout
        # Ed25519 PKCS#8 and SubjectPublicKeyInfo carry fixed DER prefixes before 32 raw bytes.
        if len(private_der) != 48 or len(public_der) != 44:
            raise RuntimeError("OpenSSL returned an unexpected Ed25519 DER encoding.")
        return private_der[-32:], public_der[-32:]
    finally:
        temporary_key.unlink(missing_ok=True)


def write_private_file(path: Path, content: bytes) -> None:
    """Write owner-only configuration without leaving a partial destination."""
    temporary = path.with_name(f".{path.name}.tmp")
    fd = os.open(temporary, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    try:
        with os.fdopen(fd, "wb") as stream:
            stream.write(content)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
        os.chmod(path, 0o600)
    finally:
        if temporary.exists():
            temporary.unlink()


def main() -> None:
    """Generate the local verifier keypair, exact pins, and host environment."""
    os.umask(0o077)
    if ACCEPTANCE_ROOT.stat().st_mode & 0o777 != 0o700:
        raise RuntimeError("Acceptance directory must have mode 0700.")
    if TLS_ROOT.stat().st_mode & 0o777 != 0o700:
        raise RuntimeError("Local TLS directory must have mode 0700.")
    if any(path.exists() for path in (ENV_FILE, SEED_FILE, REPORT_FILE)):
        raise RuntimeError("Local Relay config already exists; refusing to overwrite it.")

    server_certificate = TLS_ROOT / "relay-server.crt"
    server_key = TLS_ROOT / "relay-server.key"
    server_client_ca = TLS_ROOT / "bff-client-ca.crt"
    allowlist = TLS_ROOT / "bff-allowlist.json"
    device_certificate = DEVICE_CA_ROOT / "ca.crt"
    for path in (
        server_certificate,
        server_key,
        server_client_ca,
        allowlist,
        device_certificate,
        DEVICE_CA_ROOT / "ca.key",
    ):
        require_private_file(path)

    seed, public_key = generate_ed25519_keypair()
    write_private_file(SEED_FILE, seed)
    encoded_public_key = base64.urlsafe_b64encode(public_key).rstrip(b"=").decode("ascii")
    environment = {
        "CYRENE_WORKSPACE_RELAY_INGRESS_MODE": "native",
        "CYRENE_WORKSPACE_RELAY_BIND": "127.0.0.1:18080",
        "CYRENE_WORKSPACE_RELAY_HEALTH_BIND": "127.0.0.1:18081",
        "CYRENE_WORKSPACE_RELAY_NATIVE_SERVER_CERT_FILE": str(server_certificate),
        "CYRENE_WORKSPACE_RELAY_NATIVE_SERVER_KEY_FILE": str(server_key),
        "CYRENE_WORKSPACE_RELAY_NATIVE_DEVICE_CA_BUNDLE_FILE": str(device_certificate),
        "CYRENE_WORKSPACE_RELAY_BFF_CLIENT_CA_BUNDLE": str(server_client_ca),
        "CYRENE_WORKSPACE_RELAY_BFF_CERT_ALLOWLIST": str(allowlist),
        "CYRENE_WORKSPACE_RELAY_WEB_HANDOFF_ISSUER": HANDOFF_ISSUER,
        "CYRENE_WORKSPACE_RELAY_WEB_HANDOFF_AUDIENCE": HANDOFF_AUDIENCE,
        "CYRENE_WORKSPACE_RELAY_WEB_HANDOFF_PUBLIC_KEY_BASE64URL": encoded_public_key,
        "CYRENE_WORKSPACE_DEVICE_CA_CERTIFICATE_FILE": str(device_certificate),
        "CYRENE_WORKSPACE_DEVICE_CA_ISSUER_ID": ISSUER_ID,
    }
    env_text = "".join(f"export {name}='{value}'\n" for name, value in environment.items())
    write_private_file(ENV_FILE, env_text.encode("utf-8"))
    report = {
        "status": "READY_FOR_LOCAL_RELAY_START",
        "category": "LOCAL_ONLY_MTLS_ACCEPTANCE_CONFIG",
        "createdAtUtc": datetime.now(timezone.utc).isoformat(),
        "serverCertificate": str(server_certificate),
        "bffClientCa": str(server_client_ca),
        "bffAllowlist": str(allowlist),
        "handoffVerifierPublicKeySha256": hashlib.sha256(public_key).hexdigest(),
        "handoffIssuer": HANDOFF_ISSUER,
        "handoffAudience": HANDOFF_AUDIENCE,
        "handoffSigningSeedFile": str(SEED_FILE),
        "relayHostEnvironmentFile": str(ENV_FILE),
        "deviceCaIssuerId": ISSUER_ID,
        "scope": "Local workload transport configuration only; no AAD principal, user approval, or device certificate",
    }
    write_private_file(REPORT_FILE, (json.dumps(report, indent=2, sort_keys=True) + "\n").encode("utf-8"))
    print(f"Local Native Relay configuration staged: {ENV_FILE}")
    print(f"Local handoff verifier seed staged owner-only: {SEED_FILE}")


if __name__ == "__main__":
    try:
        main()
    except Exception as error:  # Never expose private key bytes or subprocess input on failure.
        print(f"Local Relay config preparation failed: {type(error).__name__}.", file=sys.stderr)
        raise SystemExit(1)
