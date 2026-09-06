#!/usr/bin/env python3
"""Deterministic generic capability worker for Platform TCK."""

import json
import os
import struct
import sys
import threading
import time
from pathlib import Path
from typing import Dict, List

# Add cyrene_worker_shim to sys.path
manifest_dir = Path(__file__).resolve().parents[4]
shim_dir = manifest_dir / "sdk/python/cyrene_worker_shim"
sys.path.insert(0, str(shim_dir))

try:
    from cyrene_worker_shim.cyrene_worker import (
        CyreneWorker,
        PluginErrorPayload,
        TYPED_INVOCATION_STREAM_FEATURE,
        TypedCapabilityPayload,
        TypedCapabilityStream,
        decode_varint,
        encode_bytes_field,
        encode_string_field,
        encode_uint32_field,
        run_worker_stdio,
    )
except ImportError:
    from cyrene_worker import (
        CyreneWorker,
        PluginErrorPayload,
        TYPED_INVOCATION_STREAM_FEATURE,
        TypedCapabilityPayload,
        TypedCapabilityStream,
        decode_varint,
        encode_bytes_field,
        encode_string_field,
        encode_uint32_field,
        run_worker_stdio,
    )


def decode_string_fields(payload: bytes) -> Dict[int, List[str]]:
    """Read the length-delimited string fields of a request payload.

    The TCK fixture stays dependency-free, so this does the minimum protobuf
    scan needed to observe ``EmbeddingsRequest.model`` instead of relying on a
    generated decoder.
    """
    fields: Dict[int, List[str]] = {}
    offset = 0
    while offset < len(payload):
        tag, offset = decode_varint(payload, offset)
        field_number, wire_type = tag >> 3, tag & 0x07
        if wire_type != 2:
            raise ValueError(f"unsupported wire type {wire_type}")
        length, offset = decode_varint(payload, offset)
        value = payload[offset : offset + length]
        offset += length
        fields.setdefault(field_number, []).append(value.decode("utf-8"))
    return fields


EMBEDDINGS_RESPONSE_TYPE_URL = "type.googleapis.com/cyrene.model.provider.v1.EmbeddingsResponse"


def embedding_response(instance_id: str) -> bytes:
    """Encode a deterministic valid response without adding generated fixtures."""
    vector = encode_bytes_field(1, struct.pack("<fff", 1.0, 2.0, 3.0))
    batch = (
        encode_bytes_field(1, vector)
        + encode_uint32_field(2, 3)
        + encode_string_field(3, instance_id or "default-model")
    )
    return encode_bytes_field(1, batch)


class GenericTckWorker(CyreneWorker):
    def __init__(self):
        self.cancelled_requests = set()
        self.instance_id = os.environ.get("CYRENE_TEST_INSTANCE_ID", "")
        self.embeddings_supported = os.environ.get("CYRENE_TEST_EMBEDDINGS_SUPPORTED", "1") != "0"

    def operation_default_models(self) -> Dict[str, str]:
        """The single authority for per-operation default models.

        One binding may serve both ``chat_completion`` and ``embeddings`` with
        a different default per operation. Every default resolution goes
        through this table, so an operation can never inherit another
        operation's default. Unset in the harness, the binding identity is the
        default, which preserves the pre-existing TCK expectations.
        """
        fallback = self.instance_id or "default-model"
        return {
            "chat_completion": os.environ.get("CYRENE_TEST_CHAT_DEFAULT_MODEL") or fallback,
            "embeddings": os.environ.get("CYRENE_TEST_EMBEDDINGS_DEFAULT_MODEL") or fallback,
        }

    def resolve_model(self, operation: str, payload: bytes) -> str:
        """Resolve the model for one operation: explicit selector wins."""
        if payload:
            requested = decode_string_fields(payload).get(2)
            if requested:
                return requested[-1]
        return self.operation_default_models()[operation]

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
        return [
            "test.capability.v1",
            "test.application-events.v1",
            "model.provider.v1",
        ]

    def protocol_features(self):
        if os.environ.get("CYRENE_TEST_DISABLE_TYPED_STREAM") == "1":
            return []
        return [TYPED_INVOCATION_STREAM_FEATURE]

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
                    if not emitter.emit("synthetic", self.scoped_payload(str(value).encode("ascii"))):
                        return
                emitter.complete("ordered events complete")
            elif mode == "burst":
                for value in range(1, 128):
                    if not emitter.emit("synthetic", self.scoped_payload(str(value).encode("ascii"))):
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

    def on_invoke(
        self,
        capability: str,
        action: str,
        payload: bytes,
        request_type_url: str = "",
        stream_results: bool = False,
    ):
        if action == "request_type_url":
            return True, request_type_url.encode("utf-8")

        if capability == "model.provider.v1":
            if action == "embeddings":
                if not self.embeddings_supported:
                    return False, PluginErrorPayload(
                        code=3,
                        message="embedding method is not supported by this binding",
                        details="UNKNOWN_OPERATION",
                    )
                return True, TypedCapabilityPayload(
                    value=embedding_response(self.resolve_model("embeddings", payload)),
                    type_url=EMBEDDINGS_RESPONSE_TYPE_URL,
                )
            if action == "chat_completion":
                return True, json.dumps({"model": self.operation_default_models()["chat_completion"]}).encode("utf-8")

        if capability == "test.capability.v1" and action == "stream":
            if not stream_results:
                return True, b"legacy-unary-result"
            return True, TypedCapabilityStream(
                iter(
                    (
                        TypedCapabilityPayload(
                            value=b"first",
                            type_url="type.googleapis.com/test.Chunk",
                        ),
                        TypedCapabilityPayload(
                            value=b"second",
                            type_url="type.googleapis.com/test.Chunk",
                        ),
                    )
                )
            )

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

        elif action == "read_env":
            key = req.get("key")
            if not isinstance(key, str):
                return False, b"INVALID_INPUT: key must be a string"
            return True, json.dumps({"value": os.environ.get(key)}).encode("utf-8")

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
