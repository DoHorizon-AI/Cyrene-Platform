"""
Canonical Python projection helpers for `model.provider.v1`.

`model_provider.proto` remains the only message-shape authority. This module
only supplies stable identifiers and typed pack/unpack helpers.

`model_provider.proto` 始终是唯一消息结构权威；本模块只提供稳定标识与类型化
pack/unpack 辅助函数。
"""

from __future__ import annotations

from google.protobuf.message import DecodeError

from ._generated.model_provider_pb2 import (
    ChatCompletionChunk,
    ChatCompletionRequest,
    ChatCompletionResponse,
    ChatMessage,
)
from .client import CapabilityProtocolError, TypedPayload


CAPABILITY_ID = "model.provider.v1"
INTERFACE_VERSION = "1"
CHAT_COMPLETION_METHOD = "chat_completion"
CHAT_COMPLETION_REQUEST_TYPE_URL = "type.googleapis.com/cyrene.model.provider.v1.ChatCompletionRequest"
CHAT_COMPLETION_RESPONSE_TYPE_URL = "type.googleapis.com/cyrene.model.provider.v1.ChatCompletionResponse"


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


__all__ = [
    "CAPABILITY_ID",
    "CHAT_COMPLETION_METHOD",
    "CHAT_COMPLETION_REQUEST_TYPE_URL",
    "CHAT_COMPLETION_RESPONSE_TYPE_URL",
    "INTERFACE_VERSION",
    "ChatCompletionChunk",
    "ChatCompletionRequest",
    "ChatCompletionResponse",
    "ChatMessage",
    "pack_chat_request",
    "unpack_chat_response",
]
