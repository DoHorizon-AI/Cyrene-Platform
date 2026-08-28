#!/usr/bin/env python3
"""Deterministic generic capability worker for Platform TCK."""

import json
import sys
import time
from pathlib import Path

# Add cyrene_worker_shim to sys.path
manifest_dir = Path(__file__).resolve().parents[4]
shim_dir = manifest_dir / "sdk/python/cyrene_worker_shim"
sys.path.insert(0, str(shim_dir))

from cyrene_worker import CyreneWorker, run_worker_stdio


class GenericTckWorker(CyreneWorker):
    def __init__(self):
        self.cancelled_requests = set()

    def plugin_id(self) -> str:
        return "com.cyrene.tck.generic-worker"

    def plugin_version(self) -> str:
        return "1.0.0"

    def api_version(self) -> str:
        return "1.0"

    def declared_capabilities(self):
        return ["test.capability.v1"]

    def on_cancel(self, target_request_id: str, reason: str) -> None:
        self.cancelled_requests.add(target_request_id)

    def on_invoke(self, capability: str, action: str, payload: bytes):
        try:
            req = json.loads(payload.decode("utf-8")) if payload else {}
        except Exception as e:
            return False, b"INVALID_INPUT: malformed json payload"

        if action == "echo":
            return True, json.dumps({"echo": req.get("message", "")}).encode("utf-8")

        elif action == "compute":
            val = req.get("value")
            if val is None or not isinstance(val, int) or val < 0:
                return False, b"INVALID_INPUT: value must be a non-negative integer"
            return True, json.dumps({"result": val * 2}).encode("utf-8")

        elif action == "slow_operation":
            delay = req.get("delay_ms", 500) / 1000.0
            time.sleep(delay)
            return True, json.dumps({"status": "completed"}).encode("utf-8")

        elif action == "fail":
            return False, b"EXECUTION_FAILED: internal error in capability"

        elif action == "crash":
            # Force abrupt child exit to test crash detection
            sys.exit(42)

        else:
            return False, f"INVALID_INPUT: unknown operation {action}".encode("utf-8")


if __name__ == "__main__":
    run_worker_stdio(GenericTckWorker())
