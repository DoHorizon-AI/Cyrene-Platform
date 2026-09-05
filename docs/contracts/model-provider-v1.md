# `model.provider.v1` typed payload contract

Status: **EXPERIMENTAL**

Chat completion and embedding are stateless model computations on the existing
`model.provider.v1`; neither creates a parallel capability or transport.
The configured provider endpoint, credentials, model defaults, lifecycle, and
rate-limit domain are already owned by one model-provider binding. Keeping the
method on that capability lets `openai-main` and `ollama-local` remain stable
configured identities while worker generation changes independently.

The canonical payload schema is
`contracts/proto/cyrene/model/provider/v1/model_provider.proto`. Products use
`cyrene.capability.v1.CapabilityExecutionService`; the payload file declares no
parallel transport service.

## Canonical identifiers

| Purpose | Identifier |
| --- | --- |
| Capability | `model.provider.v1` |
| Interface version | `1` |
| Chat method | `chat_completion` |
| Chat request `Any` type | `type.googleapis.com/cyrene.model.provider.v1.ChatCompletionRequest` |
| Chat result `Any` type | `type.googleapis.com/cyrene.model.provider.v1.ChatCompletionResponse` |
| Embedding method | `embeddings` |
| Embedding request `Any` type | `type.googleapis.com/cyrene.model.provider.v1.EmbeddingsRequest` |
| Embedding result `Any` type | `type.googleapis.com/cyrene.model.provider.v1.EmbeddingsResponse` |

## Chat completion semantics

`ChatCompletionRequest.messages` preserves message order and uses the stable
SYSTEM, USER, ASSISTANT, and TOOL roles. V1 content is text; `name` and
`tool_call_id` retain proto optional presence. `model`, `temperature`, and
`max_tokens` are also optional, so omission remains distinct from an explicit
zero value. Unsupported multimodal or tool-definition shapes fail before
provider dispatch rather than being silently dropped.

`ChatCompletionResponse.chunks` preserves the provider's ordered normalized
deltas, finish reason, and optional token counts. CES invocation is unary in
this version: chunk boundaries survive for Product SSE projection, but the
contract does not yet promise live first-token delivery over CES.

CES forwards the request `Any.type_url` through the generic worker protocol.
The provider must reject a non-canonical chat request URL and must return the
canonical chat response URL. Platform forwards this metadata without decoding
model-provider fields.

The request's optional `model` is only a provider-supported selector. When it
is omitted, the selected `CapabilityBinding` supplies its configured default
for the `embeddings` operation. A binding that also serves `chat_completion`
may configure a different default for that operation; an embedding request
must never inherit the chat default implicitly. This is not a Product routing,
fallback, memory, or context-selection policy.
The response records the provider-resolved model identifier and vector
dimension as computation facts. Usage metadata is deliberately absent because
the current canonical model-provider contract has no stable cross-provider
embedding usage definition.

## Embedding batch semantics

`inputs` is ordered and contains one or more UTF-8 strings. An empty batch or
zero-length string is `INVALID_INPUT`. Whitespace is data: Platform does not
trim, normalize, tokenize, or chunk it.

Response vector position N corresponds exactly to request input position N.
Every successful response has exactly one vector per input, a non-zero
`dimensions`, and every vector length equals that value. Providers must not
reorder, truncate, or partially return a batch. A partial upstream failure
fails the whole request.

V1 sets no universal numeric batch maximum because supported limits differ by
provider and model. An implementation may enforce its documented/configured
maximum, but an oversized request fails atomically as `INVALID_INPUT`; it is
never silently split or truncated. Product chunking remains outside Platform.

## Errors

Errors have two existing layers:

- `EmbeddingError` is a typed domain result and distinguishes
  `CODE_INVALID_INPUT`, `CODE_MODEL_NOT_AVAILABLE`, `CODE_DIMENSION_MISMATCH`,
  `CODE_PROVIDER_ERROR`, and `CODE_RATE_LIMITED`. `retry_after` is present only
  when the
  provider supplied a meaningful duration.
- `CapabilityExecutionError` remains the generic CES execution authority for
  `TIMEOUT`, `CANCELLED`, activation/protocol failures, unavailable bindings,
  and worker/runtime termination.

This separation follows the existing CES convention and does not create a
parallel execution error system.

A `model.provider.v1` binding is not assumed to implement every experimental
method. If the selected binding does not expose `embeddings`, invocation fails
as the existing generic CES `INVALID_REQUEST`/unsupported-operation outcome.
CES must not retry another binding, and the worker must not report
`MODEL_NOT_AVAILABLE`: that domain code applies only after the embeddings
method is supported and its requested or configured embeddings model cannot be
resolved.

## Binding and runtime identity

The optional CES `binding_id` selects the configured provider instance. Zero
matches fails deterministically; exactly one matching configured binding may
be selected implicitly for compatibility; more than one matching binding is
ambiguous and must fail. Explicit targeting never falls back to another
binding.

The binding remains stable across worker restart. CES source identity,
generation, PID, executable, stdio framing, and provider transport details are
not model-provider identity and are not exposed in these payloads.

## Boundary

This contract computes embedding facts only. It contains no memory records or
levels, persona, conversation/session identity, knowledge documents, chunks,
retrieval/ranking policy, vector database, pgvector/FAISS schema, RAG policy,
top-K, distance threshold, retention, or persistence semantics.

## TCK and maturity

The deterministic contract TCK proves chat enum/optional/`Any` stability plus
embedding batch behavior, dimensions, invalid inputs, model errors, typed
`Any`, and the error-layer split. The generic CES TCK is also normative
for two bindings, unknown and mismatched bindings, ambiguous implicit
selection, deadline/cancellation, and binding stability across generation
changes.

Graduation from **EXPERIMENTAL** requires two independent provider
implementations, two Product consumers, passing Rust/C#/Python/JVM contract
generation and conformance gates, and no required breaking schema change.
