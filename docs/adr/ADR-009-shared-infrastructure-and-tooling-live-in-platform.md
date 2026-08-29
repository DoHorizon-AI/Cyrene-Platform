# ADR-009: Shared Infrastructure and Tooling Live in Platform

- **Status**: ACCEPTED
- **Date**: 2026-08-26

## Context
Dockerfiles, Compose stacks, Kubernetes manifests, CI governance scripts, and migration tooling were previously scattered across plugin and service directories.

## Decision
Shared infrastructure assets (`infrastructure/`) and shared engineering governance tooling (`tooling/`) are consolidated directly inside `Cyrene-Platform`.

## Why
Provides a single canonical location for cluster deployments, observability dashboards, boundary guards, and workspace tooling.

## Alternatives Considered
- *Standalone Tooling / Infra Repositories*: Rejected to avoid repository proliferation and cross-repository synchronization lag.

## Consequences
- Platform CI validates infrastructure syntax and enforces architecture governance across the workspace.
