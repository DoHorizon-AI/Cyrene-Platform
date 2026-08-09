# Kernel

The kernel is the small Rust trust base. It owns host-level lifecycle and
isolation, not AI business decisions.

Allowed responsibilities:

- plugin process discovery, launch, restart, shutdown, and watchdog behavior;
- cgroups, namespace, device-node, filesystem, and process isolation adapters;
- read-only hardware discovery and telemetry adapters;
- local IPC and distributed transport primitives;
- resource assignment enforcement supplied by the framework;
- structured lifecycle and failure events.

Not allowed here: model selection, dataset processing, training policy,
inference engines, gateway business rules, user interfaces, or evaluation logic.
