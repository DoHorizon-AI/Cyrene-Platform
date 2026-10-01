# Runtime Maintenance Library

This module owns the durable update gate, activity-source catalog, task registry, and broker protocol.

| File | Responsibility |
| --- | --- |
| `lib.rs` | Shared file-locked state, journal recovery, readiness decisions, and admission lifecycle. |
| `main.rs` | Unix-socket broker and one-shot JSON IPC client used by `docker exec`. |

Read `lib.rs` for the state model, then `main.rs` for the broker transport.
