# ADR-006: HardwareFacts Authority Is Node Agent

- **Status**: ACCEPTED
- **Date**: 2026-08-26

## Context
Different components previously ran ad-hoc `nvidia-smi` or PyTorch CUDA checks, producing inconsistent telemetry and missing non-NVIDIA accelerators.

## Decision
The Platform `Node Agent` is the single source-of-truth for physical node hardware discovery (`HardwareFacts`). Services and plugins consume immutable `HardwareFacts` snapshots from the Node Agent rather than running independent driver queries.

## Why
Guarantees consistent, cached, and vendor-agnostic hardware facts across all schedulers, evaluators, and preflight checks.

## Alternatives Considered
- *Per-Plugin Hardware Probes*: Let each plugin inspect `/dev` or invoke vendor tools directly. Rejected due to permission issues and inconsistent reporting.

## Consequences
- Preflight and evaluators accept `HardwareFacts` as input arguments during estimation.
