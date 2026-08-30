#!/usr/bin/env python3
"""Validate generated Python bindings; generated sources stay in a temp dir."""

from __future__ import annotations

import sys
from pathlib import Path


if len(sys.argv) != 2:
    raise SystemExit("usage: run_tck.py <generated-python-root>")
sys.path.insert(0, str(Path(sys.argv[1]).resolve()))

from google.protobuf.any_pb2 import Any  # noqa: E402
from cyrene.capability.v1 import capability_execution_pb2  # noqa: E402
from cyrene.model.provider.v1 import model_provider_pb2  # noqa: E402


embedding_request = model_provider_pb2.EmbeddingsRequest(
    inputs=["alpha", "beta"], model="deterministic-model"
)
payload = Any()
payload.Pack(embedding_request)
old_request = capability_execution_pb2.InvokeCapabilityRequest(
    capability="model.provider.v1",
    interface_version="1",
    method="embeddings",
    request=payload,
)
old_round_trip = capability_execution_pb2.InvokeCapabilityRequest.FromString(
    old_request.SerializeToString()
)
assert not old_round_trip.HasField("binding_id")

targeted = capability_execution_pb2.InvokeCapabilityRequest()
targeted.CopyFrom(old_request)
targeted.binding_id = "openai-main"
targeted_round_trip = capability_execution_pb2.InvokeCapabilityRequest.FromString(
    targeted.SerializeToString()
)
assert targeted_round_trip.HasField("binding_id")
assert targeted_round_trip.binding_id == "openai-main"
unpacked_request = model_provider_pb2.EmbeddingsRequest()
assert targeted_round_trip.request.Unpack(unpacked_request)
assert list(unpacked_request.inputs) == ["alpha", "beta"]

response = model_provider_pb2.EmbeddingsResponse(
    embeddings=model_provider_pb2.EmbeddingBatch(
        vectors=[
            model_provider_pb2.EmbeddingVector(values=[1.0, 2.0]),
            model_provider_pb2.EmbeddingVector(values=[3.0, 4.0]),
        ],
        dimensions=2,
        model="deterministic-model",
    )
)
packed_response = Any()
packed_response.Pack(response)
unpacked_response = model_provider_pb2.EmbeddingsResponse()
assert packed_response.Unpack(unpacked_response)
assert len(unpacked_response.embeddings.vectors) == len(embedding_request.inputs)

print("model.provider.v1 embedding generated Python contract TCK: PASS")
