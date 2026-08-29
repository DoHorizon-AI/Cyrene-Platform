"""CYRENE Plugin Protocol framing and codec (length-prefixed 4-byte big-endian)."""

import struct
from typing import Optional, Tuple
from cy_plugin_sdk.pb.proto.plugin.v1.plugin_protocol_pb2 import Envelope

DEFAULT_MAX_MESSAGE_BYTES = 64 * 1024 * 1024  # 64 MiB
CURRENT_PROTOCOL_VERSION = 1


class ProtocolError(Exception):
    """Protocol error for framing, size violation, or invalid payload."""
    pass


class FramedCodec:
    def __init__(self, max_message_bytes: int = DEFAULT_MAX_MESSAGE_BYTES):
        self.max_message_bytes = max_message_bytes

    def encode(self, envelope: Envelope) -> bytes:
        payload = envelope.SerializeToString()
        payload_len = len(payload)
        if payload_len > self.max_message_bytes:
            raise ProtocolError(
                f"Message length ({payload_len} bytes) exceeds maximum allowed ({self.max_message_bytes} bytes)"
            )
        header = struct.pack(">I", payload_len)
        return header + payload

    def decode(self, buf: bytes) -> Optional[Tuple[Envelope, int]]:
        if len(buf) < 4:
            return None

        (payload_len,) = struct.unpack(">I", buf[:4])
        if payload_len > self.max_message_bytes:
            raise ProtocolError(
                f"Message length ({payload_len} bytes) exceeds maximum allowed ({self.max_message_bytes} bytes)"
            )

        total_frame_len = 4 + payload_len
        if len(buf) < total_frame_len:
            return None

        payload_bytes = buf[4:total_frame_len]
        envelope = Envelope()
        try:
            envelope.ParseFromString(payload_bytes)
        except Exception as e:
            raise ProtocolError(f"Failed to decode Protobuf payload: {e}") from e

        return envelope, total_frame_len
