"""
┌──────────────────────────────────────────────────────────────────────────┐
│  📄 preflight_component_release.py                                      │
│  Module: tooling.release.preflight_component_release                    │
│  Role: Prove repository release immutability and tag availability.       │
│                                                                          │
│  模块职责：上传前只读检查仓库不可变设置及 release/tag 冲突。             │
└──────────────────────────────────────────────────────────────────────────┘
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import urllib.error
import urllib.request
from pathlib import Path
from urllib.parse import quote


SOURCE_COMMIT = re.compile(r"^(?:[0-9a-f]{40}|[0-9a-f]{64})$")
REPOSITORY = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$")
IMMUTABLE_SETTINGS_READ_TOKEN = "CYRENE_IMMUTABLE_RELEASE_SETTINGS_READ_TOKEN"


class ReleasePreflightError(ValueError):
    """Raised when a release cannot be proven safe to create."""


def _api_get(url: str, *, immutable_settings: bool = False) -> tuple[int, dict[str, object] | None]:
    """Fetch one GitHub API object and preserve HTTP status for 404 checks."""
    headers = {
        "Accept": "application/vnd.github+json",
        "X-GitHub-Api-Version": "2022-11-28",
    }
    if immutable_settings:
        token = os.environ.get(IMMUTABLE_SETTINGS_READ_TOKEN)
        if not token:
            raise ReleasePreflightError(
                "immutable release settings require "
                f"{IMMUTABLE_SETTINGS_READ_TOKEN} with repository Administration read access"
            )
    else:
        token = os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN")
    if token:
        headers["Authorization"] = f"Bearer {token}"
    request = urllib.request.Request(url, headers=headers)
    try:
        with urllib.request.urlopen(request, timeout=20) as response:
            value = json.loads(response.read().decode("utf-8"))
            if not isinstance(value, dict):
                raise ReleasePreflightError("GitHub API returned a non-object response")
            return response.status, value
    except urllib.error.HTTPError as error:
        if error.code == 404:
            return 404, None
        if immutable_settings and error.code in {401, 403}:
            raise ReleasePreflightError(
                "GitHub denied immutable release settings access; configure "
                f"{IMMUTABLE_SETTINGS_READ_TOKEN} with repository Administration read access"
            ) from error
        raise ReleasePreflightError(f"GitHub API returned HTTP {error.code} for {url}") from error
    except (OSError, urllib.error.URLError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ReleasePreflightError(f"cannot verify GitHub release state over HTTPS: {error}") from error


def preflight(
    *,
    repository: str,
    release_id: str,
    channel: str,
    source_ref: str,
    source_commit: str,
    repository_root: Path,
) -> None:
    """Require enabled immutable releases and prove release/tag absence.

    Args:
        repository: Canonical GitHub ``owner/repository`` identity.
        release_id: Expected immutable channel plus complete source SHA.
        channel: ``stable`` or ``preview``.
        source_ref: Exact branch ref allowed for the selected channel.
        source_commit: Full lower-case source commit SHA.
        repository_root: Clean checkout whose origin will be checked for tags.
    """
    if not REPOSITORY.fullmatch(repository) or not SOURCE_COMMIT.fullmatch(source_commit):
        raise ReleasePreflightError("repository and source commit must be canonical and fully pinned")
    allowed_refs = {"stable": {"refs/heads/main", "refs/heads/release"}, "preview": {"refs/heads/develop"}}
    if channel not in allowed_refs or source_ref not in allowed_refs[channel]:
        raise ReleasePreflightError("source ref is not allowed by the immutable release channel policy")
    if release_id != f"{channel}-{source_commit}":
        raise ReleasePreflightError("release ID must equal the selected channel plus full source SHA")

    owner, name = repository.split("/", 1)
    base_url = f"https://api.github.com/repos/{owner}/{name}"
    settings_status, settings = _api_get(base_url + "/immutable-releases", immutable_settings=True)
    if settings_status == 404:
        raise ReleasePreflightError("repository immutable releases are disabled; refusing publication")
    if settings_status != 200:
        raise ReleasePreflightError(
            "GitHub did not return HTTP 200 for immutable release settings; refusing publication"
        )
    if settings is None or settings.get("enabled") is not True:
        raise ReleasePreflightError("repository immutable releases are disabled; refusing publication")

    release_url = base_url + "/releases/tags/" + quote(release_id, safe="-")
    status, release = _api_get(release_url)
    if status != 404 or release is not None:
        raise ReleasePreflightError("release tag already exists or GitHub did not prove it absent")

    expected_origins = {
        f"https://github.com/{repository}.git",
        f"https://github.com/{repository}",
    }
    try:
        origin = subprocess.run(
            ["git", "remote", "get-url", "origin"],
            cwd=repository_root,
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()
    except (OSError, subprocess.CalledProcessError) as error:
        raise ReleasePreflightError(f"cannot inspect source repository origin: {error}") from error
    if origin not in expected_origins:
        raise ReleasePreflightError("source checkout origin differs from the release publisher repository")

    tag_result = subprocess.run(
        ["git", "ls-remote", "--exit-code", "--refs", "origin", f"refs/tags/{release_id}"],
        cwd=repository_root,
        capture_output=True,
        text=True,
        check=False,
    )
    if tag_result.returncode == 0:
        raise ReleasePreflightError("immutable release tag already exists in the source repository")
    if tag_result.returncode != 2:
        detail = tag_result.stderr.strip() or tag_result.stdout.strip()
        raise ReleasePreflightError(f"cannot prove release tag absence: {detail}")


def main() -> int:
    """Parse CI inputs and run read-only immutable release preflight checks."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repository", required=True)
    parser.add_argument("--release-id", required=True)
    parser.add_argument("--channel", choices=("stable", "preview"), required=True)
    parser.add_argument("--source-ref", required=True)
    parser.add_argument("--source-commit", required=True)
    parser.add_argument("--repository-root", type=Path, required=True)
    arguments = parser.parse_args()
    try:
        preflight(
            repository=arguments.repository,
            release_id=arguments.release_id,
            channel=arguments.channel,
            source_ref=arguments.source_ref,
            source_commit=arguments.source_commit,
            repository_root=arguments.repository_root.resolve(),
        )
    except (OSError, ReleasePreflightError, ValueError) as error:
        print(f"Component release preflight failed: {error}", file=sys.stderr)
        return 2
    print(f"immutable component release preflight passed for {arguments.repository}@{arguments.release_id}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
