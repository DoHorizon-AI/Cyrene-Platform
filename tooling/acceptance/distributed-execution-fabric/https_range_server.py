#!/usr/bin/env python3
"""HTTPS Range fixture with deliberate throttling for interruption proofs.

HTTPS Range 测试服务：通过限速制造可重复的传输中断与恢复窗口。
"""

from __future__ import annotations

import argparse
import http.server
import pathlib
import ssl
import threading
import time


class RangeHandler(http.server.BaseHTTPRequestHandler):
    artifact: pathlib.Path
    trace: pathlib.Path
    delay_seconds: float
    ticket_signature_file: pathlib.Path
    trace_lock = threading.Lock()

    def do_GET(self) -> None:  # noqa: N802 - stdlib callback name | 标准库回调名称约定。
        if self.path != "/artifact.bin":
            self.send_error(404)
            return
        try:
            ticket_signature = self.ticket_signature_file.read_text(encoding="utf-8").strip()
        except OSError:
            ticket_signature = ""
        if not ticket_signature or self.headers.get("Authorization") != f"Bearer {ticket_signature}":
            self._trace("TICKET_REJECTED")
            self.send_error(403)
            return
        header = self.headers.get("Range", "")
        if not header.startswith("bytes=") or "," in header:
            self.send_error(416)
            return
        size = self.artifact.stat().st_size
        try:
            start_text, end_text = header[6:].split("-", 1)
            start = int(start_text)
            end = size - 1 if not end_text else min(int(end_text), size - 1)
        except ValueError:
            self.send_error(416)
            return
        if start < 0 or start > end:
            self.send_error(416)
            return

        self.send_response(206)
        self.send_header("Accept-Ranges", "bytes")
        self.send_header("Content-Range", f"bytes {start}-{end}/{size}")
        self.send_header("Content-Length", str(end - start + 1))
        self.end_headers()
        self._trace(f"RANGE_STARTED start={start} end={end}")
        remaining = end - start + 1
        try:
            with self.artifact.open("rb") as source:
                source.seek(start)
                while remaining:
                    chunk = source.read(min(65_536, remaining))
                    if not chunk:
                        break
                    self.wfile.write(chunk)
                    self.wfile.flush()
                    remaining -= len(chunk)
                    time.sleep(self.delay_seconds)
            self._trace(f"RANGE_COMPLETE start={start} end={end}")
        except (BrokenPipeError, ConnectionResetError, ssl.SSLError):
            self._trace(f"RANGE_INTERRUPTED start={start} end={end}")

    def log_message(self, _format: str, *_args: object) -> None:
        return

    def _trace(self, line: str) -> None:
        with self.trace_lock:
            with self.trace.open("a", encoding="utf-8") as output:
                output.write(f"{line}\n")
                output.flush()


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--bind", default="0.0.0.0")
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--artifact", type=pathlib.Path, required=True)
    parser.add_argument("--certificate", required=True)
    parser.add_argument("--key", required=True)
    parser.add_argument("--trace", type=pathlib.Path, required=True)
    parser.add_argument("--delay-ms", type=float, default=20.0)
    parser.add_argument("--ticket-signature-file", type=pathlib.Path, required=True)
    args = parser.parse_args()
    RangeHandler.artifact = args.artifact
    RangeHandler.trace = args.trace
    RangeHandler.delay_seconds = args.delay_ms / 1000.0
    RangeHandler.ticket_signature_file = args.ticket_signature_file
    server = http.server.ThreadingHTTPServer((args.bind, args.port), RangeHandler)
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(args.certificate, args.key)
    server.socket = context.wrap_socket(server.socket, server_side=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
