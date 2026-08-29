# `message.connector.v1` contract

Status: **EXPERIMENTAL**

`message.connector.v1` defines capability-specific protobuf payloads for
receiving and sending messages through connector implementations. It does not
define a transport service. Products use the existing
`cyrene.capability.v1.CapabilityExecutionService` and pack these messages in
`google.protobuf.Any`.

The canonical schema is
`contracts/proto/cyrene/message/connector/v1/message_connector.proto`.
`plugin/v1/notification.proto` remains an unrelated legacy notification
placeholder and is not part of this capability.

## Canonical identifiers

| Purpose | Identifier |
| --- | --- |
| Capability | `message.connector.v1` |
| Interface version | `1` |
| Outbound method | `send_message` |
| Inbound application-event type | `inbound_message` |
| Inbound `Any` type | `type.googleapis.com/cyrene.message.connector.v1.InboundMessagePayload` |
| Outbound request `Any` type | `type.googleapis.com/cyrene.message.connector.v1.SendMessageRequest` |
| Outbound result `Any` type | `type.googleapis.com/cyrene.message.connector.v1.DeliveryResult` |

These are the only V1 identifiers. No aliases are defined.

Inbound delivery is:

```text
SubscribeCapabilityEvents(capability = "message.connector.v1",
                          interface_version = "1")
  -> CapabilityApplicationEvent.event_type = "inbound_message"
  -> CapabilityApplicationEvent.payload = Any(InboundMessagePayload)
```

Outbound delivery is:

```text
InvokeCapability(capability = "message.connector.v1",
                 interface_version = "1",
                 method = "send_message",
                 request = Any(SendMessageRequest))
  -> Any(DeliveryResult)
```

## Evidence-bounded data model

`ConversationScope` carries only vendor/protocol identity, connector account
identity, a vendor-resolvable conversation identity, and the observed private,
group, or channel kind. The first canonical vendor identifier is
`onebot.v11`; NapCat is an implementation of that protocol, not a second
contract identity.

`InboundMessagePayload` contains vendor message identity, conversation facts,
sender identity/display name, ordered content, one optional reply reference,
and one bounded vendor extension. It does not assign Product session or
persistence identity.

`MessageContentPart` preserves ordering for text, mentions, images, and files.
Reply is deliberately absent from this `oneof`: OneBot represents reply as a
segment, but the characterization did not prove segment position changes its
cross-vendor meaning. `ReplyReference` is therefore the single authority on
both inbound and outbound messages.

`SendMessageRequest` identifies a target conversation and supplies ordered
content, an optional reply reference, and optional connector facts. It has no
timeout, retry, authorization, or Product policy fields; gRPC deadline and
cancellation remain Capability Execution Service execution semantics.

## Attachment ownership and resolution

`AttachmentReference` has exactly two forms:

- `remote_uri`: an absolute HTTP(S) URI that the connector runtime can fetch.
  Relative paths, `file:` URIs, and paths meaningful only inside a Product,
  daemon, host, or container are invalid.
- `vendor_media`: an opaque media identifier owned and resolved by the named
  vendor and account. It is not a Platform Artifact and is not assumed to be
  portable to another connector account.

V1 does not define inline base64 data, arbitrary bytes, upload protocols, or
durable Artifact identity. If a connector cannot produce one of these two
resolvable references, it must not invent a local path; any non-resolvable
vendor observation may only be retained as a bounded textual vendor fact.

## Vendor extensions

`VendorExtension` preserves textual protocol facts that cannot be normalized
safely. It has deterministic bounds:

- vendor identifier: at most 64 UTF-8 bytes;
- at most 32 uniquely named facts;
- fact name: non-empty and at most 64 UTF-8 bytes;
- fact value: at most 2048 UTF-8 bytes;
- aggregate fact name/value data: at most 8192 UTF-8 bytes.

Extensions must not contain secrets, access tokens, cryptographic material,
raw payload bytes, webhook bodies, or transport session state. Authentication,
signature verification/decryption, framing, cursor ownership, reconnect, and
heartbeat behavior remain connector implementation mechanisms.

## Delivery results

`DeliveryResult` reports connector/vendor facts only:

- `ACCEPTED`: the vendor accepted or submitted the request. It does not mean
  that an end user received the message.
- `REJECTED`: the vendor definitively rejected the request.
- `RATE_LIMITED`: the vendor rejected or deferred the request because of a
  rate limit; `retry_after` is present only when the vendor supplied a useful
  duration.

The optional vendor message ID, reason, and bounded extension preserve the
available acknowledgement facts. `DELIVERED` is intentionally absent.
Timeout, cancellation, worker crash, activation failure, protocol mismatch,
and backpressure remain generic Capability Execution Service outcomes and are
not duplicated as delivery statuses.

## Plane and policy boundaries

Inbound connector payloads are capability application events. They are not
Kernel control events and are not copied into Kernel event history by default.
V1 inherits live, bounded, generation-scoped ordering with observable stream
termination and no durable replay guarantee from Capability Execution Service.

The schema contains no wake/reply decision, authorization/admin policy,
persona, Product session mapping, persistence, memory/RAG, model/tool routing,
command semantics, Iris behavior, or Product lifecycle. A connector returns
protocol facts; the Product decides their meaning.

## Maturity and graduation

The contract remains **EXPERIMENTAL**. Graduation to
**CONTRACT_CANDIDATE** requires all of the following objective evidence:

1. an official OneBot v11/NapCat connector implementation;
2. a real AstrBot no-fallback inbound and outbound E2E through
   `CapabilityExecutionService`;
3. a second-vendor proof, expected to be WeCom;
4. the connector-specific TCK passing for both implementations; and
5. no required breaking protobuf change discovered by those proofs.

Stable status requires a later, explicit compatibility review and is not
implied by meeting the candidate criteria.
