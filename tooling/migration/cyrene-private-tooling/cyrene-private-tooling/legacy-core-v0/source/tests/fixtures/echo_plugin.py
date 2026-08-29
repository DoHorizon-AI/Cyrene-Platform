#!/usr/bin/env python3
"""Minimal Python Echo Plugin fixture operating over stdio binary framing."""

import sys
import os

# Add the public Python plugin SDK to sys.path.
repo_root = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
sys.path.insert(
    0,
    os.path.join(repo_root, "framework", "sdk", "python", "cy-plugin-sdk", "src"),
)

from cy_plugin_sdk.protocol import FramedCodec
from cy_plugin_sdk.pb.proto.plugin.v1.plugin_protocol_pb2 import (
    Envelope,
    HelloAck,
    InvokeResult,
    StreamItem,
)
from cy_plugin_sdk.pb.proto.plugin.v1.probe_pb2 import DetectHardwareResponse


def main():
    # Write log line to stderr (MUST NOT affect stdout protocol)
    sys.stderr.write("[echo_plugin] Starting echo plugin daemon on stdio...\n")
    sys.stderr.flush()

    codec = FramedCodec()
    stdin_raw = sys.stdin.buffer

    buffer = bytearray()
    while True:
        try:
            chunk = stdin_raw.read1(4096)
        except AttributeError:
            chunk = stdin_raw.read(1)
        if not chunk:
            break
        buffer.extend(chunk)

        while True:
            decoded = codec.decode(bytes(buffer))
            if decoded is None:
                break
            req_env, consumed = decoded
            buffer = buffer[consumed:]

            # Log received request to stderr
            sys.stderr.write(f"[echo_plugin] Received request_id={req_env.request_id} payload={req_env.WhichOneof('payload')}\n")
            sys.stderr.flush()

            payload_type = req_env.WhichOneof("payload")

            if payload_type == "hello":
                resp_env = Envelope(
                    request_id=req_env.request_id,
                    trace_id=req_env.trace_id,
                    plugin_id="com.cy.probe.nvidia",
                    protocol_version=1,
                )
                resp_env.hello_ack.selected_protocol_version = 1
                resp_env.hello_ack.plugin_id = req_env.plugin_id if req_env.plugin_id else "com.cy.probe.nvidia"
                resp_env.hello_ack.plugin_version = "1.0.0"
                resp_env.hello_ack.api_version = "1.0"
                resp_env.hello_ack.declared_capabilities.extend(["nvidia", "cuda"])

                frame = codec.encode(resp_env)
                sys.stdout.buffer.write(frame)
                sys.stdout.buffer.flush()

            elif payload_type == "invoke":
                if req_env.invoke.method == "crash":
                    sys.stderr.write("[echo_plugin] Simulating crash exit(1)\n")
                    sys.stderr.flush()
                    sys.exit(1)

                hw_json = '{"os":{"name":"Linux","kernel":"6.5.0","glibc":"2.35"},"cpu":{"arch":"x86_64","cores":16},"memory_gb":64.0,"disk_gb":1000.0,"gpus":[{"model":"NVIDIA RTX 4090","count":1,"vram_gb":24.0,"compute_capability":"8.9"}],"driver_version":"535.129","cuda_max_supported":"12.2","interconnect":{"nvlink":false},"container_runtime":"docker","precision_support":{"bf16":true,"fp16":true,"fp8":true}}'

                resp_env = Envelope(
                    request_id=req_env.request_id,
                    trace_id=req_env.trace_id,
                    plugin_id="com.cy.probe.nvidia",
                    protocol_version=1,
                )
                resp_env.invoke_result.detect_hardware.hardware_manifest_json = hw_json
                frame = codec.encode(resp_env)
                sys.stdout.buffer.write(frame)
                sys.stdout.buffer.flush()

            elif payload_type == "shutdown":
                sys.stderr.write("[echo_plugin] Received shutdown, exiting.\n")
                sys.stderr.flush()
                return


if __name__ == "__main__":
    main()
