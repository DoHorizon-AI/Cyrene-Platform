# ╔══════════════════════════════════════════════════════════════════════╗
# ║ 📄 File: sdk/python/cyrene_worker_shim/test_runner.py
# ║ Module: CYRENE Platform
# ║ Role: Python SDK, TCK, or test module for this repository boundary.
# ║
# ║ 模块：CYRENE Platform
# ║ 职责：Python SDK、TCK 或测试模块。
# ╚══════════════════════════════════════════════════════════════════════╝
"""Tests for cyrene_worker_shim.runner GenericCapabilityWorker."""

from __future__ import annotations

import io
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))

from cyrene_worker import (
    ApplicationEvent,
    ApplicationEventStreamEnd,
    Cancel,
    CancelAck,
    Envelope,
    Hello,
    HelloAck,
    Invoke,
    InvokeResult,
    PluginErrorPayload,
    Shutdown,
    Subscribe,
    TypedCapabilityPayload,
    read_frame,
    run_worker_stream,
    write_frame,
)
from runner import GenericCapabilityWorker


class DummyService:
    def __init__(self):
        self.cancelled_ops = []

    def echo(self, req: dict, cancellation=None) -> dict:
        if cancellation and cancellation.is_cancelled():
            raise RuntimeError("CANCELLED: operation was cancelled")
        return {"echo": req.get("text", "")}

    def fail(self, req: dict, cancellation=None) -> dict:
        raise ValueError("INVALID_INPUT: invalid argument provided")


class EventService:
    def on_subscribe(self, subscription_id, capability, filter_payload, emitter):
        emitter.emit("synthetic", b"payload")
        emitter.complete("done")


class TypedService:
    def send(self, req: dict, cancellation=None) -> TypedCapabilityPayload:
        del req, cancellation
        return TypedCapabilityPayload(
            value=b"typed-response",
            type_url="type.googleapis.com/example.TypedResponse",
        )

    def on_subscribe(self, subscription_id, capability, filter_payload, emitter):
        del subscription_id, capability, filter_payload
        emitter.emit(
            "typed-event",
            b"typed-event-payload",
            "type.googleapis.com/example.TypedEvent",
        )
        emitter.complete("done")


class DeclaredRequestTypeUrlService:
    def __init__(self):
        self.request_type_url = None

    def on_invoke(self, capability, action, payload, request_type_url):
        del capability, action, payload
        self.request_type_url = request_type_url
        return True, request_type_url.encode("utf-8")


class LegacyOnInvokeService:
    def on_invoke(self, capability, action, payload):
        del capability, action
        return True, b"legacy:" + payload


class KwargsRequestTypeUrlService:
    def __init__(self):
        self.request_type_url = None

    def on_invoke(self, capability, action, payload, **kwargs):
        del capability, action, payload
        self.request_type_url = kwargs["request_type_url"]
        return True, self.request_type_url.encode("utf-8")


def test_generic_capability_worker_lifecycle():
    service = DummyService()
    worker = GenericCapabilityWorker(
        instance=service,
        plugin_id="com.cyrene.test.dummy",
        plugin_version="0.1.0",
        capabilities=["dummy.service.v1"],
    )

    inp = io.BytesIO()
    out = io.BytesIO()

    # 1. Hello
    e1 = Envelope(
        request_id="req-1",
        plugin_id="com.cyrene.test.dummy",
        sequence_number=1,
        generation=1,
        fence_token=1,
        payload=Hello(min_protocol_version=1, max_protocol_version=1),
    )
    write_frame(e1.encode(), inp)

    # 2. Invoke echo
    payload_bytes = json.dumps({"text": "hello cyrene"}).encode("utf-8")
    e2 = Envelope(
        request_id="req-2",
        plugin_id="com.cyrene.test.dummy",
        sequence_number=2,
        generation=1,
        fence_token=1,
        payload=Invoke(capability="dummy.service.v1", action="echo", payload=payload_bytes),
    )
    write_frame(e2.encode(), inp)

    # 3. Invoke fail (error reporting)
    e3 = Envelope(
        request_id="req-3",
        plugin_id="com.cyrene.test.dummy",
        sequence_number=3,
        generation=1,
        fence_token=1,
        payload=Invoke(capability="dummy.service.v1", action="fail", payload=b"{}"),
    )
    write_frame(e3.encode(), inp)

    # 4. Shutdown
    e4 = Envelope(
        request_id="req-4",
        plugin_id="com.cyrene.test.dummy",
        sequence_number=4,
        generation=1,
        fence_token=1,
        payload=Shutdown(grace_period_ms=500),
    )
    write_frame(e4.encode(), inp)

    inp.seek(0)
    run_worker_stream(inp, out, worker)

    out.seek(0)

    # 1. HelloAck
    f1 = read_frame(out)
    assert f1 is not None
    r1 = Envelope.decode(f1)
    assert r1.request_id == "req-1"
    assert isinstance(r1.payload, HelloAck)
    assert r1.payload.plugin_id == "com.cyrene.test.dummy"
    assert r1.payload.declared_capabilities == ["dummy.service.v1"]

    # 2. Echo result
    f2 = read_frame(out)
    assert f2 is not None
    r2 = Envelope.decode(f2)
    assert r2.request_id == "req-2"
    assert isinstance(r2.payload, InvokeResult)
    data = json.loads(r2.payload.payload.decode("utf-8"))
    assert data == {"echo": "hello cyrene"}

    # 3. Fail result (PluginErrorPayload)
    f3 = read_frame(out)
    assert f3 is not None
    r3 = Envelope.decode(f3)
    assert r3.request_id == "req-3"
    assert isinstance(r3.payload, PluginErrorPayload)
    assert "INVALID_INPUT" in r3.payload.message
    assert r3.payload.code == 3

    # 4. Shutdown ACK
    f4 = read_frame(out)
    assert f4 is not None
    r4 = Envelope.decode(f4)
    assert r4.request_id == "req-4"
    assert r4.payload.message == "Shutdown ACK"

    print("test_generic_capability_worker_lifecycle PASSED!")


def test_generic_capability_worker_application_events():
    worker = GenericCapabilityWorker(
        instance=EventService(),
        plugin_id="com.cyrene.test.events",
        capabilities=["test.application-events.v1"],
    )
    inp = io.BytesIO()
    out = io.BytesIO()

    write_frame(
        Envelope(
            request_id="hello",
            plugin_id="com.cyrene.test.events",
            sequence_number=1,
            generation=1,
            fence_token=1,
            payload=Hello(min_protocol_version=1, max_protocol_version=1),
        ).encode(),
        inp,
    )
    write_frame(
        Envelope(
            request_id="sub-1",
            plugin_id="com.cyrene.test.events",
            sequence_number=2,
            generation=1,
            fence_token=1,
            payload=Subscribe(
                capability="test.application-events.v1",
                max_buffered_events=2,
            ),
        ).encode(),
        inp,
    )
    write_frame(
        Envelope(
            request_id="shutdown",
            plugin_id="com.cyrene.test.events",
            sequence_number=3,
            generation=1,
            fence_token=1,
            payload=Shutdown(grace_period_ms=500),
        ).encode(),
        inp,
    )

    inp.seek(0)
    run_worker_stream(inp, out, worker)

    out.seek(0)
    frames = []
    while True:
        frame = read_frame(out)
        if frame is None:
            break
        frames.append(Envelope.decode(frame))

    assert len(frames) == 5
    assert any(
        envelope.request_id == "sub-1" and isinstance(envelope.payload, ApplicationEvent)
        for envelope in frames
    )
    assert any(
        envelope.request_id == "sub-1"
        and isinstance(envelope.payload, ApplicationEventStreamEnd)
        for envelope in frames
    )
    assert any(envelope.request_id == "shutdown" for envelope in frames)
    print("test_generic_capability_worker_application_events PASSED!")


def test_typed_capability_payload_metadata_round_trip():
    worker = GenericCapabilityWorker(
        instance=TypedService(),
        plugin_id="com.cyrene.test.typed",
        capabilities=["example.typed.v1"],
    )
    inp = io.BytesIO()
    out = io.BytesIO()
    write_frame(
        Envelope(
            request_id="hello",
            plugin_id="com.cyrene.test.typed",
            sequence_number=1,
            generation=1,
            fence_token=1,
            payload=Hello(min_protocol_version=1, max_protocol_version=1),
        ).encode(),
        inp,
    )
    write_frame(
        Envelope(
            request_id="invoke",
            plugin_id="com.cyrene.test.typed",
            sequence_number=2,
            generation=1,
            fence_token=1,
            payload=Invoke(
                capability="example.typed.v1",
                action="send",
                payload=b"{}",
            ),
        ).encode(),
        inp,
    )
    write_frame(
        Envelope(
            request_id="subscribe",
            plugin_id="com.cyrene.test.typed",
            sequence_number=3,
            generation=1,
            fence_token=1,
            payload=Subscribe(
                capability="example.typed.v1",
                max_buffered_events=2,
            ),
        ).encode(),
        inp,
    )
    write_frame(
        Envelope(
            request_id="shutdown",
            plugin_id="com.cyrene.test.typed",
            sequence_number=4,
            generation=1,
            fence_token=1,
            payload=Shutdown(grace_period_ms=500),
        ).encode(),
        inp,
    )

    inp.seek(0)
    run_worker_stream(inp, out, worker)
    out.seek(0)
    frames = []
    while True:
        frame = read_frame(out)
        if frame is None:
            break
        frames.append(Envelope.decode(frame))

    invoke = next(
        envelope.payload
        for envelope in frames
        if envelope.request_id == "invoke"
    )
    assert isinstance(invoke, InvokeResult)
    assert invoke.payload == b"typed-response"
    assert invoke.payload_type_url == "type.googleapis.com/example.TypedResponse"

    event = next(
        envelope.payload
        for envelope in frames
        if isinstance(envelope.payload, ApplicationEvent)
    )
    assert event.payload == b"typed-event-payload"
    assert event.payload_type_url == "type.googleapis.com/example.TypedEvent"
    print("test_typed_capability_payload_metadata_round_trip PASSED!")


def test_request_type_url_handler_compatibility():
    request_type_url = "type.googleapis.com/example.Request"
    wire_invoke = Invoke(
        capability="example.request.v1",
        action="invoke",
        payload=b"request",
        payload_type_url=request_type_url,
    )
    decoded_invoke = Invoke.decode(wire_invoke.encode())
    assert decoded_invoke.payload == b"request"
    assert decoded_invoke.payload_type_url == request_type_url

    declared_service = DeclaredRequestTypeUrlService()
    declared_worker = GenericCapabilityWorker(
        instance=declared_service,
        plugin_id="com.cyrene.test.request-type-url",
        capabilities=["example.request.v1"],
    )
    ok, payload = declared_worker._invoke_request(
        "request-1",
        "example.request.v1",
        "invoke",
        decoded_invoke.payload,
        decoded_invoke.payload_type_url,
    )
    assert ok is True
    assert payload == request_type_url.encode("utf-8")
    assert declared_service.request_type_url == request_type_url

    kwargs_service = KwargsRequestTypeUrlService()
    kwargs_worker = GenericCapabilityWorker(
        instance=kwargs_service,
        plugin_id="com.cyrene.test.request-type-url-kwargs",
        capabilities=["example.request.v1"],
    )
    ok, payload = kwargs_worker._invoke_request(
        "request-2",
        "example.request.v1",
        "invoke",
        b"request",
        request_type_url,
    )
    assert ok is True
    assert payload == request_type_url.encode("utf-8")
    assert kwargs_service.request_type_url == request_type_url

    legacy_worker = GenericCapabilityWorker(
        instance=LegacyOnInvokeService(),
        plugin_id="com.cyrene.test.request-type-url-legacy",
        capabilities=["example.request.v1"],
    )
    ok, payload = legacy_worker._invoke_request(
        "request-3",
        "example.request.v1",
        "invoke",
        b"request",
        request_type_url,
    )
    assert ok is True
    assert payload == b"legacy:request"
    print("test_request_type_url_handler_compatibility PASSED!")


if __name__ == "__main__":
    test_generic_capability_worker_lifecycle()
    test_generic_capability_worker_application_events()
    test_typed_capability_payload_metadata_round_trip()
    test_request_type_url_handler_compatibility()
