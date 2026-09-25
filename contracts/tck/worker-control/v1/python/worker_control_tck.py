#!/usr/bin/env python3
# ╔══════════════════════════════════════════════════════════════════════╗
# ║ 📄 File: contracts/tck/worker-control/v1/python/worker_control_tck.py
# ║ Module: CYRENE Platform
# ║ Role: Python SDK, TCK, or test module for this repository boundary.
# ║ 中文:Python SDK、TCK 或用于此仓库边界的测试模块。
# ║
# ║ 模块：CYRENE Platform
# ║ 职责：Python SDK、TCK 或测试模块。
# ╚══════════════════════════════════════════════════════════════════════╝
"""Dependency-free Core v1 Worker-control conformance runner for Python.

中文:适用于 Python 的无依赖 Core v1 Worker-control 一致性运行器。
"""

from __future__ import annotations

import hashlib
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def rows(path: Path):
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if line and not line.startswith("#"):
            yield line.split("|")


def decode_varint(data: bytes, offset: int) -> tuple[int, int]:
    value = 0
    shift = 0
    while offset < len(data):
        byte = data[offset]
        offset += 1
        value |= (byte & 0x7F) << shift
        if byte < 0x80:
            return value, offset
        shift += 7
    raise AssertionError("truncated protobuf varint")


def verify_vectors() -> None:
    for name, _direction, field, wire_hex, expected_sha in rows(ROOT / "vectors.tsv"):
        wire = bytes.fromhex(wire_hex)
        assert wire_hex == wire_hex.lower(), f"{name}: hex must be lower-case"
        assert hashlib.sha256(wire).hexdigest().upper() == expected_sha, f"{name}: digest"
        tag, offset = decode_varint(wire, 0)
        assert tag >> 3 == int(field) and tag & 7 == 2, f"{name}: oneof envelope"
        payload_len, offset = decode_varint(wire, offset)
        assert offset + payload_len == len(wire), f"{name}: bounded payload"


def expected_trace_result(trace: str) -> str:
    state = "AWAIT_HELLO"
    generation = 0
    next_heartbeat = 1
    shutdown_id = ""
    for raw in trace.split(";"):
        direction, kind, raw_generation, raw_sequence, identifier = raw.split(":", 4)
        frame_generation, sequence = int(raw_generation), int(raw_sequence)
        if state == "AWAIT_HELLO":
            if (direction, kind) != ("W2K", "HELLO") or frame_generation == 0:
                return "HELLO_REQUIRED"
            generation, state = frame_generation, "AWAIT_WELCOME"
        elif state == "AWAIT_WELCOME":
            if (direction, kind) != ("K2W", "WELCOME") or frame_generation != generation:
                return "WELCOME_REQUIRED"
            state = "RUNNING"
        elif state == "RUNNING":
            if (direction, kind) == ("W2K", "HEARTBEAT"):
                if frame_generation != generation or sequence != next_heartbeat:
                    return "HEARTBEAT_SEQUENCE_INVALID"
                next_heartbeat += 1
            elif (direction, kind) == ("K2W", "HEARTBEAT_ACK"):
                if frame_generation != generation or sequence >= next_heartbeat:
                    return "HEARTBEAT_ACK_INVALID"
            elif (direction, kind) == ("K2W", "SHUTDOWN") and identifier:
                shutdown_id, state = identifier, "AWAIT_SHUTDOWN_ACK"
            else:
                return "FRAME_INVALID"
        elif state == "AWAIT_SHUTDOWN_ACK":
            if (direction, kind) != ("W2K", "SHUTDOWN_ACK") or frame_generation != generation:
                return "SHUTDOWN_ACK_INVALID"
            if identifier != shutdown_id:
                return "SHUTDOWN_ID_MISMATCH"
            state = "STOPPED"
        else:
            return "FRAME_AFTER_STOP"
    return "ACCEPT" if state == "STOPPED" else "TRACE_INCOMPLETE"


def verify_scenarios() -> None:
    for name, expected, trace in rows(ROOT / "scenarios.tsv"):
        actual = expected_trace_result(trace)
        assert actual == expected, f"{name}: expected {expected}, got {actual}"


def expected_semantic_trace_result(trace: str) -> str:
    state = "AWAIT_HELLO"
    worker_generation = lease_generation = fence = 0
    shutdown_id = ""
    for raw in trace.split(";"):
        direction, kind, raw_worker, raw_lease, raw_fence, identifier = raw.split(":", 5)
        current = (int(raw_worker), int(raw_lease), int(raw_fence))
        if state == "AWAIT_HELLO":
            if (direction, kind) != ("W2K", "HELLO") or 0 in current:
                return "HELLO_REQUIRED"
            worker_generation, lease_generation, fence = current
            state = "AWAIT_WELCOME"
        elif state == "AWAIT_WELCOME":
            if (direction, kind) != ("K2W", "WELCOME"):
                return "WELCOME_REQUIRED"
            if current != (worker_generation, lease_generation, fence):
                return "FENCE_MISMATCH"
            state = "RUNNING"
        elif state == "RUNNING":
            if current != (worker_generation, lease_generation, fence):
                return "FENCE_MISMATCH"
            if (direction, kind) == ("W2K", "HEARTBEAT"):
                continue
            if (direction, kind) == ("K2W", "HEARTBEAT_ACK"):
                continue
            if (direction, kind) == ("K2W", "SHUTDOWN") and identifier:
                shutdown_id, state = identifier, "AWAIT_SHUTDOWN_ACK"
            else:
                return "FRAME_INVALID"
        elif state == "AWAIT_SHUTDOWN_ACK":
            if current != (worker_generation, lease_generation, fence):
                return "FENCE_MISMATCH"
            if (direction, kind) != ("W2K", "SHUTDOWN_ACK"):
                return "SHUTDOWN_ACK_INVALID"
            if identifier != shutdown_id:
                return "SHUTDOWN_ID_MISMATCH"
            state = "STOPPED"
        else:
            return "FRAME_AFTER_STOP"
    return "ACCEPT" if state == "STOPPED" else "TRACE_INCOMPLETE"


def verify_semantic_scenarios() -> None:
    for name, expected, trace in rows(ROOT / "semantic_scenarios.tsv"):
        actual = expected_semantic_trace_result(trace)
        assert actual == expected, f"{name}: expected {expected}, got {actual}"


if __name__ == "__main__":
    verify_vectors()
    verify_scenarios()
    verify_semantic_scenarios()
    print("Python Worker-control TCK v1 passed")
