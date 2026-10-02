"""Check fail-closed preflight rules for immutable component releases.

验证组件发布 preflight 在不可变设置、release 和 tag 状态不确定时拒绝写入。
"""

from __future__ import annotations

import io
import json
import os
import urllib.error
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import MagicMock, call, patch

from preflight_component_release import (
    IMMUTABLE_SETTINGS_READ_TOKEN,
    ReleasePreflightError,
    preflight,
)


class PreflightTests(unittest.TestCase):
    """Exercise the read-only release safety gates without GitHub writes."""

    repository = "DoHorizon-AI/Cyrene-Catalyst"
    source_commit = "a" * 40
    release_id = f"preview-{source_commit}"
    repository_root = Path("/tmp/product-source")

    def test_accepts_absent_release_and_tag_when_immutable_is_enabled(self) -> None:
        with (
            patch(
                "preflight_component_release._api_get",
                side_effect=[(200, {"enabled": True}), (404, None)],
            ),
            patch(
                "preflight_component_release.subprocess.run",
                side_effect=[
                    SimpleNamespace(stdout="https://github.com/DoHorizon-AI/Cyrene-Catalyst.git\n"),
                    SimpleNamespace(returncode=2, stdout="", stderr=""),
                ],
            ) as run,
        ):
            preflight(
                repository=self.repository,
                release_id=self.release_id,
                channel="preview",
                source_ref="refs/heads/develop",
                source_commit=self.source_commit,
                repository_root=self.repository_root,
            )

        self.assertEqual(run.call_count, 2)
        self.assertEqual(
            run.call_args_list[1],
            call(
                [
                    "git",
                    "ls-remote",
                    "--exit-code",
                    "--refs",
                    "origin",
                    f"refs/tags/{self.release_id}",
                ],
                cwd=self.repository_root,
                capture_output=True,
                text=True,
                check=False,
            ),
        )

    def test_accepts_a_full_length_sha256_git_object_id(self) -> None:
        source_commit = "b" * 64
        release_id = f"preview-{source_commit}"
        with (
            patch(
                "preflight_component_release._api_get",
                side_effect=[(200, {"enabled": True}), (404, None)],
            ),
            patch(
                "preflight_component_release.subprocess.run",
                side_effect=[
                    SimpleNamespace(stdout="https://github.com/DoHorizon-AI/Cyrene-Catalyst.git\n"),
                    SimpleNamespace(returncode=2, stdout="", stderr=""),
                ],
            ),
        ):
            preflight(
                repository=self.repository,
                release_id=release_id,
                channel="preview",
                source_ref="refs/heads/develop",
                source_commit=source_commit,
                repository_root=self.repository_root,
            )

    def test_rejects_disabled_immutable_releases(self) -> None:
        with (
            patch(
                "preflight_component_release._api_get",
                return_value=(200, {"enabled": False}),
            ),
            patch("preflight_component_release.subprocess.run") as run,
        ):
            with self.assertRaisesRegex(ReleasePreflightError, "immutable releases are disabled"):
                preflight(
                    repository=self.repository,
                    release_id=self.release_id,
                    channel="preview",
                    source_ref="refs/heads/develop",
                    source_commit=self.source_commit,
                    repository_root=self.repository_root,
                )
        run.assert_not_called()

    def test_rejects_non_200_settings_response_even_when_enabled(self) -> None:
        with (
            patch(
                "preflight_component_release._api_get",
                return_value=(201, {"enabled": True}),
            ),
            patch("preflight_component_release.subprocess.run") as run,
        ):
            with self.assertRaisesRegex(ReleasePreflightError, "did not return HTTP 200"):
                preflight(
                    repository=self.repository,
                    release_id=self.release_id,
                    channel="preview",
                    source_ref="refs/heads/develop",
                    source_commit=self.source_commit,
                    repository_root=self.repository_root,
                )
        run.assert_not_called()

    def test_rejects_an_existing_release_before_tag_lookup(self) -> None:
        with (
            patch(
                "preflight_component_release._api_get",
                side_effect=[(200, {"enabled": True}), (200, {"tag_name": self.release_id})],
            ),
            patch("preflight_component_release.subprocess.run") as run,
        ):
            with self.assertRaisesRegex(ReleasePreflightError, "already exists"):
                preflight(
                    repository=self.repository,
                    release_id=self.release_id,
                    channel="preview",
                    source_ref="refs/heads/develop",
                    source_commit=self.source_commit,
                    repository_root=self.repository_root,
                )
        run.assert_not_called()

    def test_rejects_unexpected_tag_lookup_failure(self) -> None:
        with (
            patch(
                "preflight_component_release._api_get",
                side_effect=[(200, {"enabled": True}), (404, None)],
            ),
            patch(
                "preflight_component_release.subprocess.run",
                side_effect=[
                    SimpleNamespace(stdout="https://github.com/DoHorizon-AI/Cyrene-Catalyst\n"),
                    SimpleNamespace(returncode=128, stdout="", stderr="temporary network failure"),
                ],
            ),
        ):
            with self.assertRaisesRegex(ReleasePreflightError, "cannot prove release tag absence"):
                preflight(
                    repository=self.repository,
                    release_id=self.release_id,
                    channel="preview",
                    source_ref="refs/heads/develop",
                    source_commit=self.source_commit,
                    repository_root=self.repository_root,
                )

    def test_uses_dedicated_token_only_for_immutable_settings(self) -> None:
        settings_token = "settings-read-secret"
        content_token = "contents-write-secret"
        requests = []

        def open_url(request, timeout):
            requests.append(request)
            if request.full_url.endswith("/immutable-releases"):
                response = MagicMock()
                response.__enter__.return_value.status = 200
                response.__enter__.return_value.read.return_value = json.dumps({"enabled": True}).encode()
                return response
            raise urllib.error.HTTPError(request.full_url, 404, "Not Found", {}, None)

        with (
            patch.dict(
                os.environ,
                {
                    IMMUTABLE_SETTINGS_READ_TOKEN: settings_token,
                    "GH_TOKEN": content_token,
                    "GITHUB_TOKEN": "fallback-secret",
                },
                clear=True,
            ),
            patch("preflight_component_release.urllib.request.urlopen", side_effect=open_url),
            patch(
                "preflight_component_release.subprocess.run",
                side_effect=[
                    SimpleNamespace(stdout="https://github.com/DoHorizon-AI/Cyrene-Catalyst.git\n"),
                    SimpleNamespace(returncode=2, stdout="", stderr=""),
                ],
            ),
        ):
            preflight(
                repository=self.repository,
                release_id=self.release_id,
                channel="preview",
                source_ref="refs/heads/develop",
                source_commit=self.source_commit,
                repository_root=self.repository_root,
            )

        self.assertEqual(len(requests), 2)
        self.assertTrue(requests[0].full_url.endswith("/immutable-releases"))
        self.assertEqual(requests[0].get_header("Authorization"), f"Bearer {settings_token}")
        self.assertTrue(requests[1].full_url.endswith(f"/releases/tags/{self.release_id}"))
        self.assertEqual(requests[1].get_header("Authorization"), f"Bearer {content_token}")

    def test_requires_dedicated_settings_token_even_when_content_token_exists(self) -> None:
        with (
            patch.dict(os.environ, {"GH_TOKEN": "contents-write-secret"}, clear=True),
            patch("preflight_component_release.urllib.request.urlopen") as urlopen,
        ):
            with self.assertRaisesRegex(
                ReleasePreflightError,
                f"{IMMUTABLE_SETTINGS_READ_TOKEN}.*Administration read",
            ) as raised:
                preflight(
                    repository=self.repository,
                    release_id=self.release_id,
                    channel="preview",
                    source_ref="refs/heads/develop",
                    source_commit=self.source_commit,
                    repository_root=self.repository_root,
                )

        self.assertNotIn("contents-write-secret", str(raised.exception))
        urlopen.assert_not_called()

    def test_rejects_settings_forbidden_without_leaking_either_token(self) -> None:
        settings_token = "settings-read-secret"
        content_token = "contents-write-secret"
        requests = []

        def deny_settings(request, timeout):
            requests.append(request)
            raise urllib.error.HTTPError(
                request.full_url,
                403,
                "Forbidden",
                {},
                io.BytesIO(content_token.encode()),
            )

        with (
            patch.dict(
                os.environ,
                {
                    IMMUTABLE_SETTINGS_READ_TOKEN: settings_token,
                    "GH_TOKEN": content_token,
                },
                clear=True,
            ),
            patch(
                "preflight_component_release.urllib.request.urlopen",
                side_effect=deny_settings,
            ),
        ):
            with self.assertRaisesRegex(
                ReleasePreflightError,
                f"denied immutable release settings access.*{IMMUTABLE_SETTINGS_READ_TOKEN}",
            ) as raised:
                preflight(
                    repository=self.repository,
                    release_id=self.release_id,
                    channel="preview",
                    source_ref="refs/heads/develop",
                    source_commit=self.source_commit,
                    repository_root=self.repository_root,
                )

        self.assertEqual(len(requests), 1)
        self.assertEqual(requests[0].get_header("Authorization"), f"Bearer {settings_token}")
        self.assertNotIn(settings_token, str(raised.exception))
        self.assertNotIn(content_token, str(raised.exception))

    def test_rejects_settings_endpoint_404_as_disabled(self) -> None:
        with (
            patch.dict(
                os.environ,
                {IMMUTABLE_SETTINGS_READ_TOKEN: "settings-read-secret"},
                clear=True,
            ),
            patch(
                "preflight_component_release.urllib.request.urlopen",
                side_effect=urllib.error.HTTPError(
                    "https://api.github.com/repos/DoHorizon-AI/Cyrene-Catalyst/immutable-releases",
                    404,
                    "Not Found",
                    {},
                    None,
                ),
            ),
            patch("preflight_component_release.subprocess.run") as run,
        ):
            with self.assertRaisesRegex(ReleasePreflightError, "immutable releases are disabled"):
                preflight(
                    repository=self.repository,
                    release_id=self.release_id,
                    channel="preview",
                    source_ref="refs/heads/develop",
                    source_commit=self.source_commit,
                    repository_root=self.repository_root,
                )

        run.assert_not_called()

    def test_rejects_a_channel_and_source_ref_mismatch_before_api_access(self) -> None:
        with patch("preflight_component_release._api_get") as api_get:
            with self.assertRaisesRegex(ReleasePreflightError, "not allowed"):
                preflight(
                    repository=self.repository,
                    release_id=f"stable-{self.source_commit}",
                    channel="stable",
                    source_ref="refs/heads/develop",
                    source_commit=self.source_commit,
                    repository_root=self.repository_root,
                )
        api_get.assert_not_called()


if __name__ == "__main__":
    unittest.main()
