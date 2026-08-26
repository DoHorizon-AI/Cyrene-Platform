# ADR-003: Operation Is a Generic Execution Primitive

- **Status**: ACCEPTED
- **Date**: 2026-08-26

## Context
We needed a unified mechanism to represent batch tasks, setup scripts, data downloads, and fine-tuning steps across heterogeneous clusters.

## Decision
An `Operation` is defined as an ephemeral, time-bounded execution unit managed by the Kernel supervisor within an isolated sandbox. It produces an exit status, logs, and optional output artifacts.

## Why
Whether downloading a model checkpoint, building a Docker image, or running a 10-epoch training pass, the Kernel executes them as generic Operations governed by Lease tokens and signal propagation.

## Alternatives Considered
- *Domain-Specific Job Types*: Custom `TrainJob`, `EvalJob`, `BuildJob` in the Kernel. Rejected to prevent domain logic leakage.

## Consequences
- High-level multi-phase jobs are compiled into a sequence of `PlanStep` attempts executed as Operations.
