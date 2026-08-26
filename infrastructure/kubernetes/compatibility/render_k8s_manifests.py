#!/usr/bin/env python3
"""Render digest-pinned AstrBot Kubernetes manifests."""

from __future__ import annotations

import argparse
import base64
import json
import math
import os
import re
import stat
import subprocess
import sys
from pathlib import Path

IMAGE_TOKEN = "__ASTRBOT_IMAGE_REF__"
NAPCAT_IMAGE_TOKEN = "__NAPCAT_IMAGE_REF__"
POSTGRES_SECRET_TOKEN = "__ASTRBOT_POSTGRES_SECRET_NAME__"
PROVIDER_PERSISTENCE_SECRET_TOKEN = "__ASTRBOT_PROVIDER_PERSISTENCE_SECRET_NAME__"
PROVIDER_ENDPOINT_AUTH_SECRET_TOKEN = "__ASTRBOT_PROVIDER_ENDPOINT_AUTH_SECRET_NAME__"
INGRESS_HOST_TOKEN = "__ASTRBOT_INGRESS_HOST__"
TLS_SECRET_TOKEN = "__ASTRBOT_TLS_SECRET_NAME__"
IMAGE_REF_PATTERN = re.compile(
    r"^[a-z0-9.-]+(?::[0-9]+)?"
    r"(?:/[a-z0-9]+(?:[._-][a-z0-9]+)*)+"
    r"@sha256:[0-9a-f]{64}$"
)
ROOT = Path(__file__).resolve().parents[1]
VARIANT_NAMESPACES = {
    "astrbot": "astrbot-standalone-ns",
    "astrbot_with_napcat": "astrbot-ns",
}


def is_high_entropy_secret(secret: str) -> bool:
    """Validate a 32-byte-or-larger hexadecimal or Base64-encoded secret.

    Args:
        secret: Secret value read from a protected file.

    Returns:
        Whether the value has the required decoded size and entropy.
    """
    if re.fullmatch(r"[0-9a-fA-F]{64,}", secret) and len(secret) % 2 == 0:
        decoded = bytes.fromhex(secret)
    elif re.fullmatch(r"[A-Za-z0-9_-]+", secret):
        try:
            decoded = base64.b64decode(
                secret + "=" * (-len(secret) % 4), altchars=b"-_", validate=True
            )
        except ValueError:
            return False
    elif re.fullmatch(r"[A-Za-z0-9+/]+={0,2}", secret):
        try:
            decoded = base64.b64decode(secret, validate=True)
        except ValueError:
            return False
    else:
        return False

    if len(decoded) < 32:
        return False

    frequencies = [decoded.count(value) for value in range(256)]
    entropy = -sum(
        (count / len(decoded)) * math.log2(count / len(decoded))
        for count in frequencies
        if count
    )
    return entropy >= 3.5


def main(argv: list[str] | None = None) -> int:
    """Render or apply one supported Kubernetes deployment variant.

    Args:
        argv: Optional command-line arguments. Defaults to the process arguments.

    Returns:
        Process exit code.
    """
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--variant",
        choices=("astrbot", "astrbot_with_napcat"),
        required=True,
    )
    parser.add_argument("--image-ref", required=True)
    parser.add_argument(
        "--napcat-image-ref",
        help="Digest-pinned NapCat image required by the astrbot_with_napcat variant.",
    )
    parser.add_argument(
        "--postgres-secret-name",
        default="astrbot-postgres",
        help="Name of the PostgreSQL connection-string Secret.",
    )
    parser.add_argument(
        "--provider-persistence-secret-name",
        default="astrbot-provider-persistence",
        help="Name of the ProviderPersistence certificate Secret.",
    )
    parser.add_argument(
        "--provider-endpoint-auth-secret-name",
        default="astrbot-provider-endpoint-auth",
        help="Name of the Dashboard JWT and provider bootstrap Secret.",
    )
    parser.add_argument(
        "--ingress-host",
        help="DNS host for an optional TLS-only NGINX Ingress.",
    )
    parser.add_argument(
        "--tls-secret-name",
        help="Existing TLS Secret for the optional Ingress in the variant namespace.",
    )
    parser.add_argument("--output", type=Path)
    parser.add_argument(
        "--apply",
        action="store_true",
        help="Apply the rendered stream with kubectl after optional output.",
    )
    args = parser.parse_args(argv)

    if not IMAGE_REF_PATTERN.fullmatch(args.image_ref):
        parser.error(
            "--image-ref must be registry/repository@sha256 followed by 64 lowercase hex characters"
        )
    if args.variant == "astrbot_with_napcat":
        if not args.napcat_image_ref:
            parser.error(
                "--napcat-image-ref is required for the astrbot_with_napcat variant"
            )
        if not IMAGE_REF_PATTERN.fullmatch(args.napcat_image_ref):
            parser.error(
                "--napcat-image-ref must be registry/repository@sha256 followed by 64 lowercase hex characters"
            )
    elif args.napcat_image_ref is not None:
        parser.error(
            "--napcat-image-ref is only valid for the astrbot_with_napcat variant"
        )
    if bool(args.ingress_host) != bool(args.tls_secret_name):
        parser.error("--ingress-host and --tls-secret-name must be supplied together")
    if args.ingress_host and (
        len(args.ingress_host) > 253
        or not re.fullmatch(
            r"[a-z0-9](?:[-a-z0-9]*[a-z0-9])?(?:\.[a-z0-9](?:[-a-z0-9]*[a-z0-9])?)*",
            args.ingress_host,
        )
    ):
        parser.error("--ingress-host must be a valid lowercase DNS hostname")
    if args.tls_secret_name and (
        len(args.tls_secret_name) > 253
        or not re.fullmatch(
            r"[a-z0-9](?:[-a-z0-9]*[a-z0-9])?(?:\.[a-z0-9](?:[-a-z0-9]*[a-z0-9])?)*",
            args.tls_secret_name,
        )
    ):
        parser.error("--tls-secret-name must be a valid Kubernetes DNS subdomain")
    if len(args.postgres_secret_name) > 253 or not re.fullmatch(
        r"[a-z0-9](?:[-a-z0-9]*[a-z0-9])?(?:\.[a-z0-9](?:[-a-z0-9]*[a-z0-9])?)*",
        args.postgres_secret_name,
    ):
        parser.error("--postgres-secret-name must be a valid Kubernetes DNS subdomain")
    if len(args.provider_persistence_secret_name) > 253 or not re.fullmatch(
        r"[a-z0-9](?:[-a-z0-9]*[a-z0-9])?(?:\.[a-z0-9](?:[-a-z0-9]*[a-z0-9])?)*",
        args.provider_persistence_secret_name,
    ):
        parser.error(
            "--provider-persistence-secret-name must be a valid Kubernetes DNS subdomain"
        )
    if len(args.provider_endpoint_auth_secret_name) > 253 or not re.fullmatch(
        r"[a-z0-9](?:[-a-z0-9]*[a-z0-9])?(?:\.[a-z0-9](?:[-a-z0-9]*[a-z0-9])?)*",
        args.provider_endpoint_auth_secret_name,
    ):
        parser.error(
            "--provider-endpoint-auth-secret-name must be a valid Kubernetes DNS subdomain"
        )

    manifest_dir = ROOT / "deploy" / "k8s" / args.variant
    filenames = [
        "00-namespace.yaml",
        "01-pvc.yaml",
        "02-deployment.yaml",
        "03-service.yaml",
    ]
    if args.ingress_host:
        filenames.append("04-ingress.yaml")

    sources = [
        (manifest_dir / filename).read_text(encoding="utf-8") for filename in filenames
    ]
    if sum(source.count(IMAGE_TOKEN) for source in sources) != 1:
        parser.error(
            f"selected source manifests must contain {IMAGE_TOKEN} exactly once"
        )
    expected_napcat_image_tokens = 1 if args.variant == "astrbot_with_napcat" else 0
    if (
        sum(source.count(NAPCAT_IMAGE_TOKEN) for source in sources)
        != expected_napcat_image_tokens
    ):
        parser.error(
            "selected source manifests must contain "
            f"{NAPCAT_IMAGE_TOKEN} exactly {expected_napcat_image_tokens} times"
        )
    if sum(source.count(POSTGRES_SECRET_TOKEN) for source in sources) != 3:
        parser.error(
            f"selected source manifests must contain {POSTGRES_SECRET_TOKEN} exactly three times"
        )
    if sum(source.count(PROVIDER_PERSISTENCE_SECRET_TOKEN) for source in sources) != 2:
        parser.error(
            "selected source manifests must contain "
            f"{PROVIDER_PERSISTENCE_SECRET_TOKEN} exactly twice"
        )
    if (
        sum(source.count(PROVIDER_ENDPOINT_AUTH_SECRET_TOKEN) for source in sources)
        != 1
    ):
        parser.error(
            "selected source manifests must contain "
            f"{PROVIDER_ENDPOINT_AUTH_SECRET_TOKEN} exactly once"
        )
    expected_ingress_host_tokens = 2 * int(bool(args.ingress_host))
    if (
        sum(source.count(INGRESS_HOST_TOKEN) for source in sources)
        != expected_ingress_host_tokens
    ):
        parser.error(
            "selected source manifests must contain "
            f"{INGRESS_HOST_TOKEN} exactly {expected_ingress_host_tokens} times"
        )
    expected_tls_secret_tokens = int(bool(args.ingress_host))
    if (
        sum(source.count(TLS_SECRET_TOKEN) for source in sources)
        != expected_tls_secret_tokens
    ):
        parser.error(
            "selected source manifests must contain "
            f"{TLS_SECRET_TOKEN} exactly {expected_tls_secret_tokens} times"
        )
    deployment_source = sources[2]
    expected_image_reuses = 2
    if (
        f"image: &astrbot-image {IMAGE_TOKEN}" not in deployment_source
        or deployment_source.count("image: *astrbot-image") != expected_image_reuses
    ):
        parser.error(
            "deployment template must reuse the app image for each initialization, "
            "database migration, and the application"
        )
    rendered = (
        "\n---\n".join(
            source.replace(IMAGE_TOKEN, args.image_ref)
            .replace(NAPCAT_IMAGE_TOKEN, args.napcat_image_ref or "")
            .replace(POSTGRES_SECRET_TOKEN, args.postgres_secret_name)
            .replace(
                PROVIDER_PERSISTENCE_SECRET_TOKEN,
                args.provider_persistence_secret_name,
            )
            .replace(
                PROVIDER_ENDPOINT_AUTH_SECRET_TOKEN,
                args.provider_endpoint_auth_secret_name,
            )
            .replace(INGRESS_HOST_TOKEN, args.ingress_host or "")
            .replace(TLS_SECRET_TOKEN, args.tls_secret_name or "")
            .rstrip()
            for source in sources
        )
        + "\n"
    )
    if (
        IMAGE_TOKEN in rendered
        or NAPCAT_IMAGE_TOKEN in rendered
        or POSTGRES_SECRET_TOKEN in rendered
        or PROVIDER_PERSISTENCE_SECRET_TOKEN in rendered
        or PROVIDER_ENDPOINT_AUTH_SECRET_TOKEN in rendered
        or INGRESS_HOST_TOKEN in rendered
        or TLS_SECRET_TOKEN in rendered
    ):
        parser.error("rendered manifests contain an unresolved template token")
    if args.output is not None and args.output.resolve().is_relative_to(
        (ROOT / "deploy" / "k8s").resolve()
    ):
        parser.error("--output cannot overwrite Kubernetes source templates")

    if args.apply:
        migrator_connection = os.environ.get("ConnectionStrings__AstrBotMigrator")
        if migrator_connection is not None and not migrator_connection.strip():
            migrator_connection = None
        if migrator_connection is None:
            migrator_file_name = os.environ.get(
                "ASTRBOT_MIGRATOR_CONNECTION_STRING_FILE"
            )
            if migrator_file_name:
                migrator_file = Path(migrator_file_name).expanduser()
                try:
                    migrator_stat = migrator_file.stat()
                    if not stat.S_ISREG(migrator_stat.st_mode):
                        parser.error(
                            "ASTRBOT_MIGRATOR_CONNECTION_STRING_FILE must name a regular file"
                        )
                    if os.name != "nt" and stat.S_IMODE(migrator_stat.st_mode) & 0o077:
                        parser.error(
                            "ASTRBOT_MIGRATOR_CONNECTION_STRING_FILE must not be accessible by group or other users"
                        )
                    migrator_connection = migrator_file.read_text(
                        encoding="utf-8"
                    ).rstrip("\r\n")
                except OSError:
                    parser.error(
                        "could not read ASTRBOT_MIGRATOR_CONNECTION_STRING_FILE"
                    )
                if not migrator_connection.strip():
                    migrator_connection = None

        app_connection = os.environ.get("ConnectionStrings__AstrBot")
        if app_connection is not None and not app_connection.strip():
            app_connection = None
        if app_connection is None:
            app_file_name = os.environ.get("ASTRBOT_APP_CONNECTION_STRING_FILE")
            if app_file_name:
                app_file = Path(app_file_name).expanduser()
                try:
                    app_stat = app_file.stat()
                    if not stat.S_ISREG(app_stat.st_mode):
                        parser.error(
                            "ASTRBOT_APP_CONNECTION_STRING_FILE must name a regular file"
                        )
                    if os.name != "nt" and stat.S_IMODE(app_stat.st_mode) & 0o077:
                        parser.error(
                            "ASTRBOT_APP_CONNECTION_STRING_FILE must not be accessible by group or other users"
                        )
                    app_connection = app_file.read_text(encoding="utf-8").rstrip("\r\n")
                except OSError:
                    parser.error("could not read ASTRBOT_APP_CONNECTION_STRING_FILE")
                if not app_connection.strip():
                    app_connection = None

        provider_certificate: bytes | None = None
        provider_certificate_file_name = os.environ.get(
            "ASTRBOT_PROVIDER_PERSISTENCE_CERTIFICATE_FILE"
        )
        if provider_certificate_file_name:
            provider_certificate_file = Path(
                provider_certificate_file_name
            ).expanduser()
            try:
                provider_certificate_stat = provider_certificate_file.stat()
                if not stat.S_ISREG(provider_certificate_stat.st_mode):
                    parser.error(
                        "ASTRBOT_PROVIDER_PERSISTENCE_CERTIFICATE_FILE must name a regular file"
                    )
                if (
                    os.name != "nt"
                    and stat.S_IMODE(provider_certificate_stat.st_mode) & 0o077
                ):
                    parser.error(
                        "ASTRBOT_PROVIDER_PERSISTENCE_CERTIFICATE_FILE must not be accessible by group or other users"
                    )
                provider_certificate = provider_certificate_file.read_bytes()
            except OSError:
                parser.error(
                    "could not read ASTRBOT_PROVIDER_PERSISTENCE_CERTIFICATE_FILE"
                )
            if not provider_certificate:
                parser.error(
                    "ASTRBOT_PROVIDER_PERSISTENCE_CERTIFICATE_FILE must not be empty"
                )

        provider_certificate_password = os.environ.get(
            "ProviderPersistence__CertificatePassword"
        )
        if (
            provider_certificate_password is not None
            and not provider_certificate_password.strip()
        ):
            provider_certificate_password = None
        if provider_certificate_password is None:
            provider_certificate_password_file_name = os.environ.get(
                "ASTRBOT_PROVIDER_PERSISTENCE_CERTIFICATE_PASSWORD_FILE"
            )
            if provider_certificate_password_file_name:
                provider_certificate_password_file = Path(
                    provider_certificate_password_file_name
                ).expanduser()
                try:
                    provider_certificate_password_stat = (
                        provider_certificate_password_file.stat()
                    )
                    if not stat.S_ISREG(provider_certificate_password_stat.st_mode):
                        parser.error(
                            "ASTRBOT_PROVIDER_PERSISTENCE_CERTIFICATE_PASSWORD_FILE must name a regular file"
                        )
                    if (
                        os.name != "nt"
                        and stat.S_IMODE(provider_certificate_password_stat.st_mode)
                        & 0o077
                    ):
                        parser.error(
                            "ASTRBOT_PROVIDER_PERSISTENCE_CERTIFICATE_PASSWORD_FILE must not be accessible by group or other users"
                        )
                    provider_certificate_password = (
                        provider_certificate_password_file.read_text(
                            encoding="utf-8"
                        ).rstrip("\r\n")
                    )
                except OSError:
                    parser.error(
                        "could not read ASTRBOT_PROVIDER_PERSISTENCE_CERTIFICATE_PASSWORD_FILE"
                    )
                if not provider_certificate_password.strip():
                    provider_certificate_password = None

        if (provider_certificate is None) != (provider_certificate_password is None):
            parser.error(
                "ASTRBOT_PROVIDER_PERSISTENCE_CERTIFICATE_FILE and a certificate password must be supplied together"
            )

        provider_endpoint_auth_secrets: dict[str, str] = {}
        for secret_key, file_environment_name in {
            "dashboard-jwt-secret": "ASTRBOT_DASHBOARD_JWT_SECRET_FILE",
            "provider-bootstrap-secret": "ASTRBOT_PROVIDER_BOOTSTRAP_SECRET_FILE",
        }.items():
            secret_file_name = os.environ.get(file_environment_name)
            if not secret_file_name:
                continue
            secret_file = Path(secret_file_name).expanduser()
            try:
                secret_file_stat = secret_file.stat()
                if not stat.S_ISREG(secret_file_stat.st_mode):
                    parser.error(f"{file_environment_name} must name a regular file")
                if os.name != "nt" and stat.S_IMODE(secret_file_stat.st_mode) & 0o077:
                    parser.error(
                        f"{file_environment_name} must not be accessible by group or other users"
                    )
                secret = secret_file.read_text(encoding="utf-8").rstrip("\r\n")
            except OSError:
                parser.error(f"could not read {file_environment_name}")
            if not secret:
                parser.error(f"{file_environment_name} must not be empty")
            if not is_high_entropy_secret(secret):
                parser.error(
                    f"{file_environment_name} must encode at least 32 high-entropy random bytes"
                )
            provider_endpoint_auth_secrets[secret_key] = secret

        if len(provider_endpoint_auth_secrets) not in (0, 2):
            parser.error(
                "ASTRBOT_DASHBOARD_JWT_SECRET_FILE and "
                "ASTRBOT_PROVIDER_BOOTSTRAP_SECRET_FILE must be supplied together"
            )

        namespace = VARIANT_NAMESPACES[args.variant]
        secret_source_variables = {
            "ConnectionStrings__AstrBotMigrator".casefold(),
            "ConnectionStrings__AstrBot".casefold(),
            "ASTRBOT_MIGRATOR_CONNECTION_STRING_FILE".casefold(),
            "ASTRBOT_APP_CONNECTION_STRING_FILE".casefold(),
            "ASTRBOT_PROVIDER_PERSISTENCE_CERTIFICATE_FILE".casefold(),
            "ProviderPersistence__CertificatePassword".casefold(),
            "ASTRBOT_PROVIDER_PERSISTENCE_CERTIFICATE_PASSWORD_FILE".casefold(),
            "ASTRBOT_DASHBOARD_JWT_SECRET_FILE".casefold(),
            "ASTRBOT_PROVIDER_BOOTSTRAP_SECRET_FILE".casefold(),
        }
        kubectl_env = {
            key: value
            for key, value in os.environ.items()
            if key.casefold() not in secret_source_variables
        }
        provision_secret = (
            migrator_connection is not None and app_connection is not None
        )
        provision_provider_persistence_secret = (
            provider_certificate is not None
            and provider_certificate_password is not None
        )
        provision_provider_endpoint_auth_secret = bool(provider_endpoint_auth_secrets)
        if not provision_secret:
            try:
                secret_check = subprocess.run(
                    [
                        "kubectl",
                        "get",
                        "secret",
                        args.postgres_secret_name,
                        "--namespace",
                        namespace,
                        '--output=go-template={{if index .data "migrator-connection-string"}}migrator{{end}}{{if index .data "app-connection-string"}}:app{{end}}',
                    ],
                    capture_output=True,
                    text=True,
                    check=False,
                    env=kubectl_env,
                )
            except FileNotFoundError:
                parser.error("kubectl is required with --apply")
            if secret_check.returncode != 0 or secret_check.stdout != "migrator:app":
                parser.error(
                    f"Secret {namespace}/{args.postgres_secret_name} must already contain migrator-connection-string and app-connection-string, or both connection strings must be supplied through the documented environment or file variables"
                )

        if not provision_provider_persistence_secret:
            try:
                provider_persistence_secret_check = subprocess.run(
                    [
                        "kubectl",
                        "get",
                        "secret",
                        args.provider_persistence_secret_name,
                        "--namespace",
                        namespace,
                        '--output=go-template={{if index .data "provider-persistence.pfx"}}pfx{{end}}{{if index .data "provider-persistence-password"}}:password{{end}}',
                    ],
                    capture_output=True,
                    text=True,
                    check=False,
                    env=kubectl_env,
                )
            except FileNotFoundError:
                parser.error("kubectl is required with --apply")
            if (
                provider_persistence_secret_check.returncode != 0
                or provider_persistence_secret_check.stdout != "pfx:password"
            ):
                parser.error(
                    f"Secret {namespace}/{args.provider_persistence_secret_name} must already contain provider-persistence.pfx and provider-persistence-password, or the certificate file and password must be supplied through the documented environment or file variables"
                )

        if not provision_provider_endpoint_auth_secret:
            try:
                provider_endpoint_auth_secret_check = subprocess.run(
                    [
                        "kubectl",
                        "get",
                        "secret",
                        args.provider_endpoint_auth_secret_name,
                        "--namespace",
                        namespace,
                        '--output=go-template={{if index .data "dashboard-jwt-secret"}}dashboard{{end}}{{if index .data "provider-bootstrap-secret"}}:bootstrap{{end}}',
                    ],
                    capture_output=True,
                    text=True,
                    check=False,
                    env=kubectl_env,
                )
            except FileNotFoundError:
                parser.error("kubectl is required with --apply")
            if (
                provider_endpoint_auth_secret_check.returncode != 0
                or provider_endpoint_auth_secret_check.stdout != "dashboard:bootstrap"
            ):
                parser.error(
                    f"Secret {namespace}/{args.provider_endpoint_auth_secret_name} must already contain dashboard-jwt-secret and provider-bootstrap-secret, or both secret files must be supplied through the documented environment variables"
                )

        if provision_secret:
            secret_manifest = (
                json.dumps(
                    {
                        "apiVersion": "v1",
                        "kind": "Secret",
                        "metadata": {
                            "name": args.postgres_secret_name,
                            "namespace": namespace,
                        },
                        "type": "Opaque",
                        "stringData": {
                            "migrator-connection-string": migrator_connection,
                            "app-connection-string": app_connection,
                        },
                    }
                )
                + "\n"
            )
        if provision_provider_persistence_secret:
            provider_persistence_secret_manifest = (
                json.dumps(
                    {
                        "apiVersion": "v1",
                        "kind": "Secret",
                        "metadata": {
                            "name": args.provider_persistence_secret_name,
                            "namespace": namespace,
                        },
                        "type": "Opaque",
                        "data": {
                            "provider-persistence.pfx": base64.b64encode(
                                provider_certificate
                            ).decode("ascii"),
                        },
                        "stringData": {
                            "provider-persistence-password": provider_certificate_password,
                        },
                    }
                )
                + "\n"
            )
        if provision_provider_endpoint_auth_secret:
            provider_endpoint_auth_secret_manifest = (
                json.dumps(
                    {
                        "apiVersion": "v1",
                        "kind": "Secret",
                        "metadata": {
                            "name": args.provider_endpoint_auth_secret_name,
                            "namespace": namespace,
                        },
                        "type": "Opaque",
                        "stringData": provider_endpoint_auth_secrets,
                    }
                )
                + "\n"
            )

    if args.output is not None:
        args.output.write_text(rendered, encoding="utf-8", newline="\n")
    if args.apply:
        if (
            provision_secret
            or provision_provider_persistence_secret
            or provision_provider_endpoint_auth_secret
        ):
            try:
                subprocess.run(
                    ["kubectl", "apply", "-f", "-"],
                    input=sources[0].rstrip() + "\n",
                    text=True,
                    capture_output=True,
                    check=True,
                    env=kubectl_env,
                )
                if provision_secret:
                    subprocess.run(
                        ["kubectl", "apply", "-f", "-"],
                        input=secret_manifest,
                        text=True,
                        capture_output=True,
                        check=True,
                        env=kubectl_env,
                    )
                if provision_provider_persistence_secret:
                    subprocess.run(
                        ["kubectl", "apply", "-f", "-"],
                        input=provider_persistence_secret_manifest,
                        text=True,
                        capture_output=True,
                        check=True,
                        env=kubectl_env,
                    )
                if provision_provider_endpoint_auth_secret:
                    subprocess.run(
                        ["kubectl", "apply", "-f", "-"],
                        input=provider_endpoint_auth_secret_manifest,
                        text=True,
                        capture_output=True,
                        check=True,
                        env=kubectl_env,
                    )
            except FileNotFoundError:
                parser.error("kubectl is required with --apply")
            except subprocess.CalledProcessError:
                parser.error("kubectl failed to provision a deployment Secret")
        subprocess.run(
            ["kubectl", "apply", "-f", "-"],
            input=rendered,
            text=True,
            check=True,
            env=kubectl_env,
        )
    elif args.output is None:
        sys.stdout.write(rendered)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

