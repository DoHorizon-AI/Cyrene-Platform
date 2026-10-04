"""Keep Product release outputs aligned with the pinned static catalog.

确保 Product 发布目标与固定的组件目录完全一致。
"""

from __future__ import annotations

import hashlib
import json
import os
import re
import sys
import tempfile
import unittest
from unittest.mock import patch
from pathlib import Path
from typing import Any

from build_product_service_release import (
    NATIVE_PYTHON_TARGETS,
    SERVICE_COMPONENTS,
    _native_python_profile,
)
from component_artifacts import ComponentArtifactError, _component_target
from prepare_product_component import IMAGE_PRODUCTS, PRODUCTS


FALLBACK_CATALOG_SHA256 = "f12f5cd1243d16b6ec6a5194efacbdf7a8bf9dffcf8d9525c35183c45a7c7816"
CATALOG_PATH = Path("workspace-catalog/governance/component-catalog-v1.json")
PYTHON_TARGET_IDS = (
    "linux-ubuntu-22.04-x86_64-python-3.12",
    "linux-ubuntu-24.04-x86_64-python-3.12",
)
WINDOWS_DOCKER_TARGET_ID = "windows-10.0-x86_64-docker-linux"
PRODUCT_IDS = {f"cyrene-{product}" for product in PRODUCTS}


class ProductPublicationMatrixTests(unittest.TestCase):
    """Assert that builders publish only catalog-supported Product targets."""

    @classmethod
    def setUpClass(cls) -> None:
        catalog_path = Path(os.environ.get("CYRENE_COMPONENT_CATALOG", CATALOG_PATH))
        raw = catalog_path.read_bytes()
        expected_sha256 = os.environ.get("CYRENE_COMPONENT_CATALOG_SHA256", FALLBACK_CATALOG_SHA256)
        if re.fullmatch(r"[0-9a-f]{64}", expected_sha256) is None:
            raise AssertionError("Product matrix test requires a lowercase SHA-256 catalog digest")
        if hashlib.sha256(raw).hexdigest() != expected_sha256:
            raise AssertionError("Product matrix catalog bytes do not match the expected SHA-256")
        cls.catalog: dict[str, Any] = json.loads(raw.decode("utf-8", "strict"))
        cls.targets = {row["id"]: row["target"] for row in cls.catalog["targets"]}
        cls.components = {row["componentId"]: row for row in cls.catalog["components"]}

    def test_product_owner_catalogs_cover_all_six_products(self) -> None:
        self.assertEqual(
            PRODUCT_IDS,
            {"cyrene-catalyst", "cyrene-yield", "cyrene-reactor", "cyrene-exchange", "cyrene-echo", "cyrene-navigator"},
        )
        self.assertTrue(PRODUCT_IDS.issubset(self.components))

    def test_linux_python_bundles_match_the_five_supported_services_on_both_ubuntu_profiles(self) -> None:
        expected_services = {"catalyst", "yield", "reactor", "exchange", "navigator"}
        self.assertEqual(set(SERVICE_COMPONENTS), expected_services)
        self.assertNotIn("echo", SERVICE_COMPONENTS)
        self.assertEqual(set(NATIVE_PYTHON_TARGETS), set(PYTHON_TARGET_IDS))

        for target_id in PYTHON_TARGET_IDS:
            target = self.targets[target_id]
            for service, component_id in SERVICE_COMPONENTS.items():
                with self.subTest(target_id=target_id, service=service):
                    _component_target(self.catalog, component_id, {"kind": "python-bundle"}, target)

            with self.subTest(target_id=target_id, service="echo"):
                with self.assertRaisesRegex(ComponentArtifactError, "target is not authorized"):
                    _component_target(
                        self.catalog,
                        "cyrene-echo",
                        {"kind": "python-bundle"},
                        target,
                    )

    def test_native_python_profile_ids_match_the_exact_ubuntu_glibc_targets(self) -> None:
        expected = {
            "linux-ubuntu-22.04-x86_64-python-3.12": {
                "osVersion": "22.04",
                "distributionVersion": "22.04",
                "abi": "glibc-2.35",
            },
            "linux-ubuntu-24.04-x86_64-python-3.12": {
                "osVersion": "24.04",
                "distributionVersion": "24.04",
                "abi": "glibc-2.39",
            },
        }
        self.assertEqual(NATIVE_PYTHON_TARGETS, expected)
        for target_id, identity in expected.items():
            with self.subTest(target_id=target_id):
                target = self.targets[target_id]
                self.assertEqual(target.get("os"), "linux")
                self.assertEqual(target.get("distribution"), "ubuntu")
                self.assertEqual(target.get("architecture"), "x86_64")
                self.assertEqual(target.get("runtime"), "python:3.12")
                self.assertEqual({key: target.get(key) for key in identity}, identity)

    def test_native_python_build_requires_matching_locked_host_and_interpreter(self) -> None:
        version = "3.12.14"
        for target_id, identity in NATIVE_PYTHON_TARGETS.items():
            target = self.targets[target_id]
            profile = {
                key: target[key]
                for key in (
                    "os",
                    "osVersion",
                    "distribution",
                    "distributionVersion",
                    "architecture",
                    "abi",
                    "runtime",
                )
            }
            profile["pythonVersion"] = version
            profile["pythonExecutable"] = "/opt/cyrene/python/3.12.14/bin/python3.12"
            profile["pythonInput"] = "packaging/python-runtime.lock.json"
            profile["wheelResolver"] = {"version": "0.12.21"}
            with self.subTest(target_id=target_id), tempfile.TemporaryDirectory() as directory:
                workspace_root = Path(directory)
                (workspace_root / "release-lock.json").write_text(
                    json.dumps({"nativePythonProfiles": {target_id: profile}}),
                    encoding="utf-8",
                )
                with (
                    patch(
                        "build_product_service_release.platform.freedesktop_os_release",
                        return_value={"ID": "ubuntu", "VERSION_ID": identity["osVersion"]},
                    ),
                    patch("build_product_service_release.platform.machine", return_value="x86_64"),
                    patch(
                        "build_product_service_release.platform.libc_ver",
                        return_value=("glibc", identity["abi"].removeprefix("glibc-")),
                    ),
                ):
                    with patch(
                        "build_product_service_release._run",
                        side_effect=[version, "uv 0.12.21"],
                    ):
                        selected_target, selected_python, selected_uv = _native_python_profile(
                            workspace_root,
                            self.catalog,
                            "cyrene-catalyst",
                            target_id,
                            Path(sys.executable),
                            Path(sys.executable),
                        )
                self.assertEqual(selected_target, target)
                self.assertEqual(selected_python, Path(sys.executable).resolve())
                self.assertEqual(selected_uv, Path(sys.executable).resolve())

    def test_windows_docker_images_match_the_five_supported_images(self) -> None:
        expected_images = {"catalyst", "yield", "reactor", "exchange", "echo"}
        self.assertEqual(IMAGE_PRODUCTS, expected_images)
        self.assertNotIn("navigator", IMAGE_PRODUCTS)
        target = self.targets[WINDOWS_DOCKER_TARGET_ID]

        for product in sorted(IMAGE_PRODUCTS):
            with self.subTest(product=product):
                component = self.components[f"cyrene-{product}"]
                artifact = {
                    "kind": "oci-image",
                    "repository": component["ociImageRepository"],
                }
                _component_target(self.catalog, f"cyrene-{product}", artifact, target)

        with self.assertRaisesRegex(ComponentArtifactError, "artifact kind is not allowed"):
            _component_target(
                self.catalog,
                "cyrene-navigator",
                {"kind": "oci-image", "repository": "ghcr.io/dohorizon-ai/cyrene-navigator"},
                target,
            )


if __name__ == "__main__":
    unittest.main()
