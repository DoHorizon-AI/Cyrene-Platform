# Kernel

The kernel is the small, pure-safe Rust decision base. It owns lifecycle policy
and authorization, not Linux privilege or AI business decisions.

Allowed responsibilities:

- lease/fence validation, launch/stop decisions, instance state, and watchdog policy;
- generic UDS clients for separately supervised sandbox and hardware adapters;
- protocol validation and local IPC primitives;
- resource assignment enforcement supplied by the framework;
- structured lifecycle and failure events.

Not allowed here: direct process spawning, cgroup/namespace/device operations,
pidfd/BPF/prctl, hardware discovery, model selection, dataset processing,
training policy, inference engines, gateway business rules, user interfaces,
or evaluation logic.
