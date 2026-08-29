# Generic capability application-event streams

The canonical `cy.plugin.v1.Envelope` carries a narrow application-event
facet for long-lived, Product-neutral capabilities:

- `Subscribe` starts a live subscription. Its `Envelope.request_id` is the
  subscription identity; `filter_payload` is opaque to Platform.
- `SubscribeAck` confirms that the worker accepted the subscription.
- `ApplicationEvent` carries the capability identity, event type, opaque
  payload bytes, and an `event_sequence`.
- `ApplicationEventStreamEnd` makes termination observable.

`Envelope.plugin_id` is the source identity and `Envelope.generation` plus
`fence_token` scope the stream. Event sequence numbers start at one and are
contiguous within one subscription and worker generation. Transport
`sequence_number` remains the ordering field for the envelope stream; it is
not reused as the application-event sequence.

## Delivery contract

Version one is live delivery with bounded buffering. It does not promise
exactly-once delivery, durable replay, a durable cursor, or resume after a
worker generation changes. Connector-specific cursor/resume behavior belongs
above this contract.

The caller supplies a positive bounded buffer limit. The host and worker each
enforce a finite queue. If the producer outruns the consumer, the stream ends
with `BACKPRESSURE`; buffered events may be drained before that terminal
condition is observed. Events are never silently converted into a successful
completion. A consumer that stops reading therefore gets an observable
termination once the bound is reached. Unsubscribe sends the canonical
`Cancel` and the caller waits for `CancelAck` plus the authoritative
`ApplicationEventStreamEnd`.

The caller distinguishes `NORMAL_COMPLETION`, `CANCELLED`,
`WORKER_UNAVAILABLE`, `WORKER_CRASH`, `PROTOCOL_FAILURE`,
`GENERATION_TERMINATED`, and `BACKPRESSURE`. A transport EOF or decode error
can prevent a worker from sending an end frame; the host synthesizes the
corresponding worker-crash or protocol-failure termination locally.

`ServiceSupervision` remains the only process lifecycle and restart/backoff
owner. This event client observes worker generation changes and terminates
affected streams; it does not restart a worker or create another supervision
authority.

## Authority boundary

Kernel `Event` / `SubscribeEvents` remains authoritative for platform and
control state such as worker lifecycle, leases, operations, resources,
generations, endpoints, and supervision.

`ApplicationEvent` is a separate capability data plane. Arbitrary capability
payloads are not copied into Kernel event history by this protocol. An
inbound application item remains owned by the capability that emitted it.
