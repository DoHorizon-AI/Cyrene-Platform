from __future__ import annotations

import hashlib
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import build_v2_bundle


def git(path: Path, *args: str) -> str:
    result = subprocess.run(
        ["git", "-C", str(path), *args],
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    return result.stdout.strip()


class ProductV2BundleBuilderTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.repo = self.root / "Cyrene-Example"
        self.repo.mkdir()
        git(self.repo, "init", "-q")
        git(self.repo, "config", "user.email", "contract-test@example.invalid")
        git(self.repo, "config", "user.name", "Contract Test")
        git(self.repo, "remote", "add", "origin", "https://github.com/DoHorizon-AI/Cyrene-Example.git")
        catalog_path = self.repo / "contracts/product/v2/catalog.json"
        catalog_path.parent.mkdir(parents=True)
        catalog_path.write_text(json.dumps({
            "schemaVersion": "cyrene.product.operation-catalog.v2",
            "catalogVersion": "2.0.0",
            "ownerId": "example",
            "operations": [{
                "operationId": "readExample",
                "routeId": "readExample",
                "openapiPath": "contracts/product/v1/openapi.yaml",
                "kind": "READ",
                "requestSchemaPointer": None,
                "responseSchemaPointers": [],
                "resourceId": {
                    "required": False,
                    "pathParameter": None,
                    "minLength": 1,
                    "maxLength": 512,
                    "pattern": None,
                },
                "idempotency": {
                    "required": False,
                    "header": None,
                    "minLength": 1,
                    "maxLength": 200,
                },
                "scope": {
                    "organizationPathParameter": None,
                    "workspacePathParameter": None,
                    "requestBindings": [],
                    "responseBindings": [],
                },
            }],
        }, indent=2) + "\n")
        openapi = self.repo / "contracts/product/v1/openapi.yaml"
        openapi.parent.mkdir(parents=True, exist_ok=True)
        openapi.write_text(
            "openapi: 3.1.0\ninfo:\n  title: Example\n  version: 1.0.0\n"
            "paths: {}\ncomponents:\n  schemas:\n    Ref: {$ref: ./schema.json}\n"
        )
        (openapi.parent / "schema.json").write_text('{"type":"object"}\n')
        git(self.repo, "add", "contracts/product")
        git(self.repo, "commit", "-qm", "Product v2 fixture")
        self.commit = git(self.repo, "rev-parse", "HEAD")
        self.policy = self.root / "workspace-product-policy-v2.json"
        self.policy.write_text(json.dumps({
            "schemaVersion": build_v2_bundle.POLICY_SCHEMA_VERSION,
            "policyVersion": "2.0.0",
            "grants": [],
        }) + "\n")

    def tearDown(self) -> None:
        self.temp.cleanup()

    def test_build_pins_git_blobs_and_verifies_compatible_lock(self) -> None:
        out = self.root / "release-a"
        lock_path = self.root / "release.lock.json"
        lock = build_v2_bundle.build_release(
            out,
            {"example": self.repo},
            {"example": self.commit},
            self.policy,
            lock_output=lock_path,
        )
        self.assertEqual(lock["owners"][0]["sourceSha"], self.commit)
        manifest_raw = (out / build_v2_bundle.MANIFEST_NAME).read_bytes()
        self.assertEqual(hashlib.sha256(manifest_raw).hexdigest(), lock["bundle"]["manifestSha256"])
        manifest = json.loads(manifest_raw)
        self.assertEqual([item["path"] for item in manifest["files"]], sorted(item["path"] for item in manifest["files"]))
        build_v2_bundle.build_release(
            self.root / "release-b",
            {"example": self.repo},
            {"example": self.commit},
            self.policy,
            verify_lock=lock_path,
        )

    def test_only_source_commit_bytes_are_copied_even_if_checkout_is_dirty(self) -> None:
        original = (self.repo / "contracts/product/v1/schema.json").read_text()
        (self.repo / "contracts/product/v1/schema.json").write_text('{"type":"string"}\n')
        out = self.root / "release"
        build_v2_bundle.build_release(
            out,
            {"example": self.repo},
            {"example": self.commit},
            self.policy,
        )
        self.assertEqual((out / "Cyrene-Example/contracts/product/v1/schema.json").read_text(), original)

    def test_remote_reference_fails_closed(self) -> None:
        openapi = self.repo / "contracts/product/v1/openapi.yaml"
        openapi.write_text(openapi.read_text().replace("./schema.json", "https://example.invalid/schema.json"))
        git(self.repo, "add", "contracts/product/v1/openapi.yaml")
        git(self.repo, "commit", "-qm", "remote schema")
        self.commit = git(self.repo, "rev-parse", "HEAD")
        with self.assertRaisesRegex(build_v2_bundle.BundleError, "remote or absolute"):
            build_v2_bundle.build_release(
                self.root / "release",
                {"example": self.repo},
                {"example": self.commit},
                self.policy,
            )


if __name__ == "__main__":
    unittest.main()
