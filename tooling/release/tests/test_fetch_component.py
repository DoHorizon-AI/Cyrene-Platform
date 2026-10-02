"""Check exact source-SHA release lookup without latest fallback.

确保精确 source SHA 查找不会退回 channel latest。
"""

from __future__ import annotations

import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch
import sys

RELEASE_DIR = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(RELEASE_DIR))

from component_artifacts import ComponentArtifactError
from fetch_component import _download_release_index


class ExactReleaseLookupTests(unittest.TestCase):
    """Keep event-triggered consumers on the release for their exact source."""

    publisher = "DoHorizon-AI/Cyrene-Exchange"
    source_commit = "a" * 40
    release_id = f"preview-{source_commit}"
    index_asset = "component-release-index-v1.json"
    index_uri = f"https://github.com/{publisher}/releases/download/{release_id}/{index_asset}"
    api_uri = f"https://api.github.com/repos/{publisher}/releases?per_page=100"
    exact_api_uri = f"https://api.github.com/repos/{publisher}/releases/tags/{release_id}"

    def setUp(self) -> None:
        self.catalog = {
            "channels": {
                "preview": {
                    "releasePrerelease": True,
                    "sourceRefs": ["refs/heads/develop"],
                }
            },
            "publishers": [
                {
                    "repository": self.publisher,
                    "releaseDiscovery": {
                        "apiUri": self.api_uri,
                        "indexAssetName": self.index_asset,
                    },
                }
            ],
        }
        self.release = {
            "tag_name": self.release_id,
            "draft": False,
            "immutable": True,
            "prerelease": True,
            "assets": [{"name": self.index_asset, "browser_download_url": self.index_uri}],
        }
        self.index = {
            "repository": self.publisher,
            "channel": "preview",
            "source": {"ref": "refs/heads/develop", "commit": self.source_commit},
            "provenance": {"attestation": {}},
        }

    def test_queries_exact_release_endpoint_and_verifies_that_index(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            destination = Path(directory) / self.index_asset
            with (
                patch("fetch_component._api_json", return_value=self.release) as api_json,
                patch("fetch_component._request", return_value=b"{}") as request,
                patch("fetch_component._read_object", return_value=self.index),
                patch("fetch_component.validate_document", return_value=[]),
                patch("fetch_component._validate_index", return_value=[]),
                patch("fetch_component._verify_index_metadata"),
                patch("fetch_component._verify_run"),
                patch("fetch_component._verify_attestation"),
            ):
                index, release, index_uri = _download_release_index(
                    Path(directory) / "component-catalog-v1.json",
                    self.catalog,
                    self.publisher,
                    "preview",
                    destination,
                    expected_source_commit=self.source_commit,
                )

        api_json.assert_called_once_with(self.exact_api_uri)
        request.assert_called_once_with(self.index_uri)
        self.assertEqual(index, self.index)
        self.assertEqual(release, self.release)
        self.assertEqual(index_uri, self.index_uri)

    def test_exact_lookup_does_not_fall_back_to_latest(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            destination = Path(directory) / self.index_asset
            with (
                patch(
                    "fetch_component._api_json",
                    side_effect=ComponentArtifactError("exact release not found"),
                ) as api_json,
                patch("fetch_component._request") as request,
            ):
                with self.assertRaisesRegex(ComponentArtifactError, "exact release not found"):
                    _download_release_index(
                        Path(directory) / "component-catalog-v1.json",
                        self.catalog,
                        self.publisher,
                        "preview",
                        destination,
                        expected_source_commit=self.source_commit,
                    )

        api_json.assert_called_once_with(self.exact_api_uri)
        request.assert_not_called()

    def test_rejects_index_for_a_different_commit_without_latest_fallback(self) -> None:
        wrong_index = {
            **self.index,
            "source": {"ref": "refs/heads/develop", "commit": "b" * 40},
        }
        with tempfile.TemporaryDirectory() as directory:
            destination = Path(directory) / self.index_asset
            with (
                patch("fetch_component._api_json", return_value=self.release) as api_json,
                patch("fetch_component._request", return_value=b"{}") as request,
                patch("fetch_component._read_object", return_value=wrong_index),
                patch("fetch_component.validate_document", return_value=[]),
                patch("fetch_component._verify_index_metadata") as verify_metadata,
            ):
                with self.assertRaisesRegex(ComponentArtifactError, "no trusted immutable preview"):
                    _download_release_index(
                        Path(directory) / "component-catalog-v1.json",
                        self.catalog,
                        self.publisher,
                        "preview",
                        destination,
                        expected_source_commit=self.source_commit,
                    )

        api_json.assert_called_once_with(self.exact_api_uri)
        request.assert_called_once_with(self.index_uri)
        verify_metadata.assert_not_called()

    def test_rejects_noncanonical_source_sha_before_network_lookup(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            with patch("fetch_component._api_json") as api_json:
                with self.assertRaisesRegex(ComponentArtifactError, "full lowercase Git SHA"):
                    _download_release_index(
                        Path(directory) / "component-catalog-v1.json",
                        self.catalog,
                        self.publisher,
                        "preview",
                        Path(directory) / self.index_asset,
                        expected_source_commit="A" * 40,
                    )

        api_json.assert_not_called()


if __name__ == "__main__":
    unittest.main()
