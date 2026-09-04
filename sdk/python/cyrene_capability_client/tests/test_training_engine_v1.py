"""
Contract tests for the canonical `training.engine.v1` Python projection.

canonical `training.engine.v1` Python 投影的契约测试。
"""

from __future__ import annotations

import pytest

from cyrene_capability_client import CapabilityProtocolError, TypedPayload
from cyrene_capability_client.training_engine_v1 import (
    CAPABILITY_ID,
    INTERFACE_VERSION,
    RUN_TRAINING_STEP_METHOD,
    RUN_TRAINING_STEP_REQUEST_TYPE_URL,
    RUN_TRAINING_STEP_RESPONSE_TYPE_URL,
    RunTrainingStepRequest,
    RunTrainingStepResponse,
    pack_run_training_step_request,
    unpack_run_training_step_response,
)


def test_training_engine_projection_preserves_canonical_identifiers_and_payloads() -> None:
    request = RunTrainingStepRequest(
        runtime_manifest_json='{"runtime":"worker-v1"}',
        training_revision_json='{"revision":"rev-1"}',
    )

    packed = pack_run_training_step_request(request)
    decoded_request = RunTrainingStepRequest.FromString(packed.value)
    response = RunTrainingStepResponse(checkpoint_metadata_json='{"artifact":"cy://checkpoint/1"}')
    decoded_response = unpack_run_training_step_response(
        TypedPayload(
            RUN_TRAINING_STEP_RESPONSE_TYPE_URL,
            response.SerializeToString(),
        )
    )

    assert CAPABILITY_ID == "training.engine.v1"
    assert INTERFACE_VERSION == "1"
    assert RUN_TRAINING_STEP_METHOD == "run_training_step"
    assert packed.type_url == RUN_TRAINING_STEP_REQUEST_TYPE_URL
    assert decoded_request == request
    assert decoded_response == response


def test_training_engine_projection_rejects_wrong_response_type() -> None:
    with pytest.raises(CapabilityProtocolError, match="unexpected payload type"):
        unpack_run_training_step_response(TypedPayload("type.googleapis.com/example.Wrong", b""))


def test_training_engine_projection_rejects_malformed_response() -> None:
    with pytest.raises(CapabilityProtocolError, match="undecodable RunTrainingStepResponse"):
        unpack_run_training_step_response(TypedPayload(RUN_TRAINING_STEP_RESPONSE_TYPE_URL, b"\xff"))
