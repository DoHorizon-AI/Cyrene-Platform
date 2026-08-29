# Capability Execution Service v1

`cyrene.capability.v1.CapabilityExecutionService` is the Product-facing,
language-neutral boundary over the existing Platform capability runtime. It is
implemented by the Platform and transported with protobuf/gRPC. Products do
not launch workers and do not implement `cy.plugin.v1` framing.

The service is intentionally small. Resolution and activation are transparent
to the caller and there is no public `Resolve`, `Activate`, `StartWorker`,
`StopWorker`, `Heartbeat`, or `Close` method.

## Frozen public API

The source of truth is
`contracts/proto/cyrene/capability/v1/capability_execution.proto`.

| Public element | Why it exists |
| --- | --- |
| `InvokeCapability` | One request/response capability execution, including activation and cleanup. |
| `SubscribeCapabilityEvents` | One live server-streaming capability application-event subscription. The stream itself is the subscription lifecycle. |
| `InvokeCapabilityRequest.capability` | Exact capability identity used by the existing resolver. |
| `InvokeCapabilityRequest.interface_version` | Exact interface compatibility requirement used by the existing resolver. |
| `InvokeCapabilityRequest.method` | Capability operation name; the Platform forwards it without interpreting domain semantics. |
| `InvokeCapabilityRequest.request` | Required capability-owned `google.protobuf.Any` request payload. |
| `InvokeCapabilityResponse.response` | Capability-owned `google.protobuf.Any` result when execution succeeds. |
| `InvokeCapabilityResponse.error` | Stable generic execution failure, distinct from gRPC transport status and a domain result. |
| `SubscribeCapabilityEventsRequest.capability` | Exact capability identity for the application-event stream. |
| `SubscribeCapabilityEventsRequest.interface_version` | Exact interface compatibility requirement for the stream. |
| `SubscribeCapabilityEventsRequest.filter` | Optional capability-owned `Any` filter; the Platform does not inspect its fields. |
| `CapabilityEventStreamItem.application_event` | One normal application event branch of the stream `oneof`. |
| `CapabilityEventStreamItem.stream_end` | One terminal branch of the stream `oneof`; it cannot be confused with a normal event. |
| `CapabilityApplicationEvent.subscription_id` | Server-assigned opaque Product stream identity. It is not caller-controlled and is not a worker request ID. |
| `CapabilityApplicationEvent.capability` | Capability identity echoed on each event. |
| `CapabilityApplicationEvent.event_sequence` | Generation-scoped ordering evidence. |
| `CapabilityApplicationEvent.event_type` | Generic capability event kind; it is not a Product/domain field. |
| `CapabilityApplicationEvent.payload` | Capability-owned `Any` payload. |
| `CapabilityApplicationEvent.generation` | Abstract provider execution generation for diagnostics. It is never a PID or process handle. |
| `CapabilityApplicationEvent.source_id` | Abstract logical source identity for diagnostics. It contains no stdio/process identity. |
| `CapabilityEventStreamEnd.subscription_id` | Correlates the terminal item with the server-assigned identity on preceding events. |
| `CapabilityEventStreamEnd.capability` | Preserves the requested capability when no event was delivered before termination. |
| `CapabilityEventStreamEnd.generation` | Identifies the abstract source generation that ended; it is not a process identity. |
| `CapabilityEventStreamEnd.source_id` | Identifies the abstract logical source that ended without exposing worker mechanics. |
| `CapabilityEventStreamEnd.reason` | Stable terminal category for normal completion, cancellation, unavailable source, crash, protocol failure, generation termination, or backpressure. |
| `CapabilityEventStreamEnd.message` | Human-readable diagnostic context; callers must branch on `reason` and `error.code`, not parse this text. |
| `CapabilityEventStreamEnd.error` | Optional stable generic execution error for non-normal termination; it remains separate from gRPC status and capability data. |
| `CapabilityExecutionError.code` | Machine-readable generic Platform execution category. |
| `CapabilityExecutionError.message` | Human-readable diagnostic context for the generic category; it is not a domain result. |

`CapabilityEventStreamEndReason` values are all present because each is an
observable lifecycle outcome of the frozen worker event substrate:
`NORMAL_COMPLETION`, `CANCELLED`, `CAPABILITY_UNAVAILABLE`, `WORKER_CRASH`,
`PROTOCOL_FAILURE`, `GENERATION_TERMINATED`, and `BACKPRESSURE`. The
`CapabilityExecutionError.Code` values are the stable unary/terminal error
categories: `CAPABILITY_UNAVAILABLE`, `ACTIVATION_FAILED`,
`PROTOCOL_MISMATCH`, `INVALID_REQUEST`, `EXECUTION_FAILURE`,
`WORKER_CRASHED`, `TIMEOUT`, `CANCELLED`, `BACKPRESSURE`,
`STREAM_TERMINATED`, and `GENERATION_TERMINATED`; the zero values are required
protobuf sentinels and are never used as successful outcomes.

There is no caller-provided invocation ID, worker subscription ID, timeout
field, queue-size hint, or process-control field. Invocation cancellation is
the native gRPC call cancellation. Stream cancellation is the native gRPC
server-stream cancellation. The Platform allocates all internal correlation
IDs privately.

## `Any` payload policy

`Any` is the only public request, response, filter, and event payload slot. A
typed capability may use its generated protobuf message with the normal
`Any.type_url` and serialized `Any.value` contract. The current compatibility
adapter forwards `Any.value` to the existing generic worker byte payload and
uses `type.cyrene.io/cyrene.capability.v1.OpaquePayload` for worker event bytes
when the worker wire has no type URL. No arbitrary public `payload_type` field
is introduced, and worker serialization does not become the Product API
authority.

## Lifecycle and deadlines

The execution path is:

```text
Product gRPC client
  -> CapabilityExecutionService
  -> CapabilityRegistry / CapabilityResolver
  -> PluginManifest
  -> CapabilityWorkerActivator
  -> CapabilityWorkerClient
  -> cy.plugin.v1 worker
```

The service reads the native `grpc-timeout` deadline and passes that same
deadline-derived duration to the worker client; a cancelled RPC triggers the
existing cooperative worker `Cancel` path. If a client omits a deadline, the
existing Platform worker option is used as the bounded process-safety fallback;
it is not a second request field or Product timeout semantic.

`CapabilityWorkerActivator` owns one activation and handshake. The execution
service owns the request-scoped worker session and bounded graceful cleanup.
`ServiceSupervisor` remains the sole owner of the execution service process's
restart/backoff policy. It does not compete with the execution service by
restarting individual request-scoped workers, and the execution service does
not implement another restart/backoff loop.

## Application events versus Kernel control events

These are separate planes:

- **Kernel control event**: platform lifecycle, worker liveness, lease,
  operation, resource, generation, endpoint, or supervision state. These
  remain under `KernelAuthorityService.SubscribeEvents` and its history.
- **Capability application event**: capability-owned data delivered by
  `SubscribeCapabilityEvents`. It is not copied into Kernel event history by
  default and carries no domain-specific assumptions.

The service uses the existing generic application-event facet in
`cy.plugin.v1.Envelope` internally. It does not create a second worker
protocol.

## Delivery and backpressure

V1 is live delivery only. It provides bounded buffering, ordering within one
worker generation, observable disconnect, and no durable replay, exactly-once,
cursor, or resume guarantee. Connector-specific durable cursors belong above
this service.

The buffer is Platform configured with a hard maximum of
`MAX_APPLICATION_EVENT_BUFFER_CAPACITY`; the public request has no queue-size
hint. The service reserves one bounded terminal slot, so a full event buffer
cannot hide the terminal condition. If the producer outruns the consumer, the
stream ends with `BACKPRESSURE` and generic error code `BACKPRESSURE` rather
than silently dropping an event. A caller cancellation ends the worker
subscription and closes the stream. Worker EOF/crash, protocol failure, and
generation change each produce their corresponding terminal reason when that
terminal item can be delivered; a transport disconnect is itself observable
to the gRPC client.

## Error layers

1. A gRPC transport/client status represents connection, HTTP/2, or native
   deadline/cancellation failure.
2. `CapabilityExecutionError` represents stable Platform execution outcomes:
   capability unavailable, activation failed, protocol mismatch, invalid
   request, execution failure, worker crash, timeout, cancelled,
   backpressure, stream termination, or generation termination.
3. `Any` response/event values represent capability-owned results and payloads.

The Platform never serializes a Rust error enum as public API authority and
never maps a domain result into a generic gRPC transport failure merely because
the result is domain-specific.
