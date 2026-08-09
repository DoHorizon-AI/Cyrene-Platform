"""Cross-language plugin protocol framing and codec tests for Python SDK."""

import pytest
from cy_plugin_sdk.protocol import FramedCodec, ProtocolError
from cy_plugin_sdk.pb.proto.plugin.v1.plugin_protocol_pb2 import Envelope, Hello


def test_framed_codec_roundtrip():
    codec = FramedCodec()
    envelope = Envelope(
        request_id="req-py-100",
        trace_id="trace-py-200",
        plugin_id="com.cy.probe.nvidia",
        protocol_version=1,
        deadline_ms=5000,
        sequence_number=0,
    )
    envelope.hello.CopyFrom(
        Hello(
            min_protocol_version=1,
            max_protocol_version=1,
            host_version="1.0.0",
        )
    )

    encoded = codec.encode(envelope)
    assert len(encoded) > 4

    result = codec.decode(encoded)
    assert result is not None
    decoded_env, consumed = result
    assert consumed == len(encoded)
    assert decoded_env.request_id == "req-py-100"
    assert decoded_env.plugin_id == "com.cy.probe.nvidia"
    assert decoded_env.protocol_version == 1
    assert decoded_env.hello.host_version == "1.0.0"


def test_framed_codec_message_too_large():
    codec = FramedCodec(max_message_bytes=50)
    envelope = Envelope(
        request_id="x" * 100,
        trace_id="t",
        plugin_id="p",
        protocol_version=1,
    )
    with pytest.raises(ProtocolError, match="exceeds maximum allowed"):
        codec.encode(envelope)


def test_framed_codec_incomplete_buffer():
    codec = FramedCodec()
    envelope = Envelope(request_id="req-partial", plugin_id="test", protocol_version=1)
    encoded = codec.encode(envelope)

    # Cut off last 2 bytes
    partial = encoded[:-2]
    res = codec.decode(partial)
    assert res is None
