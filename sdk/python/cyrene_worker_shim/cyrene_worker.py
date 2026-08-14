#!/usr/bin/env python3
"""Lightweight CYRENE Out-of-Process Python Worker Shim.

This shim handles 4-byte big-endian length-prefixed framing over standard I/O (stdin/stdout).
Standard output (stdout) is STRICTLY reserved for protocol binary frames.
All logs, debug messages, and diagnostics MUST be routed to standard error (stderr).
"""

from __future__ import annotations

import io
import json
import struct
import sys
from typing import BinaryIO, Dict, List, Optional, Tuple


DEFAULT_MAX_MESSAGE_BYTES = 64 * 1024 * 1024  # 64 MiB
CURRENT_PROTOCOL_VERSION = 1


class CyreneWorker:
    """Base class for CYRENE out-of-process Python workers."""

    def plugin_id(self) -> str:
        raise NotImplementedError

    def plugin_version(self) -> str:
        return "1.0.0"

    def api_version(self) -> str:
        return "1.0"

    def declared_capabilities(self) -> List[str]:
        return []

    def metrics(self) -> Dict[str, str]:
        return {}

    def capabilities_json(self) -> str:
        return "{}"

    def on_configure(self, settings: Dict[str, str]) -> Optional[str]:
        """Return None on success or an error message string on failure."""
        return None

    def on_health_check(self) -> Tuple[int, str]:
        """Return (status_code, message) where 0=HEALTHY, 1=DEGRADED, 2=UNHEALTHY."""
        return 0, "OK"

    def on_invoke(self, payload: bytes) -> Tuple[bool, bytes]:
        """Return (success, response_payload_bytes)."""
        return False, b"on_invoke not implemented"

    def on_cancel(self, target_request_id: str, reason: str) -> None:
        pass

    def on_shutdown(self, grace_period_ms: int) -> None:
        pass


def read_frame(stream: Optional[BinaryIO] = None, max_bytes: int = DEFAULT_MAX_MESSAGE_BYTES) -> Optional[bytes]:
    if stream is None:
        stream = sys.stdin.buffer

    len_bytes = stream.read(4)
    if not len_bytes or len(len_bytes) < 4:
        return None

    (payload_len,) = struct.unpack(">I", len_bytes)
    if payload_len > max_bytes:
        raise ValueError(f"Frame length {payload_len} exceeds maximum allowed {max_bytes}")

    payload = stream.read(payload_len)
    if len(payload) < payload_len:
        raise IOError(f"Unexpected EOF reading payload: expected {payload_len}, got {len(payload)}")

    return payload


def write_frame(payload: bytes, stream: Optional[BinaryIO] = None, max_bytes: int = DEFAULT_MAX_MESSAGE_BYTES) -> None:
    if stream is None:
        stream = sys.stdout.buffer

    payload_len = len(payload)
    if payload_len > max_bytes:
        raise ValueError(f"Payload length {payload_len} exceeds maximum allowed {max_bytes}")

    len_bytes = struct.pack(">I", payload_len)
    stream.write(len_bytes)
    stream.write(payload)
    stream.flush()


def log(msg: str) -> None:
    """Write log messages safely to stderr."""
    sys.stderr.write(f"[CYRENE-WORKER] {msg}\n")
    sys.stderr.flush()


def run_worker_stream(
    reader: BinaryIO,
    writer: BinaryIO,
    worker: CyreneWorker,
    max_frame_bytes: int = DEFAULT_MAX_MESSAGE_BYTES,
) -> None:
    """Run the worker loop over generic binary I/O streams."""
    while True:
        frame = read_frame(reader, max_frame_bytes)
        if frame is None:
            break

        # In pure-python standard wire dispatch:
        # If payload is JSON/structured or raw bytes, dispatch lifecycle hooks
        # For shutdown signal, acknowledge and terminate
        try:
            # Check for JSON envelope test mode or raw binary handshake
            if frame.startswith(b"{") and frame.endswith(b"}"):
                req = json.loads(frame.decode("utf-8"))
                req_type = req.get("type", "")
                req_id = req.get("request_id", "")
                if req_type == "Hello":
                    resp = {
                        "request_id": req_id,
                        "type": "HelloAck",
                        "plugin_id": worker.plugin_id(),
                        "plugin_version": worker.plugin_version(),
                        "api_version": worker.api_version(),
                        "declared_capabilities": worker.declared_capabilities(),
                        "metrics": worker.metrics(),
                    }
                    write_frame(json.dumps(resp).encode("utf-8"), writer, max_frame_bytes)
                elif req_type == "HealthCheck":
                    code, msg = worker.on_health_check()
                    resp = {
                        "request_id": req_id,
                        "type": "HealthStatus",
                        "status": code,
                        "message": msg,
                    }
                    write_frame(json.dumps(resp).encode("utf-8"), writer, max_frame_bytes)
                elif req_type == "Shutdown":
                    worker.on_shutdown(req.get("grace_period_ms", 1000))
                    resp = {
                        "request_id": req_id,
                        "type": "HealthStatus",
                        "status": 0,
                        "message": "Shutdown ACK",
                    }
                    write_frame(json.dumps(resp).encode("utf-8"), writer, max_frame_bytes)
                    break
            else:
                # Raw binary frame fallback
                write_frame(b"ACK", writer, max_frame_bytes)
        except Exception as e:
            log(f"Error handling frame: {e}")


def run_worker_stdio(worker: CyreneWorker) -> None:
    """Run worker on standard input/output streams."""
    run_worker_stream(sys.stdin.buffer, sys.stdout.buffer, worker)
