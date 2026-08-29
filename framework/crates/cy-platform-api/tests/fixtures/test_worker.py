#!/usr/bin/env python3
"""Deterministic generic capability worker for Platform TCK."""

import json
import os
import sys
import threading
import time
from pathlib import Path

# Add cyrene_worker_shim to sys.path
manifest_dir = Path(__file__).resolve().parents[4]
shim_dir = manifest_dir / "sdk/python/cyrene_worker_shim"
sys.path.insert(0, str(shim_dir))

try:
    from cyrene_worker_shim.cyrene_worker import CyreneWorker, run_worker_stdio
except ImportError:
    from cyrene_worker import CyreneWorker, run_worker_stdio


class GenericTckWorker(CyreneWorker):
    def __init__(self):
        self.cancelled_requests = set()
        self.instance_id = os.environ.get("CYRENE_TEST_INSTANCE_ID", "")

    def scoped_payload(self, value: bytes) -> bytes:
        if not self.instance_id:
            return value
        return self.instance_id.encode("utf-8") + b":" + value

    def plugin_id(self) -> str:
        return "com.cyrene.tck.generic-worker"

    def plugin_version(self) -> str:
        return "1.0.0"

    def api_version(self) -> str:
        return "1.0"

    def declared_capabilities(self):
        return ["test.capability.v1", "test.application-events.v1"]

    def on_subscribe(self, subscription_id: str, capability: str, filter_payload: bytes):
        if capability != "test.application-events.v1":
            return f"unsupported capability {capability}"

        try:
            request = json.loads(filter_payload.decode("utf-8")) if filter_payload else {}
        except Exception:
            return "INVALID_INPUT: malformed subscription payload"

        mode = request.get("mode", "single")
        emitter = self.application_event_emitter(subscription_id)

        def emit_events():
            if mode == "single":
                emitter.emit("synthetic", self.scoped_payload(b"one"))
                emitter.complete("single event complete")
            elif mode == "ordered":
                for value in range(1, 4):
                    if not emitter.emit(
                        "synthetic", self.scoped_payload(str(value).encode("ascii"))
                    ):
                        return
                emitter.complete("ordered events complete")
            elif mode == "burst":
                for value in range(1, 128):
                    if not emitter.emit(
                        "synthetic", self.scoped_payload(str(value).encode("ascii"))
                    ):
                        return
                emitter.complete("burst complete")
            elif mode == "slow":
                time.sleep(0.15)
                if emitter.emit("synthetic", self.scoped_payload(b"slow")):
                    emitter.complete("slow event complete")
            elif mode == "generation":
                emitter.terminate(5, "synthetic generation changed")
            elif mode == "hold":
                return
            else:
                emitter.terminate(4, f"unknown synthetic mode {mode}")

        threading.Thread(
            target=emit_events,
            name=f"synthetic-events-{subscription_id}",
            daemon=True,
        ).start()
        return None

    def on_unsubscribe(self, subscription_id: str, reason: str) -> None:
        # The canonical runner owns terminal delivery; this hook only records
        # the lifecycle callback for the synthetic worker.
        self.cancelled_requests.add(subscription_id)

    def on_cancel(self, target_request_id: str, reason: str) -> None:
        self.cancelled_requests.add(target_request_id)

    def on_invoke(self, capability: str, action: str, payload: bytes):
        try:
            req = json.loads(payload.decode("utf-8")) if payload else {}
        except Exception as e:
            return False, b"INVALID_INPUT: malformed json payload"

        if action == "echo":
            response = {"echo": req.get("message", "")}
            if self.instance_id:
                response["instance_id"] = self.instance_id
            return True, json.dumps(response).encode("utf-8")

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
