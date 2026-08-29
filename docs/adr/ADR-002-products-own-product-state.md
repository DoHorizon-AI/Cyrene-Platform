# ADR-002: Products Own Product State

- **Status**: ACCEPTED
- **Date**: 2026-08-26

## Context
When orchestrating multi-step training or auto-scaled serving deployments, questions arose regarding where desired and observed state should reside.

## Decision
Product services (Cyrene-Yield, Cyrene-Reactor, Cyrene-Exchange) own their respective desired and observed state machines. Cyrene-Yield owns `TrainingRun` and epoch history; Cyrene-Reactor owns `Deployment` desired replicas; Cyrene-Exchange owns route topology.

## Why
Domain state models evolve rapidly with AI research. Separating product state from platform mechanisms allows services to iterate independently without requiring platform contract schema migrations.

## Alternatives Considered
- *Centralized Platform Database*: Store all product tables in Cyrene-Platform. Rejected because it violates domain boundary encapsulation.

## Consequences
- Services persist their own state and communicate with Platform via versioned SDKs and capability contracts.
