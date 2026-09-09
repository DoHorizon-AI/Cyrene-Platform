"""
┌─────────────────────────────────────────────────────────────────────┐
│  📄 publish_portable_directory.py                                    │
│  Module: cy-artifact-transfer test fixtures                          │
│  Role: Python-provider to neutral cross-SDK fixture adapter.         │
│                                                                     │
│  模块职责：调用 Python provider 并导出中性 Rust 测试 fixture。        │
└─────────────────────────────────────────────────────────────────────┘
"""

from __future__ import annotations

import json
import shutil
import sys
from pathlib import Path

from cy_artifacts import ArtifactKind, LocalArtifactProvider


def publish_fixture(export_root: Path) -> None:
    """Publish a real Python V2 directory and export a neutral test view.

    The provider's private manifest/blob paths are used only by this test
    adapter.  The Rust production API receives only the exported manifest
    bytes and a digest keyed byte source.

    使用真实 Python provider 发布 V2 目录；provider 私有路径只存在于测试
    adapter，Rust 生产 API 只接收 manifest bytes 和按 digest 的 byte source。
    """

    work_root = export_root.parent / "python-provider-work"
    source = work_root / "source"
    (source / "z").mkdir(parents=True)
    (source / "模型").mkdir(parents=True)
    (source / "weights.bin").write_bytes(b"abc")
    (source / "z" / "é.txt").write_bytes(b"\x00\xff")
    (source / "模型" / "空.txt").write_bytes(b"")

    provider = LocalArtifactProvider(work_root / "cas")
    artifact = provider.publish_portable_directory(source, kind=ArtifactKind("producer.bundle"))
    manifest_path = provider._manifest_path(artifact.digest)

    blobs_root = export_root / "blobs"
    blobs_root.mkdir(parents=True)
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    for entry in manifest["files"]:
        digest = str(entry["digest"])[len("sha256:") :]
        shutil.copyfile(provider._blob_path(entry["digest"]), blobs_root / digest)
    export_root.mkdir(exist_ok=True)
    (export_root / "manifest.json").write_bytes(manifest_path.read_bytes())
    (export_root / "artifact.json").write_text(
        json.dumps(artifact.to_dict(), ensure_ascii=False, sort_keys=True),
        encoding="utf-8",
    )


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("usage: publish_portable_directory.py EXPORT_ROOT")
    publish_fixture(Path(sys.argv[1]))
