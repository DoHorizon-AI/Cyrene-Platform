#!/usr/bin/env python3
"""Unified Protobuf generation and check script for the CYRENE Plugin Protocol."""

import argparse
import sys
import subprocess
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
PROTO_DIR = REPO_ROOT / "contracts" / "proto" / "plugin" / "v1"
PYTHON_OUT_DIR = REPO_ROOT / "framework" / "sdk" / "python" / "cy-plugin-sdk" / "src" / "cy_plugin_sdk" / "pb"


def generate_proto():
    PYTHON_OUT_DIR.mkdir(parents=True, exist_ok=True)

    proto_files = list(PROTO_DIR.glob("*.proto"))
    if not proto_files:
        print("No .proto files found in", PROTO_DIR)
        sys.exit(1)

    cmd = [
        sys.executable,
        "-m",
        "grpc_tools.protoc",
        f"-I{REPO_ROOT}",
        f"--python_out={PYTHON_OUT_DIR}",
    ] + [str(p) for p in proto_files]

    # Try running via python -m grpc_tools.protoc
    try:
        res = subprocess.run(cmd, capture_output=True, text=True, check=True)
        print("Successfully generated Python Protobuf bindings.")
    except subprocess.CalledProcessError as e:
        print("protoc stdout:", e.stdout)
        print("protoc stderr:", e.stderr)
        raise e

    # Fix package imports in generated python protobuf files
    (PYTHON_OUT_DIR / "__init__.py").write_text("# Generated CYRENE Plugin Protocol Protobuf bindings\n", encoding="utf-8")

    generated_dir = PYTHON_OUT_DIR / "proto" / "plugin" / "v1"
    if generated_dir.exists():
        for py_file in generated_dir.glob("*_pb2.py"):
            content = py_file.read_text(encoding="utf-8")
            fixed_content = content.replace("from proto.plugin.v1", "from cy_plugin_sdk.pb.proto.plugin.v1")
            py_file.write_text(fixed_content, encoding="utf-8")


def check_proto():
    # Helper to verify if generated code is up-to-date
    print("Checking Protobuf generation status...")
    target = PYTHON_OUT_DIR / "proto" / "plugin" / "v1" / "plugin_protocol_pb2.py"
    if not target.exists():
        print(f"ERROR: Missing expected generated file {target}")
        sys.exit(1)
    print("Protobuf generation check OK.")


def main():
    parser = argparse.ArgumentParser(description="CYRENE Plugin Protocol Code Generator")
    parser.add_argument("--check", action="store_true", help="Check if generated code is up to date")
    args = parser.parse_args()

    if args.check:
        check_proto()
    else:
        generate_proto()


if __name__ == "__main__":
    main()
