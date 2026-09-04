"""
Canonical Python projection helpers for `training.engine.v1`.

`training_backend.proto` remains the only message-shape authority. This module
only supplies stable identifiers and typed pack/unpack helpers.

`training_backend.proto` 始终是唯一消息结构权威；本模块只提供稳定标识与
类型化 pack/unpack 辅助函数。
"""

from __future__ import annotations

from google.protobuf.message import DecodeError

from ._generated.training_backend_pb2 import (
    RunTrainingStepRequest,
    RunTrainingStepResponse,
)
from .client import CapabilityProtocolError, TypedPayload


CAPABILITY_ID = "training.engine.v1"
INTERFACE_VERSION = "1"
RUN_TRAINING_STEP_METHOD = "run_training_step"
RUN_TRAINING_STEP_REQUEST_TYPE_URL = "type.googleapis.com/cy.plugin.v1.RunTrainingStepRequest"
RUN_TRAINING_STEP_RESPONSE_TYPE_URL = "type.googleapis.com/cy.plugin.v1.RunTrainingStepResponse"


def pack_run_training_step_request(request: RunTrainingStepRequest) -> TypedPayload:
    """Pack a canonical training-step request for `InvokeCapability`."""

    return TypedPayload(
        RUN_TRAINING_STEP_REQUEST_TYPE_URL,
        request.SerializeToString(),
    )


def unpack_run_training_step_response(payload: TypedPayload) -> RunTrainingStepResponse:
    """Decode a canonical training-step response and reject another Any type."""

    if payload.type_url != RUN_TRAINING_STEP_RESPONSE_TYPE_URL:
        raise CapabilityProtocolError(f"training.engine.v1 returned unexpected payload type '{payload.type_url}'")
    try:
        return RunTrainingStepResponse.FromString(payload.value)
    except DecodeError as error:
        raise CapabilityProtocolError("training.engine.v1 returned an undecodable RunTrainingStepResponse") from error


__all__ = [
    "CAPABILITY_ID",
    "INTERFACE_VERSION",
    "RUN_TRAINING_STEP_METHOD",
    "RUN_TRAINING_STEP_REQUEST_TYPE_URL",
    "RUN_TRAINING_STEP_RESPONSE_TYPE_URL",
    "RunTrainingStepRequest",
    "RunTrainingStepResponse",
    "pack_run_training_step_request",
    "unpack_run_training_step_response",
]
