"""Validate review-only ACA Easy Auth fragments without cloud access.

校验 Easy Auth 示例片段，不连接 Azure 或 Entra。
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any


HERE = Path(__file__).resolve().parent
EXPECTED_SCOPE = (
    "scope=openid profile email offline_access "
    "api://<BFF_API_CLIENT_ID>/Workspace.Web.Access"
)


def load_example(name: str) -> dict[str, Any]:
    """Load a JSON object and reject malformed or non-object examples.

    加载 JSON 对象，并拒绝无效 JSON 或非对象内容。
    """
    with (HERE / name).open(encoding="utf-8") as example_file:
        value = json.load(example_file)
    if not isinstance(value, dict):
        raise ValueError(f"{name} must contain a JSON object")
    return value


def validate_common(document: dict[str, Any], name: str) -> dict[str, Any]:
    """Require the exact delegated scope and enabled token store shape.

    要求委派 scope 与启用的 token store 结构符合评审约定。
    """
    aad_login = document["identityProviders"]["azureActiveDirectory"]["login"]
    if aad_login.get("loginParameters") != [EXPECTED_SCOPE]:
        raise ValueError(f"{name} must request the exact Workspace.Web.Access scope")

    token_store = document["login"]["tokenStore"]
    if token_store.get("enabled") is not True:
        raise ValueError(f"{name} must show the proposed token store as enabled")

    blob = token_store["azureBlobStorage"]
    if not isinstance(blob, dict):
        raise ValueError(f"{name} must define azureBlobStorage")
    return blob


def validate_examples() -> None:
    """Check exclusive modes and ensure examples remain placeholders.

    校验托管身份与 SAS 互斥，并确认配置只保留占位符。
    """
    managed_identity_name = "authsettings.properties.managed-identity.example.json"
    managed_identity = validate_common(
        load_example(managed_identity_name), managed_identity_name
    )
    if set(managed_identity) != {"blobContainerUri", "managedIdentityResourceId"}:
        raise ValueError("managed identity example must contain only its documented fields")
    if not managed_identity.get("blobContainerUri", "").startswith("https://<"):
        raise ValueError("managed identity example must retain its account placeholder")
    if not managed_identity.get("managedIdentityResourceId", "").startswith("<"):
        raise ValueError("managed identity example must retain its identity placeholder")
    if "clientId" in managed_identity or "sasUrlSettingName" in managed_identity:
        raise ValueError("managed identity and SAS settings must not be mixed")

    sas_name = "authsettings.properties.sas.example.json"
    sas = validate_common(load_example(sas_name), sas_name)
    if sas != {"sasUrlSettingName": "workspace-web-token-store-sas"}:
        raise ValueError("SAS example must contain only a secret setting name")

    for name in (managed_identity_name, sas_name):
        serialized = (HERE / name).read_text(encoding="utf-8").lower()
        if any(marker in serialized for marker in ("sig=", "sharedaccesssignature=", "bearer ")):
            raise ValueError(f"{name} must not contain a credential value")


if __name__ == "__main__":
    validate_examples()
    print("Workspace Web identity examples are valid review-only fragments.")
