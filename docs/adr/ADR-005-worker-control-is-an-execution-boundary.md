# ADR-005: WorkerControl Is an Execution Boundary

- **Status**: ACCEPTED
- **Date**: 2026-08-26

## Context
Long-running AI daemons (like inference servers or vector search workers) need lifecycle supervision without exposing unrestricted process spawning.

## Decision
`WorkerControl` is the authoritative platform boundary for supervising long-running out-of-process workers. It handles startup health handshakes, Lease renewal, and clean process tree termination via OS Job Objects / Cgroups.

## Why
AI workers frequently spawn CUDA child processes. Unsupervised raw process spawning leads to leaked GPU VRAM and zombie processes.

## Alternatives Considered
- *Raw `subprocess.Popen` in Services*: Rejected because service-level crashes leave orphaned child workers running indefinitely on GPU nodes.

## Consequences
- All long-running service backends must be launched and monitored via `WorkerControl`.
