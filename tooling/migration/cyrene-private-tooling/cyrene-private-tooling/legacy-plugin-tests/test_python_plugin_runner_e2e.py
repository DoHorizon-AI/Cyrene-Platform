"""E2E Integration test for Rust PluginSupervisor driving Python PluginRunner with community plugins."""

import sys
import subprocess
import os
from pathlib import Path
from cy_plugin_sdk.protocol import FramedCodec
from cy_plugin_sdk.pb.proto.plugin.v1.plugin_protocol_pb2 import (
    Envelope,
    Hello,
    Invoke,
)
from cy_plugin_sdk.pb.proto.plugin.v1.probe_pb2 import DetectHardwareRequest

REPO_ROOT = Path(__file__).resolve().parent.parent
MANIFEST_PATH = REPO_ROOT / "python" / "plugins" / "community" / "nvidia_probe" / "plugin.toml"


def test_python_plugin_runner_nvidia_probe():
    cmd = [
        sys.executable,
        "-m",
        "cy_plugin_sdk.runner",
        str(MANIFEST_PATH),
    ]

    proc = subprocess.Popen(
        cmd,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        cwd=str(REPO_ROOT),
    )

    codec = FramedCodec()

    # 1. Send Hello
    hello_env = Envelope(
        request_id="req-e2e-hello",
        trace_id="trace-1",
        plugin_id="com.cy.probe.nvidia",
        protocol_version=1,
    )
    hello_env.hello.CopyFrom(
        Hello(
            min_protocol_version=1,
            max_protocol_version=1,
            host_version="1.0.0",
        )
    )

    proc.stdin.write(codec.encode(hello_env))
    proc.stdin.flush()

    # 2. Read HelloAck
    raw_header = proc.stdout.read(4)
    assert len(raw_header) == 4
    payload_len = int.from_bytes(raw_header, "big")
    raw_payload = proc.stdout.read(payload_len)
    ack_env, _ = codec.decode(raw_header + raw_payload)

    assert ack_env.request_id == "req-e2e-hello"
    assert ack_env.hello_ack.plugin_id == "com.cy.probe.nvidia"
    assert ack_env.hello_ack.api_version == "1.0"

    # 3. Send Invoke (detect_hardware)
    invoke_env = Envelope(
        request_id="req-e2e-invoke",
        trace_id="trace-1",
        plugin_id="com.cy.probe.nvidia",
        protocol_version=1,
    )
    invoke_env.invoke.CopyFrom(
        Invoke(
            extension_point="probe",
            method="detect_hardware",
            detect_hardware=DetectHardwareRequest(),
        )
    )

    proc.stdin.write(codec.encode(invoke_env))
    proc.stdin.flush()

    # 4. Read InvokeResult
    raw_header = proc.stdout.read(4)
    assert len(raw_header) == 4
    payload_len = int.from_bytes(raw_header, "big")
    raw_payload = proc.stdout.read(payload_len)
    result_env, _ = codec.decode(raw_header + raw_payload)

    assert result_env.request_id == "req-e2e-invoke"
    if result_env.WhichOneof("payload") == "error":
        print("Received error payload:", result_env.error.message)
    hw_json = result_env.invoke_result.detect_hardware.hardware_manifest_json
    if not hw_json:
        proc.terminate()
        stderr_out = proc.stderr.read().decode("utf-8", errors="replace")
        print("Child stderr:\n", stderr_out)
    assert len(hw_json) > 0

    # Clean shutdown
    proc.terminate()
