from __future__ import annotations

import hashlib
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import build_bundle


REPOSITORIES = sorted(build_bundle.EXPECTED_REPOSITORIES)


def run_git(path: Path, *args: str) -> str:
    result = subprocess.run(
        ["git", "-C", str(path), *args],
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    return result.stdout.strip()


class BundleBuilderTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.platform = self.root / "platform"
        self.platform.mkdir()
        run_git(self.platform, "init", "-q")
        run_git(self.platform, "config", "user.email", "bundle-test@example.invalid")
        run_git(self.platform, "config", "user.name", "Bundle Test")
        tck_path = self.platform / build_bundle.TCK_PATH
        tck_path.parent.mkdir(parents=True)
        lines = ["owner\twire_operation\tproduct_operation_id\tkind\tproduct_contract"]
        for index, name in enumerate(REPOSITORIES, start=1):
            owner = name.removeprefix("Cyrene-").upper()
            spec = f"{name}/contracts/product/v1/openapi.yaml"
            lines.append(f"{owner}\tOP_{index:02}\toperation{index}\tREAD\t{spec}")
        tck_path.write_text("\n".join(lines) + "\n", encoding="utf-8")
        run_git(self.platform, "add", build_bundle.TCK_PATH)
        run_git(self.platform, "commit", "-qm", "fixture TCK")
        self.sources: dict[str, Path] = {}
        self.commits: dict[str, str] = {}
        for name in REPOSITORIES:
            repo = self.root / name
            repo.mkdir()
            run_git(repo, "init", "-q")
            run_git(repo, "config", "user.email", "bundle-test@example.invalid")
            run_git(repo, "config", "user.name", "Bundle Test")
            run_git(repo, "remote", "add", "origin", build_bundle.EXPECTED_REPOSITORIES[name])
            base = repo / "contracts/product/v1"
            base.mkdir(parents=True)
            (base / "openapi.yaml").write_text(
                "openapi: 3.1.2\ninfo:\n  title: fixture\n  version: 1.0.0\npaths: {}\ncomponents:\n  schemas:\n    Fixture:\n      $ref: ./schema.json\n",
                encoding="utf-8",
            )
            (base / "schema.json").write_text('{"type":"object"}\n', encoding="utf-8")
            run_git(repo, "add", "contracts/product/v1")
            run_git(repo, "commit", "-qm", "fixture contract")
            self.sources[name] = repo
            self.commits[name] = run_git(repo, "rev-parse", "HEAD")

    def tearDown(self) -> None:
        self.temp.cleanup()

    def build(self, output_name: str = "bundle") -> dict[str, object]:
        return build_bundle.build_bundle(
            self.root / output_name,
            self.sources,
            self.commits,
            platform_root=self.platform,
        )

    def test_build_is_deterministic_and_contains_full_inventory(self) -> None:
        first = self.build("bundle-a")
        second = self.build("bundle-b")
        self.assertEqual(first, second)
        self.assertEqual(len(first["repositories"]), 6)
        self.assertEqual(len(first["files"]), 12)
        for item in first["files"]:
            path = self.root / "bundle-a" / item["path"]
            self.assertEqual(hashlib.sha256(path.read_bytes()).hexdigest(), item["sha256"])
        self.assertEqual(
            json.loads((self.root / "bundle-a/product-contract-bundle.json").read_text()), first
        )

    def test_dirty_checkout_is_rejected(self) -> None:
        spec = self.sources[REPOSITORIES[0]] / "contracts/product/v1/openapi.yaml"
        spec.write_text(spec.read_text() + "# modified\n", encoding="utf-8")
        with self.assertRaisesRegex(build_bundle.BundleError, "dirty"):
            self.build()

    def test_pinned_commit_must_match_checkout_head(self) -> None:
        name = REPOSITORIES[0]
        self.commits[name] = "0" * 40
        with self.assertRaisesRegex(build_bundle.BundleError, "unavailable|does not match"):
            self.build()

    def test_remote_reference_is_rejected(self) -> None:
        name = REPOSITORIES[0]
        spec = self.sources[name] / "contracts/product/v1/openapi.yaml"
        spec.write_text(spec.read_text().replace("./schema.json", "https://example.invalid/schema.json"), encoding="utf-8")
        run_git(self.sources[name], "add", "contracts/product/v1/openapi.yaml")
        run_git(self.sources[name], "commit", "-qm", "remote ref")
        self.commits[name] = run_git(self.sources[name], "rev-parse", "HEAD")
        with self.assertRaisesRegex(build_bundle.BundleError, "remote"):
            self.build()

    def test_reference_escape_is_rejected(self) -> None:
        name = REPOSITORIES[0]
        spec = self.sources[name] / "contracts/product/v1/openapi.yaml"
        spec.write_text(spec.read_text().replace("./schema.json", "../../../secret.yaml"), encoding="utf-8")
        run_git(self.sources[name], "add", "contracts/product/v1/openapi.yaml")
        run_git(self.sources[name], "commit", "-qm", "escaping ref")
        self.commits[name] = run_git(self.sources[name], "rev-parse", "HEAD")
        with self.assertRaisesRegex(build_bundle.BundleError, "leaves the Product v1 contract root"):
            self.build()

    def test_missing_reference_is_rejected(self) -> None:
        name = REPOSITORIES[0]
        spec = self.sources[name] / "contracts/product/v1/openapi.yaml"
        spec.write_text(spec.read_text().replace("./schema.json", "./missing.json"), encoding="utf-8")
        run_git(self.sources[name], "add", "contracts/product/v1/openapi.yaml")
        run_git(self.sources[name], "commit", "-qm", "missing ref")
        self.commits[name] = run_git(self.sources[name], "rev-parse", "HEAD")
        with self.assertRaisesRegex(build_bundle.BundleError, "absent from pinned commit"):
            self.build()

    def test_symlink_reference_is_rejected(self) -> None:
        name = REPOSITORIES[0]
        repo = self.sources[name]
        schema = repo / "contracts/product/v1/schema.json"
        schema.unlink()
        schema.symlink_to("openapi.yaml")
        run_git(repo, "add", "-A", "contracts/product/v1")
        run_git(repo, "commit", "-qm", "symlink ref")
        self.commits[name] = run_git(repo, "rev-parse", "HEAD")
        with self.assertRaisesRegex(build_bundle.BundleError, "symlink or non-regular"):
            self.build()

    def test_openapi_version_mismatch_is_rejected(self) -> None:
        name = REPOSITORIES[0]
        spec = self.sources[name] / "contracts/product/v1/openapi.yaml"
        spec.write_text(spec.read_text().replace("3.1.2", "3.0.3"), encoding="utf-8")
        run_git(self.sources[name], "add", "contracts/product/v1/openapi.yaml")
        run_git(self.sources[name], "commit", "-qm", "version mismatch")
        self.commits[name] = run_git(self.sources[name], "rev-parse", "HEAD")
        with self.assertRaisesRegex(build_bundle.BundleError, "expected OpenAPI"):
            self.build()

    def test_duplicate_yaml_keys_are_rejected(self) -> None:
        name = REPOSITORIES[0]
        spec = self.sources[name] / "contracts/product/v1/openapi.yaml"
        spec.write_text("openapi: 3.1.2\ninfo:\n  version: 1.0.0\n  version: 1.0.0\n", encoding="utf-8")
        run_git(self.sources[name], "add", "contracts/product/v1/openapi.yaml")
        run_git(self.sources[name], "commit", "-qm", "duplicate key")
        self.commits[name] = run_git(self.sources[name], "rev-parse", "HEAD")
        with self.assertRaisesRegex(build_bundle.BundleError, "duplicate YAML mapping key"):
            self.build()


if __name__ == "__main__":
    unittest.main()
