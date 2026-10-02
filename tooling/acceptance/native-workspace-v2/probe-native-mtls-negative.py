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
    check_name: str,
) -> dict[str, str | None]:
    context = ssl.create_default_context(ssl.Purpose.SERVER_AUTH, cafile=str(server_ca))
    context.minimum_version = ssl.TLSVersion.TLSv1_2
    context.set_alpn_protocols(["h2"])
    if cert is not None and key is not None:
        context.load_cert_chain(certfile=str(cert), keyfile=str(key))

    raw_socket = socket.create_connection((host, port), timeout=3.0)
    try:
        try:
            raw_socket.settimeout(3.0)
            wrapped_socket = context.wrap_socket(raw_socket, server_hostname=server_name)
        except ssl.SSLCertVerificationError as error:
            raise RuntimeError(
                "Relay server certificate verification failed; the client rejection is inconclusive"
            ) from error
        except ssl.SSLError as error:
            return classify_tls_error(error, check_name, None, None, "handshake")
        try:
            tls_version = wrapped_socket.version()
            negotiated_alpn = wrapped_socket.selected_alpn_protocol() or "none"
            if tls_version == "TLSv1.2":
                return {
                    "result": "tls_auth_bypass",
                    "reason": "client TLS 1.2 handshake completed",
                    "tlsVersion": tls_version,
                    "alpn": negotiated_alpn,
                }

            # TLS 1.3 clients may return from wrap_socket after sending Finished but
            # before reading the server's mandatory-client-certificate alert.
            wrapped_socket.settimeout(0.25)
            try:
                first_response = wrapped_socket.recv(1)
            except socket.timeout:
                first_response = None
            except ssl.SSLError as error:
                return classify_tls_error(error, check_name, tls_version, negotiated_alpn, "application_read")
            except OSError as error:
                return classify_transport_close(error, tls_version, negotiated_alpn, "application_read")
            if first_response == b"":
                return classify_transport_close(None, tls_version, negotiated_alpn, "application_read")
            if first_response:
                return {
                    "result": "application_data_received",
                    "reason": "server sent application data before the client HTTP/2 preface",
                    "tlsVersion": tls_version,
                    "alpn": negotiated_alpn,
                    "phase": "application_read",
                }

            # If no alert is pending, send HTTP/2's connection preface so an
            # optional-auth server can expose itself with SETTINGS/application data.
            try:
                wrapped_socket.settimeout(3.0)
                wrapped_socket.sendall(b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n")
                response = wrapped_socket.recv(1)
            except ssl.SSLError as error:
                return classify_tls_error(error, check_name, tls_version, negotiated_alpn, "application_write_or_read")
            except socket.timeout:
                return {
                    "result": "timeout_requires_tonic_rpc",
                    "reason": "no server response after HTTP/2 preface",
                    "tlsVersion": tls_version,
                    "alpn": negotiated_alpn,
                    "phase": "application_read_after_http2_preface",
                }
            except OSError as error:
                return classify_transport_close(error, tls_version, negotiated_alpn, "application_write_or_read")
            if response == b"":
                return classify_transport_close(
                    None, tls_version, negotiated_alpn, "application_read_after_http2_preface"
                )
            return {
                "result": "application_data_received",
                "reason": "server returned application data after HTTP/2 preface",
                "tlsVersion": tls_version,
                "alpn": negotiated_alpn,
                "phase": "application_read_after_http2_preface",
            }
        finally:
            wrapped_socket.close()
    finally:
        raw_socket.close()


def classify_tls_error(
    error: ssl.SSLError,
    check_name: str,
    tls_version: str | None,
    negotiated_alpn: str | None,
    phase: str,
) -> dict[str, str | None]:
    reason = (error.reason or type(error).__name__).upper()
    expected_alerts = {
        "missing": ("CERTIFICATE_REQUIRED", "HANDSHAKE_FAILURE"),
        "untrusted": ("UNKNOWN_CA", "HANDSHAKE_FAILURE", "BAD_CERTIFICATE", "CERTIFICATE_UNKNOWN"),
    }[check_name]
    if any(alert in reason for alert in expected_alerts):
        result = "rejected"
    elif any(marker in reason for marker in ("UNEXPECTED_EOF", "EOF", "ZERO_RETURN")):
        result = "transport_closed_after_client_data"
    else:
        result = "unexpected_tls_error"
    return {
        "result": result,
        "reason": reason,
        "tlsVersion": tls_version,
        "alpn": negotiated_alpn,
        "phase": phase,
    }


def classify_transport_close(
    error: OSError | None,
    tls_version: str,
    negotiated_alpn: str,
    phase: str,
) -> dict[str, str | None]:
    reason = "peer closed the connection" if error is None else type(error).__name__
    return {
        "result": "transport_closed_after_client_data",
        "reason": reason,
        "tlsVersion": tls_version,
        "alpn": negotiated_alpn,
        "phase": phase,
    }


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
                    "-addext",
                    "basicConstraints=critical,CA:FALSE",
                    "-addext",
                    "keyUsage=critical,digitalSignature,keyEncipherment",
                    "-addext",
                    "extendedKeyUsage=clientAuth",
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
                check_name="missing",
            )
            untrusted_certificate = check_rejected_handshake(
                host=host,
                port=port,
                server_name=server_name,
                server_ca=server_ca,
                cert=client_certificate,
                key=client_private_key,
                check_name="untrusted",
            )
        except (OSError, ssl.SSLError, RuntimeError) as error:
            raise SystemExit(f"Native Relay mTLS rejection probe failed: {error}") from error

        bypass_results = {"tls_auth_bypass", "application_data_received", "unexpected_tls_error"}
        for label, result in (
            ("missing-certificate", missing_certificate),
            ("untrusted-certificate", untrusted_certificate),
        ):
            if result["result"] in bypass_results:
                raise SystemExit(f"Relay did not reject the {label} at TLS/transport level: {result['reason']}")

        explicit_rejection = all(
            result["result"] == "rejected" for result in (missing_certificate, untrusted_certificate)
        )

    report = {
        "status": "PASS" if explicit_rejection else "TONIC_RPC_REQUIRED",
        "category": "REAL_NATIVE_RELAY_TLS_NEGATIVE_LOCAL_ONLY",
        "endpoint": f"{host}:{port}",
        "serverName": server_name,
        "serverCaSha256": hashlib.sha256(server_ca.read_bytes()).hexdigest(),
        "transport": "TLS 1.2+ with h2 ALPN; TLS 1.3 outcomes are read from the server before any Tonic RPC",
        "checks": {
            "missingClientCertificate": missing_certificate,
            "untrustedClientCertificate": untrusted_certificate,
        },
        "scope": "Raw Native Relay TLS evidence only; transport close/timeout must be adjudicated by the paired Tonic RPC probe",
        "runAtUtc": datetime.now(UTC).isoformat(),
        **source_details(platform_root),
    }
    report_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    os.chmod(report_path, 0o600)
    print(f"REAL_NATIVE_RELAY_TLS_NEGATIVE_LOCAL_ONLY {report['status']}: report={report_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
