#!/usr/bin/env python3
"""Check that a local Native Relay TLS listener rejects missing and untrusted clients."""

from __future__ import annotations

import hashlib
import ipaddress
import json
import os
import socket
import ssl
import subprocess
import tempfile
from datetime import UTC, datetime
from pathlib import Path


def required_path(name: str) -> Path:
    value = os.environ.get(name)
    if not value:
        raise SystemExit(f"required environment variable is missing: {name}")
    path = Path(value).expanduser().resolve(strict=True)
    if not path.is_file():
        raise SystemExit(f"{name} must point to a regular file")
    return path


def source_details(platform_root: Path) -> dict[str, object]:
    def git(*args: str) -> str:
        return subprocess.check_output(["git", "-C", str(platform_root), *args], text=True).strip()

    return {
        "platformHead": git("rev-parse", "HEAD"),
        "platformBranch": git("branch", "--show-current"),
        "platformDirtyPaths": git("status", "--short", "--untracked-files=all").splitlines(),
    }


def check_rejected_handshake(
    *,
    host: str,
    port: int,
    server_name: str,
    server_ca: Path,
    cert: Path | None,
    key: Path | None,
) -> dict[str, str]:
    context = ssl.create_default_context(ssl.Purpose.SERVER_AUTH, cafile=str(server_ca))
    context.minimum_version = ssl.TLSVersion.TLSv1_2
    context.set_alpn_protocols(["h2"])
    if cert is not None and key is not None:
        context.load_cert_chain(certfile=str(cert), keyfile=str(key))

    raw_socket = socket.create_connection((host, port), timeout=3.0)
    raw_socket.settimeout(3.0)
    try:
        wrapped_socket = context.wrap_socket(raw_socket, server_hostname=server_name)
    except ssl.SSLCertVerificationError as error:
        raise RuntimeError(
            "Relay server certificate verification failed; the client rejection is inconclusive"
        ) from error
    except ssl.SSLError as error:
        return {"result": "rejected", "reason": error.reason or type(error).__name__}
    else:
        negotiated_alpn = wrapped_socket.selected_alpn_protocol() or "none"
        wrapped_socket.close()
        raise RuntimeError(f"Relay accepted a client TLS handshake without required trust; ALPN={negotiated_alpn}")
    finally:
        raw_socket.close()


def main() -> int:
    os.umask(0o077)
    host = os.environ.get("CYRENE_NATIVE_RELAY_HOST", "127.0.0.1")
    try:
        address = ipaddress.ip_address(host)
    except ValueError as error:
        raise SystemExit("CYRENE_NATIVE_RELAY_HOST must be a loopback IP address") from error
    if not address.is_loopback:
        raise SystemExit("this local-only probe requires a loopback Relay address")

    try:
        port = int(os.environ.get("CYRENE_NATIVE_RELAY_PORT", "8080"))
    except ValueError as error:
        raise SystemExit("CYRENE_NATIVE_RELAY_PORT must be an integer") from error
    if not 1 <= port <= 65535:
        raise SystemExit("CYRENE_NATIVE_RELAY_PORT must be between 1 and 65535")

    server_name = os.environ.get("CYRENE_NATIVE_RELAY_SERVER_NAME", host)
    server_ca = required_path("CYRENE_NATIVE_RELAY_SERVER_CA_FILE")
    acceptance_root = Path(
        os.environ.get("CYRENE_NATIVE_ACCEPTANCE_DIR", "/tmp/cyrene-components-v2-acceptance/native-relay")
    )
    acceptance_root.mkdir(mode=0o700, parents=True, exist_ok=True)
    os.chmod(acceptance_root, 0o700)
    report_path = acceptance_root / "native-mtls-negative-report.json"
    platform_root = Path(__file__).resolve().parents[3]

    with tempfile.TemporaryDirectory(prefix="native-mtls-negative-", dir=acceptance_root) as temporary_directory:
        ephemeral = Path(temporary_directory)
        os.chmod(ephemeral, 0o700)
        client_certificate = ephemeral / "untrusted-client.crt"
        client_private_key = ephemeral / "untrusted-client.key"
        try:
            subprocess.run(
                [
                    "openssl",
                    "req",
                    "-x509",
                    "-newkey",
                    "rsa:2048",
                    "-nodes",
                    "-subj",
                    "/CN=native-relay-untrusted-acceptance-client",
                    "-keyout",
                    str(client_private_key),
                    "-out",
                    str(client_certificate),
                    "-days",
                    "1",
                ],
                check=True,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
        except (FileNotFoundError, subprocess.CalledProcessError) as error:
            raise SystemExit("could not create the temporary untrusted client certificate") from error
        os.chmod(client_private_key, 0o600)
        os.chmod(client_certificate, 0o600)

        try:
            missing_certificate = check_rejected_handshake(
                host=host,
                port=port,
                server_name=server_name,
                server_ca=server_ca,
                cert=None,
                key=None,
            )
            untrusted_certificate = check_rejected_handshake(
                host=host,
                port=port,
                server_name=server_name,
                server_ca=server_ca,
                cert=client_certificate,
                key=client_private_key,
            )
        except (OSError, ssl.SSLError, RuntimeError) as error:
            raise SystemExit(f"Native Relay mTLS rejection probe failed: {error}") from error

        missing_reason = missing_certificate["reason"].upper()
        untrusted_reason = untrusted_certificate["reason"].upper()
        missing_is_rejected = "CERTIFICATE_REQUIRED" in missing_reason or ("HANDSHAKE_FAILURE" in missing_reason)
        if not missing_is_rejected:
            raise SystemExit(
                "Relay did not return a client-certificate rejection alert for the missing-certificate case"
            )
        untrusted_is_rejected = "UNKNOWN_CA" in untrusted_reason or ("HANDSHAKE_FAILURE" in untrusted_reason)
        if not untrusted_is_rejected:
            raise SystemExit("Relay did not return an untrusted-client-certificate rejection alert")

    report = {
        "status": "PASS",
        "category": "REAL_NATIVE_RELAY_TLS_NEGATIVE_LOCAL_ONLY",
        "endpoint": f"{host}:{port}",
        "serverName": server_name,
        "serverCaSha256": hashlib.sha256(server_ca.read_bytes()).hexdigest(),
        "transport": "TLS 1.2+ with h2 ALPN; no gRPC RPC was sent",
        "checks": {
            "missingClientCertificate": missing_certificate,
            "untrustedClientCertificate": untrusted_certificate,
        },
        "scope": ("Native Relay TLS listener rejection only; not Tonic RPC, identity approval, ACK, CRL, or dispatch"),
        "runAtUtc": datetime.now(UTC).isoformat(),
        **source_details(platform_root),
    }
    report_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    os.chmod(report_path, 0o600)
    print(f"REAL_NATIVE_RELAY_TLS_NEGATIVE_LOCAL_ONLY PASS: report={report_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
