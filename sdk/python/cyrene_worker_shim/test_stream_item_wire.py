# ╔══════════════════════════════════════════════════════════════════════╗
# ║ 📄 File: sdk/python/cyrene_worker_shim/test_stream_item_wire.py       ║
# ║ Module: CYRENE Platform                                               ║
# ║ Role: Python-to-generated-Protobuf worker stream wire TCK.             ║
# ║                                                                      ║
# ║ 模块：CYRENE Platform                                                  ║
# ║ 职责：Python 与生成 Protobuf 之间的 Worker 流式 wire TCK。               ║
# ╚══════════════════════════════════════════════════════════════════════╝
"""Verify that the Python shim preserves typed stream oneof presence."""

from __future__ import annotations

import importlib
import sys
from pathlib import Path
from types import ModuleType

from grpc_tools import protoc

sys.path.insert(0, str(Path(__file__).parent))
from cyrene_worker import StreamItem


REPOSITORY_ROOT = Path(__file__).resolve().parents[3]
PROTO_ROOT = REPOSITORY_ROOT / "contracts" / "proto"
PLUGIN_PROTO_ROOT = PROTO_ROOT / "plugin" / "v1"


def _generated_plugin_protocol(tmp_path: Path) -> ModuleType:
    """Compile the checked-in contract into an isolated generated module."""
    proto_files = sorted(PLUGIN_PROTO_ROOT.glob("*.proto"))
    result = protoc.main(
        (
            "grpc_tools.protoc",
            f"-I{PROTO_ROOT}",
            f"--python_out={tmp_path}",
            *(str(path.relative_to(PROTO_ROOT)) for path in proto_files),
        )
    )
    assert result == 0, "checked-in plugin protocol must compile with grpcio-tools"
    sys.path.insert(0, str(tmp_path))
    try:
        return importlib.import_module("plugin.v1.plugin_protocol_pb2")
    finally:
        sys.path.pop(0)


def test_empty_binary_chunk_retains_generated_oneof_presence(tmp_path: Path) -> None:
    """An empty vLLM/provider chunk is still a typed binary stream item."""
    generated = _generated_plugin_protocol(tmp_path)
    wire = StreamItem(
        request_id="request-1",
        sequence_number=1,
        payload_type_url="type.googleapis.com/cyrene.StreamChunk",
        data_tag=11,
        data=b"",
    ).encode()

    shim_decoded = StreamItem.decode(wire)
    assert shim_decoded.data_tag == 11
    assert shim_decoded.data == b""

    decoded = generated.StreamItem.FromString(wire)

    assert decoded.request_id == "request-1"
    assert decoded.sequence_number == 1
    assert decoded.payload_type_url == "type.googleapis.com/cyrene.StreamChunk"
    assert decoded.WhichOneof("data") == "binary_chunk"
    assert decoded.binary_chunk == b""
