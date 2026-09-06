"""
Canonical Python projection helpers for `model.provider.v1`.

Interface version 1 remains the compatible text/embedding surface. The
additive `chat_completion_v2` method uses the same typed messages for
structured tool calls and provider-reported stream usage.

`model_provider.proto` remains the only message-shape authority. This module
only supplies stable identifiers and typed pack/unpack helpers.

`model_provider.proto` 始终是唯一消息结构权威；本模块只提供稳定标识与类型化
pack/unpack 辅助函数。
"""

from __future__ import annotations

from google.protobuf.message import DecodeError

from ._generated.model_provider_pb2 import (
    ChatFunction,
    ChatCompletionChunk,
    ChatCompletionRequest,
    ChatCompletionResponse,
    ChatMessage,
    ChatTool,
    ChatToolCall,
    ChatToolCallDelta,
    ChatToolCallFunction,
    ChatToolChoice,
)
from .client import CapabilityProtocolError, TypedPayload


CAPABILITY_ID = "model.provider.v1"
INTERFACE_VERSION = "1"
CHAT_COMPLETION_METHOD = "chat_completion"
CHAT_COMPLETION_V2_METHOD = "chat_completion_v2"
CHAT_COMPLETION_V2_INTERFACE_VERSION = "2"
CHAT_COMPLETION_REQUEST_TYPE_URL = "type.googleapis.com/cyrene.model.provider.v1.ChatCompletionRequest"
CHAT_COMPLETION_RESPONSE_TYPE_URL = "type.googleapis.com/cyrene.model.provider.v1.ChatCompletionResponse"
CHAT_COMPLETION_CHUNK_TYPE_URL = "type.googleapis.com/cyrene.model.provider.v1.ChatCompletionChunk"


def pack_chat_request(request: ChatCompletionRequest) -> TypedPayload:
    """Pack a canonical chat request for `InvokeCapability`."""

    return TypedPayload(
        CHAT_COMPLETION_REQUEST_TYPE_URL,
        request.SerializeToString(),
    )


def unpack_chat_response(payload: TypedPayload) -> ChatCompletionResponse:
    """Decode a canonical chat response and reject another Any type."""

    if payload.type_url != CHAT_COMPLETION_RESPONSE_TYPE_URL:
        raise CapabilityProtocolError(f"model.provider.v1 returned unexpected payload type '{payload.type_url}'")
    try:
        return ChatCompletionResponse.FromString(payload.value)
    except DecodeError as error:
        raise CapabilityProtocolError("model.provider.v1 returned an undecodable ChatCompletionResponse") from error


def unpack_chat_chunk(payload: TypedPayload) -> ChatCompletionChunk:
    """Decode one typed chunk from the additive CES streaming RPC."""

    if payload.type_url != CHAT_COMPLETION_CHUNK_TYPE_URL:
        raise CapabilityProtocolError(f"model.provider.v1 returned unexpected stream payload type '{payload.type_url}'")
    try:
        return ChatCompletionChunk.FromString(payload.value)
    except DecodeError as error:
        raise CapabilityProtocolError("model.provider.v1 returned an undecodable ChatCompletionChunk") from error


def unpack_chat_chunk_v2(payload: TypedPayload) -> ChatCompletionChunk:
    """Decode one structured chat v2 stream chunk."""

    return unpack_chat_chunk(payload)


def pack_chat_request_v2(request: ChatCompletionRequest) -> TypedPayload:
    """Pack structured chat fields for the explicitly versioned v2 method."""

    return TypedPayload(
        CHAT_COMPLETION_REQUEST_TYPE_URL,
        request.SerializeToString(),
    )


def unpack_chat_response_v2(payload: TypedPayload) -> ChatCompletionResponse:
    """Decode a structured chat response using the same additive protobuf type."""

    return unpack_chat_response(payload)


__all__ = [
    "CAPABILITY_ID",
    "CHAT_COMPLETION_METHOD",
    "CHAT_COMPLETION_V2_INTERFACE_VERSION",
    "CHAT_COMPLETION_V2_METHOD",
    "CHAT_COMPLETION_REQUEST_TYPE_URL",
    "CHAT_COMPLETION_CHUNK_TYPE_URL",
    "CHAT_COMPLETION_RESPONSE_TYPE_URL",
    "INTERFACE_VERSION",
    "ChatCompletionChunk",
    "ChatCompletionRequest",
    "ChatCompletionResponse",
    "ChatFunction",
    "ChatMessage",
    "ChatTool",
    "ChatToolCall",
    "ChatToolCallDelta",
    "ChatToolCallFunction",
    "ChatToolChoice",
    "pack_chat_request",
    "pack_chat_request_v2",
    "unpack_chat_response",
    "unpack_chat_response_v2",
    "unpack_chat_chunk",
    "unpack_chat_chunk_v2",
]
