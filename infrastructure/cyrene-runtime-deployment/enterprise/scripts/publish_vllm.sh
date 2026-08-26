#!/usr/bin/env bash
# Build and publish the Pro vLLM image from the single-repository root.

set -euo pipefail

REGISTRY="${1:-docker.io}"
IMAGE_NAME="${2:-cy-llm-pro-vllm}"
TAG="${3:-latest}"
CUDA_VERSION="${4:-12.4.1}"
CUDA_TAG="${5:-124}"
BASE_IMAGE="${PRO_VLLM_BASE_IMAGE:-nvidia/cuda:${CUDA_VERSION}-cudnn-runtime-ubuntu22.04}"

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(CDPATH= cd -- "${SCRIPT_DIR}/../../.." && pwd)"
FULL_IMAGE="${REGISTRY}/${IMAGE_NAME}:${TAG}-cuda${CUDA_VERSION}"

printf 'Building Pro vLLM image: %s\n' "${FULL_IMAGE}"
printf 'Base image: %s\n' "${BASE_IMAGE}"
printf 'PyTorch CUDA tag: cu%s\n' "${CUDA_TAG}"

docker build \
  --build-arg BASE_IMAGE="${BASE_IMAGE}" \
  --build-arg CUDA_TAG="${CUDA_TAG}" \
  -f "${REPO_ROOT}/infrastructure/cyrene-runtime-deployment/enterprise/docker/Dockerfile.vllm" \
  -t "${FULL_IMAGE}" \
  "${REPO_ROOT}"

# Authenticate separately when the selected registry requires it.
docker push "${FULL_IMAGE}"
printf 'Published: %s\n' "${FULL_IMAGE}"
