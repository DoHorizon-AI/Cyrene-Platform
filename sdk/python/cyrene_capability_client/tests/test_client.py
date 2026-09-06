"""
Contract and lifecycle tests for the canonical CES Python client.

canonical CES Python 客户端的契约与生命周期测试。
"""

from __future__ import annotations

import time
from concurrent.futures import ThreadPoolExecutor
from threading import Event

import grpc
import pytest
from google.protobuf.any_pb2 import Any as AnyMessage

from cyrene_capability_client import (
    CapabilityDeadlineExceeded,
    CapabilityExecutionClient,
    CapabilityExecutionFailure,
    CapabilityInvocationCancelled,
    CapabilityProtocolError,
    TypedPayload,
)
from cyrene_capability_client._generated import (
    capability_execution_pb2 as execution_pb2,
)
from cyrene_capability_client._generated import (
    capability_execution_pb2_grpc as execution_pb2_grpc,
)
from cyrene_capability_client.model_provider_v1 import (
    CHAT_COMPLETION_REQUEST_TYPE_URL,
    CHAT_COMPLETION_RESPONSE_TYPE_URL,
    ChatCompletionChunk,
    ChatCompletionRequest,
    ChatCompletionResponse,
    ChatMessage,
    pack_chat_request,
    unpack_chat_response,
)


REQUEST_TYPE = "type.googleapis.com/example.Request"
RESPONSE_TYPE = "type.googleapis.com/example.Response"


class RecordingService(execution_pb2_grpc.CapabilityExecutionServiceServicer):
    """Small in-process CES peer used only to verify the generated client."""

    def __init__(self) -> None:
        self.requests: list[execution_pb2.InvokeCapabilityRequest] = []
        self.started = Event()
        self.behavior = "success"

    def InvokeCapability(self, request, context):  # noqa: N802 - generated gRPC API
        self.requests.append(request)
        self.started.set()
        if self.behavior == "wait":
            while context.is_active():
                time.sleep(0.005)
            return execution_pb2.InvokeCapabilityResponse()
        if self.behavior == "error":
            return execution_pb2.InvokeCapabilityResponse(
                error=execution_pb2.CapabilityExecutionError(
                    code=execution_pb2.CapabilityExecutionError.CODE_EXECUTION_FAILURE,
                    message="worker rejected request",
                )
            )
        if self.behavior == "cancelled-error":
            return execution_pb2.InvokeCapabilityResponse(
                error=execution_pb2.CapabilityExecutionError(
                    code=execution_pb2.CapabilityExecutionError.CODE_CANCELLED,
                    message="worker invocation cancelled",
                )
            )
        if self.behavior == "empty":
            return execution_pb2.InvokeCapabilityResponse()
        return execution_pb2.InvokeCapabilityResponse(response=AnyMessage(type_url=RESPONSE_TYPE, value=b"response"))

    def InvokeCapabilityStream(self, request, context):  # noqa: N802 - generated gRPC API
        self.requests.append(request)
        self.started.set()
        if self.behavior == "stream-activation-error":
            yield execution_pb2.CapabilityInvocationStreamItem(
                sequence=0,
                error=execution_pb2.CapabilityExecutionError(
                    code=execution_pb2.CapabilityExecutionError.CODE_ACTIVATION_FAILED,
                    message="worker activation failed",
                ),
            )
            return
        yield execution_pb2.CapabilityInvocationStreamItem(
            sequence=1,
            response=AnyMessage(type_url=RESPONSE_TYPE, value=b"first"),
        )
        if self.behavior == "stream-eof":
            return
        yield execution_pb2.CapabilityInvocationStreamItem(
            sequence=2,
            stream_end=execution_pb2.CapabilityInvocationStreamEnd(
                reason=execution_pb2.CapabilityInvocationStreamEnd.REASON_NORMAL_COMPLETION,
                message="done",
            ),
        )


@pytest.fixture
def live_client():
    service = RecordingService()
    server = grpc.server(ThreadPoolExecutor(max_workers=4))
    execution_pb2_grpc.add_CapabilityExecutionServiceServicer_to_server(service, server)
    port = server.add_insecure_port("127.0.0.1:0")
    server.start()
    client = CapabilityExecutionClient.for_local_endpoint(f"127.0.0.1:{port}")
    try:
        yield client, service
    finally:
        client.close()
        server.stop(grace=0).wait(timeout=2)


def invoke(client: CapabilityExecutionClient, **overrides: object) -> TypedPayload:
    arguments: dict[str, object] = {
        "capability": "example.v1",
        "interface_version": "1",
        "method": "run",
        "request": TypedPayload(REQUEST_TYPE, b"request"),
        "binding_id": "binding-main",
        "deadline_seconds": 2.0,
    }
    arguments.update(overrides)
    return client.invoke(**arguments)


def test_typed_payload_and_binding_round_trip(live_client):
    client, service = live_client

    result = invoke(client)

    assert result == TypedPayload(RESPONSE_TYPE, b"response")
    assert len(service.requests) == 1
    request = service.requests[0]
    assert request.capability == "example.v1"
    assert request.interface_version == "1"
    assert request.method == "run"
    assert request.binding_id == "binding-main"
    assert request.request.type_url == REQUEST_TYPE
    assert request.request.value == b"request"


def test_typed_execution_error_is_not_transport_error(live_client):
    client, service = live_client
    service.behavior = "error"

    with pytest.raises(CapabilityExecutionFailure) as captured:
        invoke(client)

    assert captured.value.code == execution_pb2.CapabilityExecutionError.CODE_EXECUTION_FAILURE
    assert captured.value.message == "worker rejected request"
    assert not captured.value.is_cancelled


def test_typed_ces_cancellation_is_distinguishable_from_provider_failure(live_client):
    client, service = live_client
    service.behavior = "cancelled-error"

    with pytest.raises(CapabilityExecutionFailure) as captured:
        invoke(client)

    assert captured.value.is_cancelled


def test_caller_event_cancels_native_grpc_future(live_client):
    client, service = live_client
    service.behavior = "wait"
    cancellation = Event()

    with ThreadPoolExecutor(max_workers=1) as executor:
        pending = executor.submit(invoke, client, cancel_event=cancellation)
        assert service.started.wait(timeout=1)
        cancellation.set()
        with pytest.raises(CapabilityInvocationCancelled):
            pending.result(timeout=2)


def test_native_deadline_maps_without_typed_ces_error(live_client):
    client, service = live_client
    service.behavior = "wait"

    with pytest.raises(CapabilityDeadlineExceeded):
        invoke(client, deadline_seconds=0.05)


def test_missing_result_is_protocol_error(live_client):
    client, service = live_client
    service.behavior = "empty"

    with pytest.raises(CapabilityProtocolError):
        invoke(client)


def test_typed_stream_forwards_chunks_and_terminal_marker(live_client):
    client, service = live_client

    chunks = list(
        client.invoke_stream(
            capability="example.v1",
            interface_version="1",
            method="run",
            request=TypedPayload(REQUEST_TYPE, b"request"),
            binding_id="binding-main",
            deadline_seconds=2.0,
        )
    )

    assert chunks == [TypedPayload(RESPONSE_TYPE, b"first")]
    assert len(service.requests) == 1


def test_typed_stream_rejects_eof_without_terminal_marker(live_client):
    client, service = live_client
    service.behavior = "stream-eof"

    with pytest.raises(CapabilityProtocolError, match="terminal marker"):
        list(
            client.invoke_stream(
                capability="example.v1",
                interface_version="1",
                method="run",
                request=TypedPayload(REQUEST_TYPE, b"request"),
                binding_id="binding-main",
                deadline_seconds=2.0,
            )
        )


def test_typed_stream_accepts_pre_execution_error_sequence_zero(live_client):
    client, service = live_client
    service.behavior = "stream-activation-error"

    with pytest.raises(CapabilityExecutionFailure, match="worker activation failed") as captured:
        list(
            client.invoke_stream(
                capability="example.v1",
                interface_version="1",
                method="run",
                request=TypedPayload(REQUEST_TYPE, b"request"),
                binding_id="binding-main",
                deadline_seconds=2.0,
            )
        )

    assert captured.value.code == execution_pb2.CapabilityExecutionError.CODE_ACTIVATION_FAILED


def test_model_provider_projection_preserves_optional_presence_and_type_urls():
    request = ChatCompletionRequest(
        messages=[
            ChatMessage(
                role=ChatMessage.ROLE_USER,
                content="hello",
                name="operator",
            )
        ],
        model="local-model",
        stream=True,
        temperature=0.0,
        max_tokens=16,
    )

    packed = pack_chat_request(request)
    decoded_request = ChatCompletionRequest.FromString(packed.value)
    response = ChatCompletionResponse(
        chunks=[
            ChatCompletionChunk(
                delta="world",
                finish_reason="stop",
                prompt_tokens=1,
                completion_tokens=2,
            )
        ]
    )
    decoded_response = unpack_chat_response(
        TypedPayload(
            CHAT_COMPLETION_RESPONSE_TYPE_URL,
            response.SerializeToString(),
        )
    )

    assert packed.type_url == CHAT_COMPLETION_REQUEST_TYPE_URL
    assert decoded_request.messages[0].role == ChatMessage.ROLE_USER
    assert decoded_request.HasField("temperature")
    assert decoded_request.temperature == 0.0
    assert decoded_response == response


def test_model_provider_projection_rejects_wrong_response_type():
    with pytest.raises(CapabilityProtocolError):
        unpack_chat_response(TypedPayload("type.googleapis.com/example.Wrong", b""))


@pytest.mark.parametrize("target", ["0.0.0.0:50051", "ces.example.com:443", "missing-port"])
def test_insecure_factory_rejects_non_loopback_target(target: str):
    with pytest.raises(ValueError):
        CapabilityExecutionClient.for_local_endpoint(target)
