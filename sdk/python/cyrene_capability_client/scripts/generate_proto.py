#!/usr/bin/env python3
"""
Generate or verify the Python projection of the canonical CES protobuf.

生成或校验 canonical CES protobuf 的 Python 投影。
"""

from __future__ import annotations

import argparse
import filecmp
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

import grpc_tools


PACKAGE_ROOT = Path(__file__).resolve().parents[1]
PLATFORM_ROOT = Path(__file__).resolve().parents[4]
CAPABILITY_PROTO = PLATFORM_ROOT / "contracts/proto/cyrene/capability/v1/capability_execution.proto"
MODEL_PROVIDER_PROTO = PLATFORM_ROOT / "contracts/proto/cyrene/model/provider/v1/model_provider.proto"
OUTPUT = PACKAGE_ROOT / "src/cyrene_capability_client/_generated"
GENERATED_FILES = (
    "capability_execution_pb2.py",
    "capability_execution_pb2_grpc.py",
    "model_provider_pb2.py",
)


def _run_protoc(proto: Path, destination: Path, *, grpc_service: bool) -> None:
    include = Path(grpc_tools.__file__).resolve().parent / "_proto"
    outputs = [f"--python_out={destination}"]
    if grpc_service:
        outputs.append(f"--grpc_python_out={destination}")
    completed = subprocess.run(
        [
            sys.executable,
            "-m",
            "grpc_tools.protoc",
            f"-I{proto.parent}",
            f"-I{include}",
            *outputs,
            proto.name,
        ],
        cwd=proto.parent,
        check=False,
    )
    if completed.returncode != 0:
        raise SystemExit(completed.returncode)


def _generate(destination: Path) -> None:
    _run_protoc(CAPABILITY_PROTO, destination, grpc_service=True)
    _run_protoc(MODEL_PROVIDER_PROTO, destination, grpc_service=False)

    grpc_projection = destination / "capability_execution_pb2_grpc.py"
    source = grpc_projection.read_text(encoding="utf-8")
    source = source.replace(
        "import capability_execution_pb2 as capability__execution__pb2",
        "from . import capability_execution_pb2 as capability__execution__pb2",
    )
    grpc_projection.write_text(source, encoding="utf-8")


def _check(generated: Path) -> bool:
    stale = [
        name
        for name in GENERATED_FILES
        if not (OUTPUT / name).is_file() or not filecmp.cmp(generated / name, OUTPUT / name, shallow=False)
    ]
    if stale:
        print(
            "stale generated CES Python projection: " + ", ".join(stale),
            file=sys.stderr,
        )
        return False
    print("CES Python protobuf projection is current")
    return True


def main() -> int:
    parser = argparse.ArgumentParser()
    action = parser.add_mutually_exclusive_group(required=True)
    action.add_argument("--check", action="store_true")
    action.add_argument("--write", action="store_true")
    args = parser.parse_args()

    with tempfile.TemporaryDirectory(prefix="cyrene-ces-python-") as temporary:
        generated = Path(temporary)
        _generate(generated)
        if args.check:
            return 0 if _check(generated) else 1
        OUTPUT.mkdir(parents=True, exist_ok=True)
        for name in GENERATED_FILES:
            shutil.copyfile(generated / name, OUTPUT / name)
    print("generated CES Python protobuf projection")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
