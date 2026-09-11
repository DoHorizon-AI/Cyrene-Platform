# Kernel Reliability Recovery

Phase 3 keeps recovery evidence separate from semantic authority. A restarted Kernel never adopts a pre-crash Worker
merely because a process record exists. It raises the fence floor, discovers Provider/runtime reality, classifies the
record, and reconciles from the normal authority path.

| Fact                                              | Recovery classification                                                              | Storage / source                       |
|---------------------------------------------------|--------------------------------------------------------------------------------------|----------------------------------------|
| Provider logical identity and session generation  | reconstruct from Provider re-registration; stale sessions cannot overwrite a new one | Provider registration                  |
| Provider inventory snapshot generation            | reconstruct from a current Provider observation; obsolete after a session reconnect  | Provider observation                   |
| Resource identity and generation                  | reconstruct from current Provider inventory                                          | Provider observation                   |
| Lease state and fence                             | must persist; fence floor is never reused                                            | runtime journal plus resource ledger   |
| Worker identity, generation, and runtime evidence | persist as unclosed runtime evidence; never auto-adopt                               | runtime journal `RuntimeProcessRecord` |
| Operation terminal state                          | must persist before terminal event visibility                                        | durable semantic event store           |
| Endpoint authority metadata                       | reconstruct only from a current Worker and active Lease; stale data is revoked       | reconciliation                         |
| Event source and cursor                           | source epoch and event sequence persist with each durable event                      | durable event store                    |

Durable-before-visible boundaries are lease acquisition, lease revoke/fence advance, Worker LOST, operation terminal
transition, and semantic event publication. The JSONL composition adapter calls `sync_data` after each append; the
Kernel depends only on `RuntimeJournalSink` and `DurableEventStore` ports.

## Restart policy: Discover → Classify → Recover → Reconcile → Ready

On restart the composition root advances the Kernel epoch before opening authority listeners, seeds the resource
manager above every durable fence, and discovers unclosed runtime records. Those records are stale evidence, not
authority. Provider inventory refresh and Provider-scoped reconciliation then decide whether a Worker is missing,
stale, or still valid under a newly issued authority. A changed Kernel epoch changes the event source, so clients
obtain a fresh authority snapshot before resuming a cursor.

1. **Discover** — advance `node_epoch` in the runtime journal (`begin_epoch`), recover the durable fence floor and
   unclosed `RuntimeProcessRecord` evidence, and probe current hardware/Provider reality. The fence floor is taken
   from the durably persisted journal (`recover()` returns `max(historical fence) + 1`); a fresh manager seeded with
   that floor allocates a strictly greater token than any lease that existed before the restart.
2. **Classify** — `FileRuntimeJournal::classify_recovery` maps discovered runtime evidence against recorded
   `RuntimeProcessRecord`s into one of four verdicts: `Valid`, `Stale`, `Unknown`, `Foreign`. Classification is
   evidence review only; it never rehydrates a Worker into the new process.
3. **Recover** — `recover_before_listeners` runs before any listener opens. `Stale` processes with exactly matched
   sandbox evidence are reaped and journaled terminal. `Valid`, `Unknown`, and `Foreign` processes are never adopted
   or killed by the Kernel; their presence blocks startup fail-closed for operator action
   (`RECOVERY_VALID_PROCESS_UNADOPTED`, `RECOVERY_UNKNOWN_PROCESS`, `RECOVERY_FOREIGN_PROCESS`). A valid old managed
   process therefore never becomes a permanent orphan: it is surfaced for operator decision rather than silently
   re-owned or silently left running under new authority.
4. **Reconcile** — Provider re-registration, inventory observation, and the normal `reconcile_provider` path rebuild
   Worker/Lease/Operation/Endpoint authority from current reality. A `ResourceFactsOnly` provider refreshes only
   resource facts and never participates in Worker reconciliation. Per-adapter Provider isolation means one adapter
   failing to `ADAPTER_UNAVAILABLE` does not hide another adapter's facts, and a reconnect invalidates only the
   session-bound evidence, not resource or snapshot numbers.
5. **Ready** — once reconciliation converges (no pending `TerminateStaleWorker`), the Kernel serves a fresh snapshot
   whose `source` generation equals the new epoch and whose `cursor` starts at the new epoch's sequence. Old
   authority is never restored: the fresh snapshot contains no old Lease/Worker/Operation/Endpoint, and any replacement
   allocation is issued with a fence strictly greater than the old fence.

## Durable event source and cursor semantics

For a source that has durable event history, that ordered history is the replay and snapshot-cursor source of truth;
the in-process 256-event queue is only a cache for stores that do not support history reads. JSONL replay reads are
serialized with appends, require an exact `(namespace, source)` match, and reject malformed or out-of-order durable
records. A read error or corruption fails closed: the Kernel must not fall back to cached events or use historical
events to rebuild active authority state. Events from an older epoch remain audit history only; a new epoch starts with
newly reconciled authority.

`Snapshot @ C + read_events(C) = current state` is the single consistency boundary. `LocalKernelAuthority::snapshot`
captures the event cursor **before** reading authority state, and `read_events` replays from that cursor using the
same `DurableEventStore` ordering. Because a transition publishes its durable event only after mutating state, the
snapshot is always a superset of every event already counted in the cursor, so a concurrent transition can never vanish
from both the snapshot and the incremental replay.

`EventCursor::status_against` defines three outcomes for `read_events`:

| Status         | Condition                                                            | Client action                                                                 |
|----------------|----------------------------------------------------------------------|-------------------------------------------------------------------------------|
| `Current`      | cursor source matches and no gap below the retained history         | apply returned events; persist `next_sequence` as the new cursor              |
| `Gap`          | retained durable history no longer contains `cursor.sequence + 1`    | discard the incremental model; `GetSnapshot`; persist the new cursor; resume  |
| `SourceChanged`| cursor source (epoch/generation) differs from the live Kernel       | discard the incremental model; `GetSnapshot`; persist the new cursor; resume  |

## Client recovery: GAP / SOURCE_CHANGED

A client that receives `GAP` or `SOURCE_CHANGED` discards its incremental model, obtains the existing
`LocalKernelAuthority::snapshot`, reconciles that snapshot, persists its fresh cursor, and resumes `read_events` from
it. `GAP` means the cursor predates retained durable history; `SOURCE_CHANGED` means the cursor belongs to another
Kernel epoch (e.g. after a two-epoch restart). A durable replay read failure is not resumable from memory and requires
the durable store to be repaired or made available before rebuilding client state.

### Client recovery matrix

| Signal                                  | Incremental model | New cursor | Next client step                                  |
|-----------------------------------------|------------------|------------|---------------------------------------------------|
| `read_events` returns `Current`        | keep             | `next_sequence` | persist cursor; continue                         |
| `read_events` returns `Gap`            | discard          | `snapshot.cursor` | `GetSnapshot`; persist; resume `read_events`    |
| `read_events` returns `SourceChanged`  | discard          | `snapshot.cursor` | `GetSnapshot`; persist; resume `read_events`     |
| durable replay read error               | discard          | —          | repair/restore durable store; then `GetSnapshot`  |

The two-epoch restart golden test proves this end to end: an epoch N snapshot carrying a lease, Worker, Operation, and
Endpoint; a persisted cursor; restart into epoch N+1 where the old cursor yields `SourceChanged`; a fresh snapshot
carrying no old authority; and a replacement allocation whose fence exceeds the old fence.
