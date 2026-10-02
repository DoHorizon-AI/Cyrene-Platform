#!/usr/bin/env python3
"""Provision isolated, adapter-specific Native Workspace runtime logins.

Module: tooling.acceptance.native_workspace_v2.provision_runtime_logins
Role: Create only the finalized BFF and Relay PostgreSQL LOGIN memberships.

模块职责：为 Native Workspace V2 本地验收配置逐适配器数据库登录。
· 仅使用专属验收库与固定 NOLOGIN 角色  · 连接凭据只写入 0600 文件
"""

from __future__ import annotations

import json
import os
import secrets
import shlex
import subprocess
import sys
import tempfile
from datetime import datetime, timezone
from pathlib import Path
from urllib.parse import quote, unquote, urlencode, urlsplit
from uuid import uuid4


ACCEPTANCE_ROOT = Path("/tmp/cyrene-components-v2-acceptance/native-relay")
MIGRATOR_ENV = ACCEPTANCE_ROOT / "migrator.env"
CONTAINER = "cyrene-components-v2-postgres"
DATABASE = "cyrene_native_workspace_acceptance"
OPERATOR = "cyrene_acceptance_operator"
PORT = 44626
CONTAINER_PORT = 5432
SECRET_FILE = ACCEPTANCE_ROOT / "runtime-login-secrets.json"
BFF_ENV_FILE = ACCEPTANCE_ROOT / "runtime-bff.env"
RELAY_ENV_FILE = ACCEPTANCE_ROOT / "runtime-relay.env"
REPORT_FILE = ACCEPTANCE_ROOT / "runtime-provisioning-report.json"

MIGRATION_URL_NAMES = (
    "CYRENE_WORKSPACE_DIRECTORY_MIGRATION_DATABASE_URL",
    "CYRENE_WORKSPACE_DEVICE_AUTHORIZATION_MIGRATION_DATABASE_URL",
    "CYRENE_WORKSPACE_DEVICE_REGISTRY_MIGRATION_DATABASE_URL",
    "CYRENE_WORKSPACE_WEBAUTHN_MIGRATION_DATABASE_URL",
    "CYRENE_WORKSPACE_WEBAUTHN_HTTP_SESSION_BINDING_MIGRATION_DATABASE_URL",
    "CYRENE_WORKSPACE_DEVICE_CA_MIGRATION_DATABASE_URL",
)

RUNTIME_ROLES = {
    "cyrene_bff_directory_login": "cyrene_workspace_directory_reader",
    "cyrene_bff_authorization_login": "cyrene_workspace_device_authorization_app",
    "cyrene_bff_registry_login": "cyrene_workspace_device_registry_app",
    "cyrene_bff_device_ca_login": "cyrene_workspace_device_ca_app",
    "cyrene_bff_webauthn_login": "cyrene_workspace_webauthn_app",
    "cyrene_bff_webauthn_http_login": "cyrene_workspace_webauthn_http_binding_app",
    "cyrene_relay_directory_login": "cyrene_workspace_directory_reader",
    "cyrene_relay_registry_login": "cyrene_workspace_device_registry_relay_reader",
    "cyrene_relay_device_ca_login": "cyrene_workspace_device_ca_reader",
}

BFF_DATABASE_URLS = {
    "CYRENE_WORKSPACE_DIRECTORY_DATABASE_URL": "cyrene_bff_directory_login",
    "CYRENE_WORKSPACE_DEVICE_AUTHORIZATION_DATABASE_URL": "cyrene_bff_authorization_login",
    "CYRENE_WORKSPACE_DEVICE_REGISTRY_DATABASE_URL": "cyrene_bff_registry_login",
    "CYRENE_WORKSPACE_DEVICE_CA_DATABASE_URL": "cyrene_bff_device_ca_login",
    "CYRENE_WORKSPACE_WEBAUTHN_DATABASE_URL": "cyrene_bff_webauthn_login",
    "CYRENE_WORKSPACE_WEBAUTHN_HTTP_SESSION_BINDING_DATABASE_URL": "cyrene_bff_webauthn_http_login",
}

RELAY_DATABASE_URLS = {
    "CYRENE_WORKSPACE_DIRECTORY_DATABASE_URL": "cyrene_relay_directory_login",
    "CYRENE_WORKSPACE_RELAY_DEVICE_REGISTRY_DATABASE_URL": "cyrene_relay_registry_login",
    "CYRENE_WORKSPACE_RELAY_DEVICE_CA_DATABASE_URL": "cyrene_relay_device_ca_login",
}

MIGRATION_HISTORIES = {
    "public": 4,
    "cyrene_workspace_device_authorization": 9,
    "cyrene_workspace_device_registry": 5,
    "cyrene_workspace_webauthn": 1,
    "cyrene_workspace_webauthn_http_binding": 1,
    "cyrene_workspace_device_ca": 1,
}


def load_operator_url() -> tuple[str, str]:
    """Read and validate the protected operator URL without printing it.

    读取并验证专用迁移环境；任何失败都只报告字段或目标类别。

    Returns:
        The operator login name and verified public root certificate path.
    """
    if not MIGRATOR_ENV.is_file() or MIGRATOR_ENV.stat().st_mode & 0o777 != 0o600:
        raise RuntimeError("Private migration environment must be a regular 0600 file.")

    values: dict[str, str] = {}
    for raw_line in MIGRATOR_ENV.read_text(encoding="utf-8").splitlines():
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("export "):
            line = line.removeprefix("export ")
        if "=" not in line:
            raise RuntimeError("Migration environment has an unsupported line.")
        name, raw_value = line.split("=", 1)
        if name not in MIGRATION_URL_NAMES:
            continue
        parsed_value = shlex.split(raw_value, comments=False, posix=True)
        if len(parsed_value) != 1:
            raise RuntimeError("Migration environment URL value is malformed.")
        values[name] = parsed_value[0]

    if set(values) != set(MIGRATION_URL_NAMES) or len(set(values.values())) != 1:
        raise RuntimeError("All six operator URLs must target the same acceptance database.")

    url = urlsplit(values[MIGRATION_URL_NAMES[0]])
    query = dict(item.split("=", 1) for item in url.query.split("&") if "=" in item)
    root_certificate = Path(unquote(query.get("sslrootcert", "")))
    if (
        url.scheme not in ("postgres", "postgresql")
        or url.hostname != "127.0.0.1"
        or url.port != PORT
        or unquote(url.path.removeprefix("/")) != DATABASE
        or unquote(url.username or "") != OPERATOR
        or query.get("sslmode") != "verify-full"
        or not root_certificate.is_file()
        or not root_certificate.is_relative_to(Path("/tmp/cyrene-components-v2-acceptance"))
    ):
        raise RuntimeError("Operator URL is outside the approved TLS-verified acceptance target.")
    return OPERATOR, str(root_certificate)


def psql(
    sql: str | None = None,
    *,
    stdin: str | None = None,
    arguments: tuple[str, ...] = (),
    check: bool = True,
) -> str:
    """Run container-local psql and keep all output private to the caller.

    在隔离 PostgreSQL 容器本地 socket 执行固定 SQL，避免 operator 密码进入 argv。

    Args:
        sql: Optional SQL passed as the psql command string.
        stdin: Optional private input, used only for psql's password prompts.
        arguments: Additional non-secret psql options.
        check: Raise a sanitized error when psql fails.
    Returns:
        Captured standard output, which callers must validate before reporting.
    """
    command = [
        "docker",
        "exec",
        "-i",
        CONTAINER,
        "psql",
        "-X",
        "-A",
        "-t",
        "-v",
        "ON_ERROR_STOP=1",
        "-U",
        OPERATOR,
        "-d",
        DATABASE,
        *arguments,
    ]
    if sql is not None:
        command.extend(("-c", sql))
    result = subprocess.run(
        command,
        input=stdin,
        text=True,
        capture_output=True,
        check=False,
    )
    if check and result.returncode != 0:
        raise RuntimeError(f"Isolated PostgreSQL command failed with exit {result.returncode}.")
    return result.stdout.strip()


def migration_history() -> dict[str, int]:
    """Check every expected SQLx history is fully applied and successful."""
    clauses = []
    for schema in MIGRATION_HISTORIES:
        clauses.append(
            "SELECT "
            f"'{schema}' || '|' || count(*)::text || '|' || "
            "coalesce(min(version), 0)::text || '|' || "
            "coalesce(max(version), 0)::text || '|' || "
            "coalesce(bool_and(success), false)::text "
            f'FROM "{schema}"._sqlx_migrations'
        )
    output = psql(" UNION ALL ".join(clauses))
    observed: dict[str, int] = {}
    for row in output.splitlines():
        schema, count, minimum, maximum, success = row.split("|")
        expected = MIGRATION_HISTORIES.get(schema)
        if (
            expected is None
            or int(count) != expected
            or int(minimum) != 1
            or int(maximum) != expected
            or success != "true"
        ):
            raise RuntimeError("A required storage migration history is incomplete or failed.")
        observed[schema] = expected
    if observed != MIGRATION_HISTORIES:
        raise RuntimeError("One or more required storage migration histories are missing.")
    return observed


def assert_role_acl() -> None:
    """Verify fixed roles and the least-privilege relay fence ACL."""
    fixed_roles = tuple(sorted(set(RUNTIME_ROLES.values())))
    quoted_roles = ",".join("'" + role + "'" for role in fixed_roles)
    role_sql = (
        "SELECT rolname || '|' || rolcanlogin::text || '|' || rolsuper::text "
        f"FROM pg_roles WHERE rolname IN ({quoted_roles}) ORDER BY rolname"
    )
    observed = psql(role_sql).splitlines()
    expected = [f"{role}|false|false" for role in fixed_roles]
    if observed != expected:
        raise RuntimeError("A required fixed runtime group is missing or has unsafe role flags.")

    fence_sql = (
        "SELECT coalesce(string_agg(item::text, ',' ORDER BY item::text), '') "
        "FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace "
        "CROSS JOIN LATERAL unnest(p.proacl) AS item "
        "WHERE n.nspname='cyrene_workspace_device_registry' "
        "AND p.proname='relay_dispatch_fence'"
    )
    fence_acl = psql(fence_sql)
    expected_acl = (
        "cyrene_workspace_device_registry_app=X/cyrene_workspace_device_registry_owner,"
        "cyrene_workspace_device_registry_relay_reader=X/cyrene_workspace_device_registry_owner"
    )
    if fence_acl != expected_acl:
        raise RuntimeError("Relay dispatch fence EXECUTE ACL is not the reviewed least-privilege ACL.")


def load_or_create_secrets() -> dict[str, str]:
    """Load a prior private staging file or create credentials for each adapter."""
    if SECRET_FILE.exists():
        if SECRET_FILE.stat().st_mode & 0o777 != 0o600:
            raise RuntimeError("Incomplete runtime credential staging file must have mode 0600.")
        payload = json.loads(SECRET_FILE.read_text(encoding="utf-8"))
        if payload.get("generatedBy") != "provision-runtime-logins.py":
            raise RuntimeError("Existing runtime credential staging file is not owned by this runner.")
        passwords = payload.get("passwords")
        if not isinstance(passwords, dict) or set(passwords) != set(RUNTIME_ROLES):
            raise RuntimeError("Incomplete runtime credential staging file has an invalid role set.")
        return {role: str(passwords[role]) for role in RUNTIME_ROLES}

    passwords = {role: secrets.token_urlsafe(48) for role in RUNTIME_ROLES}
    write_private_json(
        SECRET_FILE,
        {"generatedBy": "provision-runtime-logins.py", "passwords": passwords},
    )
    return passwords


def write_private_json(path: Path, payload: dict[str, object]) -> None:
    """Atomically persist JSON under mode 0600."""
    data = json.dumps(payload, indent=2, sort_keys=True) + "\n"
    write_private_text(path, data)


def write_private_text(path: Path, data: str) -> None:
    """Atomically persist a private text file under mode 0600."""
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    os.chmod(path.parent, 0o700)
    fd, temporary_name = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    temporary_path = Path(temporary_name)
    try:
        os.fchmod(fd, 0o600)
        with os.fdopen(fd, "w", encoding="utf-8") as stream:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary_path, path)
        os.chmod(path, 0o600)
    finally:
        if temporary_path.exists():
            temporary_path.unlink()


def inspect_runtime_roles() -> dict[str, tuple[bool, bool, bool, bool, bool, bool, bool]]:
    """Read login flags so the runner can safely resume a partial setup."""
    quoted_roles = ",".join("'" + role + "'" for role in sorted(RUNTIME_ROLES))
    sql = (
        "SELECT rolname || '|' || rolcanlogin::text || '|' || rolinherit::text || '|' || "
        "rolsuper::text || '|' || rolcreatedb::text || '|' || rolcreaterole::text || '|' || "
        "rolreplication::text || '|' || rolbypassrls::text "
        f"FROM pg_roles WHERE rolname IN ({quoted_roles}) ORDER BY rolname"
    )
    roles: dict[str, tuple[bool, bool, bool, bool, bool, bool, bool]] = {}
    for line in psql(sql).splitlines():
        fields = line.split("|")
        roles[fields[0]] = tuple(field == "true" for field in fields[1:])  # type: ignore[assignment]
    return roles


def inspect_memberships() -> dict[str, list[tuple[str, bool, bool, bool]]]:
    """Read role membership options for the nine dedicated runtime logins."""
    quoted_roles = ",".join("'" + role + "'" for role in sorted(RUNTIME_ROLES))
    sql = (
        "SELECT member.rolname || '|' || parent.rolname || '|' || "
        "m.admin_option::text || '|' || m.inherit_option::text || '|' || m.set_option::text "
        "FROM pg_auth_members m JOIN pg_roles member ON member.oid=m.member "
        "JOIN pg_roles parent ON parent.oid=m.roleid "
        f"WHERE member.rolname IN ({quoted_roles}) ORDER BY member.rolname, parent.rolname"
    )
    memberships: dict[str, list[tuple[str, bool, bool, bool]]] = {}
    for line in psql(sql).splitlines():
        member, parent, admin, inherit, can_set = line.split("|")
        memberships.setdefault(member, []).append((parent, admin == "true", inherit == "true", can_set == "true"))
    return memberships


def validate_existing_roles(
    roles: dict[str, tuple[bool, bool, bool, bool, bool, bool, bool]],
    memberships: dict[str, list[tuple[str, bool, bool, bool]]],
) -> None:
    """Reject pre-existing or partially altered roles outside this runner's contract."""
    allowed_flags = (False, True, False, False, False, False, False)
    for role, flags in roles.items():
        if flags != allowed_flags:
            raise RuntimeError("An existing acceptance login has unexpected role attributes.")
        expected_membership = [(RUNTIME_ROLES[role], False, True, False)]
        if memberships.get(role) != expected_membership:
            raise RuntimeError("An existing acceptance login has unexpected role memberships.")


def create_roles() -> None:
    """Create only missing NOLOGIN roles and their exact database grants."""
    existing = inspect_runtime_roles()
    membership_state = inspect_memberships()
    for role, flags in existing.items():
        allowed_flags = (False, True, False, False, False, False, False)
        if flags not in (allowed_flags, (True, True, False, False, False, False, False)):
            raise RuntimeError("An existing acceptance login has unexpected role attributes.")
        expected_membership = [(RUNTIME_ROLES[role], False, True, False)]
        if membership_state.get(role) != expected_membership:
            raise RuntimeError("An existing acceptance login has unexpected role memberships.")

    statements = ["BEGIN;"]
    for role, group in RUNTIME_ROLES.items():
        if role not in existing:
            statements.append(
                f"CREATE ROLE {role} NOLOGIN INHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;"
            )
            statements.append(f"GRANT CONNECT ON DATABASE {DATABASE} TO {role};")
            statements.append(f"GRANT {group} TO {role} WITH INHERIT TRUE, SET FALSE;")
    statements.append("COMMIT;")
    psql(" ".join(statements))


def assign_passwords(passwords: dict[str, str]) -> None:
    """Set each password through psql's hidden password prompt, never in SQL or argv."""
    for role, password in passwords.items():
        result = subprocess.run(
            [
                "docker",
                "exec",
                "-i",
                CONTAINER,
                "psql",
                "-X",
                "-v",
                "ON_ERROR_STOP=1",
                "-U",
                OPERATOR,
                "-d",
                DATABASE,
                "-c",
                f"\\password {role}",
            ],
            input=f"{password}\n{password}\n",
            text=True,
            capture_output=True,
            check=False,
        )
        if result.returncode != 0:
            raise RuntimeError("A runtime login password could not be set by psql's hidden prompt.")


def enable_logins() -> None:
    """Enable LOGIN only after all nine randomly generated passwords are set."""
    statements = ["BEGIN;"]
    statements.extend(f"ALTER ROLE {role} LOGIN;" for role in RUNTIME_ROLES)
    statements.append("COMMIT;")
    psql(" ".join(statements))


def verify_provisioned_roles() -> None:
    """Verify every LOGIN has exactly its intended inherited NOLOGIN membership."""
    roles = inspect_runtime_roles()
    memberships = inspect_memberships()
    expected_flags = (True, True, False, False, False, False, False)
    if set(roles) != set(RUNTIME_ROLES):
        raise RuntimeError("The runtime login set differs from the approved adapter map.")
    for role, flags in roles.items():
        if flags != expected_flags:
            raise RuntimeError("A provisioned runtime login has unsafe role attributes.")
        expected_membership = [(RUNTIME_ROLES[role], False, True, False)]
        if memberships.get(role) != expected_membership:
            raise RuntimeError("A provisioned runtime login has an unexpected role membership.")


def make_runtime_url(role: str, password: str, root_certificate: str) -> str:
    """Build a verify-full URL for one dedicated adapter login."""
    query = urlencode(
        {"sslmode": "verify-full", "sslrootcert": root_certificate},
        safe="/-_",
    )
    return f"postgresql://{role}:{quote(password, safe='-_')}@127.0.0.1:{PORT}/{DATABASE}?{query}"


def render_env(urls: dict[str, str]) -> str:
    """Render only the required adapter URL variables for a host process."""
    return "".join(f"export {name}='{url}'\n" for name, url in urls.items())


def verify_login_connections(passwords: dict[str, str], root_certificate: str) -> None:
    """Authenticate each login over TLS verify-full and check its exact membership."""
    suffix = uuid4().hex
    local_pgpass = ACCEPTANCE_ROOT / f".runtime-{suffix}.pgpass"
    container_pgpass = f"/tmp/native-relay-runtime-{suffix}.pgpass"
    container_ca = f"/tmp/native-relay-root-{suffix}.crt"
    pgpass = "".join(
        f"127.0.0.1:{CONTAINER_PORT}:{DATABASE}:{role}:{password}\n" for role, password in passwords.items()
    )
    write_private_text(local_pgpass, pgpass)
    try:
        subprocess.run(
            ["docker", "cp", str(local_pgpass), f"{CONTAINER}:{container_pgpass}"], check=True, capture_output=True
        )
        subprocess.run(
            ["docker", "cp", root_certificate, f"{CONTAINER}:{container_ca}"], check=True, capture_output=True
        )
        subprocess.run(
            ["docker", "exec", CONTAINER, "chmod", "0600", container_pgpass], check=True, capture_output=True
        )
        for role in RUNTIME_ROLES:
            command = [
                "docker",
                "exec",
                "-e",
                f"PGPASSFILE={container_pgpass}",
                "-e",
                "PGSSLMODE=verify-full",
                "-e",
                f"PGSSLROOTCERT={container_ca}",
                CONTAINER,
                "psql",
                "-X",
                "-v",
                "ON_ERROR_STOP=1",
                "-h",
                "127.0.0.1",
                "-p",
                str(CONTAINER_PORT),
                "-U",
                role,
                "-d",
                DATABASE,
                "-At",
                "-c",
                "SELECT current_user",
            ]
            result = subprocess.run(command, check=False, capture_output=True, text=True)
            if result.returncode != 0 or result.stdout.strip() != role:
                raise RuntimeError("A dedicated runtime login failed TLS/password authentication.")
    finally:
        local_pgpass.unlink(missing_ok=True)
        subprocess.run(["docker", "exec", CONTAINER, "unlink", container_pgpass], check=False, capture_output=True)
        subprocess.run(["docker", "exec", CONTAINER, "unlink", container_ca], check=False, capture_output=True)


def main() -> int:
    """Provision roles, validate TLS logins, and publish protected runtime envs."""
    if len(sys.argv) != 1:
        raise RuntimeError("This runner takes no command-line arguments.")
    os.umask(0o077)
    if not ACCEPTANCE_ROOT.is_dir() or ACCEPTANCE_ROOT.stat().st_mode & 0o777 != 0o700:
        raise RuntimeError("Acceptance directory must exist with mode 0700.")
    if any(path.exists() for path in (BFF_ENV_FILE, RELAY_ENV_FILE, REPORT_FILE)):
        raise RuntimeError("Runtime env or report already exists; refusing to overwrite credentials.")

    operator, root_certificate = load_operator_url()
    if operator != OPERATOR:
        raise RuntimeError("Unexpected isolated database operator.")
    histories = migration_history()
    assert_role_acl()
    passwords = load_or_create_secrets()
    create_roles()
    assign_passwords(passwords)
    enable_logins()
    verify_provisioned_roles()
    verify_login_connections(passwords, root_certificate)

    bff_urls = {
        name: make_runtime_url(role, passwords[role], root_certificate) for name, role in BFF_DATABASE_URLS.items()
    }
    relay_urls = {
        name: make_runtime_url(role, passwords[role], root_certificate) for name, role in RELAY_DATABASE_URLS.items()
    }
    write_private_text(BFF_ENV_FILE, render_env(bff_urls))
    write_private_text(RELAY_ENV_FILE, render_env(relay_urls))
    REPORT_FILE.write_text(
        json.dumps(
            {
                "status": "PASS",
                "category": "ISOLATED_POSTGRES_RUNTIME_LOGIN_PROVISIONING",
                "completedAtUtc": datetime.now(timezone.utc).isoformat(),
                "database": DATABASE,
                "migrations": histories,
                "runtimeLoginMemberships": RUNTIME_ROLES,
                "runtimeLoginCount": len(RUNTIME_ROLES),
                "bffEnvironmentFile": str(BFF_ENV_FILE),
                "relayEnvironmentFile": str(RELAY_ENV_FILE),
                "passwordsVerifiedOver": "TLS verify-full",
                "runtimeApplicationData": "none",
            },
            indent=2,
            sort_keys=True,
        )
        + "\n",
        encoding="utf-8",
    )
    os.chmod(REPORT_FILE, 0o600)
    SECRET_FILE.unlink(missing_ok=True)
    print("Provisioned nine least-privilege isolated runtime logins; TLS verify-full login checks passed.")
    print(f"BFF runtime environment: {BFF_ENV_FILE}")
    print(f"Relay runtime environment: {RELAY_ENV_FILE}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as error:  # Sanitize failures because subprocess inputs can carry protected credentials.
        print(f"Runtime login provisioning failed: {type(error).__name__}.", file=sys.stderr)
        raise SystemExit(1)
