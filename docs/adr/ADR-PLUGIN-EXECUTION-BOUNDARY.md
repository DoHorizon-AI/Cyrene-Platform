# ADR-PLUGIN-EXECUTION-BOUNDARY: Installable Plugin Isolation

- Status: Approved / Normative
- Date: 2026-08-10
- Updated: 2026-09-09

## Decision

Installable business Plugins run outside Platform and Kernel processes. The
Plugin package owns its language runtime adapter, direct data-plane protocol,
payload contracts, and entrypoint. Platform owns verified installation,
compatibility selection, process lifecycle, health, permissions, and an opaque
`connection_ref`.

A managed service package supplies a language-neutral launch command. Platform
passes only generic readiness arguments and never imports a Python module,
loads a JVM/.NET assembly, or dispatches a capability method. Product clients
call the Plugin endpoint directly.

## Required invariants

- launch uses a verified package-relative executable or prepared runtime and an
  argv array, never a shell command string;
- identity, interface version, package digest, permissions, and readiness match
  before the endpoint is published;
- crash, timeout, cancellation, and protocol corruption are observable failures
  and cannot crash Kernel;
- stdout readiness is bounded, logs use stderr, and `connection_ref` is opaque;
- capability request/response/stream bytes never enter Platform control APIs.
